//! A cross-protocol advisory lock on the library root.
//!
//! The index already takes an exclusive `flock` on `library.db` and holds it
//! for as long as the library is open, which is the fast, authoritative answer
//! to "is someone else in here?" — but only among clients that share a lock
//! table.  A Mac reaching the library over SMB and a Linux box reaching the
//! same ZFS dataset over NFS do not: Samba and `nfsd` keep their own, and
//! nothing makes one wait for the other.  Each sees an unlocked file and each
//! opens the library.
//!
//! So alongside the `flock` — never instead of it — the library root carries a
//! small JSON file naming who has it open and when they last said so.  A
//! holder rewrites its own timestamp periodically; anyone opening the library
//! reads the file first and refuses if a *different host* is still refreshing
//! it.  Two processes on one machine are left to the `flock`, which is exact
//! and immediate where this is neither.
//!
//! # What this is not
//!
//! Advisory and best-effort, deliberately.  Reading the file and replacing it
//! are two operations with a gap between them, so two hosts starting within
//! the same instant can both come away believing they won, and a host whose
//! connection stalls past the staleness window can have its lock taken while
//! it is still very much alive.  Nothing here makes concurrent access *safe*;
//! it makes the common mistake — leaving a CLI rebuild running on one machine
//! and opening the GUI on another — visible instead of silent.  A library that
//! must tolerate real concurrent writers needs a different design, not a
//! longer timeout.
//!
//! # Intervals
//!
//! [`REFRESH_INTERVAL`] is a minute: often enough that a clean crash costs a
//! bounded wait, rare enough that a write to a network share every so often is
//! not worth noticing next to what importing a photo costs.
//!
//! [`STALE_AFTER`] is thirty times that.  The number is deliberately
//! extravagant, because the cost of the two mistakes is not symmetric.
//! Declaring a live holder dead lets two machines write one library, which is
//! the thing this exists to prevent; leaving a dead holder's lock in place
//! costs a wait and, if the user is sure, one environment variable.  Half an
//! hour of silence is well past anything a working client does — an SMB
//! reconnect, a server reboot, a laptop closed and reopened, a large fsync on
//! a busy array — so a lock that has gone quiet that long really has been
//! abandoned.
//!
//! # Clearing a lock by hand
//!
//! Setting `RASTERLAB_TAKE_LIBRARY_LOCK=1` opens the library whatever the file
//! says, and replaces it.  Deleting `library.lock` from the library root does
//! the same thing.  Both are for the case where the user knows the other
//! holder is gone; neither disables the `flock`, so a second process on the
//! same machine is still refused.

use std::{
    io,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex},
    thread::JoinHandle,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use rasterlab_core::verified_write::write_atomic;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The lock file's name inside the library root.
pub const LOCK_FILE: &str = "library.lock";

/// Environment variable that opens the library regardless of the lock file.
pub const OVERRIDE_ENV: &str = "RASTERLAB_TAKE_LIBRARY_LOCK";

/// How often a holder rewrites its timestamp.  See the module docs.
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(60);

/// How many missed refreshes before a lock counts as abandoned.
const STALE_AFTER_REFRESHES: u32 = 30;

/// How long a lock may go unrefreshed before anyone may take it over.
pub const STALE_AFTER: Duration =
    Duration::from_secs(REFRESH_INTERVAL.as_secs() * STALE_AFTER_REFRESHES as u64);

/// What the hostname becomes when the platform will not give one up.
///
/// Two such machines look like one host to each other, so the cross-protocol
/// guard quietly does nothing and the `flock` is all that is left — which is
/// exactly where things stood before this file existed.  Failing the open
/// instead would break a working setup over a missing string.
const UNKNOWN_HOST: &str = "unknown-host";

/// Who holds a library open, as recorded in `library.lock`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockHolder {
    /// The machine, which is the field the check actually turns on.
    pub host: String,
    /// The holder's process id, for a human trying to find it.
    pub pid: u32,
    /// A fresh random id per process.  A pid on its own is not enough: after a
    /// reboot the OS hands the same number out again, and a stale lock naming
    /// pid 4213 would otherwise look alive because *some* process 4213 exists.
    pub session: String,
    /// The RasterLab that wrote it, so a puzzling lock can be traced to a
    /// version rather than guessed at.
    pub version: String,
    /// Seconds since the Unix epoch at the last refresh.
    pub heartbeat: u64,
}

impl LockHolder {
    /// How long ago this holder last refreshed, as of `now`.
    ///
    /// A timestamp from the future — two machines rarely agree on the time to
    /// the second, and one of them may be badly wrong — reads as zero rather
    /// than wrapping, so a skewed clock makes the lock look *fresher* than it
    /// is.  That errs toward refusing to open, which is the safe direction.
    /// The other direction is the unsafe one: a holder whose clock runs behind
    /// ours looks staler than it is, and enough of a lag would let us take the
    /// lock from a machine still using it.  `STALE_AFTER` is the margin — half
    /// an hour swallows any clock skew a roughly synchronised network produces.
    pub fn silent_for(&self, now: u64) -> Duration {
        Duration::from_secs(now.saturating_sub(self.heartbeat))
    }

    /// Whether this holder is still refreshing, as of `now`.
    pub fn is_live(&self, now: u64) -> bool {
        self.silent_for(now) < STALE_AFTER
    }
}

impl std::fmt::Display for LockHolder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} (RasterLab {}, pid {})",
            self.host, self.version, self.pid
        )
    }
}

/// A held cross-protocol lock on a library root.
///
/// Refreshing happens on a thread of its own for as long as this is alive.
/// Dropping it stops the thread and removes the lock file, so a library closed
/// normally leaves nothing for the next opener to wait out.
#[derive(Debug)]
pub struct HostLock {
    path: PathBuf,
    me: LockHolder,
    /// Set when the refresh thread should stop; the condvar wakes it at once
    /// rather than letting a close wait out the rest of a minute.
    stop: Arc<(Mutex<bool>, Condvar)>,
    refresher: Option<JoinHandle<()>>,
}

impl HostLock {
    /// Take the lock on the library at `root`, or say who already has it.
    ///
    /// A lock belonging to this same machine is not a conflict here: the
    /// `flock` on `library.db` has already decided that case, exactly, and
    /// second-guessing it with a timestamp would only add a way to be wrong.
    pub fn acquire(root: &Path) -> Result<Self, LockHolder> {
        Self::acquire_as(root, identity(), now_unix(), override_requested())
    }

    /// [`HostLock::acquire`] with its three inputs supplied, so the decision
    /// can be tested without a second machine, a clock or an environment.
    fn acquire_as(root: &Path, me: LockHolder, now: u64, force: bool) -> Result<Self, LockHolder> {
        let path = root.join(LOCK_FILE);
        if let Some(holder) = blocking_holder(read_holder(&path).as_ref(), &me, now, force) {
            return Err(holder.clone());
        }

        let mut lock = Self {
            path,
            me,
            stop: Arc::new((Mutex::new(false), Condvar::new())),
            refresher: None,
        };
        // A first stamp that cannot be written is not a reason to refuse the
        // library: the user would be locked out by the very guard meant to
        // help, on a share that is merely read-only or full.  Say so once and
        // carry on without the protection.
        if let Err(e) = lock.write_stamp(now) {
            eprintln!(
                "Warning: could not write {}: {e}. \
                 Another machine opening this library will not be warned off.",
                lock.path.display()
            );
        }
        lock.spawn_refresher();
        Ok(lock)
    }

    /// Who this process claims to be in the lock file.
    pub fn holder(&self) -> &LockHolder {
        &self.me
    }

    fn write_stamp(&mut self, now: u64) -> io::Result<()> {
        self.me.heartbeat = now;
        let json = serde_json::to_vec_pretty(&self.me).map_err(io::Error::other)?;
        write_atomic(&self.path, &json)
    }

    fn spawn_refresher(&mut self) {
        let stop = Arc::clone(&self.stop);
        let path = self.path.clone();
        let mut me = self.me.clone();
        self.refresher = Some(std::thread::spawn(move || {
            // Consecutive failures, so a share that goes away for a while
            // reports once rather than once a minute for an afternoon.
            let mut failures: u64 = 0;
            loop {
                let (lock, cvar) = &*stop;
                let mut stopping = lock.lock().unwrap_or_else(|e| e.into_inner());
                if !*stopping {
                    let (guard, _) = cvar
                        .wait_timeout(stopping, REFRESH_INTERVAL)
                        .unwrap_or_else(|e| e.into_inner());
                    stopping = guard;
                }
                if *stopping {
                    return;
                }
                drop(stopping);

                me.heartbeat = now_unix();
                let written = serde_json::to_vec_pretty(&me)
                    .map_err(io::Error::other)
                    .and_then(|json| write_atomic(&path, &json));
                match written {
                    Ok(()) => {
                        if failures > 0 {
                            eprintln!(
                                "Note: {} is being refreshed again after {failures} \
                                 failed attempt(s).",
                                path.display()
                            );
                        }
                        failures = 0;
                    }
                    Err(e) => {
                        failures += 1;
                        // A refresh that cannot be written costs the guard,
                        // not the session: the photos are elsewhere and the
                        // `flock` is untouched.  Worst case another machine
                        // decides this lock went stale and takes it.
                        if failures == 1 {
                            eprintln!(
                                "Warning: could not refresh {}: {e}. \
                                 The library stays open; another machine may \
                                 stop treating it as in use.",
                                path.display()
                            );
                        }
                    }
                }
            }
        }));
    }

    /// Stop refreshing and remove the lock file, if it is still ours.
    ///
    /// Someone else's file is left alone: if this lock was declared stale and
    /// taken over while we were quiet, deleting what the new holder wrote
    /// would hand the library to a third opener.
    fn release(&mut self) {
        let (lock, cvar) = &*self.stop;
        {
            let mut stopping = lock.lock().unwrap_or_else(|e| e.into_inner());
            *stopping = true;
        }
        cvar.notify_all();
        if let Some(handle) = self.refresher.take() {
            let _ = handle.join();
        }
        if read_holder(&self.path).is_some_and(|h| h.session == self.me.session) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

impl Drop for HostLock {
    fn drop(&mut self) {
        self.release();
    }
}

/// Remove a library's lock file whoever wrote it.
///
/// The by-hand escape hatch for a lock left behind by a machine that is not
/// coming back, for a caller that would rather not ask the user to set
/// [`OVERRIDE_ENV`].  Removing a *live* holder's file does not close that
/// library; it only stops the next opener being warned.
pub fn clear(root: &Path) -> io::Result<()> {
    match std::fs::remove_file(root.join(LOCK_FILE)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Who holds the library at `root`, if anyone says so.
pub fn current_holder(root: &Path) -> Option<LockHolder> {
    read_holder(&root.join(LOCK_FILE))
}

/// The holder that stands between `me` and the library, if there is one.
///
/// Split out from [`HostLock::acquire`] because it is the whole of the policy
/// and none of the I/O: every rule about who may take a lock lives here.
fn blocking_holder<'a>(
    existing: Option<&'a LockHolder>,
    me: &LockHolder,
    now: u64,
    force: bool,
) -> Option<&'a LockHolder> {
    if force {
        return None;
    }
    // Nothing there, or a file nothing could parse — a truncated write, an
    // older format, something else's `library.lock`.  An unreadable claim is
    // no claim; refusing on one would strand a library over a stray byte.
    let holder = existing?;
    // This machine's own business, and the `flock` has already settled it.
    // Our own leftover file from a previous run of this very process lands
    // here too, which is right: it is ours to replace.
    if holder.host == me.host || holder.session == me.session {
        return None;
    }
    holder.is_live(now).then_some(holder)
}

fn read_holder(path: &Path) -> Option<LockHolder> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn override_requested() -> bool {
    std::env::var_os(OVERRIDE_ENV).is_some_and(|v| v != "0" && !v.is_empty())
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// This process's claim, minus the timestamp, which each write supplies.
fn identity() -> LockHolder {
    LockHolder {
        host: hostname(),
        pid: std::process::id(),
        session: session_id().to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        heartbeat: 0,
    }
}

/// One random id for the life of the process.
fn session_id() -> &'static str {
    static SESSION: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    SESSION.get_or_init(|| Uuid::new_v4().to_string())
}

#[cfg(unix)]
fn hostname() -> String {
    // SAFETY: the buffer is 256 bytes and that length is what is passed, so
    // gethostname writes within it.  It either fills the buffer with a
    // NUL-terminated name or returns non-zero, and nothing is read on failure.
    let mut buf = [0u8; 256];
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
    if rc != 0 {
        return UNKNOWN_HOST.to_string();
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    match std::str::from_utf8(&buf[..end]) {
        Ok(name) if !name.is_empty() => name.to_string(),
        _ => UNKNOWN_HOST.to_string(),
    }
}

#[cfg(not(unix))]
fn hostname() -> String {
    std::env::var("COMPUTERNAME")
        .ok()
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| UNKNOWN_HOST.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_700_000_000;

    fn holder(host: &str, session: &str, heartbeat: u64) -> LockHolder {
        LockHolder {
            host: host.to_string(),
            pid: 4213,
            session: session.to_string(),
            version: "0.6.0".to_string(),
            heartbeat,
        }
    }

    fn me() -> LockHolder {
        holder("my-mac", "session-a", 0)
    }

    /// Every rule the policy has, in one table: who is refused and why.
    #[test]
    fn who_may_take_the_lock() {
        let fresh = NOW - 5;
        let missed_one = NOW - REFRESH_INTERVAL.as_secs();
        let stale = NOW - STALE_AFTER.as_secs();
        let long_stale = NOW - STALE_AFTER.as_secs() * 10;

        // (what is on disk, force, blocked?, why)
        let cases: [(Option<LockHolder>, bool, bool, &str); 9] = [
            (None, false, false, "no lock file at all"),
            (
                Some(holder("linux-box", "session-b", fresh)),
                false,
                true,
                "another host, refreshing right now",
            ),
            (
                Some(holder("linux-box", "session-b", missed_one)),
                false,
                true,
                "another host, one refresh ago — well inside the window",
            ),
            (
                Some(holder("linux-box", "session-b", stale)),
                false,
                false,
                "another host, silent for the whole staleness window",
            ),
            (
                Some(holder("linux-box", "session-b", long_stale)),
                false,
                false,
                "another host, silent for hours",
            ),
            (
                Some(holder("linux-box", "session-b", fresh)),
                true,
                false,
                "another host, live, but the override was asked for",
            ),
            (
                Some(holder("my-mac", "session-z", fresh)),
                false,
                false,
                "this machine — the flock decides, not us",
            ),
            (
                Some(holder("linux-box", "session-a", fresh)),
                false,
                false,
                "our own session, whatever host it claims",
            ),
            (
                Some(holder("linux-box", "session-b", NOW + 3600)),
                false,
                true,
                "a clock an hour fast reads as fresh, not as wrapped-around",
            ),
        ];

        for (existing, force, blocked, why) in cases {
            let got = blocking_holder(existing.as_ref(), &me(), NOW, force);
            assert_eq!(got.is_some(), blocked, "{why}");
        }
    }

    /// A file that is not a lock record must not lock anyone out.
    #[test]
    fn an_unreadable_lock_file_is_no_claim() {
        let dir = tempfile::tempdir().unwrap();
        for junk in [b"".as_slice(), b"{", b"not json at all", b"{\"host\":1}"] {
            std::fs::write(dir.path().join(LOCK_FILE), junk).unwrap();
            assert!(read_holder(&dir.path().join(LOCK_FILE)).is_none());
            let lock = HostLock::acquire_as(dir.path(), me(), NOW, false)
                .unwrap_or_else(|h| panic!("refused by {h}"));
            drop(lock);
        }
    }

    /// Taking a fresh lock writes who we are, and closing removes it.
    #[test]
    fn a_lock_is_written_on_open_and_gone_on_close() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LOCK_FILE);

        let lock = HostLock::acquire_as(dir.path(), me(), NOW, false).unwrap();
        let written = read_holder(&path).expect("lock file written");
        assert_eq!(written.host, "my-mac");
        assert_eq!(written.session, "session-a");
        assert_eq!(written.heartbeat, NOW);
        assert_eq!(written.version, "0.6.0");

        drop(lock);
        assert!(!path.exists(), "a clean close leaves no lock behind");
    }

    /// The case this whole file exists for: a Mac over SMB and a Linux box
    /// over NFS, where `flock` does not carry between them.
    #[test]
    fn a_live_lock_from_another_host_is_refused_and_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let theirs = holder("linux-box", "session-b", NOW - 30);
        std::fs::write(
            dir.path().join(LOCK_FILE),
            serde_json::to_vec(&theirs).unwrap(),
        )
        .unwrap();

        let err = HostLock::acquire_as(dir.path(), me(), NOW, false).unwrap_err();
        assert_eq!(err, theirs);
        assert!(err.to_string().contains("linux-box"), "{err}");
        assert_eq!(
            read_holder(&dir.path().join(LOCK_FILE)).as_ref(),
            Some(&theirs),
            "a refused open must not disturb the holder's file"
        );
    }

    /// An abandoned lock is taken over, and the takeover is recorded.
    #[test]
    fn a_stale_lock_from_another_host_is_taken_over() {
        let dir = tempfile::tempdir().unwrap();
        let theirs = holder("linux-box", "session-b", NOW - STALE_AFTER.as_secs() - 1);
        std::fs::write(
            dir.path().join(LOCK_FILE),
            serde_json::to_vec(&theirs).unwrap(),
        )
        .unwrap();

        let lock = HostLock::acquire_as(dir.path(), me(), NOW, false).unwrap();
        let now_held = read_holder(&dir.path().join(LOCK_FILE)).unwrap();
        assert_eq!(now_held.session, "session-a");
        assert_eq!(now_held.host, "my-mac");
        drop(lock);
    }

    /// Closing must not delete a lock someone else took over meanwhile.
    #[test]
    fn a_close_leaves_a_successors_lock_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let lock = HostLock::acquire_as(dir.path(), me(), NOW, false).unwrap();

        let successor = holder("linux-box", "session-b", NOW + 1);
        std::fs::write(
            dir.path().join(LOCK_FILE),
            serde_json::to_vec(&successor).unwrap(),
        )
        .unwrap();

        drop(lock);
        assert_eq!(
            read_holder(&dir.path().join(LOCK_FILE)).as_ref(),
            Some(&successor)
        );
    }

    #[test]
    fn clearing_removes_the_file_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(LOCK_FILE), b"{}").unwrap();
        clear(dir.path()).unwrap();
        assert!(!dir.path().join(LOCK_FILE).exists());
        clear(dir.path()).unwrap();
    }

    /// The identity a real open writes has to be usable by a human reading the
    /// file: a hostname that is not empty, this process's pid, a version.
    #[test]
    fn a_real_identity_names_this_machine_and_process() {
        let me = identity();
        assert!(!me.host.is_empty());
        assert_eq!(me.pid, std::process::id());
        assert_eq!(me.version, env!("CARGO_PKG_VERSION"));
        // Stable within a process, so our own leftover file is recognised.
        assert_eq!(me.session, identity().session);
        assert_ne!(me.session, Uuid::new_v4().to_string());
    }

    /// The intervals are a documented promise; a careless edit should trip.
    #[test]
    fn the_staleness_window_is_a_generous_multiple_of_the_refresh() {
        assert_eq!(STALE_AFTER, REFRESH_INTERVAL * STALE_AFTER_REFRESHES);
        assert!(STALE_AFTER >= Duration::from_secs(20 * 60));
    }
}
