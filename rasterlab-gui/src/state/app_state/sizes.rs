//! Sizing library photos on disk, for the Recently Deleted entry and the
//! multi-photo detail panel.
//!
//! Each photo sized is a stat, and on a library kept on a network share a
//! stat can block for as long as the share is not answering.  So the work
//! runs on one long-lived worker rather than a thread per request: a hung
//! share then holds up one thread, not one more per click.  Requests that
//! queue up behind a slow one are coalesced — only the latest for each figure
//! is run, since an older one's answer would be stale on arrival anyway.

use std::sync::mpsc;

use rasterlab_core::panic_guard;
use rasterlab_library::{DiskSize, PhotoId};

use super::{AppState, BgMessage};
use crate::state::library_state::{DiskUsage, SizeTicket};

/// The worker only stats files and walks small row lists.
const SIZE_WORKER_STACK: usize = 1024 * 1024;

/// Which of the library's [`DiskUsage`] figures a sizing answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SizeTarget {
    RecentlyDeleted,
    Selection,
}

impl SizeTarget {
    const COUNT: usize = 2;

    fn index(self) -> usize {
        match self {
            SizeTarget::RecentlyDeleted => 0,
            SizeTarget::Selection => 1,
        }
    }
}

type Measure = Box<dyn FnOnce() -> Option<DiskSize> + Send>;

/// One sizing for the worker to run.
pub(super) struct SizeJob {
    target: SizeTarget,
    token: u64,
    measure: Measure,
}

/// Keep the latest job for each target, in the order the targets first
/// appear, and drop the rest unrun: the [`DiskUsage`] they answer has already
/// issued a newer token, so their answers would only be thrown away.
fn coalesce(jobs: impl IntoIterator<Item = SizeJob>) -> Vec<SizeJob> {
    let mut latest: [Option<SizeJob>; SizeTarget::COUNT] = Default::default();
    let mut order = Vec::with_capacity(SizeTarget::COUNT);
    for job in jobs {
        let slot = job.target.index();
        if latest[slot].is_none() {
            order.push(slot);
        }
        latest[slot] = Some(job);
    }
    order
        .into_iter()
        .filter_map(|slot| latest[slot].take())
        .collect()
}

impl AppState {
    /// Keep the Recently Deleted size in step with what is in it, sizing it
    /// again whenever its set of photos changes.
    pub(crate) fn sync_recently_deleted_size(&mut self) {
        let Some(lib) = self.library.library.clone() else {
            return;
        };
        let state = &mut self.library;
        let Some(ticket) = state
            .recently_deleted_size
            .retarget(&state.recently_deleted_ids)
        else {
            return;
        };
        // One call that cannot be stopped part way; the token is what keeps a
        // superseded answer out.
        self.submit_size_job(SizeTarget::RecentlyDeleted, ticket, move || {
            lib.recently_deleted_bytes().ok().map(|files| DiskSize {
                files,
                originals: None,
            })
        });
    }

    /// Keep the size of a multi-photo selection in step with it.
    ///
    /// The photos are looked up in the index by the worker, not in the grid's
    /// rows: the selection can hold photos the current filter hides, and the
    /// total has to match the "N photos selected" above it.
    pub(crate) fn sync_selection_size(&mut self) {
        let Some(lib) = self.library.library.clone() else {
            return;
        };
        let mut photos: Vec<PhotoId> = self.library.selected.clone();
        photos.sort_unstable();
        let Some(ticket) = self.library.selection_size.retarget(&photos) else {
            return;
        };
        let stop = std::sync::Arc::clone(&ticket.stop);
        self.submit_size_job(SizeTarget::Selection, ticket, move || {
            lib.photos_size(&photos, &stop).ok().flatten()
        });
    }

    fn submit_size_job(
        &mut self,
        target: SizeTarget,
        ticket: SizeTicket,
        measure: impl FnOnce() -> Option<DiskSize> + Send + 'static,
    ) {
        let job = SizeJob {
            target,
            token: ticket.token,
            measure: Box::new(measure),
        };
        let undelivered = match self.size_worker() {
            Some(tx) => tx.send(job).err().map(|mpsc::SendError(job)| job),
            None => Some(job),
        };
        // No worker to run it: settle the figure as unavailable rather than
        // leave a spinner waiting on an answer that will never come.
        if let Some(job) = undelivered {
            self.size_req_tx = None;
            self.on_size_measured(job.target, job.token, None);
        }
    }

    /// The sender into the size worker, starting it on first use.
    fn size_worker(&mut self) -> Option<&mpsc::Sender<SizeJob>> {
        if self.size_req_tx.is_none() {
            let (tx, rx) = mpsc::channel::<SizeJob>();
            let bg_tx = self.bg_tx.clone();
            let ctx = self.ctx.clone();
            let spawned = std::thread::Builder::new()
                .name("rasterlab-size".into())
                .stack_size(SIZE_WORKER_STACK)
                .spawn(move || {
                    // Ends when the sender is dropped with the app.
                    while let Ok(first) = rx.recv() {
                        let queued = std::iter::once(first).chain(rx.try_iter());
                        for job in coalesce(queued) {
                            let total = panic_guard::guard(job.measure).unwrap_or_else(|panic| {
                                eprintln!("sizing photos panicked: {panic}");
                                None
                            });
                            let _ = bg_tx.send(BgMessage::SizeMeasured {
                                target: job.target,
                                token: job.token,
                                total,
                            });
                            ctx.request_repaint();
                        }
                    }
                });
            match spawned {
                Ok(_) => self.size_req_tx = Some(tx),
                Err(error) => eprintln!("could not start the size worker: {error}"),
            }
        }
        self.size_req_tx.as_ref()
    }

    pub(super) fn on_size_measured(
        &mut self,
        target: SizeTarget,
        token: u64,
        total: Option<DiskSize>,
    ) {
        let usage: &mut DiskUsage = match target {
            SizeTarget::RecentlyDeleted => &mut self.library.recently_deleted_size,
            SizeTarget::Selection => &mut self.library.selection_size,
        };
        usage.finish(token, total);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(target: SizeTarget, token: u64) -> SizeJob {
        SizeJob {
            target,
            token,
            measure: Box::new(|| None),
        }
    }

    #[test]
    fn queued_sizings_coalesce_to_the_latest_per_figure() {
        use SizeTarget::{RecentlyDeleted as Deleted, Selection};
        for (queued, expected) in [
            (vec![], vec![]),
            (vec![(Selection, 1)], vec![(Selection, 1)]),
            (
                vec![(Selection, 1), (Selection, 2), (Selection, 3)],
                vec![(Selection, 3)],
            ),
            (
                vec![(Selection, 1), (Deleted, 7), (Selection, 2)],
                vec![(Selection, 2), (Deleted, 7)],
            ),
        ] {
            let ran: Vec<(SizeTarget, u64)> =
                coalesce(queued.iter().map(|&(target, token)| job(target, token)))
                    .into_iter()
                    .map(|job| (job.target, job.token))
                    .collect();
            assert_eq!(ran, expected, "queued {queued:?}");
        }
    }
}
