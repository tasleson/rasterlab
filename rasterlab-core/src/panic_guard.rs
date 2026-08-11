//! Containment for panics that would otherwise cross a worker boundary.
//!
//! Background workers run code that is not guaranteed panic-free on hostile
//! input: third-party decoders, image codecs, and plugin-supplied operations.
//! A panic unwinds only the worker thread, so without containment the worker
//! dies without reporting anything, and whatever state marked the task as in
//! flight — a loading flag, a progress bar, a Start/Stop toggle — is never
//! cleared. Wrapping the worker body turns that silent death into an ordinary
//! error the caller already knows how to report.

use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind};

/// Run `body`, converting a panic into `Err` carrying the panic's message.
///
/// The body is asserted unwind-safe because a worker's captured state is
/// dropped as soon as this returns: nothing observes a value the panic left
/// half-updated.
pub fn guard<T>(body: impl FnOnce() -> T) -> Result<T, String> {
    catch_unwind(AssertUnwindSafe(body)).map_err(|payload| panic_message(&*payload).to_string())
}

/// Best-effort message text for a panic payload.
///
/// `panic!` produces either a `&'static str` or a `String`; a payload from
/// `panic_any` carries no displayable message, so it is reported by shape only.
pub fn panic_message(payload: &(dyn Any + Send)) -> &str {
    if let Some(message) = payload.downcast_ref::<&'static str>() {
        message
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message
    } else {
        "unknown panic payload"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_the_value_when_the_body_completes() {
        assert_eq!(guard(|| 7), Ok(7));
    }

    #[test]
    fn reports_static_str_and_formatted_panic_messages() {
        assert_eq!(
            guard(|| panic!("decoder gave up")),
            Err("decoder gave up".into())
        );
        let width = 4096;
        assert_eq!(
            guard(|| panic!("bad width {width}")),
            Err("bad width 4096".into())
        );
    }

    #[test]
    fn reports_a_message_less_payload_without_panicking_itself() {
        let result: Result<(), String> = guard(|| std::panic::panic_any(42u8));
        assert_eq!(result, Err("unknown panic payload".into()));
    }

    #[test]
    fn a_contained_panic_leaves_the_thread_usable() {
        let _ = guard(|| panic!("first"));
        assert_eq!(guard(|| "still running"), Ok("still running"));
    }
}
