//! Golden-image regression tests for `Operation` implementations.
//!
//! Each case applies one op (with fixed params) to a deterministic synthetic
//! image and asserts that the blake3 hash of the resulting RGBA8 buffer matches
//! a stored value.  This catches accidental pixel-level regressions in ops
//! without depending on external test images.
//!
//! ## Updating hashes
//!
//! When you intentionally change op output, run:
//!
//! ```text
//! RASTERLAB_GOLDEN_UPDATE=1 cargo test --package rasterlab-core --test golden -- --nocapture
//! ```
//!
//! That prints a copy-pasteable `EXPECTED` table.  Replace the table below
//! with the new hashes and verify the diff matches the change you intended.

use std::sync::OnceLock;

use rasterlab_core::{
    image::Image,
    ops::{
        BlackAndWhiteOp, BlurOp, BrightnessContrastOp, ClarityTextureOp, ColorBalanceOp, CropOp,
        CurvesOp, FauxHdrOp, FlipOp, GrainOp, HighlightsShadowsOp, HueShiftOp, LevelsOp,
        NoiseReductionOp, NrMethod, ResampleMode, ResizeOp, RotateOp, SaturationOp, SepiaOp,
        ShadowExposureOp, SharpenOp, SplitToneOp, VibranceOp, VignetteOp, WhiteBalanceOp,
    },
    traits::operation::Operation,
};

const W: u32 = 256;
const H: u32 = 192;

/// Initialise rayon's global pool with a 16 MiB stack so fold accumulators
/// (e.g. histogram) don't overflow macOS's 512 KiB secondary-thread default.
fn init_rayon() {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        let _ = rayon::ThreadPoolBuilder::new()
            .stack_size(16 * 1024 * 1024)
            .build_global();
    });
}

/// Deterministic test image: per-pixel gradient with channel-specific
/// frequencies so ops that touch hue/sat/luma all see structure.
fn make_image() -> Image {
    let mut data = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            let r = ((x.wrapping_mul(3) ^ y.wrapping_mul(5)) & 0xff) as u8;
            let g = ((x ^ y.wrapping_mul(2)) & 0xff) as u8;
            let b = ((x.wrapping_add(y).wrapping_mul(7)) & 0xff) as u8;
            data.extend_from_slice(&[r, g, b, 0xff]);
        }
    }
    Image::from_rgba8(W, H, data).unwrap()
}

fn hash_image(img: &Image) -> String {
    let mut h = blake3::Hasher::new();
    h.update(&img.width.to_le_bytes());
    h.update(&img.height.to_le_bytes());
    h.update(&img.data);
    h.finalize().to_hex().to_string()
}

/// Stable blake3 hashes of `apply(make_image())` for each op listed below.
///
/// Sorted by name so diffs are minimal when a single op changes.
const EXPECTED: &[(&str, &str)] = &[
    (
        "blur_r2",
        "c5d6d036d9bbe52cd04d36cbfbd35642b36e328ea07841b4e9c26eb059b5ad97",
    ),
    (
        "brightness_contrast",
        "2905af82f51c6e22852739a7da435d7a3a794d91c227759d24cb8a5da9b0a92a",
    ),
    (
        "bw_luminance",
        "dacbc3864eca119dcfbdecdd271ae8cc4ac7af63152ea1c502c15f1be768a373",
    ),
    (
        "clarity_texture",
        "e0b3d02de25e9672679e1e50ae0b1785e304f36666af99cc4a0e5d0ad2e694ed",
    ),
    (
        "color_balance_default",
        "28296e6af73c1d54c71e9ba6b3e6b06fc5ee2bb40dd6ef89291cda78707809f4",
    ),
    (
        "crop",
        "f37b6a30405d7953b7484b498bb5029d762f37f1fb45492860e77ddd5dcefe6b",
    ),
    (
        "curves_identity",
        "28296e6af73c1d54c71e9ba6b3e6b06fc5ee2bb40dd6ef89291cda78707809f4",
    ),
    (
        "faux_hdr",
        "db4f348813cdfacbdeb5fe71b0b4ffe8f812fdb0b3c032817fb042c487cd4037",
    ),
    (
        "flip_h",
        "3247bcd852de24b75caa2d624bed62fee163b6a99cfb0aea368165e75b4ed49a",
    ),
    (
        "flip_v",
        "61179c3ac539733e75d967f21885c3a5ec2a1affe795ec85740381e0a93f097b",
    ),
    (
        "grain",
        "8446080c4665f363c39959940f7ed6b6ce59cc5b55cd0b21e5d6e5350a5d7032",
    ),
    (
        "highlights_shadows",
        "6ffba8f20ba1110bda2c7b7cbef5d4c6c328d190f69d299f29ee22c34d6c4d00",
    ),
    (
        "hue_shift_30",
        "945e940bcd51243b465faa8cf37ba6960c73687920c24f00f79136b61befc81d",
    ),
    (
        "levels",
        "6f30c43113cdab2a514caa8c54e705daf1802a14f71947b80715556cf7c73aab",
    ),
    (
        "nr_wavelet",
        "622f6e0c7ac24b5036d6495b1e38748bffd4d20a2b5cc6f208c32db0ad10d3d6",
    ),
    (
        "resize_half_bilinear",
        "a9ae08ca1fcae5de94255927c980a507da07954fa3ed311d7dded332908be355",
    ),
    (
        "rotate_arbitrary_15",
        "7d76f896ee1f0de8a8cc8e8ba9457597953a519727509386e9230a87f2819ee8",
    ),
    (
        "rotate_cw90",
        "ca6945f1f4b1fdb40314ab318a9c495382dbaa2b6ff22fa26a8ba3cf68783b5a",
    ),
    (
        "saturation_up",
        "60c68e4315829c46db030cce704a01ea614779c59a5e21bffd64716a8d07280b",
    ),
    (
        "sepia_full",
        "12cc969bc61ad6f599b3807ddcc1c57ecde282157331cc1ca012ec20ba181fed",
    ),
    (
        "shadow_exposure",
        "135d6bbfb882de767b1826f121b2a26364e34c2c3377f989bb2dee8034355db0",
    ),
    (
        "sharpen",
        "cbedeb73dc89cf5c10b0c813bc4eb5a5e3e064c33db384cb42b7fba252fbc38d",
    ),
    (
        "split_tone_default",
        "e12f189cd74f3cda84ea95e5930ebedb9f73981925a11ecd4a03dda99d85efff",
    ),
    (
        "vibrance",
        "5ec560051586beb39007c1a1012a9fdea07e670b80408343729eac1c5edba936",
    ),
    (
        "vignette",
        "ca9376eac4ffc78e97b8d35a885beb2cbab5bbfb9ea554b06b1e938a2c26c67f",
    ),
    (
        "white_balance_warm",
        "bf3e7253ca015a67f1eae68841bdc1b868aa1c94ee8c74438e56717f7d1e9e58",
    ),
];

/// Build the op set in the same order as `EXPECTED`.  Kept as a function (not a
/// const) because `Box<dyn Operation>` isn't const-constructible.
fn cases() -> Vec<(&'static str, Box<dyn Operation>)> {
    vec![
        ("blur_r2", Box::new(BlurOp::new(2.0))),
        (
            "brightness_contrast",
            Box::new(BrightnessContrastOp::new(0.15, 0.20)),
        ),
        ("bw_luminance", Box::new(BlackAndWhiteOp::luminance())),
        ("clarity_texture", Box::new(ClarityTextureOp::new(0.5, 0.3))),
        ("color_balance_default", Box::new(ColorBalanceOp::default())),
        ("crop", Box::new(CropOp::new(16, 12, 200, 150))),
        ("curves_identity", Box::new(CurvesOp::identity())),
        ("faux_hdr", Box::new(FauxHdrOp::new(0.6))),
        ("flip_h", Box::new(FlipOp::horizontal())),
        ("flip_v", Box::new(FlipOp::vertical())),
        ("grain", Box::new(GrainOp::new(0.3, 1.0, 42))),
        (
            "highlights_shadows",
            Box::new(HighlightsShadowsOp::new(-0.4, 0.4)),
        ),
        ("hue_shift_30", Box::new(HueShiftOp::new(30.0))),
        ("levels", Box::new(LevelsOp::new(0.05, 0.95, 1.1))),
        (
            "nr_wavelet",
            Box::new(NoiseReductionOp {
                method: NrMethod::Wavelet,
                luma_strength: 0.4,
                color_strength: 0.6,
                detail_preservation: 0.5,
            }),
        ),
        (
            "resize_half_bilinear",
            Box::new(ResizeOp::new(W / 2, H / 2, ResampleMode::Bilinear)),
        ),
        ("rotate_arbitrary_15", Box::new(RotateOp::arbitrary(15.0))),
        ("rotate_cw90", Box::new(RotateOp::cw90())),
        ("saturation_up", Box::new(SaturationOp::new(0.5))),
        ("sepia_full", Box::new(SepiaOp::new(1.0))),
        ("shadow_exposure", Box::new(ShadowExposureOp::new(0.6, 0.5))),
        ("sharpen", Box::new(SharpenOp::new(0.7))),
        ("split_tone_default", Box::new(SplitToneOp::default())),
        ("vibrance", Box::new(VibranceOp::new(0.4))),
        ("vignette", Box::new(VignetteOp::new(0.6, 0.7, 0.3))),
        (
            "white_balance_warm",
            Box::new(WhiteBalanceOp::new(0.3, -0.1)),
        ),
    ]
}

#[test]
fn golden_hashes_match() {
    init_rayon();

    // Sanity: case list and expected table must agree on shape & order.
    let case_list = cases();
    assert_eq!(
        case_list.len(),
        EXPECTED.len(),
        "cases() and EXPECTED have different lengths — keep them in sync",
    );
    for ((name, _), (ename, _)) in case_list.iter().zip(EXPECTED.iter()) {
        assert_eq!(
            name, ename,
            "case order differs from EXPECTED — both must be sorted by name",
        );
    }

    let img = make_image();
    let mut actual: Vec<(&str, String)> = Vec::with_capacity(case_list.len());
    for (name, op) in case_list {
        let out = op
            .apply(img.deep_clone())
            .unwrap_or_else(|e| panic!("op {name} failed: {e:?}"));
        actual.push((name, hash_image(&out)));
    }

    let update = std::env::var("RASTERLAB_GOLDEN_UPDATE").is_ok();
    if update {
        println!("\n// Paste this into golden.rs (replacing EXPECTED):");
        println!("const EXPECTED: &[(&str, &str)] = &[");
        for (name, hash) in &actual {
            println!("    (\"{name}\", \"{hash}\"),");
        }
        println!("];\n");
        return;
    }

    let mut mismatches: Vec<String> = Vec::new();
    for ((name, got), (_, want)) in actual.iter().zip(EXPECTED.iter()) {
        if want.is_empty() {
            mismatches.push(format!("{name}: missing baseline (got {got})"));
        } else if got != want {
            mismatches.push(format!("{name}: expected {want}, got {got}"));
        }
    }
    if !mismatches.is_empty() {
        panic!(
            "{} golden hash mismatches:\n  {}\n\n\
             To accept and update, re-run with RASTERLAB_GOLDEN_UPDATE=1.",
            mismatches.len(),
            mismatches.join("\n  "),
        );
    }
}
