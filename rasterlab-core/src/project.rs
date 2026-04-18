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
//! `RECC` is reserved for a future v4 format; v3 readers skip it safely.
//! Unknown chunks are skipped on read, enabling forward compatibility.

use std::{
    io::Read,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::{
    error::{RasterError, RasterResult},
    library_meta::LibraryMeta,
    pipeline::PipelineState,
};

// ── Constants ────────────────────────────────────────────────────────────────

/// Magic bytes that identify every `.rlab` file.
const MAGIC: &[u8; 8] = b"RLAB\x00\x01\r\n";

/// Current file format version.  Bump when the layout changes incompatibly.
pub const FORMAT_VERSION: u16 = 3;

const TAG_META: &[u8; 4] = b"META";
const TAG_ORIG: &[u8; 4] = b"ORIG";
#[allow(dead_code)] // v1 only — used as a literal in the read match arm
const TAG_EDIT: &[u8; 4] = b"EDIT";
const TAG_VCPS: &[u8; 4] = b"VCPS"; // v2+ — replaces EDIT
const TAG_PREV: &[u8; 4] = b"PREV";
const TAG_LMTA: &[u8; 4] = b"LMTA"; // v3+ — library metadata (optional)
                                     // TAG_RECC b"RECC" reserved for v4 Reed-Solomon ECC

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

    /// Serialise and write the project to `path`, computing all hashes.
    pub fn write(&self, path: &Path) -> RasterResult<()> {
        let mut buf: Vec<u8> = Vec::new();

        // Header
        buf.extend_from_slice(MAGIC);
        buf.extend_from_slice(&FORMAT_VERSION.to_le_bytes());

        // META
        let meta_json = serde_json::to_vec(&self.meta)
            .map_err(|e| RasterError::Serialization(e.to_string()))?;
        write_chunk(&mut buf, TAG_META, &meta_json);

        // ORIG
        write_chunk(&mut buf, TAG_ORIG, &self.original_bytes);

        // VCPS — all virtual copies + active index
        let vcps = VcpsChunk {
            active: self.active_copy_index,
            copies: self.copies.clone(),
        };
        let vcps_json =
            serde_json::to_vec(&vcps).map_err(|e| RasterError::Serialization(e.to_string()))?;
        write_chunk(&mut buf, TAG_VCPS, &vcps_json);

        // PREV (optional)
        if let Some(thumb) = &self.thumbnail {
            write_chunk(&mut buf, TAG_PREV, thumb);
        }

        // LMTA (optional) — library metadata
        if let Some(lmta) = &self.lmta {
            let lmta_json = serde_json::to_vec(lmta)
                .map_err(|e| RasterError::Serialization(e.to_string()))?;
            write_chunk(&mut buf, TAG_LMTA, &lmta_json);
        }

        // File-level hash covers everything written so far
        let file_hash = blake3::hash(&buf);
        buf.extend_from_slice(file_hash.as_bytes());

        std::fs::write(path, &buf)?;
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
    /// - The format version is newer than [`FORMAT_VERSION`].
    pub fn read(path: &Path) -> RasterResult<Self> {
        let data = std::fs::read(path)?;

        // ── File-level hash ───────────────────────────────────────────────
        if data.len() < MAGIC.len() + 2 + 32 {
            return Err(RasterError::decode("rlab", "file too short"));
        }
        let (payload, file_hash_stored) = data.split_at(data.len() - 32);
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
        if format_version > FORMAT_VERSION {
            return Err(RasterError::decode(
                "rlab",
                format!(
                    "unsupported format version {format_version} \
                     (this build supports up to {FORMAT_VERSION})"
                ),
            ));
        }

        // ── Chunks ────────────────────────────────────────────────────────
        let mut meta: Option<RlabMeta> = None;
        let mut original_bytes: Option<Vec<u8>> = None;
        let mut original_hash: Option<[u8; 32]> = None;
        // v1 fallback: a single PipelineState from the EDIT chunk
        let mut edit_v1: Option<PipelineState> = None;
        // v2+: all copies from the VCPS chunk
        let mut vcps: Option<VcpsChunk> = None;
        let mut thumbnail: Option<Vec<u8>> = None;
        // v3+: library metadata
        let mut lmta: Option<LibraryMeta> = None;

        loop {
            // Peek: stop when we've consumed all payload bytes
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

            // Verify per-chunk hash
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
                    // Unknown chunk (including reserved RECC) — skip for forward compatibility
                }
            }
        }

        // ── Require mandatory chunks ───────────────────────────────────────
        let meta = meta.ok_or_else(|| RasterError::decode("rlab", "missing META chunk"))?;
        let original_bytes =
            original_bytes.ok_or_else(|| RasterError::decode("rlab", "missing ORIG chunk"))?;
        let original_hash =
            original_hash.ok_or_else(|| RasterError::decode("rlab", "missing ORIG chunk"))?;

        // Version 1: synthesise a single copy from the EDIT chunk.
        // Version 2+: use the VCPS chunk directly.
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

// ── Private helpers ───────────────────────────────────────────────────────────

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
