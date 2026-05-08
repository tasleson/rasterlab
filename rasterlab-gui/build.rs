use std::process::Command;

fn main() {
    // Git commit hash
    let git_hash = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into());
    println!("cargo::rustc-env=GIT_HASH={git_hash}");

    // Dirty flag
    let dirty = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);
    println!(
        "cargo::rustc-env=GIT_DIRTY={}",
        if dirty { "yes" } else { "no" }
    );

    // Build timestamp (UTC) — computed via std::time so this works on all platforms.
    println!("cargo::rustc-env=BUILD_DATE={}", utc_now());

    // Rustc version
    let rustc_version = Command::new("rustc")
        .args(["--version"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into());
    println!("cargo::rustc-env=RUSTC_VERSION_STR={rustc_version}");

    // Target triple
    println!(
        "cargo::rustc-env=TARGET_TRIPLE={}",
        std::env::var("TARGET").unwrap_or_else(|_| "unknown".into())
    );

    // Re-run if git HEAD changes
    println!("cargo::rerun-if-changed=../.git/HEAD");
    println!("cargo::rerun-if-changed=../.git/index");
}

/// Format the current UTC time as "YYYY-MM-DD HH:MM:SS UTC" using only std::time,
/// avoiding any shell invocation so the build works on Windows and Linux as well as macOS.
fn utc_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let h = (secs % 86400) / 3600;
    let mi = (secs % 3600) / 60;
    let s = secs % 60;
    let mut rem = secs / 86400; // days since 1970-01-01

    let mut year = 1970u32;
    loop {
        let leap = (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400);
        let days_in_year: u64 = if leap { 366 } else { 365 };
        if rem < days_in_year {
            break;
        }
        rem -= days_in_year;
        year += 1;
    }
    let leap = (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400);
    let month_lengths: [u64; 12] = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 1u32;
    for ml in month_lengths {
        if rem < ml {
            break;
        }
        rem -= ml;
        month += 1;
    }
    let day = rem + 1;
    format!("{year}-{month:02}-{day:02} {h:02}:{mi:02}:{s:02} UTC")
}
