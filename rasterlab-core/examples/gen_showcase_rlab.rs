//! Builds a colourful synthetic source image and a `.rlab` project file whose
//! virtual copies cover **every** built-in operation, so a release candidate
//! can be browsed end-to-end in the GUI.
//!
//! Run with:
//!
//! ```bash
//! cargo run --release -p rasterlab-core --example gen_showcase_rlab
//! ```
//!
//! Output (under `test_images/`):
//!
//! * `showcase_source.png` — 1024×768 RGB PNG; gradient sky, primary-coloured
//!   bands, a checker patch (high-frequency content for sharpen/blur/grain),
//!   and a noise patch (target for noise reduction). Used as the `ORIG` chunk
//!   of the `.rlab` so the project survives even if the PNG is deleted.
//! * `showcase.rlab` — five virtual copies:
//!     1. `All Adjustments` — every single-image op stacked in order, with
//!        parameters chosen so the image is still recognisable at the end.
//!        Some destructive ops (Black & White, Sepia, Color Space) are pushed
//!        but disabled so the user can toggle them on individually.
//!     2. `HDR Merge` — `HdrMergeOp` over the `hdr_bracket_*.png` test images.
//!     3. `Panorama` — `PanoramaOp` over `pano_left.png` and `pano_right.png`.
//!     4. `Focus Stack` — `FocusStackOp` over `focus_top/mid/bot.png`.
//!     5. `Masked Edits` — Linear- and Radial-masked variants of a few colour
//!        ops, to exercise the `MaskedOp` wrapper / `MaskShape` path.
//!
//! The file paths embedded in the multi-image ops are **absolute**, resolved
//! against `CARGO_MANIFEST_DIR/../test_images` at generation time. Move the
//! `.rlab` to another machine and those ops will fail to apply (the rest of
//! the stack still works).

use std::path::Path;

use image::{ImageBuffer, Rgb, RgbImage};
use rasterlab_core::{
    image::Image,
    ops::{
        BlackAndWhiteOp, BlurOp, BrightnessContrastOp, ClarityTextureOp, ColorBalanceOp,
        ColorSpaceConversion, ColorSpaceOp, CropOp, CurvesOp, DenoiseOp, FauxHdrOp, FlipOp,
        FocusStackOp, GrainOp, HdrMergeOp, HealOp, HealSpot, HighlightsShadowsOp, HslPanelOp,
        HueShiftOp, LevelsOp, LinearMask, LutOp, MaskShape, MaskedOp, NoiseReductionOp, NrMethod,
        PanoramaOp, PerspectiveOp, RadialMask, ResampleMode, ResizeOp, RotateOp, SaturationOp,
        SepiaOp, ShadowExposureOp, SharpenOp, SplitToneOp, VibranceOp, VignetteOp, WhiteBalanceOp,
    },
    pipeline::{EditPipeline, PipelineState},
    project::{RlabFile, RlabMeta, SavedCopy},
    traits::operation::Operation,
};

const SRC_W: u32 = 1024;
const SRC_H: u32 = 768;

fn main() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let test_dir = manifest
        .parent()
        .expect("workspace root")
        .join("test_images");

    std::fs::create_dir_all(&test_dir).expect("create test_images dir");

    // ── 1. Synthesise the showcase source image. ─────────────────────────
    let source = build_showcase_image();
    let source_path = test_dir.join("showcase_source.png");
    source.save(&source_path).expect("save showcase_source.png");
    println!("wrote {}", source_path.display());

    // Read the encoded PNG bytes; the .rlab embeds them verbatim in ORIG.
    let png_bytes = std::fs::read(&source_path).expect("read showcase_source.png");

    // Also decode the PNG through the registry so the pipeline source matches
    // exactly what the GUI will see when opening the project.
    let registry = rasterlab_core::formats::FormatRegistry::with_builtins();
    let decoded = registry
        .decode_bytes(&png_bytes, Some(&source_path))
        .expect("decode showcase_source.png");

    // ── 2. Build each virtual copy. ──────────────────────────────────────
    let copies = vec![
        SavedCopy {
            name: "All Adjustments".into(),
            pipeline_state: build_all_adjustments(&decoded),
        },
        SavedCopy {
            name: "HDR Merge".into(),
            pipeline_state: build_hdr_merge(&test_dir, &decoded),
        },
        SavedCopy {
            name: "Panorama".into(),
            pipeline_state: build_panorama(&test_dir, &decoded),
        },
        SavedCopy {
            name: "Focus Stack".into(),
            pipeline_state: build_focus_stack(&test_dir, &decoded),
        },
        SavedCopy {
            name: "Masked Edits".into(),
            pipeline_state: build_masked_edits(&decoded),
        },
    ];

    // ── 3. Serialise the .rlab. ──────────────────────────────────────────
    let app_version = env!("CARGO_PKG_VERSION").to_string();
    let meta = RlabMeta::new(
        app_version,
        Some(source_path.to_string_lossy().into_owned()),
        decoded.width,
        decoded.height,
    );
    let rlab = RlabFile::new(meta, png_bytes, copies, 0, None);
    let out_path = test_dir.join("showcase.rlab");
    rlab.write(&out_path).expect("write showcase.rlab");
    println!("wrote {}", out_path.display());
    println!();
    println!("Open the .rlab in the GUI:");
    println!(
        "  cargo run --release -p rasterlab-gui -- {}",
        out_path.display()
    );
}

// ────────────────────────────────────────────────────────────────────────────
// Source image
// ────────────────────────────────────────────────────────────────────────────

fn build_showcase_image() -> RgbImage {
    let w = SRC_W;
    let h = SRC_H;
    let mut img: RgbImage = ImageBuffer::new(w, h);

    let sky_top = [120u8, 170, 230];
    let sky_bot = [240u8, 230, 200];

    let bands: [[u8; 3]; 8] = [
        [220, 60, 60],   // red
        [230, 140, 50],  // orange
        [230, 220, 60],  // yellow
        [80, 200, 100],  // green
        [60, 200, 220],  // cyan
        [80, 110, 220],  // blue
        [180, 100, 220], // violet
        [220, 120, 180], // magenta
    ];

    let band_top = h * 6 / 10;
    let band_bot = h * 8 / 10;
    let band_w = w / bands.len() as u32;

    // Checker patch (sharp/blur target) — top-right corner.
    let checker_x0 = w * 3 / 4;
    let checker_y0 = h / 8;
    let checker_w = w / 8;
    let checker_h = h / 8;
    let checker_size = 8;

    // Noise patch (NR target) — bottom-right corner.
    let noise_x0 = w * 3 / 4;
    let noise_y0 = h * 5 / 6;
    let noise_w = w / 6;
    let noise_h = h / 8;

    for y in 0..h {
        for x in 0..w {
            let mut px = if y < band_top {
                // Sky gradient.
                let t = y as f32 / band_top as f32;
                lerp_rgb(sky_top, sky_bot, t)
            } else if y < band_bot {
                // Coloured bands.
                let b = ((x / band_w) as usize).min(bands.len() - 1);
                bands[b]
            } else {
                // Bottom: greyscale ramp left→right.
                let v = ((x as f32 / w as f32) * 255.0) as u8;
                [v, v, v]
            };

            // Checker overlay.
            if x >= checker_x0
                && x < checker_x0 + checker_w
                && y >= checker_y0
                && y < checker_y0 + checker_h
            {
                let cx = (x - checker_x0) / checker_size;
                let cy = (y - checker_y0) / checker_size;
                px = if (cx + cy).is_multiple_of(2) {
                    [255, 255, 255]
                } else {
                    [20, 20, 20]
                };
            }

            // Noise patch overlay: deterministic hashed noise on a medium grey.
            if x >= noise_x0 && x < noise_x0 + noise_w && y >= noise_y0 && y < noise_y0 + noise_h {
                let h = hash_xy(x, y);
                let n = ((h & 0xff) as i16 - 128).clamp(-90, 90);
                let base = 130_i16;
                let v = (base + n).clamp(0, 255) as u8;
                px = [v, v, v];
            }

            img.put_pixel(x, y, Rgb(px));
        }
    }

    img
}

#[inline]
fn lerp_rgb(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    [
        ((a[0] as f32) + (b[0] as f32 - a[0] as f32) * t) as u8,
        ((a[1] as f32) + (b[1] as f32 - a[1] as f32) * t) as u8,
        ((a[2] as f32) + (b[2] as f32 - a[2] as f32) * t) as u8,
    ]
}

#[inline]
fn hash_xy(x: u32, y: u32) -> u32 {
    let mut h = x
        .wrapping_mul(0x9e3779b1)
        .wrapping_add(y.wrapping_mul(0x85ebca77));
    h ^= h >> 13;
    h = h.wrapping_mul(0xc2b2ae35);
    h ^= h >> 16;
    h
}

// ────────────────────────────────────────────────────────────────────────────
// Pipeline builders
// ────────────────────────────────────────────────────────────────────────────

fn pipeline_state_from_ops(ops: Vec<(Box<dyn Operation>, bool)>, source: &Image) -> PipelineState {
    let mut p = EditPipeline::new(source.deep_clone());
    for (op, enabled) in ops {
        let idx = p.ops().len();
        p.push_op(op);
        if !enabled {
            p.set_enabled_no_snapshot(idx, false);
        }
    }
    p.save_state().expect("serialise pipeline")
}

fn build_all_adjustments(source: &Image) -> PipelineState {
    let w = source.width;
    let h = source.height;

    // Mild crop to trim ~5% off each edge, leaving the bulk of the image.
    let crop = CropOp::new(
        (w * 5) / 100,
        (h * 5) / 100,
        w - (w * 10) / 100,
        h - (h * 10) / 100,
    );

    // Identity LUT — visible in the stack but no pixel change.
    let lut = LutOp::identity(17);

    // Heal: one mock spot in the sky region. The op will gracefully handle
    // out-of-bounds gracefully via clamping.
    let heal = HealOp::new(vec![HealSpot {
        dest_x: (w as i32) / 4,
        dest_y: (h as i32) / 8,
        src_x: (w as i32) / 3,
        src_y: (h as i32) / 8,
        radius: 22,
    }]);

    let ops: Vec<(Box<dyn Operation>, bool)> = vec![
        // ── Geometry first so subsequent ops see the final pixel grid. ──
        (Box::new(crop), true),
        (Box::new(RotateOp::arbitrary(3.0)), true),
        (Box::new(FlipOp::horizontal()), false), // disabled — flips back
        (
            Box::new(PerspectiveOp::new([
                [0.02, 0.0],
                [-0.02, 0.0],
                [0.0, 0.0],
                [0.0, 0.0],
            ])),
            true,
        ),
        // ── Colour and tone ─────────────────────────────────────────────
        (Box::new(WhiteBalanceOp::new(0.15, -0.05)), true),
        (
            Box::new(ColorBalanceOp::new(
                [0.10, 0.05, -0.05],
                [0.00, 0.00, 0.00],
                [-0.05, 0.00, 0.10],
            )),
            true,
        ),
        (Box::new(LevelsOp::new(0.03, 0.97, 1.05)), true),
        (Box::new(BrightnessContrastOp::new(0.05, 0.10)), true),
        (Box::new(HighlightsShadowsOp::new(-0.30, 0.30)), true),
        (Box::new(ShadowExposureOp::new(0.40, 2.0)), true),
        (Box::new(CurvesOp::identity()), true),
        // ── Colour space round trip (visible / harmless on sRGB display). ─
        (
            Box::new(ColorSpaceOp::new(ColorSpaceConversion::SrgbToDisplayP3)),
            false,
        ),
        (
            Box::new(ColorSpaceOp::new(ColorSpaceConversion::DisplayP3ToSrgb)),
            false,
        ),
        // ── Saturation, hue, HSL ────────────────────────────────────────
        (Box::new(HueShiftOp::new(15.0)), true),
        (Box::new(SaturationOp::new(1.10)), true),
        (Box::new(VibranceOp::new(0.20)), true),
        (Box::new(HslPanelOp::default()), true),
        // ── Detail / local contrast ─────────────────────────────────────
        (Box::new(ClarityTextureOp::new(0.25, 0.15)), true),
        (Box::new(SharpenOp::new(0.5)), true),
        (Box::new(BlurOp::new(0.6)), true),
        (Box::new(DenoiseOp::new(0.20, 2)), true),
        (
            Box::new(NoiseReductionOp {
                method: NrMethod::Wavelet,
                luma_strength: 0.20,
                color_strength: 0.30,
                detail_preservation: 0.55,
            }),
            true,
        ),
        // ── Effects / looks ─────────────────────────────────────────────
        (Box::new(FauxHdrOp::new(0.4)), true),
        (Box::new(SplitToneOp::default()), true),
        (Box::new(SepiaOp::new(0.0)), false), // disabled
        (Box::new(BlackAndWhiteOp::luminance()), false), // disabled
        (Box::new(lut), true),
        (Box::new(GrainOp::new(0.06, 1.5, 42)), true),
        (Box::new(VignetteOp::new(0.45, 0.55, 0.40)), true),
        // ── Heal + Resize last ──────────────────────────────────────────
        (Box::new(heal), true),
        (
            Box::new(ResizeOp::new(800, 600, ResampleMode::Bilinear)),
            true,
        ),
    ];

    pipeline_state_from_ops(ops, source)
}

fn build_hdr_merge(test_dir: &Path, source: &Image) -> PipelineState {
    let paths = [
        "hdr_bracket_under.png",
        "hdr_bracket_mid.png",
        "hdr_bracket_over.png",
    ]
    .iter()
    .map(|n| test_dir.join(n).to_string_lossy().into_owned())
    .collect::<Vec<String>>();

    let ops: Vec<(Box<dyn Operation>, bool)> = vec![
        (Box::new(HdrMergeOp::new(paths)), true),
        (Box::new(LevelsOp::new(0.02, 0.98, 1.0)), true),
        (Box::new(SaturationOp::new(1.05)), true),
    ];
    pipeline_state_from_ops(ops, source)
}

fn build_panorama(test_dir: &Path, source: &Image) -> PipelineState {
    let paths = ["pano_left.png", "pano_right.png"]
        .iter()
        .map(|n| test_dir.join(n).to_string_lossy().into_owned())
        .collect::<Vec<String>>();

    let ops: Vec<(Box<dyn Operation>, bool)> = vec![
        (Box::new(PanoramaOp::new(paths, 16)), true),
        (Box::new(BrightnessContrastOp::new(0.0, 0.05)), true),
    ];
    pipeline_state_from_ops(ops, source)
}

fn build_focus_stack(test_dir: &Path, source: &Image) -> PipelineState {
    let paths = ["focus_top.png", "focus_mid.png", "focus_bot.png"]
        .iter()
        .map(|n| test_dir.join(n).to_string_lossy().into_owned())
        .collect::<Vec<String>>();

    let ops: Vec<(Box<dyn Operation>, bool)> = vec![
        (Box::new(FocusStackOp::new(paths)), true),
        (Box::new(ClarityTextureOp::new(0.15, 0.10)), true),
    ];
    pipeline_state_from_ops(ops, source)
}

fn build_masked_edits(source: &Image) -> PipelineState {
    // Linear-masked saturation: drop saturation in the right half of the
    // frame so the user can see the gradient hand-off on the bands.
    let lin_mask = MaskShape::Linear(LinearMask {
        cx: 0.5,
        cy: 0.5,
        angle_deg: 0.0,
        feather: 0.30,
        invert: false,
    });
    let linear_op: Box<dyn Operation> = Box::new(MaskedOp {
        inner: Box::new(SaturationOp::new(0.3)),
        mask: lin_mask,
    });

    // Radial-masked vignette-like darkening at the bottom-right grey ramp,
    // good for visualising the radial mask falloff.
    let rad_mask = MaskShape::Radial(RadialMask {
        cx: 0.75,
        cy: 0.85,
        radius: 0.20,
        feather: 0.60,
        invert: true,
    });
    let radial_op: Box<dyn Operation> = Box::new(MaskedOp {
        inner: Box::new(BrightnessContrastOp::new(-0.35, 0.10)),
        mask: rad_mask,
    });

    let ops: Vec<(Box<dyn Operation>, bool)> = vec![
        (linear_op, true),
        (radial_op, true),
        // Plus an unmasked sharpen so the user can see the masked ops sit
        // alongside ordinary entries in the stack.
        (Box::new(SharpenOp::new(0.4)), true),
    ];
    pipeline_state_from_ops(ops, source)
}
