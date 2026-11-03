//! File sizes as the GUI shows them: always in MiB, so figures side by side —
//! a photo beside its original, one selection beside another — compare at a
//! glance without the unit changing under the reader.

const BYTES_PER_MIB: u64 = 1024 * 1024;

/// `bytes` in MiB. One decimal is plenty for photographs; below 1 MiB a
/// second one keeps a small file from reading as nothing at all.
pub(crate) fn mib(bytes: u64) -> String {
    let mib = bytes as f64 / BYTES_PER_MIB as f64;
    if bytes < BYTES_PER_MIB {
        format!("{mib:.2} MiB")
    } else {
        format!("{mib:.1} MiB")
    }
}

/// A file's size, followed by the size of the original it holds when that is
/// known — "24.3 MiB (original 18.1 MiB)".
pub(crate) fn with_original(bytes: u64, original: Option<u64>) -> String {
    paired(bytes, original, "original")
}

/// [`with_original`] for several photos' files and their originals.
pub(crate) fn with_originals(bytes: u64, originals: Option<u64>) -> String {
    paired(bytes, originals, "originals")
}

fn paired(bytes: u64, original: Option<u64>, label: &str) -> String {
    match original {
        Some(original) => format!("{} ({label} {})", mib(bytes), mib(original)),
        None => mib(bytes),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_are_shown_in_mib() {
        for (bytes, expected) in [
            (0, "0.00 MiB"),
            (512 * 1024, "0.50 MiB"),
            (BYTES_PER_MIB - 1, "1.00 MiB"),
            (BYTES_PER_MIB, "1.0 MiB"),
            (BYTES_PER_MIB * 3 / 2, "1.5 MiB"),
            (25_480_000, "24.3 MiB"),
            (5 * 1024 * BYTES_PER_MIB, "5120.0 MiB"),
        ] {
            assert_eq!(mib(bytes), expected, "{bytes} bytes");
        }
    }

    #[test]
    fn the_original_follows_when_known() {
        for (format, bytes, original, expected) in [
            (
                with_original as fn(u64, Option<u64>) -> String,
                25_480_000,
                None,
                "24.3 MiB",
            ),
            (
                with_original,
                25_480_000,
                Some(18_979_000),
                "24.3 MiB (original 18.1 MiB)",
            ),
            (with_originals, 25_480_000, None, "24.3 MiB"),
            (
                with_originals,
                357_000_000,
                Some(262_144_000),
                "340.5 MiB (originals 250.0 MiB)",
            ),
        ] {
            assert_eq!(format(bytes, original), expected);
        }
    }
}
