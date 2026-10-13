//! Browsing the library by when photos were taken.
//!
//! Import sessions group by the import that brought a photo in, so two imports
//! covering the same dates produce overlapping sessions.  A timeline is built
//! from each photo's own `capture_date` instead, and a day means the same thing
//! no matter how its photos arrived.
//!
//! Dates are read off the raw EXIF string (`"YYYY:MM:DD HH:MM:SS"`) as written,
//! never converted: the camera has no time zone to offer, and shifting a
//! late-evening shot onto the next UTC day is the wrong answer here.

use std::collections::BTreeMap;

use crate::search::SearchFilter;

/// Earliest year taken as a real capture date.  Cameras with an unset clock
/// write `0000:00:00 00:00:00`, and treating that as a date would put a
/// "Year 0" at the bottom of the tree.
const MIN_YEAR: i32 = 1900;

/// Photo counts per calendar day, plus the photos no day can be found for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Timeline {
    /// `(year, month, day)` to photo count, ascending; iterate in reverse for
    /// newest first.
    pub days: BTreeMap<(i32, u32, u32), i64>,
    /// Photos with no usable capture date.
    pub unknown: i64,
}

impl Timeline {
    /// Fold the database's per-prefix counts into days.  A day the strings
    /// spell more than one way is added together.
    pub fn from_day_counts(counts: Vec<(Option<String>, i64)>) -> Self {
        let mut timeline = Self::default();
        for (prefix, count) in counts {
            match prefix.as_deref().and_then(parse_day) {
                Some(day) => *timeline.days.entry(day).or_default() += count,
                None => timeline.unknown += count,
            }
        }
        timeline
    }

    pub fn is_empty(&self) -> bool {
        self.days.is_empty() && self.unknown == 0
    }

    /// Photos taken within `span`.
    pub fn count(&self, span: DateSpan) -> i64 {
        let (from, to) = span.day_range();
        self.days.range(from..=to).map(|(_, n)| n).sum()
    }
}

/// Parse the date part of a capture-date string: `"YYYY:MM:DD"`, optionally
/// followed by a time.  Returns `None` for anything that is not a real day,
/// which is what makes a photo "unknown date".
pub fn parse_day(s: &str) -> Option<(i32, u32, u32)> {
    let bytes = s.as_bytes();
    if bytes.len() < 10 || bytes[4] != b':' || bytes[7] != b':' {
        return None;
    }
    let year: i32 = s.get(0..4)?.parse().ok()?;
    let month: u32 = s.get(5..7)?.parse().ok()?;
    let day: u32 = s.get(8..10)?.parse().ok()?;
    ((MIN_YEAR..).contains(&year) && (1..=12).contains(&month) && (1..=31).contains(&day))
        .then_some((year, month, day))
}

/// A stretch of the calendar to show photos from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DateSpan {
    Year(i32),
    Month(i32, u32),
    Day(i32, u32, u32),
}

impl DateSpan {
    /// First and last `(year, month, day)` the span covers.  A month always
    /// ends on the 31st: no real date falls between its true last day and
    /// that, so the bound needs no calendar.
    fn day_range(self) -> ((i32, u32, u32), (i32, u32, u32)) {
        match self {
            Self::Year(y) => ((y, 1, 1), (y, 12, 31)),
            Self::Month(y, m) => ((y, m, 1), (y, m, 31)),
            Self::Day(y, m, d) => ((y, m, d), (y, m, d)),
        }
    }

    /// Constrain `filter` to this span, replacing any capture-date bounds it
    /// already had.
    pub fn apply(self, filter: &mut SearchFilter) {
        let ((fy, fm, fd), (ty, tm, td)) = self.day_range();
        filter.capture_date_from = Some(format!("{fy:04}:{fm:02}:{fd:02}"));
        filter.capture_date_to = Some(format!("{ty:04}:{tm:02}:{td:02} 23:59:59"));
        filter.capture_date_unknown = false;
    }
}

/// Constrain `filter` to the photos that have no usable capture date.
pub fn apply_unknown(filter: &mut SearchFilter) {
    filter.capture_date_from = None;
    filter.capture_date_to = None;
    filter.capture_date_unknown = true;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_exif_dates_with_or_without_a_time() {
        assert_eq!(parse_day("2026:07:10"), Some((2026, 7, 10)));
        assert_eq!(parse_day("2026:07:10 23:59:59"), Some((2026, 7, 10)));
    }

    #[test]
    fn rejects_what_is_not_a_calendar_day() {
        for bad in [
            "",
            "2026",
            "0000:00:00 00:00:00",
            "1899:12:31",
            "2026:00:10",
            "2026:13:10",
            "2026:07:00",
            "2026:07:32",
            "2026-07-10",
            "abcd:ef:gh",
        ] {
            assert_eq!(parse_day(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn day_counts_merge_and_unknowns_pool() {
        let timeline = Timeline::from_day_counts(vec![
            (Some("2026:07:10".into()), 3),
            (Some("2026:07:10".into()), 2),
            (Some("2026:07:11".into()), 1),
            (Some("0000:00:00".into()), 4),
            (None, 5),
        ]);
        assert_eq!(timeline.days[&(2026, 7, 10)], 5);
        assert_eq!(timeline.unknown, 9);
    }

    #[test]
    fn spans_count_the_days_they_cover() {
        let timeline = Timeline::from_day_counts(vec![
            (Some("2025:12:31".into()), 1),
            (Some("2026:06:30".into()), 2),
            (Some("2026:07:01".into()), 4),
            (Some("2026:07:31".into()), 8),
            (Some("2026:08:01".into()), 16),
        ]);
        assert_eq!(timeline.count(DateSpan::Year(2026)), 30);
        assert_eq!(timeline.count(DateSpan::Month(2026, 7)), 12);
        assert_eq!(timeline.count(DateSpan::Day(2026, 7, 31)), 8);
        assert_eq!(timeline.count(DateSpan::Day(2026, 7, 2)), 0);
    }

    #[test]
    fn a_span_bounds_the_filter_inclusively() {
        let mut filter = SearchFilter::default();
        DateSpan::Month(2026, 7).apply(&mut filter);
        assert_eq!(filter.capture_date_from.as_deref(), Some("2026:07:01"));
        assert_eq!(
            filter.capture_date_to.as_deref(),
            Some("2026:07:31 23:59:59")
        );
        assert!(!filter.is_empty());
    }
}
