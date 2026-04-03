/// Generates a set of synthetic test images useful for reverse-engineering
/// image filters — both colour (LUTs, Looks) and black-and-white.
///
/// Usage:
///   cargo run --release --example gen_test_images -- [output_dir]
///
/// Output (default dir: test_images/):
///
///   — Colour filter analysis —
///   hald_clut_identity_L8.png   HALD CLUT identity L=8 (512×512, full 3-D colour cube)
///   luminance_wedge.png         256-step greyscale ramp (tone curve shape)
///   channel_ramps.png           R, G, B ramps stacked (per-channel curves)
///   hue_wheel.png               Full hue rotation at 5 lightness levels
///   saturation_ramp.png         360 hues × saturation 0→1 (sat response per hue)
///   color_patches.png           24-patch Macbeth-style reference chart
///
///   — B&W filter analysis —
///   hue_primaries.png           6 pure hue bands at full saturation (R G B C M Y)
///   equal_luminance_grid.png    Grid: all cells same L* (LAB), different hues
///   skin_tone_ramp.png          Warm desaturated tones across luminance (skin/sky range)
///   grey_color_pairs.png        Pure grey next to same-luminance hue (detects channel mixing)
///
/// Workflow:
///   1. Run this once to generate all images.
///   2. Apply the mystery filter to every image.
///   3. Compare input vs output pixel-by-pixel to characterise the transform.
///   4. For hald_clut_identity_L8.png: the filtered output IS the 3-D LUT —
///      drop it into RasterLab's luts/ directory as a PNG HALD CLUT.
///   5. For B&W: equal_luminance_grid.png output reveals per-hue luminance
///      weighting; uniform grey = simple desaturate, variation = channel mixer.
use std::{env, path::Path};

use image::{ImageBuffer, Rgb, RgbImage};

// ─── colour helpers ──────────────────────────────────────────────────────────

/// HSL (all in [0, 1]) → sRGB [0, 255].
fn hsl_to_rgb(h: f32, s: f32, l: f32) -> [u8; 3] {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let h6 = h * 6.0;
    let x = c * (1.0 - (h6 % 2.0 - 1.0).abs());
    let (r1, g1, b1) = match h6 as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let to_u8 = |v: f32| ((v + m).clamp(0.0, 1.0) * 255.0).round() as u8;
    [to_u8(r1), to_u8(g1), to_u8(b1)]
}

/// Perceived luminance (sRGB, [0, 1]).
fn luma(r: f32, g: f32, b: f32) -> f32 {
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

fn save(img: RgbImage, dir: &Path, name: &str) {
    let path = dir.join(name);
    img.save(&path).expect("failed to save image");
    println!("  wrote {}", path.display());
}

// ═══════════════════════════════════════════════════════════════════════════════
// COLOUR FILTER IMAGES
// ═══════════════════════════════════════════════════════════════════════════════

// ─── 1. HALD CLUT identity ───────────────────────────────────────────────────

/// A HALD CLUT at level L is an (L³ × L³) image that tiles the full colour
/// cube at L³ levels per channel.  Apply a filter → output pixels ARE the LUT.
/// Level 8 → 512 × 512, 8³ = 512 levels per channel.
fn gen_hald_clut_identity(level: u32) -> RgbImage {
    let n = level * level;
    let side = n * level;
    let step = 255.0 / (n as f32 - 1.0);

    ImageBuffer::from_fn(side, side, |x, y| {
        let idx = y * side + x;
        let b_idx = idx / (n * n);
        let g_idx = (idx / n) % n;
        let r_idx = idx % n;
        let r = (r_idx as f32 * step).round() as u8;
        let g = (g_idx as f32 * step).round() as u8;
        let b = (b_idx as f32 * step).round() as u8;
        Rgb([r, g, b])
    })
}

// ─── 2. Luminance step wedge ─────────────────────────────────────────────────

/// 256 grey steps × 8 px wide × 128 px tall.
fn gen_luminance_wedge() -> RgbImage {
    ImageBuffer::from_fn(256 * 8, 128, |x, _y| {
        let v = (x / 8) as u8;
        Rgb([v, v, v])
    })
}

// ─── 3. Per-channel ramps ────────────────────────────────────────────────────

/// R, G, B ramps stacked vertically (256 × 128 each).
fn gen_channel_ramps() -> RgbImage {
    ImageBuffer::from_fn(256, 128 * 3, |x, y| {
        let v = x as u8;
        match y / 128 {
            0 => Rgb([v, 0, 0]),
            1 => Rgb([0, v, 0]),
            _ => Rgb([0, 0, v]),
        }
    })
}

// ─── 4. Hue wheel ────────────────────────────────────────────────────────────

/// 360 columns × 5 lightness bands (64 px each).
fn gen_hue_wheel() -> RgbImage {
    let lightnesses: &[f32] = &[0.2, 0.35, 0.5, 0.65, 0.8];
    let band = 64u32;
    ImageBuffer::from_fn(360, band * lightnesses.len() as u32, |x, y| {
        let l = lightnesses[(y / band) as usize];
        Rgb(hsl_to_rgb(x as f32 / 360.0, 1.0, l))
    })
}

// ─── 5. Saturation ramp ──────────────────────────────────────────────────────

/// 360 hue columns × 256 saturation rows (0 top → 1 bottom), L = 0.5.
fn gen_saturation_ramp() -> RgbImage {
    ImageBuffer::from_fn(360, 256, |x, y| {
        Rgb(hsl_to_rgb(x as f32 / 360.0, y as f32 / 255.0, 0.5))
    })
}

// ─── 6. Macbeth-style 24-patch colour chart ───────────────────────────────

const MACBETH: [[u8; 3]; 24] = [
    // Row 1 — natural objects
    [115, 82, 68],   // Dark Skin
    [194, 150, 130], // Light Skin
    [98, 122, 157],  // Blue Sky
    [87, 108, 67],   // Foliage
    [133, 128, 177], // Blue Flower
    [103, 189, 170], // Bluish Green
    // Row 2 — saturated colours
    [214, 126, 44], // Orange
    [80, 91, 166],  // Purplish Blue
    [193, 90, 99],  // Moderate Red
    [94, 60, 108],  // Purple
    [157, 188, 64], // Yellow Green
    [224, 163, 46], // Orange Yellow
    // Row 3 — primaries & secondaries
    [56, 61, 150],  // Blue
    [70, 148, 73],  // Green
    [175, 54, 60],  // Red
    [231, 199, 31], // Yellow
    [187, 86, 149], // Magenta
    [8, 133, 161],  // Cyan
    // Row 4 — grey scale
    [243, 243, 243], // White
    [200, 200, 200], // Neutral 8
    [160, 160, 160], // Neutral 6.5
    [122, 122, 122], // Neutral 5
    [85, 85, 85],    // Neutral 3.5
    [52, 52, 52],    // Black
];

fn gen_color_patches() -> RgbImage {
    let patch = 96u32;
    ImageBuffer::from_fn(6 * patch, 4 * patch, |x, y| {
        let idx = ((y / patch) * 6 + (x / patch)) as usize;
        Rgb(MACBETH[idx])
    })
}

// ═══════════════════════════════════════════════════════════════════════════════
// B&W FILTER IMAGES
// ═══════════════════════════════════════════════════════════════════════════════

// ─── 7. Hue primary bands ────────────────────────────────────────────────────

/// Six 128 px wide bands: R, G, B, C, M, Y — all at full saturation, L=0.5.
/// Lightroom/ACR B&W mix sliders target exactly these six hue ranges.
/// After filtering, compare each band's grey value against each other.
fn gen_hue_primaries() -> RgbImage {
    // hue angles: R=0°, Y=60°, G=120°, C=180°, B=240°, M=300°
    let hues: [f32; 6] = [0.0, 60.0, 120.0, 180.0, 240.0, 300.0];
    let patch_w = 128u32;
    let h = 256u32;
    ImageBuffer::from_fn(patch_w * hues.len() as u32, h, |x, y| {
        let hue = hues[(x / patch_w) as usize];
        // gradient: top = L 0.85 (near white), bottom = L 0.15 (near black)
        // keeps the hue identifiable across lightness
        let l = 0.85 - (y as f32 / h as f32) * 0.70;
        Rgb(hsl_to_rgb(hue / 360.0, 1.0, l))
    })
}

// ─── 8. Equal-luminance colour grid ─────────────────────────────────────────

/// Grid of colour patches where every cell has the same perceived luminance
/// (Rec. 709 luma ≈ 0.5) but a different hue and saturation.
///
/// After a B&W filter:
///   • Uniform grey → simple luminance / desaturate conversion.
///   • Variation in grey values → per-hue channel weighting (channel mixer,
///     emulated colour filter, etc.).
///
/// This is the single most diagnostic image for B&W filter analysis.
fn gen_equal_luminance_grid() -> RgbImage {
    // We want luma(r,g,b) ≈ target_luma for every patch.
    // Iterate hues × saturations; for each (h, s) find the lightness L
    // that gives the target luma via bisection, then emit the patch.
    let target_luma = 0.50f32;
    let hue_steps = 24u32;
    let sat_steps = 8u32;
    let patch = 64u32;

    let w = hue_steps * patch;
    let h = sat_steps * patch;

    ImageBuffer::from_fn(w, h, |x, y| {
        let hue = (x / patch) as f32 / hue_steps as f32;
        // saturation from 0.3 (top) to 1.0 (bottom) — skip near-grey rows
        let sat = 0.3 + (y / patch) as f32 / (sat_steps as f32 - 1.0) * 0.7;

        // Bisect to find L that yields target_luma
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        for _ in 0..20 {
            let mid = (lo + hi) / 2.0;
            let [r, g, b] = hsl_to_rgb(hue, sat, mid);
            let l = luma(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
            if l < target_luma {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let l = (lo + hi) / 2.0;
        Rgb(hsl_to_rgb(hue, sat, l))
    })
}

// ─── 9. Skin-tone ramp ───────────────────────────────────────────────────────

/// Warm desaturated tones (hue ≈ 20°–35°, sat 0.15–0.55) across luminance.
/// B&W film filters (orange, red, yellow) were designed primarily to lighten
/// skin and darken sky; this ramp makes that effect visible.
fn gen_skin_tone_ramp() -> RgbImage {
    let hue_steps = 8u32; // hue columns across warm skin range
    let luma_steps = 16u32; // luminance rows
    let patch = 64u32;

    ImageBuffer::from_fn(hue_steps * patch, luma_steps * patch, |x, y| {
        // Hue 20°–35° (warm skin / amber)
        let hue = (20.0 + (x / patch) as f32 / (hue_steps as f32 - 1.0) * 15.0) / 360.0;
        let sat = 0.35f32;
        // Lightness dark → light top-to-bottom
        let l = 0.10 + (y / patch) as f32 / (luma_steps as f32 - 1.0) * 0.80;
        Rgb(hsl_to_rgb(hue, sat, l))
    })
}

// ─── 10. Grey / same-luminance colour pairs ──────────────────────────────────

/// Each row: a pure grey patch paired with a hue patch of the same perceived
/// luminance.  After a simple desaturate both should be identical.  Any
/// difference exposes channel mixing or hue-weighted conversion.
fn gen_grey_color_pairs() -> RgbImage {
    // 8 hues, each with a matching-luma grey
    let hues: [f32; 8] = [0.0, 45.0, 90.0, 135.0, 180.0, 225.0, 270.0, 315.0];
    let patch_w = 120u32;
    let patch_h = 80u32;
    let gap = 4u32; // thin separator between grey and colour patch

    let row_h = patch_h + 4; // a little breathing room
    let total_h = row_h * hues.len() as u32;
    let total_w = patch_w * 2 + gap;

    ImageBuffer::from_fn(total_w, total_h, |x, y| {
        let row = (y / row_h) as usize;
        if row >= hues.len() {
            return Rgb([20u8, 20, 20]);
        }
        let local_y = y % row_h;
        if local_y >= patch_h {
            return Rgb([20u8, 20, 20]); // row gap
        }
        // Separator column
        if x >= patch_w && x < patch_w + gap {
            return Rgb([20u8, 20, 20]);
        }

        let hue = hues[row] / 360.0;
        // Find L at full saturation that gives luma ≈ 0.45
        let target = 0.45f32;
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        for _ in 0..20 {
            let mid = (lo + hi) / 2.0;
            let [r, g, b] = hsl_to_rgb(hue, 1.0, mid);
            let l = luma(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
            if l < target {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let colour_l = (lo + hi) / 2.0;
        let [r, g, b] = hsl_to_rgb(hue, 1.0, colour_l);
        let grey_v = luma(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
        let grey_u8 = (grey_v * 255.0).round() as u8;

        if x < patch_w {
            Rgb([grey_u8, grey_u8, grey_u8]) // left: matching grey
        } else {
            Rgb([r, g, b]) // right: saturated colour
        }
    })
}

// ─── main ───────────────────────────────────────────────────────────────────

fn main() {
    let args: Vec<String> = env::args().collect();
    let out_dir = Path::new(args.get(1).map(String::as_str).unwrap_or("test_images"));
    std::fs::create_dir_all(out_dir).expect("cannot create output directory");

    println!("Generating test images → {}", out_dir.display());

    println!("\n  [colour filter images]");
    save(
        gen_hald_clut_identity(8),
        out_dir,
        "hald_clut_identity_L8.png",
    );
    save(gen_luminance_wedge(), out_dir, "luminance_wedge.png");
    save(gen_channel_ramps(), out_dir, "channel_ramps.png");
    save(gen_hue_wheel(), out_dir, "hue_wheel.png");
    save(gen_saturation_ramp(), out_dir, "saturation_ramp.png");
    save(gen_color_patches(), out_dir, "color_patches.png");

    println!("\n  [B&W filter images]");
    save(gen_hue_primaries(), out_dir, "hue_primaries.png");
    save(
        gen_equal_luminance_grid(),
        out_dir,
        "equal_luminance_grid.png",
    );
    save(gen_skin_tone_ramp(), out_dir, "skin_tone_ramp.png");
    save(gen_grey_color_pairs(), out_dir, "grey_color_pairs.png");

    println!("\nDone.");
    println!("\nColour filter workflow:");
    println!("  hald_clut_identity_L8.png → apply filter → output IS the 3-D LUT");
    println!("  channel_ramps / hue_wheel / saturation_ramp → plot input→output curves");
    println!("  color_patches → spot-check 24 known reference values");
    println!("\nB&W filter workflow:");
    println!("  equal_luminance_grid → uniform grey = desaturate; variation = channel mixer");
    println!("  hue_primaries → read grey value of each band = per-hue luminance weight");
    println!("  grey_color_pairs → left/right should match for simple desaturate");
    println!("  skin_tone_ramp → shows warm-tone / sky response of film-emulation filters");
}
