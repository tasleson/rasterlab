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
//! On every OS we also remove the owner's write permission as a cheap extra
//! guard against accidental in-place overwrite. All operations are best-effort:
//! the real protection is the in-app guard, so a platform that refuses the lock
//! must not break the feature.

use std::path::Path;

/// Apply or remove the OS-level lock on `path`.
///
/// Only the write-permission change is reported through the returned
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

/// True if `path` currently carries our write-permission lock.
#[cfg(unix)]
pub fn is_locked(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path)
        .map(|m| m.permissions().mode() & OWNER_WRITE == 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
pub fn is_locked(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|m| m.permissions().readonly())
        .unwrap_or(false)
}

/// Run `f` with `path` temporarily unlocked, restoring the prior lock state
/// afterwards. Used by the legitimate in-app rewrite paths so a protected
/// file's metadata and edits can still be updated.
pub fn with_unlocked<T>(path: &Path, f: impl FnOnce() -> T) -> T {
    let _guard = RelockOnDrop::new(path);
    f()
}

struct RelockOnDrop<'a> {
    path: &'a Path,
    was_locked: bool,
}

impl<'a> RelockOnDrop<'a> {
    fn new(path: &'a Path) -> Self {
        let was_locked = is_locked(path);
        if was_locked {
            let _ = set_locked(path, false);
        }
        Self { path, was_locked }
    }
}

impl Drop for RelockOnDrop<'_> {
    fn drop(&mut self) {
        if self.was_locked {
            let _ = set_locked(self.path, true);
        }
    }
}

#[cfg(unix)]
const OWNER_WRITE: u32 = 0o200;

#[cfg(unix)]
fn set_readonly_bit(path: &Path, readonly: bool) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut perms = std::fs::metadata(path)?.permissions();
    let mode = perms.mode();
    perms.set_mode(if readonly {
        mode & !OWNER_WRITE
    } else {
        mode | OWNER_WRITE
    });
    std::fs::set_permissions(path, perms)
}

#[cfg(not(unix))]
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn unlocking_preserves_group_and_other_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("photo.rlab");
        std::fs::write(&path, b"project").unwrap();

        for (unlocked, locked) in [(0o644, 0o444), (0o600, 0o400), (0o664, 0o464)] {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(unlocked)).unwrap();

            set_locked(&path, true).unwrap();
            assert_eq!(mode(&path), locked);
            assert!(is_locked(&path));

            set_locked(&path, false).unwrap();
            assert_eq!(mode(&path), unlocked);
            assert!(!is_locked(&path));
        }
    }

    #[test]
    fn temporary_unlock_relocks_during_unwinding() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("photo.rlab");
        std::fs::write(&path, b"project").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        set_locked(&path, true).unwrap();

        let unwound = std::panic::catch_unwind(|| {
            with_unlocked(&path, || panic!("simulated rewrite failure"));
        });

        assert!(unwound.is_err());
        assert_eq!(mode(&path), 0o440);
        assert!(is_locked(&path));
        set_locked(&path, false).unwrap();
    }
}
