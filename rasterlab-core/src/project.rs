//! `.rlab` native project file format.
//!
//! # Binary layout
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────┐
//! │ Magic         8 bytes   b"RLAB\x00\x01\r\n"             │
//! │ Format ver.   2 bytes   u16 LE                           │
//! ├─────────────────────────────────────────────────────────┤
//! │ Chunk (repeated):                                        │
//! │   Tag         4 bytes   ASCII identifier                 │
//! │   Length      8 bytes   u64 LE  — byte length of Data   │
//! │   Data        N bytes                                    │
//! │   Hash       32 bytes   Blake3 of Data                   │
//! ├─────────────────────────────────────────────────────────┤
//! │ File hash    32 bytes   Blake3 of all preceding bytes    │
//! └─────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Defined chunks (in write order)
//!
//! | Tag    | Ver | Required | Contents                                        |
//! |--------|-----|----------|-------------------------------------------------|
//! | `META` | 1+  | yes      | JSON-encoded [`RlabMeta`]                       |
//! | `ORIG` | 1+  | yes      | Verbatim original source-file bytes             |
//! | `EDIT` | 1   | yes      | JSON-encoded [`PipelineState`] (single copy)    |
//! | `VCPS` | 2+  | yes      | JSON-encoded [`VcpsChunk`] (all virtual copies) |
//! | `PREV` | 1+  | no       | JPEG thumbnail of the rendered result           |
//! | `LMTA` | 3+  | no       | JSON-encoded [`LibraryMeta`] (library metadata) |
//! | `RECC` | 4+  | no       | Reed-Solomon parity blocks (bitrot recovery)    |
//!
//! Version 1 files have an `EDIT` chunk; version 2+ files use `VCPS` instead.
//! `LMTA` is written by the library importer and absent in editor-only files.
//! Unknown chunks are skipped on read, enabling forward compatibility.
//!
//! ## `RECC` parity placement
//!
//! `RECC` holds Reed-Solomon parity over a contiguous *protected region*, plus a
//! Blake3 hash of every data shard so damage can be pinpointed to a shard rather
//! than to a whole chunk.  Two identical copies are written; where they sit is
//! the difference between v4 and v5:
//!
//! ```text
//! v4:  MAGIC VER │ META ORIG VCPS PREV LMTA │ RECC RECC │ hash
//!      └──────── protected ────────────────┘
//!
//! v5:  MAGIC VER │ RECC │ META ORIG VCPS PREV LMTA │ RECC │ hash
//!                        └──────── protected ─────┘
//! ```
//!
//! v4 puts both copies at the tail, so truncation deep enough to reach the first
//! copy has already taken the second with it and recovery becomes impossible.
//! v5 anchors one copy at each end: truncation from either direction leaves the
//! opposite copy intact, and since the parity records `protected_len`, the
//! surviving copy's offset pins the region's alignment exactly.  Bytes missing
//! from either end then reduce to erased shards, recoverable within the parity
//! budget like any other damage.
//!
//! v5's protected region excludes the 10-byte file header, which sits ahead of
//! the leading `RECC` copy.  Nothing is lost: the header is two constants, which
//! [`verify_and_repair`] re-emits.
//!
//! [`verify_and_repair`] locates `RECC` by scanning for the tag signature and
//! validating candidates against the parity plan and Blake3 — never by walking
//! the chunk chain, since a corrupt length field would otherwise sever access to
//! the very parity that could have repaired it.

use std::{
    io::Read,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use reed_solomon_erasure::galois_8::ReedSolomon;
use serde::{Deserialize, Serialize};

use crate::{
    degraded_read::{DegradedRead, read_degraded_file},
    error::{RasterError, RasterResult},
    library_meta::LibraryMeta,
    pipeline::PipelineState,
    verified_write::write_verified,
};

// ── Constants ────────────────────────────────────────────────────────────────

/// Magic bytes that identify every `.rlab` file.
const MAGIC: &[u8; 8] = b"RLAB\x00\x01\r\n";

/// File extension of a project / managed-library container.
pub const RLAB_EXTENSION: &str = "rlab";

/// Format version written by [`RlabFile::write`] (v3, no ECC).
pub const FORMAT_VERSION: u16 = 3;

/// Format version written by [`RlabFile::write_v4`] (v4, both `RECC` copies at
/// the tail).  Still read, no longer written by new code — see
/// [`RlabFile::write_v5`].
pub const FORMAT_VERSION_V4: u16 = 4;

/// Format version written by [`RlabFile::write_v5`] (v5, one `RECC` copy at each
/// end of the protected region).
pub const FORMAT_VERSION_V5: u16 = 5;

/// `MAGIC` + `u16` format version.
const FILE_HEADER_LEN: usize = MAGIC.len() + 2;

/// Chunk tag (4) + `u64` length (8), preceding every chunk's data.
const CHUNK_HEADER_LEN: usize = 12;

/// Width of a Blake3 digest as stored in the file.
const HASH_LEN: usize = 32;

/// Largest `META` / `LMTA` payload a chunk-scanning reader will allocate.
/// Both hold small JSON documents; anything past this is a damaged length
/// field, not metadata.
const MAX_JSON_CHUNK_LEN: u64 = 16 * 1024 * 1024;

/// Fixed prefix of a `RECC` payload: shard size, shard counts, protected length.
const RECC_HEADER_LEN: usize = 20;

/// GF(2^8) Reed-Solomon cannot address more than this many shards in total.
const GF8_MAX_SHARDS: usize = 255;

const TAG_META: &[u8; 4] = b"META";
const TAG_ORIG: &[u8; 4] = b"ORIG";
#[allow(dead_code)] // v1 only — used as a literal in the read match arm
const TAG_EDIT: &[u8; 4] = b"EDIT";
const TAG_VCPS: &[u8; 4] = b"VCPS"; // v2+ — replaces EDIT
const TAG_PREV: &[u8; 4] = b"PREV";
const TAG_LMTA: &[u8; 4] = b"LMTA"; // v3+ — library metadata (optional)
const TAG_RECC: &[u8; 4] = b"RECC"; // v4+ — Reed-Solomon ECC parity (optional)

// GF(2^8) max total shards = 256; reserve 26 for parity → 230 data shards max.
const RECC_MAX_DATA_SHARDS: usize = 230;
const RECC_MIN_SHARD_SIZE: usize = 4096;

/// Data-shard cap used for files large enough to need shards bigger than
/// [`RECC_MIN_SHARD_SIZE`]. Leaves room for ~20 % parity (212 + 42 = 254 ≤ 255).
const RECC_LARGE_MAX_DATA_SHARDS: usize = 212;

/// Number of identical `RECC` chunks written by [`RlabFile::write_v4`].
/// Redundancy guards against bitrot inside the parity region itself: losing any
/// one copy still leaves a valid parity set for reconstruction.
const RECC_COPIES: usize = 2;

// ── Public types ─────────────────────────────────────────────────────────────

/// One virtual copy stored in a `.rlab` file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedCopy {
    /// Display name shown in the tab bar (e.g. "Copy 1", "B&W version").
    pub name: String,
    /// Serialised edit stack and undo cursor for this copy.
    pub pipeline_state: PipelineState,
}

/// JSON payload for the `VCPS` chunk.
#[derive(Debug, Serialize, Deserialize)]
struct VcpsChunk {
    /// Index of the copy that was active at save time.
    active: usize,
    copies: Vec<SavedCopy>,
}

/// Metadata stored in the `META` chunk of every `.rlab` file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RlabMeta {
    /// Semver string of the application that created the file (e.g. `"0.1.0"`).
    pub app_version: String,
    /// Unix timestamp (seconds) when the project was first saved.
    pub created_at: u64,
    /// Unix timestamp (seconds) when the project was most recently saved.
    pub modified_at: u64,
    /// Original source-file path at save time, if known.
    pub source_path: Option<String>,
    /// Width of the source image in pixels.
    pub width: u32,
    /// Height of the source image in pixels.
    pub height: u32,
}

impl RlabMeta {
    pub fn new(
        app_version: impl Into<String>,
        source_path: Option<impl Into<String>>,
        width: u32,
        height: u32,
    ) -> Self {
        let now = unix_now();
        Self {
            app_version: app_version.into(),
            created_at: now,
            modified_at: now,
            source_path: source_path.map(Into::into),
            width,
            height,
        }
    }

    /// Return a copy of `self` with `modified_at` updated to the current time.
    pub fn touch(mut self) -> Self {
        self.modified_at = unix_now();
        self
    }
}

/// In-memory representation of a `.rlab` project file.
#[derive(Debug)]
pub struct RlabFile {
    /// Format version read from the file header.
    pub format_version: u16,
    /// Project metadata.
    pub meta: RlabMeta,
    /// Verbatim bytes of the original source image — never re-encoded.
    pub original_bytes: Vec<u8>,
    /// Blake3 hash of [`original_bytes`](Self::original_bytes), verified on load.
    pub original_hash: [u8; 32],
    /// All virtual copies, in tab order.  Always non-empty.
    pub copies: Vec<SavedCopy>,
    /// Index of the copy that was active at save time.
    pub active_copy_index: usize,
    /// Embedded JPEG thumbnail of the rendered result, if present.
    pub thumbnail: Option<Vec<u8>>,
    /// Library metadata (keywords, rating, EXIF snapshot, etc.).
    /// Present only in files that were imported through the library.
    pub lmta: Option<LibraryMeta>,
}

/// Result returned by [`verify_and_repair`].
#[derive(Debug)]
pub struct VerifyReport {
    /// Format version from the file header, or `None` if the magic bytes did
    /// not match.  Only meaningful when the file is otherwise clean — on a
    /// damaged file the header itself may be part of the damage.
    pub format_version: Option<u16>,
    /// Bytes the media refused to return, zero-filled before verification.
    /// Non-zero means the file is on failing storage: even if the content
    /// verifies, it should be rewritten to relocate it off the bad sectors.
    pub unreadable_bytes: usize,
    /// Whether the whole-file Blake3 hash matched.
    pub file_hash_ok: bool,
    /// Tags of chunks whose per-chunk hash failed (e.g. `["ORIG"]`).
    pub damaged_chunks: Vec<String>,
    /// Whether a `RECC` chunk was found (and had a valid hash).
    pub recc_present: bool,
    /// Whether repair succeeded and was written to the output path.
    pub repaired: bool,
}

impl RlabFile {
    /// Construct a new [`RlabFile`] ready for writing.
    ///
    /// `original_bytes` should be the verbatim bytes of the source image file.
    /// `copies` is the ordered list of virtual copies (must be non-empty).
    /// `active_copy_index` is the index of the currently selected copy.
    /// `thumbnail` is an optional JPEG of the rendered result (e.g. 512 px wide).
    pub fn new(
        meta: RlabMeta,
        original_bytes: Vec<u8>,
        copies: Vec<SavedCopy>,
        active_copy_index: usize,
        thumbnail: Option<Vec<u8>>,
    ) -> Self {
        let original_hash = *blake3::hash(&original_bytes).as_bytes();
        Self {
            format_version: FORMAT_VERSION,
            meta,
            original_bytes,
            original_hash,
            copies,
            active_copy_index,
            thumbnail,
            lmta: None,
        }
    }

    /// Replace (or clear) the library metadata chunk.
    pub fn set_lmta(&mut self, lmta: Option<LibraryMeta>) {
        self.lmta = lmta;
    }

    // ── Write ────────────────────────────────────────────────────────────────

    /// Serialise and write the project to `path` as format v3 (no ECC).
    pub fn write(&self, path: &Path) -> RasterResult<()> {
        let mut buf: Vec<u8> = Vec::new();

        buf.extend_from_slice(MAGIC);
        buf.extend_from_slice(&FORMAT_VERSION.to_le_bytes());

        self.write_content_chunks(&mut buf)?;

        let file_hash = blake3::hash(&buf);
        buf.extend_from_slice(file_hash.as_bytes());

        std::fs::write(path, &buf)?;
        Ok(())
    }

    /// Serialise and write the project to `path` as format v5: content chunks
    /// bracketed by a `RECC` parity copy at each end.
    ///
    /// Prefer this over [`write_v4`](Self::write_v4) for all new files.  Both
    /// carry the same parity budget (~10 % of the protected region for small
    /// files, ~20 % for large ones, stored twice), but v5's split placement
    /// keeps one copy reachable when the other end of the file is truncated.
    ///
    /// The resulting file can be verified and repaired with [`verify_and_repair`].
    pub fn write_v5(&self, path: &Path) -> RasterResult<()> {
        let mut content: Vec<u8> = Vec::new();
        self.write_content_chunks(&mut content)?;
        write_verified(path, &assemble_v5(&content)?)?;
        Ok(())
    }

    /// Serialise and write the project to `path` as format v4 with `RECC`
    /// Reed-Solomon parity chunks (~10% parity × [`RECC_COPIES`] ≈ 20% overhead).
    ///
    /// The parity chunk is written [`RECC_COPIES`] times so that bitrot landing
    /// inside the parity region itself is survivable — losing any one copy
    /// still leaves a valid parity set.
    ///
    /// Retained so the v4 layout stays exercised by tests; new files should use
    /// [`write_v5`](Self::write_v5), which survives truncation from either end.
    ///
    /// The resulting file can be verified and repaired with [`verify_and_repair`].
    pub fn write_v4(&self, path: &Path) -> RasterResult<()> {
        // Build the "protected" region: header + all content chunks.
        let mut protected: Vec<u8> = Vec::new();
        protected.extend_from_slice(MAGIC);
        protected.extend_from_slice(&FORMAT_VERSION_V4.to_le_bytes());
        self.write_content_chunks(&mut protected)?;

        // Compute RECC parity once, append it RECC_COPIES times.
        let recc_payload = build_recc_payload(&protected)?;
        let mut buf = protected;
        for _ in 0..RECC_COPIES {
            write_chunk(&mut buf, TAG_RECC, &recc_payload);
        }

        let file_hash = blake3::hash(&buf);
        buf.extend_from_slice(file_hash.as_bytes());

        std::fs::write(path, &buf)?;
        Ok(())
    }

    /// Write META, ORIG, VCPS, PREV, LMTA chunks into `buf` (shared by v3 and v4 write paths).
    fn write_content_chunks(&self, buf: &mut Vec<u8>) -> RasterResult<()> {
        let meta_json = serde_json::to_vec(&self.meta)
            .map_err(|e| RasterError::Serialization(e.to_string()))?;
        write_chunk(buf, TAG_META, &meta_json);

        write_chunk(buf, TAG_ORIG, &self.original_bytes);

        let vcps = VcpsChunk {
            active: self.active_copy_index,
            copies: self.copies.clone(),
        };
        let vcps_json =
            serde_json::to_vec(&vcps).map_err(|e| RasterError::Serialization(e.to_string()))?;
        write_chunk(buf, TAG_VCPS, &vcps_json);

        if let Some(thumb) = &self.thumbnail {
            write_chunk(buf, TAG_PREV, thumb);
        }

        if let Some(lmta) = &self.lmta {
            let lmta_json =
                serde_json::to_vec(lmta).map_err(|e| RasterError::Serialization(e.to_string()))?;
            write_chunk(buf, TAG_LMTA, &lmta_json);
        }

        Ok(())
    }

    // ── Read ─────────────────────────────────────────────────────────────────

    /// Read and fully verify a `.rlab` project from `path`.
    ///
    /// Returns an error if:
    /// - The file-level hash does not match (corrupted or truncated file).
    /// - Any required chunk hash does not match.
    /// - A required chunk (`META`, `ORIG`, `EDIT`) is missing.
    /// - The magic bytes do not match.
    /// - The format version is newer than [`FORMAT_VERSION_V5`].
    pub fn read(path: &Path) -> RasterResult<Self> {
        Self::read_bytes(&std::fs::read(path)?)
    }

    /// Read and fully verify a `.rlab` project from an in-memory image.
    ///
    /// Same contract as [`read`](Self::read); used by the repair path to prove a
    /// reconstruction actually parses before it is committed to disk.
    pub fn read_bytes(data: &[u8]) -> RasterResult<Self> {
        // ── File-level hash ───────────────────────────────────────────────
        if data.len() < FILE_HEADER_LEN + HASH_LEN {
            return Err(RasterError::decode("rlab", "file too short"));
        }
        let (payload, file_hash_stored) = data.split_at(data.len() - HASH_LEN);
        let file_hash_computed = blake3::hash(payload);
        if file_hash_computed.as_bytes() != file_hash_stored {
            return Err(RasterError::decode(
                "rlab",
                "file integrity check failed — file may be corrupted",
            ));
        }

        let mut cur = std::io::Cursor::new(payload);

        // ── Magic ─────────────────────────────────────────────────────────
        let mut magic = [0u8; 8];
        cur.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(RasterError::decode(
                "rlab",
                "invalid magic bytes — not a .rlab project file",
            ));
        }

        // ── Format version ────────────────────────────────────────────────
        let mut ver = [0u8; 2];
        cur.read_exact(&mut ver)?;
        let format_version = u16::from_le_bytes(ver);
        if format_version > FORMAT_VERSION_V5 {
            return Err(RasterError::decode(
                "rlab",
                format!(
                    "unsupported format version {format_version} \
                     (this build supports up to {FORMAT_VERSION_V5})"
                ),
            ));
        }

        // ── Chunks ────────────────────────────────────────────────────────
        let mut meta: Option<RlabMeta> = None;
        let mut original_bytes: Option<Vec<u8>> = None;
        let mut original_hash: Option<[u8; 32]> = None;
        let mut edit_v1: Option<PipelineState> = None;
        let mut vcps: Option<VcpsChunk> = None;
        let mut thumbnail: Option<Vec<u8>> = None;
        let mut lmta: Option<LibraryMeta> = None;

        loop {
            if cur.position() as usize >= payload.len() {
                break;
            }

            let mut tag = [0u8; 4];
            match cur.read_exact(&mut tag) {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e.into()),
            }

            let mut len_buf = [0u8; 8];
            cur.read_exact(&mut len_buf)?;
            let len = u64::from_le_bytes(len_buf) as usize;

            let mut chunk_data = vec![0u8; len];
            cur.read_exact(&mut chunk_data)?;

            let mut chunk_hash_stored = [0u8; 32];
            cur.read_exact(&mut chunk_hash_stored)?;

            let chunk_hash_computed = blake3::hash(&chunk_data);
            if chunk_hash_computed.as_bytes() != &chunk_hash_stored {
                return Err(RasterError::decode(
                    "rlab",
                    format!(
                        "chunk '{}' integrity check failed",
                        String::from_utf8_lossy(&tag)
                    ),
                ));
            }

            match &tag {
                b"META" => {
                    let m: RlabMeta = serde_json::from_slice(&chunk_data)
                        .map_err(|e| RasterError::Serialization(e.to_string()))?;
                    meta = Some(m);
                }
                b"ORIG" => {
                    original_hash = Some(chunk_hash_stored);
                    original_bytes = Some(chunk_data);
                }
                b"EDIT" => {
                    // Version 1 files only — synthesised into a single SavedCopy on load.
                    let state: PipelineState = serde_json::from_slice(&chunk_data)
                        .map_err(|e| RasterError::Serialization(e.to_string()))?;
                    edit_v1 = Some(state);
                }
                b"VCPS" => {
                    let v: VcpsChunk = serde_json::from_slice(&chunk_data)
                        .map_err(|e| RasterError::Serialization(e.to_string()))?;
                    vcps = Some(v);
                }
                b"PREV" => {
                    thumbnail = Some(chunk_data);
                }
                b"LMTA" => {
                    let m: LibraryMeta = serde_json::from_slice(&chunk_data)
                        .map_err(|e| RasterError::Serialization(e.to_string()))?;
                    lmta = Some(m);
                }
                _ => {
                    // Unknown/reserved chunks (including RECC) — skip for forward compat
                }
            }
        }

        // ── Require mandatory chunks ───────────────────────────────────────
        let meta = meta.ok_or_else(|| RasterError::decode("rlab", "missing META chunk"))?;
        let original_bytes =
            original_bytes.ok_or_else(|| RasterError::decode("rlab", "missing ORIG chunk"))?;
        let original_hash =
            original_hash.ok_or_else(|| RasterError::decode("rlab", "missing ORIG chunk"))?;

        let (copies, active_copy_index) = if format_version == 1 {
            let ps = edit_v1.ok_or_else(|| RasterError::decode("rlab", "missing EDIT chunk"))?;
            (
                vec![SavedCopy {
                    name: "Copy 1".into(),
                    pipeline_state: ps,
                }],
                0usize,
            )
        } else {
            let v = vcps.ok_or_else(|| RasterError::decode("rlab", "missing VCPS chunk"))?;
            let active = v.active.min(v.copies.len().saturating_sub(1));
            (v.copies, active)
        };

        Ok(Self {
            format_version,
            meta,
            original_bytes,
            original_hash,
            copies,
            active_copy_index,
            thumbnail,
            lmta,
        })
    }
}

// ── Verify / repair ───────────────────────────────────────────────────────────

/// Verify the integrity of a `.rlab` file and optionally repair it.
///
/// Pass `repair_to = Some(path)` to write a repaired copy when corruption is
/// detected and a usable `RECC` chunk is present.  Repair succeeds as long as
/// the number of damaged shards does not exceed the parity shard count (~10 %
/// of the protected region for small files, ~20 % for large ones).
///
/// Damage need not be in-place: bytes missing from either end of the file are
/// treated as erased shards and reconstructed on the same budget, because the
/// surviving `RECC` copy's offset and its recorded `protected_len` between them
/// pin where the protected region began.
///
/// Repaired output is always written in the v5 layout, whatever the input was.
///
/// Sectors the media cannot return are zero-filled rather than aborting the
/// read (see [`crate::degraded_read`]), so a latent sector error costs the
/// shards it landed in and nothing more.
///
/// If the file is clean, no output file is written even when `repair_to` is
/// `Some`.
pub fn verify_and_repair(path: &Path, repair_to: Option<&Path>) -> RasterResult<VerifyReport> {
    verify_and_repair_degraded(&read_degraded_file(path)?, repair_to)
}

/// [`verify_and_repair`] over an already-read image.
///
/// Split out so callers that have paid for the read can reuse it, and so the
/// unreadable-media path is testable without a failing disk.
pub fn verify_and_repair_degraded(
    read: &DegradedRead,
    repair_to: Option<&Path>,
) -> RasterResult<VerifyReport> {
    let data = &read.data;
    if data.len() < FILE_HEADER_LEN + HASH_LEN {
        return Err(RasterError::decode("rlab", "file too short"));
    }

    let (payload, file_hash_bytes) = data.split_at(data.len() - HASH_LEN);
    let file_hash_ok = blake3::hash(payload).as_bytes() == file_hash_bytes;

    // Chunk-chain walk, used only for the human-facing report. It gives up at
    // the first unparseable length field, which is exactly why repair does not
    // depend on it.
    let scan = scan_chunks(payload);

    // Parity lookup is independent of the chain: scan the whole file (including
    // the trailing hash region, which on a truncated file is really chunk data)
    // for RECC signatures.
    let recc_copies = find_recc_copies(data);

    let mut damaged_chunks: Vec<String> = scan
        .chunks
        .iter()
        .filter(|c| !c.hash_ok)
        .map(|c| String::from_utf8_lossy(&c.tag).into_owned())
        .collect();
    if scan.any_recc_damaged {
        // Surfaces "bad chunks: RECC" in the verify report when a parity copy
        // rotted. Harmless alongside the other triggers: any_recc_damaged
        // implies !file_hash_ok too, so repair would fire anyway — this just
        // makes the cause visible in the report.
        damaged_chunks.push("RECC".into());
    }

    let recc_present = scan.recc_present || !recc_copies.is_empty();

    let format_version = read_format_version(data);
    let unreadable_bytes = read.unreadable_bytes();

    // Unreadable sectors count as damage even when the content still verifies —
    // which happens when the lost bytes were zero anyway. The bytes are fine;
    // the media is not, and rewriting is what moves the file off it.
    if file_hash_ok && damaged_chunks.is_empty() && read.is_intact() {
        return Ok(VerifyReport {
            format_version,
            unreadable_bytes,
            file_hash_ok: true,
            damaged_chunks: vec![],
            recc_present,
            repaired: false,
        });
    }

    let repaired = match repair_to {
        Some(repair_path) => attempt_repair(data, &recc_copies, repair_path)?,
        None => false,
    };

    Ok(VerifyReport {
        format_version,
        unreadable_bytes,
        file_hash_ok,
        damaged_chunks,
        recc_present,
        repaired,
    })
}

/// True when `path` names a `.rlab` container, matching the extension
/// case-insensitively so `PHOTO.RLAB` is recognised too.
pub fn is_rlab_path(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(RLAB_EXTENSION))
}

/// Name of the image a `.rlab` was made from, for display.
///
/// Library files are named after the Blake3 of their content, so their own
/// file name tells the user nothing; the name they imported lives in `LMTA`,
/// and an editor-only project keeps its source path in `META`.  Prefers the
/// former, falls back to the latter, and returns `None` when neither records
/// one.
///
/// Seeks over `ORIG` and the parity chunks rather than loading them, so this
/// is cheap enough to call while building a list of frames.
pub fn read_original_filename(path: &Path) -> RasterResult<Option<String>> {
    use std::io::{Seek, SeekFrom};

    let mut file = std::fs::File::open(path)?;
    let file_len = file.metadata()?.len();

    let mut header = [0u8; FILE_HEADER_LEN];
    file.read_exact(&mut header)?;
    if !header.starts_with(MAGIC) {
        return Err(RasterError::decode(
            "rlab",
            "invalid magic bytes — not a .rlab project file",
        ));
    }

    // The whole-file digest sits past the last chunk; walking into it would
    // read its bytes as a chunk header.
    let chunks_end = file_len.saturating_sub(HASH_LEN as u64);
    let mut pos = FILE_HEADER_LEN as u64;

    // `META` precedes `LMTA` in write order, so hold its name until the whole
    // chunk chain has been walked.
    let mut from_meta = None;

    while pos + (CHUNK_HEADER_LEN + HASH_LEN) as u64 <= chunks_end {
        let mut head = [0u8; CHUNK_HEADER_LEN];
        file.read_exact(&mut head)?;
        let tag: [u8; 4] = head[..4].try_into().expect("4-byte tag");
        let len = u64::from_le_bytes(head[4..].try_into().expect("8-byte length"));

        // A damaged length can point past the chunk chain. Stop the walk and
        // report whatever was read so far — this is a display name, and
        // refusing to produce one is worse than producing a partial answer.
        let Some(next) = pos
            .checked_add((CHUNK_HEADER_LEN + HASH_LEN) as u64)
            .and_then(|p| p.checked_add(len))
            .filter(|next| *next <= chunks_end)
        else {
            break;
        };

        // Only the two small JSON chunks are worth reading.
        if (tag == *TAG_META || tag == *TAG_LMTA) && len <= MAX_JSON_CHUNK_LEN {
            let mut data = vec![0u8; len as usize];
            file.read_exact(&mut data)?;

            if tag == *TAG_LMTA {
                // The imported original's name — the best answer, so stop here.
                if let Ok(lmta) = serde_json::from_slice::<LibraryMeta>(&data)
                    && let Some(name) = lmta.original_filename
                {
                    return Ok(Some(name));
                }
            } else if let Ok(meta) = serde_json::from_slice::<RlabMeta>(&data) {
                from_meta = meta
                    .source_path
                    .as_deref()
                    .map(Path::new)
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().into_owned());
            }
        }

        pos = next;
        file.seek(SeekFrom::Start(pos))?;
    }

    Ok(from_meta)
}

/// Read the Blake3 of the embedded original image out of the `ORIG` chunk.
///
/// Seeks over the image payload rather than loading it, so this stays cheap
/// enough to run across a whole library.  The point is to learn *which* photo a
/// file holds, not to re-verify that it holds it intact — [`verify_and_repair`]
/// covers the latter, and covers it over the same bytes this hash describes.
pub fn read_original_hash(path: &Path) -> RasterResult<[u8; 32]> {
    use std::io::{Seek, SeekFrom};

    let mut file = std::fs::File::open(path)?;

    let mut header = [0u8; FILE_HEADER_LEN];
    file.read_exact(&mut header)?;
    if !header.starts_with(MAGIC) {
        return Err(RasterError::decode(
            "rlab",
            "invalid magic bytes — not a .rlab project file",
        ));
    }

    loop {
        let mut tag = [0u8; 4];
        match file.read_exact(&mut tag) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        }

        let mut len_buf = [0u8; 8];
        file.read_exact(&mut len_buf)?;
        let len = i64::try_from(u64::from_le_bytes(len_buf))
            .map_err(|_| RasterError::decode("rlab", "chunk length out of range"))?;

        if &tag == TAG_ORIG {
            file.seek(SeekFrom::Current(len))?;
            let mut hash = [0u8; HASH_LEN];
            file.read_exact(&mut hash)?;
            return Ok(hash);
        }

        file.seek(SeekFrom::Current(len + HASH_LEN as i64))?;
    }

    Err(RasterError::decode("rlab", "missing ORIG chunk"))
}

/// Read the format version straight from the file header, without parsing.
fn read_format_version(data: &[u8]) -> Option<u16> {
    if !data.starts_with(MAGIC) || data.len() < FILE_HEADER_LEN {
        return None;
    }
    Some(u16::from_le_bytes(
        data[MAGIC.len()..FILE_HEADER_LEN].try_into().unwrap(),
    ))
}

// ── Internal scan helpers ─────────────────────────────────────────────────────

struct ChunkInfo {
    tag: [u8; 4],
    hash_ok: bool,
}

struct ScanResult {
    chunks: Vec<ChunkInfo>,
    /// True if at least one `RECC` tag was reached by the chain walk.
    recc_present: bool,
    /// True if any `RECC` copy the walk reached failed its per-chunk hash —
    /// triggers a heal-on-repair even when the protected region is intact.
    any_recc_damaged: bool,
}

/// Walk the chunk chain from the header, recording each chunk's tag and whether
/// its Blake3 matched.  Best-effort: stops at the first chunk whose declared
/// length runs past the end of `payload`.
fn scan_chunks(payload: &[u8]) -> ScanResult {
    let mut pos = FILE_HEADER_LEN;
    let mut chunks = Vec::new();
    let mut recc_present = false;
    let mut any_recc_damaged = false;

    while pos + CHUNK_HEADER_LEN <= payload.len() {
        let tag: [u8; 4] = payload[pos..pos + 4].try_into().unwrap();
        let len = u64::from_le_bytes(payload[pos + 4..pos + 12].try_into().unwrap()) as usize;
        let data_start = pos + CHUNK_HEADER_LEN;
        let Some(hash_end) = data_start
            .checked_add(len)
            .and_then(|e| e.checked_add(HASH_LEN))
        else {
            break;
        };
        if hash_end > payload.len() {
            break;
        }
        let data_end = hash_end - HASH_LEN;

        let hash_ok =
            blake3::hash(&payload[data_start..data_end]).as_bytes() == &payload[data_end..hash_end];

        if &tag == TAG_RECC {
            recc_present = true;
            any_recc_damaged |= !hash_ok;
        } else {
            chunks.push(ChunkInfo { tag, hash_ok });
        }

        pos = hash_end;
    }

    ScanResult {
        chunks,
        recc_present,
        any_recc_damaged,
    }
}

// ── Parity location ───────────────────────────────────────────────────────────

/// The shard geometry recorded in a `RECC` payload header.
#[derive(Clone, Copy)]
struct ReccPlan {
    shard_size: usize,
    data_shards: usize,
    parity_shards: usize,
    protected_len: usize,
}

/// A `RECC` chunk found by signature scan and validated end to end.
struct ReccCopy {
    /// Byte offset of the `RECC` tag within the file as it exists on disk.
    offset: usize,
    /// On-disk size of the whole chunk: tag + length + payload + hash.
    chunk_len: usize,
    payload: Vec<u8>,
    plan: ReccPlan,
}

/// Parse and sanity-check a `RECC` payload header.
///
/// Every field is cross-checked against the others so a `RECC` byte sequence
/// occurring by chance inside `ORIG` image data is rejected before its Blake3 is
/// computed.
fn parse_recc_plan(payload: &[u8]) -> Option<ReccPlan> {
    if payload.len() < RECC_HEADER_LEN {
        return None;
    }
    let shard_size = u32::from_le_bytes(payload[0..4].try_into().unwrap()) as usize;
    let data_shards = u32::from_le_bytes(payload[4..8].try_into().unwrap()) as usize;
    let parity_shards = u32::from_le_bytes(payload[8..12].try_into().unwrap()) as usize;
    let protected_len = u64::from_le_bytes(payload[12..20].try_into().unwrap()) as usize;

    if shard_size == 0
        || !shard_size.is_multiple_of(RECC_MIN_SHARD_SIZE)
        || data_shards == 0
        || parity_shards == 0
        || data_shards + parity_shards > GF8_MAX_SHARDS
        || protected_len == 0
    {
        return None;
    }

    // The payload length is fully determined by the geometry, as is the shard
    // count by the protected length — both must agree exactly.
    let expected_len = RECC_HEADER_LEN
        .checked_add(data_shards.checked_mul(HASH_LEN)?)?
        .checked_add(parity_shards.checked_mul(shard_size)?)?;
    if payload.len() != expected_len || protected_len.div_ceil(shard_size) != data_shards {
        return None;
    }

    Some(ReccPlan {
        shard_size,
        data_shards,
        parity_shards,
        protected_len,
    })
}

/// Find every intact `RECC` chunk in `data` by scanning for the tag signature.
///
/// Deliberately independent of the chunk chain: a corrupt length field anywhere
/// ahead of the parity must not hide the parity that would repair it.
fn find_recc_copies(data: &[u8]) -> Vec<ReccCopy> {
    let mut out = Vec::new();
    if data.len() < CHUNK_HEADER_LEN {
        return out;
    }

    for offset in 0..=data.len() - CHUNK_HEADER_LEN {
        if &data[offset..offset + 4] != TAG_RECC {
            continue;
        }
        let len = u64::from_le_bytes(
            data[offset + 4..offset + CHUNK_HEADER_LEN]
                .try_into()
                .unwrap(),
        ) as usize;
        let data_start = offset + CHUNK_HEADER_LEN;
        let Some(hash_end) = data_start
            .checked_add(len)
            .and_then(|e| e.checked_add(HASH_LEN))
        else {
            continue;
        };
        if hash_end > data.len() {
            continue;
        }
        let data_end = hash_end - HASH_LEN;
        let payload = &data[data_start..data_end];

        let Some(plan) = parse_recc_plan(payload) else {
            continue;
        };
        if blake3::hash(payload).as_bytes() != &data[data_end..hash_end] {
            continue;
        }

        out.push(ReccCopy {
            offset,
            chunk_len: hash_end - offset,
            payload: payload.to_vec(),
            plan,
        });
    }
    out
}

/// Offsets at which the protected region could begin, given a copy found at a
/// known offset.
///
/// A copy does not record which layout it belongs to, so all three placements
/// are tried.  Values may be negative, meaning that many leading bytes are
/// missing from the file.  A wrong guess is not a correctness hazard: it
/// misaligns every shard, so essentially all shard hashes fail and the candidate
/// is rejected long before Reed-Solomon runs.
fn candidate_starts(copy: &ReccCopy) -> [i64; 3] {
    let offset = copy.offset as i64;
    let chunk_len = copy.chunk_len as i64;
    let protected_len = copy.plan.protected_len as i64;
    [
        offset + chunk_len,                 // v5 leading copy
        offset - protected_len,             // v5 trailing copy, or v4 first copy
        offset - protected_len - chunk_len, // v4 second copy
    ]
}

// ── Repair ────────────────────────────────────────────────────────────────────

/// Rebuild the protected region assuming it begins at `start` in `data`.
///
/// `start` is signed: a negative value means the file lost that many leading
/// bytes, and the shards covering them are simply erased.  Bytes falling outside
/// the file in either direction are zero-filled, which makes truncation
/// indistinguishable from in-place corruption as far as the erasure decoder is
/// concerned.
///
/// Returns `None` if erasures exceed the parity budget — the same signal used to
/// reject a wrong `start` hypothesis.
fn rebuild_protected(data: &[u8], copy: &ReccCopy, start: i64) -> Option<Vec<u8>> {
    let ReccPlan {
        shard_size,
        data_shards,
        parity_shards,
        protected_len,
    } = copy.plan;

    let hashes_end = RECC_HEADER_LEN + data_shards * HASH_LEN;
    let shard_hashes = &copy.payload[RECC_HEADER_LEN..hashes_end];
    let parity_data = &copy.payload[hashes_end..];

    let mut shards: Vec<Option<Vec<u8>>> = Vec::with_capacity(data_shards + parity_shards);
    let mut erasures = 0usize;

    for i in 0..data_shards {
        let mut shard = vec![0u8; shard_size];

        // Byte range this shard covers within the protected region. The tail
        // shard stops at protected_len; the encoder zero-padded the remainder,
        // so leaving it zero here reproduces what was hashed.
        let region_begin = i * shard_size;
        let region_end = ((i + 1) * shard_size).min(protected_len);
        if region_begin < region_end {
            // Project onto the file and keep only what actually exists.
            let file_begin = start + region_begin as i64;
            let file_end = start + region_end as i64;
            let lo = file_begin.max(0);
            let hi = file_end.min(data.len() as i64);
            if lo < hi {
                let dst = (lo - file_begin) as usize;
                let n = (hi - lo) as usize;
                shard[dst..dst + n].copy_from_slice(&data[lo as usize..hi as usize]);
            }
        }

        if blake3::hash(&shard).as_bytes() == &shard_hashes[i * HASH_LEN..(i + 1) * HASH_LEN] {
            shards.push(Some(shard));
        } else {
            shards.push(None);
            erasures += 1;
            if erasures > parity_shards {
                return None;
            }
        }
    }

    for i in 0..parity_shards {
        shards.push(Some(
            parity_data[i * shard_size..(i + 1) * shard_size].to_vec(),
        ));
    }

    let rs = ReedSolomon::new(data_shards, parity_shards).ok()?;
    rs.reconstruct_data(&mut shards).ok()?;

    let mut protected = Vec::with_capacity(protected_len);
    for shard in shards.iter().take(data_shards) {
        protected.extend_from_slice(shard.as_ref()?);
    }
    protected.truncate(protected_len);
    Some(protected)
}

/// Attempt to reconstruct the file from any surviving `RECC` copy.
///
/// Returns `true` and writes a fresh v5 file if reconstruction succeeds. The
/// output is validated by a full parse before it is committed, so a repair that
/// somehow produced nonsense is reported as a failure rather than written.
fn attempt_repair(data: &[u8], copies: &[ReccCopy], repair_to: &Path) -> RasterResult<bool> {
    for copy in copies {
        for start in candidate_starts(copy) {
            let Some(protected) = rebuild_protected(data, copy, start) else {
                continue;
            };

            // v4 protects the file header along with the content chunks; v5
            // protects the content alone. The magic tells the two apart.
            let content = match protected.strip_prefix(MAGIC.as_slice()) {
                Some(rest) => &rest[2..],
                None => &protected[..],
            };

            // Parity is recomputed rather than reused: a v4 payload describes a
            // protected region that included the header, so it would not
            // describe the v5 file being written here.
            let rebuilt = assemble_v5(content)?;
            if RlabFile::read_bytes(&rebuilt).is_err() {
                continue;
            }

            std::fs::write(repair_to, &rebuilt)?;
            return Ok(true);
        }
    }
    Ok(false)
}

// ── RECC encoding helpers ─────────────────────────────────────────────────────

/// Lay out a complete v5 file around already-serialised `content` chunks.
///
/// ```text
/// MAGIC │ VER │ RECC │ content │ RECC │ file hash
/// ```
fn assemble_v5(content: &[u8]) -> RasterResult<Vec<u8>> {
    let recc_payload = build_recc_payload(content)?;
    let recc_chunk_len = CHUNK_HEADER_LEN + recc_payload.len() + HASH_LEN;

    let mut buf =
        Vec::with_capacity(FILE_HEADER_LEN + content.len() + 2 * recc_chunk_len + HASH_LEN);
    buf.extend_from_slice(MAGIC);
    buf.extend_from_slice(&FORMAT_VERSION_V5.to_le_bytes());
    write_chunk(&mut buf, TAG_RECC, &recc_payload);
    buf.extend_from_slice(content);
    write_chunk(&mut buf, TAG_RECC, &recc_payload);

    let file_hash = blake3::hash(&buf);
    buf.extend_from_slice(file_hash.as_bytes());
    Ok(buf)
}

/// Build the binary payload stored inside the `RECC` chunk.
///
/// Layout:
/// ```text
/// [shard_size u32 LE][data_shards u32 LE][parity_shards u32 LE][protected_len u64 LE]
/// [data_shards × 32 bytes]  — per-data-shard Blake3 hashes (used to pinpoint erasures)
/// [parity_shards × shard_size bytes]  — RS parity shards
/// ```
fn build_recc_payload(protected: &[u8]) -> RasterResult<Vec<u8>> {
    let (shard_size, data_shards, parity_shards) = compute_shard_plan(protected.len());

    let rs = ReedSolomon::new(data_shards, parity_shards)
        .map_err(|e| RasterError::decode("recc", e.to_string()))?;

    let mut shards: Vec<Vec<u8>> = (0..data_shards)
        .map(|i| {
            let start = i * shard_size;
            let end = ((i + 1) * shard_size).min(protected.len());
            let mut s = vec![0u8; shard_size];
            s[..end - start].copy_from_slice(&protected[start..end]);
            s
        })
        .chain((0..parity_shards).map(|_| vec![0u8; shard_size]))
        .collect();

    rs.encode(&mut shards)
        .map_err(|e| RasterError::encode("recc", e.to_string()))?;

    let mut payload = Vec::with_capacity(20 + data_shards * 32 + parity_shards * shard_size);

    // Fixed header
    payload.extend_from_slice(&(shard_size as u32).to_le_bytes());
    payload.extend_from_slice(&(data_shards as u32).to_le_bytes());
    payload.extend_from_slice(&(parity_shards as u32).to_le_bytes());
    payload.extend_from_slice(&(protected.len() as u64).to_le_bytes());

    // Per-data-shard hashes (enable precise erasure detection during repair)
    for shard in shards.iter().take(data_shards) {
        payload.extend_from_slice(blake3::hash(shard).as_bytes());
    }

    // Parity shards
    for shard in &shards[data_shards..] {
        payload.extend_from_slice(shard);
    }
    Ok(payload)
}

/// Choose a shard layout for a protected region of `data_len` bytes.
///
/// Returns `(shard_size, data_shards, parity_shards)`.
///
/// Two regimes:
/// - **Small files** — fit within [`RECC_MAX_DATA_SHARDS`] shards of
///   [`RECC_MIN_SHARD_SIZE`]: use 4 KiB shards with ~10 % parity.
/// - **Large files** — need shards bigger than [`RECC_MIN_SHARD_SIZE`]: size
///   shards against [`RECC_LARGE_MAX_DATA_SHARDS`] and raise the target to
///   ~20 % parity, trading a little data capacity for correction capacity.
///   Guaranteed to stay under the 255-shard GF(2^8) limit.
fn compute_shard_plan(data_len: usize) -> (usize, usize, usize) {
    let small_shards = data_len.div_ceil(RECC_MIN_SHARD_SIZE).max(1);
    if small_shards <= RECC_MAX_DATA_SHARDS {
        let parity = (small_shards / 10).max(1);
        return (RECC_MIN_SHARD_SIZE, small_shards, parity);
    }

    let shard_size = round_up(
        data_len.div_ceil(RECC_LARGE_MAX_DATA_SHARDS),
        RECC_MIN_SHARD_SIZE,
    );
    let data_shards = data_len.div_ceil(shard_size);
    let parity_shards = (data_shards / 5).max(1).min(255 - data_shards);
    (shard_size, data_shards, parity_shards)
}

fn round_up(n: usize, align: usize) -> usize {
    n.div_ceil(align) * align
}

// ── Private chunk writer ──────────────────────────────────────────────────────

/// Append a chunk (`tag` + `u64 LE length` + `data` + `blake3(data)`) to `buf`.
fn write_chunk(buf: &mut Vec<u8>, tag: &[u8; 4], data: &[u8]) {
    let hash = blake3::hash(data);
    buf.extend_from_slice(tag);
    buf.extend_from_slice(&(data.len() as u64).to_le_bytes());
    buf.extend_from_slice(data);
    buf.extend_from_slice(hash.as_bytes());
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
