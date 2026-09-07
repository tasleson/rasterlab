//! The photo library: `.rlab` files on disk, indexed by a database.
//!
//! # What is consistent, and how it recovers
//!
//! Two stores have to be kept in step — the `.rlab` files under `files/` and
//! the `library.db` index — and no filesystem lets us update both at once.  So
//! rather than pretend the two-step write is atomic, the crate fixes which of
//! them is right and makes every step recoverable from that:
//!
//! **The `.rlab` file is the record.  The index is a cache of it.**  A `.rlab`
//! holds the original photograph, its edit stack, its thumbnail and its `LMTA`
//! metadata chunk; the database holds nothing that cannot be read back out of
//! the files.  That asymmetry decides the rest.
//!
//! * **Files are replaced atomically and verified.**  Every `.rlab` write goes
//!   through [`rasterlab_core::verified_write::write_verified_atomic`]: staged
//!   beside the destination, fsynced, read back, compared, then renamed into
//!   place.  A reader — including the next run after a crash — sees either the
//!   old file whole or the new one whole, never a half-written one.  Shard
//!   directories are created with
//!   [`create_dir_all_synced`](rasterlab_core::verified_write::create_dir_all_synced)
//!   so a first import into a fresh shard cannot lose the directory that names
//!   the file it just verified.
//!
//! * **Each index mutation is a transaction.**  One photo spans six tables, so
//!   [`db_trait::LibraryDb`] promises that a method's writes all land or none
//!   do.  An error means the index is unchanged, not half-changed.
//!
//! * **The file is written first.**  [`library::Library::update_metadata`] and
//!   the collection operations write the `.rlab` before the index, so an
//!   interruption costs at worst a stale row — the edit is already durable and
//!   a rebuild recovers it.  Deletion is the one inversion: storage goes
//!   first. Recently Deleted renames the file inside the library and then marks
//!   its row hidden, rolling the rename back if the index update fails.
//!
//! * **Names are the index's, identities are the files'.**  A photo's `.rlab`
//!   records the uuid of each collection it is in, so
//!   [`library::Library::rename_collection`] is one index row rather than a
//!   rewrite of every member file.  Each file also carries the name it last
//!   saw, used only when [`reconstruct::rebuild`] has to recreate a collection
//!   the index no longer knows — losing the index therefore costs a name's
//!   freshness, not the name.
//!
//! * **Multi-step operations are idempotent.**  Imports are keyed by content
//!   hash and skip what is already there, so a cancelled or crashed import
//!   resumes by simply being run again.  Session photo counts are recomputed
//!   from the rows rather than incremented, so an interrupted run leaves a
//!   correct number behind.  The bulk Recently Deleted operations are the same
//!   shape: each photo is moved, restored or erased on its own, so a run that
//!   is stopped or that trips over one bad photo leaves a library the user can
//!   simply carry on from.
//!
//! * **Reconciliation is one pass, and it is re-runnable.**
//!   [`reconstruct::rebuild`] walks the files and brings the index back to
//!   them: it indexes photos no row mentions, refreshes rows the files
//!   disagree with, and drops rows whose file is gone.  It never empties the
//!   index up front, and it declines to prune when it found no files at all —
//!   an empty `files/` is usually an unmounted volume.
//!
//! What is *not* claimed: nothing here coordinates two processes writing one
//! library at once, and a `.rlab` that is rewritten while another process
//! reads it hands that reader the old bytes, not an error.  What keeps that
//! from mattering is that a library is single-process to begin with: the index
//! holds an exclusive `flock` on `library.db` while it is open, so a second
//! process opening the same library fails with [`LibraryBusy`] instead of
//! joining it.  The lock lives on the file rather than in this crate, so it
//! covers a CLI run and a GUI session equally — but note it is only as good as
//! the filesystem underneath, and on a network mount that means checking that
//! locking is really passed to the server (an NFS mount with `nolock` or
//! `local_lock=flock` admits two writers).

pub mod db_trait;
pub mod import;
pub mod library;
pub mod reconstruct;
pub mod scrub;
pub mod search;
pub mod stoolap_db;
pub mod thumbnail;

pub use db_trait::{
    CollectionId, CollectionRow, ImportSessionRow, LibraryDb, PhotoId, PhotoRow,
    RecentlyDeletedRow, SortOrder,
};
pub use import::{ImportCollection, ImportSession, MONTH_NAMES, ymd_from_unix};
pub use library::{
    DeleteOutcome, DeleteProgress, ImportProgress, Library, LibraryBusy, NotALibrary,
    is_library_root,
};
pub use rasterlab_core::library_meta::{CollectionRef, LibraryExif, LibraryMeta};
pub use reconstruct::{RebuildOutcome, RebuildProgress};
pub use scrub::{ScrubOutcome, ScrubProgress};
pub use search::{Resolution, SearchFilter};
pub use stoolap_db::StoolapDb;
