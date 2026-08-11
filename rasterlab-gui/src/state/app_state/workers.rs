//! Spawning background workers that are guaranteed to report back.
//!
//! Every long-running task in the GUI marks something as in flight — the
//! loading flag, an import progress bar, the scrub Start/Stop toggle — and
//! clears it when the worker's terminal message arrives. A worker that panics,
//! or that never starts because the thread could not be spawned, would leave
//! that state set for the rest of the session: the progress bar spins forever
//! and the action that owns it refuses to run again.
//!
//! [`spawn`] closes that gap. The body returns the message that ends the task,
//! and both a panic and a spawn failure are turned into a caller-chosen failure
//! message, so exactly one terminal message is always delivered and the handler
//! that clears the in-flight state always runs.

use std::sync::mpsc::Sender;

use egui::Context;
use rasterlab_core::panic_guard;

use super::BgMessage;

/// Stack size for workers that decode, render, or encode images. Matches the
/// render thread: rayon fold accumulators need far more than the 512 KiB a
/// secondary thread gets by default on macOS.
pub(super) const IMAGE_WORKER_STACK: usize = 32 * 1024 * 1024;

/// Spawn a named background worker whose terminal message always arrives.
///
/// `body` runs on the new thread and returns the message that completes the
/// task; it may post as many progress messages as it likes on its own clone of
/// `tx` first. `on_failure` builds the terminal message from a description of
/// what went wrong, and must clear the same state the successful terminal
/// message would.
pub(super) fn spawn<F>(
    name: &'static str,
    stack_size: usize,
    tx: Sender<BgMessage>,
    ctx: Context,
    on_failure: fn(String) -> BgMessage,
    body: F,
) where
    F: FnOnce() -> BgMessage + Send + 'static,
{
    let failure_tx = tx.clone();
    let failure_ctx = ctx.clone();
    let spawned = std::thread::Builder::new()
        .name(name.into())
        .stack_size(stack_size)
        .spawn(move || {
            let message = panic_guard::guard(body)
                .unwrap_or_else(|panic| on_failure(format!("{name} panicked: {panic}")));
            // A send error means the app is shutting down and the receiver is
            // already gone; there is nobody left to tell.
            let _ = tx.send(message);
            ctx.request_repaint();
        });

    if let Err(e) = spawned {
        let _ = failure_tx.send(on_failure(format!("could not start {name}: {e}")));
        failure_ctx.request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// Run `body` through [`spawn`] and return the one message it delivered.
    fn terminal_message(body: impl FnOnce() -> BgMessage + Send + 'static) -> BgMessage {
        let (tx, rx) = mpsc::channel();
        spawn(
            "test-worker",
            64 * 1024,
            tx,
            Context::default(),
            BgMessage::ScrubFailed,
            body,
        );
        let message = rx.recv().expect("a worker must always report");
        assert!(
            rx.recv().is_err(),
            "a worker must report exactly one terminal message"
        );
        message
    }

    #[test]
    fn a_completed_worker_delivers_its_own_terminal_message() {
        let message = terminal_message(|| BgMessage::TaskFailed("done".into()));
        assert!(matches!(message, BgMessage::TaskFailed(m) if m == "done"));
    }

    #[test]
    fn a_panicking_worker_still_delivers_a_terminal_message() {
        // The panic is expected; keep the default hook from printing a
        // backtrace that makes a passing run look like a failing one.
        let previous_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let message = terminal_message(|| panic!("scrub blew up"));
        std::panic::set_hook(previous_hook);

        match message {
            // Routed through `on_failure`, so the handler that releases the
            // scrub's cancellation handle runs just as it would on success.
            BgMessage::ScrubFailed(m) => assert!(
                m.contains("test-worker panicked") && m.contains("scrub blew up"),
                "unhelpful panic report: {m}"
            ),
            _ => panic!("a panic must be reported through on_failure"),
        }
    }
}
