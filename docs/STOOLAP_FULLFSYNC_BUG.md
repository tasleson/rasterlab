# Stoolap: every sync fails on a file system without F_FULLFSYNC

A database opened on an SMB mount from macOS **cannot write at all**. The first
`CREATE TABLE` fails, the WAL poisons itself, and close reports a checkpoint
that can never complete:

```
Warning: final checkpoint during close failed: WAL is poisoned after a write failure; reopen the database
Error: ... failed to sync WAL: Operation not supported (os error 45)
```

Nothing is wrong with the data or the mount. The write landed; only the *kind
of flush* stoolap asked for is unimplemented there, and `File::sync_all` has no
fallback, so the refusal is reported as a failed write.

- **Crate:** `stoolap` 0.4.1 (crates.io), <https://github.com/stoolap/stoolap>
- **rustc:** 1.98.1
- **Platform:** macOS (Darwin 25.6.0, aarch64), database on an `smbfs` mount
  served by Samba over a ZFS dataset
- **Also affects:** any file system that does not implement `F_FULLFSYNC` —
  other SMB clients, some FUSE and virtual file systems, and some VM shared
  folders. Local APFS and HFS+ are fine, and so is every non-Apple platform.
- **Found in:** [RasterLab](https://github.com/tasleson/rasterlab), whose photo
  library — `library.db` plus the photographs beside it — lives on a file
  server that several machines reach.

## The mechanism

On macOS, Rust's `File::sync_all` and `File::sync_data` do not compile to
`fsync(2)`. They compile to `fcntl(fd, F_FULLFSYNC)`, which is a stronger
request: it asks the *drive* to empty its own write cache onto the platters.
That is the right default for a laptop, and it is why a Mac tends to survive a
power cut with its data intact.

A file system that cannot make that promise does not quietly do less. It
refuses the fcntl outright:

| file system                        | `F_FULLFSYNC`     |
| ---------------------------------- | ----------------- |
| macOS `smbfs`                      | `ENOTSUP` (45)    |
| assorted network / virtual systems | `EINVAL`, `ENOTTY`|

Rust's standard library passes that refusal straight through as an
`io::Error`, with no fallback — confirmed against rustc 1.98.1. So on such a
mount *every* `sync_all` in stoolap fails, and since the WAL sync is on the
commit path, so does every write.

The important part is that the refusal says nothing about the data. Plain
`fsync(2)` works perfectly on the same descriptor on the same mount.

## Evidence

A probe on the affected share, writing a file and then flushing the same
descriptor three ways:

```
/Volumes/filer/picture-library   (//tony@filer.local/filer on smbfs)
  F_FULLFSYNC:  FAIL errno=45 (Operation not supported)
  fsync:        ok
  std sync_all: FAIL Operation not supported (os error 45)
```

On a local APFS volume all three succeed.

End to end, with the same binary, the same share and the same photographs —
only the stoolap version differs:

| stoolap                 | `library create` on the share | 25-photo import |
| ----------------------- | ----------------------------- | --------------- |
| 0.4.1 as published      | `failed to sync WAL: Operation not supported (os error 45)` | never reached |
| 0.4.1 + the fix below   | created                       | 25 of 25, 0 errors |

## Where it bites

`grep -rn "sync_all()\|sync_data()" src --include="*.rs"`. The sites whose
failure fails a user operation:

- `src/storage/mvcc/wal_manager.rs` — the WAL sync (`sync_with_file`), the
  retired-file sync, and the checkpoint metadata write. This is the one the
  user sees, because it is on the commit path.
- `src/storage/mvcc/engine.rs` — snapshot metadata, the per-timestamp DDL file,
  the snapshot manifest, the standalone volume copy and its marker.
- `src/storage/mvcc/snapshot.rs` — the snapshot file's final sync.
- `src/storage/volume/io.rs` — the volume catalog and its directory.
- `src/storage/volume/secondary.rs` — the secondary index side file.
- `src/storage/volume/manifest.rs` — the manifest directory.

A handful of others are already written `let _ = file.sync_all();` — best
effort by construction, and unaffected.

`src/storage/mvcc/file_lock.rs:95` writes the holder's pid and syncs with
`.ok()`, so it does not fail either; worth knowing it is silently not syncing
on these mounts.

## The proposed fix

Stoolap already has exactly this shape for the *barrier* in
`src/storage/volume/io.rs`: `sync_ordered` asks for `F_BARRIERFSYNC` and, when
`barrier_unsupported` recognises the refusal, stands something else in. The fix
is the same move one level down — a `sync_durable` that asks for the full sync
and, on `ENOTSUP`/`EINVAL`/`ENOTTY`, stands a plain `fsync(2)` in and reports
*its* result:

```rust
pub(crate) fn sync_durable(file: &std::fs::File) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        match file.sync_all() {
            Err(e) if barrier_unsupported(e.raw_os_error()) => {
                use std::os::unix::io::AsRawFd;
                // SAFETY: `file` is borrowed for the call, so the descriptor
                // stays open and valid, and fsync does not take ownership.
                if unsafe { libc::fsync(file.as_raw_fd()) } == 0 {
                    Ok(())
                } else {
                    Err(std::io::Error::last_os_error())
                }
            }
            other => other,
        }
    }
    #[cfg(not(unix))]
    {
        file.sync_all()
    }
}
```

Any other errno is the write's own and still propagates, so a full disk, a
broken connection or a late I/O error still fails the write as it should.

`sync_ordered`'s own fallback needs the same treatment: it currently falls back
to `sync_all`, which meets the identical wall on these mounts.

### On the weaker guarantee

`fsync` is genuinely less than `F_FULLFSYNC`: it promises the data reached the
file system, not that the far end has pushed it out of a drive cache. That
trade is worth making precisely because of *where* it applies. A drive cache
flush is a local operation; the SMB client has no way to carry it across the
wire to the server's disks, so on a network mount the stronger promise was
never on offer in the first place. The real choice there is between syncing as
far as the protocol reaches and storing nothing at all, and durability on such
a setup lives at the server anyway — in this user's case a ZFS mirror on ECC
memory, which is rather better placed to make the promise than the client is.

The fallback never applies on a local volume, where `F_FULLFSYNC` succeeds and
nothing changes.

## Reproduction

Any machine with a Mac and an SMB share; no special configuration.

```rust
use std::io::Write;

fn main() -> std::io::Result<()> {
    let path = std::env::args().nth(1).expect("usage: probe <dir-on-smb-share>");
    let probe = std::path::Path::new(&path).join("fullfsync-probe.tmp");

    let mut f = std::fs::File::create(&probe)?;
    f.write_all(b"probe")?;

    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;
        let fd = f.as_raw_fd();
        let full = unsafe { libc::fcntl(fd, libc::F_FULLFSYNC) };
        println!("F_FULLFSYNC: {:?}", (full == -1).then(std::io::Error::last_os_error));
        let plain = unsafe { libc::fsync(fd) };
        println!("fsync:       {:?}", (plain == -1).then(std::io::Error::last_os_error));
    }
    println!("sync_all:    {:?}", f.sync_all());

    std::fs::remove_file(&probe)
}
```

Or, straight through stoolap:

```rust
let db = stoolap::api::Database::open("file:///Volumes/your-smb-share/db")?;
db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY)", ())?;
// Error: failed to sync WAL: Operation not supported (os error 45)
```

## Working around it without a patch

Nothing inside stoolap's API helps: the sync is unconditional and on the commit
path. The only workarounds are to move the database to a local volume — which
defeats the point of a shared library — or to carry the patch, which is what
RasterLab does for now (`[patch.crates-io]` in its workspace `Cargo.toml`,
retired once this is merged).

---

🤖 Assisted-by: Claude Opus 5
