//! A small cache of decoded `.rlab` projects for editor navigation.
//!
//! Arrowing through library photos in the editor used to read and decode each
//! project from scratch. On network-attached storage the read alone can take
//! seconds, so every key press stalled — including the one that returns to the
//! photo just left. [`ProjectCache`] keeps the few projects the user is likely
//! to want next: the one being opened, the one just left, and the next one in
//! the direction of travel, which a background worker reads ahead of time.
//!
//! Entries are keyed by path and validated against the file's modification
//! time and length, taken *before* the read so a write that races the read
//! makes the entry look stale rather than fresh. Some network filesystems
//! report mtimes too coarsely to catch a same-size rewrite, so writers the
//! editor knows about also [`invalidate`](ProjectCache::invalidate) explicitly;
//! the stamp is the backstop for everything else.
//!
//! A load in flight is recorded as [`Slot::Loading`], and an open of the same
//! path waits for it rather than reading the file a second time. Whoever owns
//! that slot holds a [`Ticket`] whose drop clears the slot and wakes waiters,
//! so a failed, panicked, or never-started loader cannot leave anyone waiting.
//!
//! Holding an arrow key down starts opens faster than slow storage finishes
//! them, and pruning drops the slots those opens were waiting on. An open
//! that the user has already moved past must then give up rather than read
//! the file itself, or each key press would add another read to the queue
//! ahead of the photo the user actually stops on.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError},
    time::SystemTime,
};

use rasterlab_core::{Image, formats::FormatRegistry, project::RlabFile};

use super::workers;

/// Most projects held at once. Navigation needs three — the photo being
/// opened, the one just left, and the one prefetched — and each costs a
/// decoded image plus its embedded original, easily 100 MB+ for a RAW.
pub(super) const CACHE_CAPACITY: usize = 3;

/// Thread name for read-ahead workers, so they are identifiable in a debugger
/// or profiler alongside `rasterlab-load`.
const PREFETCH_THREAD: &str = "rasterlab-prefetch";

/// A loaded project, shared between the cache and the document that opened it.
#[derive(Debug, Clone)]
pub(super) struct LoadedProject {
    pub rlab: Arc<RlabFile>,
    pub image: Arc<Image>,
}

/// What a loader produces: the project, or a message fit for the status bar.
pub(super) type LoadResult = Result<LoadedProject, String>;

/// Why [`ProjectCache::load`] gave up without loading. Nobody sees it: the
/// open it answers has been superseded, so its result is dropped unread.
pub(super) const SUPERSEDED: &str = "superseded by a newer open";

/// Read and decode the project at `path`. The loader the editor uses both for
/// opens and for read-ahead, so a prefetched entry is exactly what an open
/// would have produced.
pub(super) fn read_project(path: &Path) -> LoadResult {
    let rlab = RlabFile::read(path).map_err(|e| e.to_string())?;
    let hint = rlab.meta.source_path.as_deref().map(Path::new);
    let image = FormatRegistry::with_builtins()
        .decode_bytes(&rlab.original_bytes, hint)
        .map_err(|e| e.to_string())?;
    Ok(LoadedProject {
        rlab: Arc::new(rlab),
        image: Arc::new(image),
    })
}

/// Identity of a file's contents as far as a stat can tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    modified: SystemTime,
    len: u64,
}

impl Stamp {
    /// `None` when the file cannot be stat'ed or the platform has no mtime; an
    /// entry that cannot be validated is not worth caching.
    fn of(path: &Path) -> Option<Self> {
        let metadata = std::fs::metadata(path).ok()?;
        Some(Self {
            modified: metadata.modified().ok()?,
            len: metadata.len(),
        })
    }
}

enum Slot {
    /// A loader holding the ticket numbered `token` is reading this path.
    Loading { token: u64 },
    Ready {
        stamp: Stamp,
        project: LoadedProject,
    },
}

struct Entry {
    slot: Slot,
    /// Recency for eviction: bumped when the entry is created or hit.
    last_used: u64,
}

#[derive(Default)]
struct Inner {
    entries: HashMap<PathBuf, Entry>,
    /// Source of both recency stamps and loader tokens.
    counter: u64,
    disabled: bool,
}

impl Inner {
    fn tick(&mut self) -> u64 {
        self.counter += 1;
        self.counter
    }

    /// Whether the ticket `token` still owns the `Loading` slot for `path`. It
    /// no longer does once the entry was invalidated, pruned, or evicted, and
    /// the loader's result must then not be stored: it may predate whatever
    /// made the entry go away.
    fn is_loading(&self, path: &Path, token: u64) -> bool {
        matches!(
            self.entries.get(path),
            Some(Entry { slot: Slot::Loading { token: t }, .. }) if *t == token
        )
    }

    /// Evict least-recently-used entries until at most `capacity` remain,
    /// sparing `keep`. Returns whether anything was removed.
    ///
    /// Only `Ready` entries are evicted. Evicting a `Loading` one would not
    /// stop its read, only orphan it, and whoever waits on it would wake and
    /// start a second read of the same file — which may evict the next one in
    /// turn. The bound can therefore be exceeded while reads are in flight,
    /// and is restored as each of them lands.
    fn evict_to(&mut self, capacity: usize, keep: &Path) -> bool {
        let mut evicted = false;
        while self.entries.len() > capacity {
            let Some(oldest) = self
                .entries
                .iter()
                .filter(|(path, entry)| {
                    path.as_path() != keep && matches!(entry.slot, Slot::Ready { .. })
                })
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(path, _)| path.clone())
            else {
                break;
            };
            self.entries.remove(&oldest);
            evicted = true;
        }
        evicted
    }
}

struct Shared {
    inner: Mutex<Inner>,
    /// Signalled whenever a `Loading` slot resolves or disappears.
    changed: Condvar,
}

/// Cache of decoded projects shared between the UI thread and load workers.
/// Cloning yields another handle to the same cache.
#[derive(Clone)]
pub(super) struct ProjectCache {
    shared: Arc<Shared>,
}

impl Default for ProjectCache {
    fn default() -> Self {
        Self {
            shared: Arc::new(Shared {
                inner: Mutex::new(Inner::default()),
                changed: Condvar::new(),
            }),
        }
    }
}

impl ProjectCache {
    /// A loader never runs under the lock, so a panic cannot leave the map
    /// half-updated; a poisoned lock is safe to keep using.
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.shared
            .inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Change the cache's contents and wake every waiter, who re-examine the
    /// slot they were waiting on.
    fn update<T>(&self, change: impl FnOnce(&mut Inner) -> T) -> T {
        let result = change(&mut self.lock());
        self.shared.changed.notify_all();
        result
    }

    /// Mark `path` as loading and return the ticket that owns it.
    fn begin(&self, inner: &mut Inner, path: PathBuf) -> Ticket {
        let token = inner.tick();
        let last_used = inner.tick();
        inner.entries.insert(
            path.clone(),
            Entry {
                slot: Slot::Loading { token },
                last_used,
            },
        );
        if inner.evict_to(CACHE_CAPACITY, &path) {
            self.shared.changed.notify_all();
        }
        Ticket {
            cache: self.clone(),
            path,
            token,
        }
    }

    /// Load `path` through the cache: wait for a read already in flight, reuse
    /// a fresh entry, or run `loader` and keep what it produces.
    ///
    /// `is_current` says whether the open asking for this is still the one the
    /// user wants. It is checked before any read and on every wake-up, and once
    /// it is false the call returns [`SUPERSEDED`] without touching the file.
    ///
    /// Blocks on file I/O and on other loaders, so call it from a worker, never
    /// the UI thread.
    pub(super) fn load(
        &self,
        path: &Path,
        is_current: impl Fn() -> bool,
        loader: impl FnOnce(&Path) -> LoadResult,
    ) -> LoadResult {
        if !is_current() {
            return Err(SUPERSEDED.into());
        }
        if self.lock().disabled {
            return loader(path);
        }
        let Some(stamp) = Stamp::of(path) else {
            return loader(path);
        };
        let ticket = {
            let mut inner = self.lock();
            loop {
                // Re-checked after the stat, which can stall on a network
                // share, and after every wait, since the user may have moved
                // on in the meantime.
                if !is_current() {
                    return Err(SUPERSEDED.into());
                }
                if inner.disabled {
                    drop(inner);
                    return loader(path);
                }
                let hit = match inner.entries.get(path).map(|entry| &entry.slot) {
                    Some(Slot::Loading { .. }) => {
                        inner = self
                            .shared
                            .changed
                            .wait(inner)
                            .unwrap_or_else(PoisonError::into_inner);
                        continue;
                    }
                    Some(Slot::Ready { stamp: s, project }) if *s == stamp => Some(project.clone()),
                    _ => None,
                };
                if let Some(project) = hit {
                    let now = inner.tick();
                    if let Some(entry) = inner.entries.get_mut(path) {
                        entry.last_used = now;
                    }
                    return Ok(project);
                }
                break self.begin(&mut inner, path.to_path_buf());
            }
        };
        ticket.run(stamp, loader)
    }

    /// Start reading `path` on a background worker so a later [`load`] finds it
    /// ready, or waits for it instead of starting a second read.
    ///
    /// Does nothing if the path is already cached or in flight; whether a
    /// cached entry is still fresh is left to the open that uses it.
    ///
    /// [`load`]: Self::load
    pub(super) fn prefetch<F>(&self, path: PathBuf, loader: F)
    where
        F: FnOnce(&Path) -> LoadResult + Send + 'static,
    {
        let ticket = {
            let mut inner = self.lock();
            if inner.disabled || inner.entries.contains_key(&path) {
                return;
            }
            self.begin(&mut inner, path)
        };
        // A spawn failure drops the closure, and with it the ticket, which
        // clears the slot just as a failed read would.
        let _ = std::thread::Builder::new()
            .name(PREFETCH_THREAD.into())
            .stack_size(workers::IMAGE_WORKER_STACK)
            .spawn(move || {
                if let Some(stamp) = Stamp::of(&ticket.path) {
                    // A failure is not reported: nothing is waiting on a
                    // prefetch, and the open that wanted it will run into the
                    // same problem and report it properly.
                    let _ = ticket.run(stamp, loader);
                }
            });
    }

    /// Keep only the entries for `keep`, releasing everything else.
    pub(super) fn retain(&self, keep: &[&Path]) {
        self.update(|inner| {
            inner
                .entries
                .retain(|path, _| keep.contains(&path.as_path()))
        });
    }

    /// Forget `path`, because it has been rewritten.
    pub(super) fn invalidate(&self, path: &Path) {
        self.update(|inner| inner.entries.remove(path));
    }

    /// Forget everything.
    pub(super) fn clear(&self) {
        self.update(|inner| inner.entries.clear());
    }

    /// Turn the cache on or off. Turning it off releases every entry at once,
    /// and the loaders still in flight find their slot gone and keep nothing.
    pub(super) fn set_enabled(&self, enabled: bool) {
        self.update(|inner| {
            inner.disabled = !enabled;
            if !enabled {
                inner.entries.clear();
            }
        });
    }

    #[cfg(test)]
    pub(super) fn contains(&self, path: &Path) -> bool {
        self.lock().entries.contains_key(path)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.lock().entries.len()
    }
}

/// Ownership of one `Loading` slot. Dropping it without completing removes the
/// slot — on error, on panic, or when a worker never starts — and wakes
/// everyone waiting on it, who then load the path themselves.
struct Ticket {
    cache: ProjectCache,
    path: PathBuf,
    token: u64,
}

impl Ticket {
    /// Run `loader` and, if this ticket still owns the slot, store the result.
    ///
    /// A panic is left to unwind to the caller's own handling — for an open,
    /// the worker wrapper that reports it — and the ticket's drop clears the
    /// slot on the way out.
    fn run(self, stamp: Stamp, loader: impl FnOnce(&Path) -> LoadResult) -> LoadResult {
        let result = loader(&self.path)?;
        let mut inner = self.cache.lock();
        if inner.is_loading(&self.path, self.token)
            && let Some(entry) = inner.entries.get_mut(&self.path)
        {
            entry.slot = Slot::Ready {
                stamp,
                project: result.clone(),
            };
            // Reads that finish together can overshoot the bound, since
            // `begin` never evicts one in flight; trim as each lands.
            inner.evict_to(CACHE_CAPACITY, &self.path);
        }
        Ok(result)
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        self.cache.update(|inner| {
            if inner.is_loading(&self.path, self.token) {
                inner.entries.remove(&self.path);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc,
    };
    use std::time::Duration;

    /// Long enough that a missed wake-up fails the test instead of hanging it.
    const WAIT: Duration = Duration::from_secs(10);

    /// For opens the user has not moved past.
    fn current() -> bool {
        true
    }

    fn project(width: u32) -> LoadedProject {
        LoadedProject {
            rlab: Arc::new(RlabFile::new(
                rasterlab_core::project::RlabMeta::new("test", None::<String>, width, 1),
                Vec::new(),
                Vec::new(),
                0,
                None,
            )),
            image: Arc::new(Image::new(width, 1)),
        }
    }

    /// A loader that counts its calls and yields an image `width` wide.
    fn counting(calls: &Arc<AtomicUsize>, width: u32) -> impl FnOnce(&Path) -> LoadResult + use<> {
        let calls = Arc::clone(calls);
        move |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(project(width))
        }
    }

    struct Files {
        _dir: tempfile::TempDir,
        paths: Vec<PathBuf>,
    }

    fn files(n: usize) -> Files {
        let dir = tempfile::tempdir().unwrap();
        let paths = (0..n)
            .map(|i| {
                let path = dir.path().join(format!("{i}.rlab"));
                std::fs::write(&path, b"project").unwrap();
                path
            })
            .collect();
        Files { _dir: dir, paths }
    }

    fn width(result: LoadResult) -> u32 {
        result.unwrap().image.width
    }

    #[test]
    fn a_fresh_entry_is_reused_and_a_changed_file_is_reread() {
        // (rewrite the file between loads?, expected loader calls, width seen)
        for (rewrite, calls_expected, width_expected) in [(false, 1, 1), (true, 2, 2)] {
            let files = files(1);
            let path = &files.paths[0];
            let cache = ProjectCache::default();
            let calls = Arc::new(AtomicUsize::new(0));

            assert_eq!(width(cache.load(path, current, counting(&calls, 1))), 1);
            if rewrite {
                // A different length changes the stamp however coarse the
                // filesystem's mtime is.
                std::fs::write(path, b"a longer project").unwrap();
            }
            let second = cache.load(path, current, counting(&calls, 2));

            assert_eq!(width(second), width_expected, "rewrite={rewrite}");
            assert_eq!(
                calls.load(Ordering::SeqCst),
                calls_expected,
                "rewrite={rewrite}"
            );
        }
    }

    #[test]
    fn a_hit_shares_the_decoded_image_instead_of_copying_it() {
        let files = files(1);
        let cache = ProjectCache::default();
        let first = cache
            .load(&files.paths[0], current, |_| Ok(project(1)))
            .unwrap();
        let second = cache
            .load(&files.paths[0], current, |_| {
                panic!("must be served from the cache")
            })
            .unwrap();
        assert!(Arc::ptr_eq(&first.image, &second.image));
    }

    #[test]
    fn an_explicit_invalidation_forces_a_reread() {
        // The stamp cannot see a same-size rewrite within the mtime
        // granularity, which is what the explicit call is for.
        let files = files(1);
        let path = &files.paths[0];
        let cache = ProjectCache::default();
        let calls = Arc::new(AtomicUsize::new(0));
        cache.load(path, current, counting(&calls, 1)).unwrap();
        cache.invalidate(path);
        assert_eq!(width(cache.load(path, current, counting(&calls, 2))), 2);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn failures_are_returned_and_not_cached() {
        let files = files(1);
        let path = &files.paths[0];
        let cache = ProjectCache::default();
        assert_eq!(
            cache
                .load(path, current, |_| Err("unreadable".into()))
                .unwrap_err(),
            "unreadable"
        );
        assert!(!cache.contains(path));

        let previous_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            cache.load(path, current, |_| panic!("decoder blew up"))
        }));
        std::panic::set_hook(previous_hook);
        assert!(panicked.is_err(), "the panic reaches the open's worker");
        assert!(!cache.contains(path));
    }

    #[test]
    fn the_cache_never_holds_more_than_its_capacity() {
        let files = files(CACHE_CAPACITY + 2);
        let cache = ProjectCache::default();
        for path in &files.paths {
            cache.load(path, current, |_| Ok(project(1))).unwrap();
            assert!(cache.len() <= CACHE_CAPACITY);
        }
        // Least recently used goes first.
        assert!(!cache.contains(&files.paths[0]));
        assert!(cache.contains(files.paths.last().unwrap()));
    }

    #[test]
    fn a_hit_counts_as_use_for_eviction() {
        let files = files(CACHE_CAPACITY + 1);
        let cache = ProjectCache::default();
        for path in &files.paths[..CACHE_CAPACITY] {
            cache.load(path, current, |_| Ok(project(1))).unwrap();
        }
        cache
            .load(&files.paths[0], current, |_| unreachable!())
            .unwrap();
        cache
            .load(&files.paths[CACHE_CAPACITY], current, |_| Ok(project(1)))
            .unwrap();
        assert!(cache.contains(&files.paths[0]));
        assert!(!cache.contains(&files.paths[1]));
    }

    #[test]
    fn eviction_spares_reads_in_flight() {
        let files = files(CACHE_CAPACITY + 1);
        let cache = ProjectCache::default();
        let releases: Vec<_> = files.paths[..CACHE_CAPACITY]
            .iter()
            .map(|path| blocked_prefetch(&cache, path, Ok(project(1))))
            .collect();
        cache
            .load(&files.paths[CACHE_CAPACITY], current, |_| Ok(project(2)))
            .unwrap();
        for path in &files.paths[..CACHE_CAPACITY] {
            assert!(cache.contains(path), "a Loading slot was evicted");
        }
        for release in releases {
            release.send(()).unwrap();
        }
        // Once they land the bound holds again. Loading each path waits for
        // its read to finish (or re-reads one that was trimmed meanwhile).
        for path in &files.paths[..CACHE_CAPACITY] {
            cache.load(path, current, |_| Ok(project(3))).unwrap();
        }
        assert!(cache.len() <= CACHE_CAPACITY, "{} entries", cache.len());
    }

    #[test]
    fn a_superseded_open_never_reads() {
        let files = files(1);
        let cache = ProjectCache::default();
        let result = cache.load(&files.paths[0], || false, |_| panic!("must not read"));
        assert_eq!(result.unwrap_err(), SUPERSEDED);
    }

    #[test]
    fn holding_an_arrow_key_reads_each_photo_at_most_once() {
        // Replays what editor navigation does per key press — prune, prefetch
        // the next photo, open this one — faster than storage answers: every
        // read blocks until all the presses are in.
        const PRESSES: usize = 6;
        const OPENED: u32 = 1;
        const PREFETCHED: u32 = 2;
        let files = files(PRESSES + 2);
        let cache = ProjectCache::default();
        let reads = Arc::new(Mutex::new(HashMap::<PathBuf, usize>::new()));
        let gate = Arc::new(std::sync::RwLock::new(()));
        let held = gate.write().unwrap();
        let latest = Arc::new(AtomicUsize::new(0));

        let gated = |width: u32| {
            let reads = Arc::clone(&reads);
            let gate = Arc::clone(&gate);
            move |path: &Path| -> LoadResult {
                *reads.lock().unwrap().entry(path.to_path_buf()).or_default() += 1;
                drop(gate.read().unwrap());
                Ok(project(width))
            }
        };

        // Photo 0 is open; each press moves one photo to the right.
        let mut opens = Vec::new();
        for press in 1..=PRESSES {
            latest.store(press, Ordering::SeqCst);
            let [left, opening, next] = [press - 1, press, press + 1].map(|i| &files.paths[i]);
            cache.retain(&[left, opening, next]);
            cache.prefetch(next.clone(), gated(PREFETCHED));

            let (cache, path, latest) = (cache.clone(), opening.clone(), Arc::clone(&latest));
            let loader = gated(OPENED);
            opens.push(std::thread::spawn(move || {
                cache.load(&path, || latest.load(Ordering::SeqCst) == press, loader)
            }));
        }
        drop(held);
        let mut results: Vec<LoadResult> = opens.into_iter().map(|t| t.join().unwrap()).collect();

        let landing = results.pop().unwrap();
        assert_eq!(
            width(landing),
            PREFETCHED,
            "the landing photo must use its prefetch"
        );
        for (path, count) in reads.lock().unwrap().iter() {
            assert_eq!(*count, 1, "{} was read {count} times", path.display());
        }
    }

    #[test]
    fn retain_keeps_only_the_named_paths() {
        let files = files(3);
        let cache = ProjectCache::default();
        for path in &files.paths {
            cache.load(path, current, |_| Ok(project(1))).unwrap();
        }
        cache.retain(&[&files.paths[0], &files.paths[2]]);
        let kept: Vec<bool> = files.paths.iter().map(|p| cache.contains(p)).collect();
        assert_eq!(kept, [true, false, true]);
    }

    /// Start a prefetch whose loader blocks until the returned sender fires,
    /// and wait until it is actually running.
    fn blocked_prefetch(
        cache: &ProjectCache,
        path: &Path,
        outcome: LoadResult,
    ) -> mpsc::Sender<()> {
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let (started_tx, started_rx) = mpsc::channel();
        cache.prefetch(path.to_path_buf(), move |_| {
            started_tx.send(()).unwrap();
            release_rx.recv_timeout(WAIT).unwrap();
            outcome
        });
        started_rx
            .recv_timeout(WAIT)
            .expect("prefetch never started");
        release_tx
    }

    /// Run `cache.load(path)` on another thread, with a loader that yields
    /// width 99 and counts its calls.
    fn load_in_background(
        cache: &ProjectCache,
        path: &Path,
        calls: &Arc<AtomicUsize>,
    ) -> mpsc::Receiver<LoadResult> {
        let (tx, rx) = mpsc::channel();
        let cache = cache.clone();
        let path = path.to_path_buf();
        let loader = counting(calls, 99);
        std::thread::spawn(move || tx.send(cache.load(&path, current, loader)).unwrap());
        rx
    }

    #[test]
    fn an_open_waits_for_the_prefetch_in_flight_and_shares_its_outcome() {
        // (what the prefetch produces, width the waiting open ends with,
        //  times the open had to read the file itself)
        let cases: [(LoadResult, u32, usize); 2] = [
            // Success: the open uses the prefetched project, no second read.
            (Ok(project(7)), 7, 0),
            // Failure: the open is woken and reads the file itself.
            (Err("network went away".into()), 99, 1),
        ];
        for (outcome, width_expected, reads_expected) in cases {
            let files = files(1);
            let path = &files.paths[0];
            let cache = ProjectCache::default();
            let release = blocked_prefetch(&cache, path, outcome);

            let calls = Arc::new(AtomicUsize::new(0));
            let opened = load_in_background(&cache, path, &calls);
            assert!(
                opened.recv_timeout(Duration::from_millis(100)).is_err(),
                "the open must wait while the prefetch is in flight"
            );

            release.send(()).unwrap();
            let result = opened.recv_timeout(WAIT).expect("waiter was never woken");
            assert_eq!(width(result), width_expected);
            assert_eq!(calls.load(Ordering::SeqCst), reads_expected);
        }
    }

    #[test]
    fn a_panicking_prefetch_wakes_waiters_and_clears_its_slot() {
        let files = files(1);
        let path = &files.paths[0];
        let cache = ProjectCache::default();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let (started_tx, started_rx) = mpsc::channel();

        let previous_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        cache.prefetch(path.clone(), move |_| {
            started_tx.send(()).unwrap();
            release_rx.recv_timeout(WAIT).unwrap();
            panic!("decoder blew up")
        });
        started_rx.recv_timeout(WAIT).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let opened = load_in_background(&cache, path, &calls);
        release_tx.send(()).unwrap();
        let result = opened.recv_timeout(WAIT).expect("waiter was never woken");
        std::panic::set_hook(previous_hook);

        assert_eq!(width(result), 99);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_prefetch_of_a_cached_or_loading_path_does_nothing() {
        let files = files(1);
        let path = &files.paths[0];
        let cache = ProjectCache::default();
        let release = blocked_prefetch(&cache, path, Ok(project(1)));
        // Would panic on the worker and clear the slot if it ever ran.
        cache.prefetch(path.clone(), |_| panic!("duplicate prefetch"));
        release.send(()).unwrap();
        // The first prefetch's result is what an open finds.
        assert_eq!(width(cache.load(path, current, |_| Ok(project(2)))), 1);
    }

    #[test]
    fn a_prefetch_invalidated_in_flight_does_not_store_its_result() {
        let files = files(1);
        let path = &files.paths[0];
        let cache = ProjectCache::default();
        let release = blocked_prefetch(&cache, path, Ok(project(1)));
        cache.invalidate(path);
        release.send(()).unwrap();
        // The open is not blocked by the orphaned prefetch and reads afresh.
        assert_eq!(width(cache.load(path, current, |_| Ok(project(2)))), 2);
    }

    #[test]
    fn a_disabled_cache_neither_serves_nor_stores() {
        let files = files(1);
        let path = &files.paths[0];
        let cache = ProjectCache::default();
        cache.load(path, current, |_| Ok(project(1))).unwrap();

        cache.set_enabled(false);
        assert_eq!(cache.len(), 0, "disabling must release memory at once");
        assert_eq!(width(cache.load(path, current, |_| Ok(project(2)))), 2);
        cache.prefetch(path.clone(), |_| Ok(project(3)));
        assert_eq!(cache.len(), 0);

        cache.set_enabled(true);
        assert_eq!(width(cache.load(path, current, |_| Ok(project(4)))), 4);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn disabling_wakes_waiters_and_drops_the_prefetch_in_flight() {
        let files = files(1);
        let path = &files.paths[0];
        let cache = ProjectCache::default();
        let release = blocked_prefetch(&cache, path, Ok(project(1)));
        let calls = Arc::new(AtomicUsize::new(0));
        let opened = load_in_background(&cache, path, &calls);
        assert!(opened.recv_timeout(Duration::from_millis(100)).is_err());

        cache.set_enabled(false);
        // The waiter no longer waits on a cache that is off; it reads itself.
        assert_eq!(width(opened.recv_timeout(WAIT).unwrap()), 99);
        release.send(()).unwrap();
        // Give the orphaned prefetch its chance to (wrongly) store a result.
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(cache.len(), 0);
    }
}
