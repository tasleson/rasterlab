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
//! `RECC` (v4) holds RS parity over all preceding bytes; ~10% overhead; up to 5%
//! corruption in any position can be recovered via [`verify_and_repair`].
//! Unknown chunks are skipped on read, enabling forward compatibility.

use std::{
    io::Read,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use reed_solomon_erasure::galois_8::ReedSolomon;
use serde::{Deserialize, Serialize};

use crate::{
    error::{RasterError, RasterResult},
    library_meta::LibraryMeta,
    pipeline::PipelineState,
};

// ── Constants ────────────────────────────────────────────────────────────────

/// Magic bytes that identify every `.rlab` file.
const MAGIC: &[u8; 8] = b"RLAB\x00\x01\r\n";

/// Format version written by [`RlabFile::write`] (v3, no ECC).
pub const FORMAT_VERSION: u16 = 3;

/// Format version written by [`RlabFile::write_v4`] (v4, with RECC ECC chunk).
pub const FORMAT_VERSION_V4: u16 = 4;

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

    /// Serialise and write the project to `path` as format v4 with a `RECC`
    /// Reed-Solomon parity chunk (~10% size overhead).
    ///
    /// The resulting file can be verified and repaired with [`verify_and_repair`].
    pub fn write_v4(&self, path: &Path) -> RasterResult<()> {
        // Build the "protected" region: header + all content chunks.
        let mut protected: Vec<u8> = Vec::new();
        protected.extend_from_slice(MAGIC);
        protected.extend_from_slice(&FORMAT_VERSION_V4.to_le_bytes());
        self.write_content_chunks(&mut protected)?;

        // Compute and append the RECC chunk.
        let recc_payload = build_recc_payload(&protected)?;
        let mut buf = protected;
        write_chunk(&mut buf, TAG_RECC, &recc_payload);

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
    /// - The format version is newer than [`FORMAT_VERSION_V4`].
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
        if format_version > FORMAT_VERSION_V4 {
            return Err(RasterError::decode(
                "rlab",
                format!(
                    "unsupported format version {format_version} \
                     (this build supports up to {FORMAT_VERSION_V4})"
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
/// detected and a valid `RECC` chunk is present.  Repair succeeds as long as
/// the number of damaged shards does not exceed the parity shard count (~10%
/// of the file).
///
/// If the file is clean, no output file is written even when `repair_to` is
/// `Some`.
pub fn verify_and_repair(path: &Path, repair_to: Option<&Path>) -> RasterResult<VerifyReport> {
    let data = std::fs::read(path)?;
    if data.len() < MAGIC.len() + 2 + 32 {
        return Err(RasterError::decode("rlab", "file too short"));
    }

    let (payload, file_hash_bytes) = data.split_at(data.len() - 32);
    let file_hash_ok = blake3::hash(payload).as_bytes() == file_hash_bytes;

    let scan = scan_chunks(payload)?;

    let damaged_chunks: Vec<String> = scan
        .chunks
        .iter()
        .filter(|c| !c.hash_ok)
        .map(|c| String::from_utf8_lossy(&c.tag).into_owned())
        .collect();

    if file_hash_ok && damaged_chunks.is_empty() {
        return Ok(VerifyReport {
            file_hash_ok: true,
            damaged_chunks: vec![],
            recc_present: scan.recc_present,
            repaired: false,
        });
    }

    let repaired = if let (Some(repair_path), Some(recc_data)) = (repair_to, &scan.recc_data) {
        attempt_repair(
            payload,
            recc_data,
            &scan.chunks,
            scan.recc_start,
            repair_path,
        )?
    } else {
        false
    };

    Ok(VerifyReport {
        file_hash_ok,
        damaged_chunks,
        recc_present: scan.recc_present,
        repaired,
    })
}

// ── Internal scan helpers ─────────────────────────────────────────────────────

struct ChunkInfo {
    tag: [u8; 4],
    hash_ok: bool,
}

struct ScanResult {
    chunks: Vec<ChunkInfo>,
    recc_present: bool,
    /// RECC payload bytes, only populated when the RECC chunk hash is valid.
    recc_data: Option<Vec<u8>>,
    /// Byte offset of the RECC chunk tag within `payload`, or `payload.len()` if absent.
    recc_start: usize,
}

fn scan_chunks(payload: &[u8]) -> RasterResult<ScanResult> {
    let header_size = MAGIC.len() + 2;
    let mut pos = header_size;
    let mut chunks = Vec::new();
    let mut recc_present = false;
    let mut recc_data = None;
    let mut recc_start = payload.len();

    while pos + 4 + 8 <= payload.len() {
        let tag: [u8; 4] = payload[pos..pos + 4].try_into().unwrap();
        let len = u64::from_le_bytes(payload[pos + 4..pos + 12].try_into().unwrap()) as usize;
        let data_start = pos + 12;
        let data_end = data_start + len;
        let hash_end = data_end + 32;

        if hash_end > payload.len() {
            break;
        }

        let chunk_data = &payload[data_start..data_end];
        let stored_hash = &payload[data_end..hash_end];
        let hash_ok = blake3::hash(chunk_data).as_bytes() == stored_hash;

        if &tag == TAG_RECC {
            recc_present = true;
            recc_start = pos;
            if hash_ok {
                recc_data = Some(chunk_data.to_vec());
            }
        } else {
            chunks.push(ChunkInfo { tag, hash_ok });
        }

        pos = hash_end;
    }

    Ok(ScanResult {
        chunks,
        recc_present,
        recc_data,
        recc_start,
    })
}

// ── Repair ────────────────────────────────────────────────────────────────────

/// Attempt to reconstruct damaged chunks using RS parity from the RECC chunk.
///
/// Returns `true` and writes the repaired file if reconstruction succeeds.
fn attempt_repair(
    payload: &[u8],
    recc_payload: &[u8],
    _chunks: &[ChunkInfo],
    recc_start: usize,
    repair_to: &Path,
) -> RasterResult<bool> {
    if recc_payload.len() < 20 {
        return Ok(false);
    }

    let shard_size = u32::from_le_bytes(recc_payload[0..4].try_into().unwrap()) as usize;
    let data_shards = u32::from_le_bytes(recc_payload[4..8].try_into().unwrap()) as usize;
    let parity_shards = u32::from_le_bytes(recc_payload[8..12].try_into().unwrap()) as usize;
    let protected_len = u64::from_le_bytes(recc_payload[12..20].try_into().unwrap()) as usize;

    // RECC payload layout: 20-byte fixed header + data_shards*32 shard hashes + parity data.
    let hashes_size = data_shards * 32;
    let parity_size = parity_shards * shard_size;
    if shard_size == 0
        || data_shards == 0
        || parity_shards == 0
        || recc_payload.len() < 20 + hashes_size + parity_size
        || recc_start < protected_len
    {
        return Ok(false);
    }

    let shard_hashes = &recc_payload[20..20 + hashes_size];
    let parity_data = &recc_payload[20 + hashes_size..20 + hashes_size + parity_size];
    let protected = &payload[..recc_start];

    // Build shard vec: data shards (zero-padded at tail) + parity shards from RECC.
    let mut shards: Vec<Option<Vec<u8>>> = (0..data_shards)
        .map(|i| {
            let start = i * shard_size;
            let end = ((i + 1) * shard_size).min(protected.len());
            let mut s = vec![0u8; shard_size];
            if start < protected.len() {
                s[..end - start].copy_from_slice(&protected[start..end]);
            }
            Some(s)
        })
        .collect();

    for i in 0..parity_shards {
        let start = i * shard_size;
        shards.push(Some(parity_data[start..start + shard_size].to_vec()));
    }

    // Mark erased: data shards whose per-shard Blake3 doesn't match the stored hash.
    // This precisely identifies which 4 KiB blocks are corrupted, not just which chunk.
    for i in 0..data_shards {
        let stored = &shard_hashes[i * 32..(i + 1) * 32];
        let computed = blake3::hash(shards[i].as_ref().unwrap());
        if computed.as_bytes() != stored {
            shards[i] = None;
        }
    }

    let erasures: usize = shards
        .iter()
        .take(data_shards + parity_shards)
        .filter(|s| s.is_none())
        .count();
    if erasures > parity_shards {
        return Ok(false);
    }

    let rs = ReedSolomon::new(data_shards, parity_shards)
        .map_err(|e| RasterError::decode("recc", e.to_string()))?;

    rs.reconstruct_data(&mut shards)
        .map_err(|e| RasterError::decode("recc", format!("reconstruction failed: {e}")))?;

    // Reassemble the protected region from reconstructed data shards.
    let mut reconstructed = Vec::with_capacity(protected_len);
    for s in shards.iter().take(data_shards) {
        reconstructed.extend_from_slice(s.as_ref().unwrap());
    }
    reconstructed.truncate(protected_len);

    // Append the original RECC chunk (it had a valid hash; reuse it).
    let recc_data_len =
        u64::from_le_bytes(payload[recc_start + 4..recc_start + 12].try_into().unwrap()) as usize;
    let recc_chunk_end = recc_start + 4 + 8 + recc_data_len + 32;
    if recc_chunk_end > payload.len() {
        return Ok(false);
    }
    reconstructed.extend_from_slice(&payload[recc_start..recc_chunk_end]);

    // New file-level hash over the repaired content.
    let new_hash = blake3::hash(&reconstructed);
    reconstructed.extend_from_slice(new_hash.as_bytes());

    std::fs::write(repair_to, &reconstructed)?;
    Ok(true)
}

// ── RECC encoding helpers ─────────────────────────────────────────────────────

/// Build the binary payload stored inside the `RECC` chunk.
///
/// Layout:
/// ```text
/// [shard_size u32 LE][data_shards u32 LE][parity_shards u32 LE][protected_len u64 LE]
/// [data_shards × 32 bytes]  — per-data-shard Blake3 hashes (used to pinpoint erasures)
/// [parity_shards × shard_size bytes]  — RS parity shards
/// ```
fn build_recc_payload(protected: &[u8]) -> RasterResult<Vec<u8>> {
    let shard_size = compute_shard_size(protected.len());
    let padded_len = round_up(protected.len(), shard_size);
    let data_shards = padded_len / shard_size;
    let parity_shards = (data_shards / 10).max(1);

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

/// Choose a shard size that keeps `data_shards` within [`RECC_MAX_DATA_SHARDS`].
fn compute_shard_size(data_len: usize) -> usize {
    let min_size = data_len.div_ceil(RECC_MAX_DATA_SHARDS);
    let size = min_size.max(RECC_MIN_SHARD_SIZE);
    round_up(size, RECC_MIN_SHARD_SIZE)
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
