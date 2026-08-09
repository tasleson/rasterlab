//! `.rlab` parsing against malformed and hostile files.
//!
//! The whole-file Blake3 stops accidental corruption, but it is trivially
//! recomputed, so it is no barrier to a file built on purpose.  Every tag and
//! length the chunk parser reads after that digest is therefore attacker-chosen,
//! and none of it may reach an allocation before it has been checked.  Each
//! fixture below is a file that parses far enough to matter and must come back
//! as an error, not a panic or an abort.

use rasterlab_core::{
    pipeline::PipelineState,
    project::{RlabFile, RlabMeta, SavedCopy, read_original_hash},
};
use tempfile::NamedTempFile;

/// Duplicated from `project.rs`, which keeps it private — these tests build
/// file images by hand precisely so they do not go through the writer.
const MAGIC: &[u8; 8] = b"RLAB\x00\x01\r\n";
const HASH_LEN: usize = 32;

/// Wrap chunk bytes in a header and a correct whole-file digest, so the fixture
/// reaches the chunk parser instead of being turned away by the file hash.
fn seal(version: u16, chunks: &[u8]) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(MAGIC);
    buf.extend_from_slice(&version.to_le_bytes());
    buf.extend_from_slice(chunks);
    buf.extend_from_slice(blake3::hash(&buf).as_bytes());
    buf
}

/// A chunk whose length field says `declared` while it actually carries `data`.
fn chunk(tag: &[u8; 4], declared: u64, data: &[u8]) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(tag);
    buf.extend_from_slice(&declared.to_le_bytes());
    buf.extend_from_slice(data);
    buf.extend_from_slice(blake3::hash(data).as_bytes());
    buf
}

/// A well-formed chunk.
fn honest_chunk(tag: &[u8; 4], data: &[u8]) -> Vec<u8> {
    chunk(tag, data.len() as u64, data)
}

fn err_of(data: &[u8]) -> String {
    RlabFile::read_bytes(data)
        .err()
        .map(|e| e.to_string())
        .unwrap_or_else(|| panic!("expected a parse error, got a file"))
}

// ── Chunk lengths ─────────────────────────────────────────────────────────────

/// A length field is a `u64` straight off disk.  `vec![0u8; len]` for a length
/// of this size does not fail — it aborts the process in the allocator.
#[test]
fn absurd_chunk_length_does_not_allocate() {
    for declared in [u64::MAX, u64::MAX / 2, 1 << 48, 1 << 40] {
        let data = seal(3, &chunk(b"ORIG", declared, b"tiny"));
        let msg = err_of(&data);
        assert!(msg.contains("ORIG"), "unhelpful message: {msg}");
    }
}

/// A length that runs a single byte past the chunk chain is still a length that
/// cannot be honoured.
#[test]
fn chunk_length_past_end_is_rejected() {
    let body = b"payload";
    let data = seal(3, &chunk(b"ORIG", body.len() as u64 + 1, body));
    assert!(err_of(&data).contains("remain"));
}

/// The digest that follows a chunk is part of the space its length competes
/// for: a chunk claiming every remaining byte leaves nowhere for its own hash.
#[test]
fn chunk_length_leaving_no_room_for_hash_is_rejected() {
    let body = b"payload";
    let declared = (body.len() + HASH_LEN) as u64;
    let data = seal(3, &chunk(b"ORIG", declared, body));
    assert!(err_of(&data).contains("remain"));
}

/// Bytes at the tail too few to form a chunk header are damage, not padding.
#[test]
fn truncated_chunk_header_is_rejected() {
    let mut chunks = honest_chunk(b"JUNK", b"skipped");
    chunks.extend_from_slice(b"ORI");
    assert!(err_of(&seal(3, &chunks)).contains("truncated chunk header"));
}

/// A chunk header with no room for a payload or digest behind it.
#[test]
fn chunk_header_without_body_is_rejected() {
    let chunks = [b"ORIG".as_slice(), &0u64.to_le_bytes()].concat();
    assert!(err_of(&seal(3, &chunks)).contains("truncated chunk header"));
}

// ── Per-chunk limits ──────────────────────────────────────────────────────────

/// `META`, `VCPS`, `PREV` and `LMTA` hold documents whose size the writer
/// controls.  A length orders of magnitude past that is rejected on sight,
/// without first materialising a file big enough to hold it.
#[test]
fn oversized_typed_chunks_are_rejected() {
    for (tag, over_limit) in [
        (b"META", 17 * 1024 * 1024),
        (b"LMTA", 17 * 1024 * 1024),
        (b"VCPS", 257 * 1024 * 1024),
        (b"EDIT", 257 * 1024 * 1024),
        (b"PREV", 65 * 1024 * 1024),
    ] {
        let data = seal(3, &chunk(tag, over_limit, b"x"));
        let msg = err_of(&data);
        let name = std::str::from_utf8(tag).unwrap();
        assert!(msg.contains("limit for its kind"), "{name}: {msg}");
    }
}

/// `ORIG` is the photograph itself, so it carries no per-kind ceiling — only
/// the file it lives in bounds it.
#[test]
fn orig_is_bounded_only_by_the_file() {
    let data = seal(3, &chunk(b"ORIG", 17 * 1024 * 1024, b"x"));
    let msg = err_of(&data);
    assert!(msg.contains("remain"), "{msg}");
    assert!(!msg.contains("limit for its kind"), "{msg}");
}

// ── Seeking readers ───────────────────────────────────────────────────────────

/// `read_original_hash` skips each chunk by seeking past its payload *and* its
/// digest.  Adding the two overflows `i64` for a length near its maximum.
#[test]
fn read_original_hash_survives_extreme_lengths() {
    for declared in [u64::MAX, i64::MAX as u64] {
        let tmp = NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), seal(3, &chunk(b"JUNK", declared, b""))).unwrap();
        assert!(read_original_hash(tmp.path()).is_err());
    }
}

// ── Well-formed files still load ──────────────────────────────────────────────

#[test]
fn round_trip_still_works() {
    let original = vec![0xABu8; 4096];
    let file = RlabFile::new(
        RlabMeta::new("0.3.0", Some("photo.jpg"), 64, 48),
        original.clone(),
        vec![SavedCopy {
            name: "Copy 1".into(),
            pipeline_state: PipelineState {
                entries: vec![],
                cursor: 0,
            },
        }],
        0,
        Some(vec![0xCD; 128]),
    );

    let tmp = NamedTempFile::new().unwrap();
    file.write_v5(tmp.path()).unwrap();

    let read = RlabFile::read(tmp.path()).unwrap();
    assert_eq!(read.original_bytes, original);
    assert_eq!(read.thumbnail.as_deref(), Some(&[0xCDu8; 128][..]));
    assert_eq!(read.meta.width, 64);
    assert_eq!(read.copies.len(), 1);
}
