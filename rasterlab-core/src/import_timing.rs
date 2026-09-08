//! Opt-in benchmark instrumentation, never ordinary user output.
//!
//! Each thread owns its counters. Nested spans subtract their inclusive time
//! from their parent, so reported phases do not overlap. A codec's Rayon work
//! is represented by elapsed time on its waiting caller, not worker CPU time.
//! Reset and snapshot on the importing thread outside all measured spans.
//!
//! Library import prepares photos on worker threads and commits them on the
//! importing thread, so a snapshot taken there covers the writing phases only
//! — reads, decoding, thumbnails and serialisation are counted on workers that
//! are never snapshotted. Their cost shows up as unattributed wall time, which
//! is the point: overlapped preparation is time the writer did not spend.

use std::{
    cell::RefCell,
    collections::BTreeMap,
    marker::PhantomData,
    rc::Rc,
    time::{Duration, Instant},
};

#[derive(Default)]
struct Timings {
    phases: BTreeMap<&'static str, Duration>,
    children: Vec<Duration>,
}

thread_local! {
    static TIMINGS: RefCell<Timings> = RefCell::new(Timings::default());
}

/// RAII span used by `import_phase!`; also records early returns and errors.
pub struct Span {
    name: &'static str,
    start: Instant,
    // Counters belong to the current thread; a span must not migrate.
    _thread: PhantomData<Rc<()>>,
}

impl Span {
    pub fn new(name: &'static str) -> Self {
        TIMINGS.with_borrow_mut(|t| t.children.push(Duration::ZERO));
        Self {
            name,
            start: Instant::now(),
            _thread: PhantomData,
        }
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        let elapsed = self.start.elapsed();
        TIMINGS.with_borrow_mut(|t| {
            let children = t.children.pop().expect("active timing span");
            *t.phases.entry(self.name).or_default() += elapsed.saturating_sub(children);
            if let Some(parent) = t.children.last_mut() {
                *parent += elapsed;
            }
        });
    }
}

/// Clear fixture/validation timings before each import invocation.
pub fn reset() {
    TIMINGS.with_borrow_mut(|t| {
        assert!(t.children.is_empty(), "reset inside timing span");
        t.phases.clear();
    });
}

/// Exclusive phase durations in seconds for this thread since the last reset.
pub fn snapshot_seconds() -> BTreeMap<&'static str, f64> {
    TIMINGS.with_borrow(|t| {
        assert!(t.children.is_empty(), "snapshot inside timing span");
        t.phases
            .iter()
            .map(|(&name, time)| (name, time.as_secs_f64()))
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_spans_partition_wall_time_and_reset() {
        reset();
        let start = Instant::now();
        crate::import_phase!("outer", {
            crate::import_phase!(
                "inner",
                crate::import_phase!("inner", std::hint::black_box(42))
            );
        });
        let wall = start.elapsed().as_secs_f64();
        let phases = snapshot_seconds();
        assert!(phases["inner"] > 0.0);
        assert!(phases.values().sum::<f64>() <= wall);
        reset();
        assert!(snapshot_seconds().is_empty());
    }

    #[test]
    fn error_return_closes_span_and_other_threads_are_isolated() {
        reset();
        fn fail() -> Result<(), ()> {
            crate::import_phase!("error", {
                Err(())?;
                Ok(())
            })
        }
        assert!(fail().is_err());
        assert!(snapshot_seconds().contains_key("error"));
        std::thread::spawn(|| {
            assert!(snapshot_seconds().is_empty());
            crate::import_phase!("other_thread", ());
        })
        .join()
        .unwrap();
        assert!(!snapshot_seconds().contains_key("other_thread"));
        let result = std::panic::catch_unwind(|| {
            crate::import_phase!("panic", panic!("test timing unwind"));
        });
        assert!(result.is_err());
        assert!(snapshot_seconds().contains_key("panic"));
        reset();
    }
}
