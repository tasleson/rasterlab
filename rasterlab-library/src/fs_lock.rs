//! Best-effort, per-OS filesystem locking for protected `.rlab` files.
//!
//! "Protected" library photos must not go missing. On top of the in-app delete
//! guard we lock the file on disk using the strongest mechanism the platform
//! offers:
//!
//! * **macOS** — the user-immutable flag (`chflags uchg`), which blocks
//!   deletion, renaming and overwrite even from Finder.
//! * **Linux** — the immutable attribute (`chattr +i`); this usually requires
//!   `CAP_LINUX_IMMUTABLE`, so failure is tolerated.
//! * **Windows / other** — the read-only file attribute is the available
//!   mechanism.
//!
//! On every OS we also toggle the read-only permission bit as a cheap extra
//! guard against accidental in-place overwrite. All operations are best-effort:
//! the real protection is the in-app guard, so a platform that refuses the lock
//! must not break the feature.

use std::path::Path;

/// Apply or remove the OS-level lock on `path`.
///
/// Only the read-only permission bit is reported through the returned
/// `Result`; the immutable flag is applied separately and best-effort.
pub fn set_locked(path: &Path, locked: bool) -> std::io::Result<()> {
    if locked {
        // Set the read-only bit first: once the file is immutable the OS may
        // refuse the permission change.
        let res = set_readonly_bit(path, true);
        set_immutable(path, true);
        res
    } else {
        // Clear the immutable flag first so the permission change is allowed.
        set_immutable(path, false);
        set_readonly_bit(path, false)
    }
}

/// True if `path` currently carries our lock (its read-only bit is set).
pub fn is_locked(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|m| m.permissions().readonly())
        .unwrap_or(false)
}

/// Run `f` with `path` temporarily unlocked, restoring the prior lock state
/// afterwards. Used by the legitimate in-app rewrite paths so a protected
/// file's metadata and edits can still be updated.
pub fn with_unlocked<T>(path: &Path, f: impl FnOnce() -> T) -> T {
    let was_locked = is_locked(path);
    if was_locked {
        let _ = set_locked(path, false);
    }
    let result = f();
    if was_locked {
        let _ = set_locked(path, true);
    }
    result
}

fn set_readonly_bit(path: &Path, readonly: bool) -> std::io::Result<()> {
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_readonly(readonly);
    std::fs::set_permissions(path, perms)
}

#[cfg(target_os = "macos")]
fn set_immutable(path: &Path, immutable: bool) {
    let flag = if immutable { "uchg" } else { "nouchg" };
    let _ = std::process::Command::new("chflags")
        .arg(flag)
        .arg(path)
        .status();
}

#[cfg(target_os = "linux")]
fn set_immutable(path: &Path, immutable: bool) {
    let flag = if immutable { "+i" } else { "-i" };
    let _ = std::process::Command::new("chattr")
        .arg(flag)
        .arg(path)
        .status();
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn set_immutable(_path: &Path, _immutable: bool) {
    // The read-only attribute applied via `set_readonly_bit` is the available
    // mechanism on these platforms; nothing further to do.
}
