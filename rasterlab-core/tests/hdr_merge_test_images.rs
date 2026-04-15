//! End-to-end HDR merge test driven by real on-disk bracket images.
//!
//! Loads the three bracketed PNGs produced by the `gen_test_images`
//! example (checked in under `test_images/`) and verifies the full
//! decode → merge → tone-map pipeline.  Complements the inline unit
//! tests in `ops/hdr_merge.rs`, which exercise the algorithm with
//! synthesised pixel buffers only — these tests additionally cover the
//! PNG decode path and guard against regressions in the checked-in
//! reference images themselves.

use std::path::PathBuf;

use rasterlab_core::formats::FormatRegistry;
use rasterlab_core::ops::hdr_merge::{merge_images, merge_linear};

fn test_images_dir() -> PathBuf {
    // CARGO_MANIFEST_DIR = rasterlab-core/, so go up one to workspace root.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("test_images")
}

fn load(name: &str) -> rasterlab_core::image::Image {
    let reg = FormatRegistry::with_builtins();
    let path = test_images_dir().join(name);
    reg.decode_file(&path)
        .unwrap_or_else(|e| panic!("decode {}: {e}", path.display()))
}

#[test]
fn hdr_brackets_merge_preserves_dimensions_and_alpha() {
    let under = load("hdr_bracket_under.png");
    let mid = load("hdr_bracket_mid.png");
    let over = load("hdr_bracket_over.png");

    assert_eq!((under.width, under.height), (1024, 512));
    assert_eq!((mid.width, mid.height), (1024, 512));
    assert_eq!((over.width, over.height), (1024, 512));

    let out = merge_images(&[&under, &mid, &over]).unwrap();
    assert_eq!((out.width, out.height), (1024, 512));
    assert!(out.data.chunks_exact(4).all(|p| p[3] == 255));
}

#[test]
fn hdr_brackets_radiance_monotone_left_to_right() {
    // The scene is a 10-stop horizontal brightness ramp.  After merging,
    // the recovered linear radiance must be monotone-increasing across
    // the width, including through regions where each individual frame
    // clips.  This is the load-bearing correctness check.
    let under = load("hdr_bracket_under.png");
    let mid = load("hdr_bracket_mid.png");
    let over = load("hdr_bracket_over.png");

    let w = under.width as usize;
    let h = under.height as usize;
    let rad = merge_linear(&[&under, &mid, &over]).unwrap();

    // Sample the middle row's green channel (no chroma tilt factor).
    let row = h / 2;
    let sample: Vec<f32> = (0..w)
        .step_by(8)
        .map(|x| rad[(row * w + x) * 3 + 1])
        .collect();
    for pair in sample.windows(2) {
        assert!(
            pair[1] >= pair[0] * 0.98,
            "radiance regressed along gradient: {} → {}",
            pair[0],
            pair[1],
        );
    }

    // We synthesised 10 stops of scene-referred dynamic range.  The
    // recovered radiance must span at least 100× (~6.6 stops) — well
    // beyond what any single LDR frame could carry.
    let min = sample.first().copied().unwrap().max(1e-6);
    let max = sample.last().copied().unwrap();
    assert!(
        max / min > 100.0,
        "merged radiance span {:.1}× too narrow for 10-stop input",
        max / min,
    );
}

#[test]
fn hdr_brackets_merge_preserves_vertical_chroma_tilt() {
    // The scene's top half leans red, bottom half leans blue.  After
    // tone-mapping, sampling a mid-brightness column should still show
    // R > B at the top and B > R at the bottom.
    let under = load("hdr_bracket_under.png");
    let mid = load("hdr_bracket_mid.png");
    let over = load("hdr_bracket_over.png");

    let out = merge_images(&[&under, &mid, &over]).unwrap();
    let w = out.width as usize;
    let h = out.height as usize;

    // Sample a middle-brightness column (well away from clipped ends).
    let col = w / 2;
    let top_px = &out.data[((h / 8) * w + col) * 4..][..4];
    let bot_px = &out.data[(((7 * h) / 8) * w + col) * 4..][..4];

    assert!(
        top_px[0] as i32 > top_px[2] as i32 + 5,
        "top row should lean red, got {:?}",
        top_px,
    );
    assert!(
        bot_px[2] as i32 > bot_px[0] as i32 + 5,
        "bottom row should lean blue, got {:?}",
        bot_px,
    );
}

#[test]
fn hdr_brackets_recover_shadow_and_highlight_detail() {
    // Single-frame sanity: the mid frame crushes deep shadows and blows
    // highlights.  The merged result must carry detail in BOTH regions,
    // i.e. neither edge is pinned to 0 or 255.
    let under = load("hdr_bracket_under.png");
    let mid = load("hdr_bracket_mid.png");
    let over = load("hdr_bracket_over.png");

    let out = merge_images(&[&under, &mid, &over]).unwrap();
    let w = out.width as usize;
    let h = out.height as usize;
    let row = h / 2;

    // Green channel at the darkest column and brightest column.
    let dark_g = out.data[(row * w + 2) * 4 + 1];
    let bright_g = out.data[(row * w + (w - 3)) * 4 + 1];

    assert!(dark_g > 0, "shadow detail crushed to 0");
    assert!(bright_g < 255, "highlight detail clipped to 255");
    assert!(
        bright_g as i32 - dark_g as i32 > 100,
        "merged dynamic range too compressed: dark={dark_g} bright={bright_g}",
    );
}
