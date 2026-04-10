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

// ═══════════════════════════════════════════════════════════════════════════════
// PANORAMA TEST IMAGES
// ═══════════════════════════════════════════════════════════════════════════════

/// Deterministic xorshift64 — avoids pulling in the `rand` crate.
fn xs64(x: u64) -> u64 {
    let x = x ^ (x << 13);
    let x = x ^ (x >> 7);
    x ^ (x << 17)
}

/// Draw a filled axis-aligned rectangle into `img`.
fn fill_rect(img: &mut RgbImage, x0: u32, y0: u32, x1: u32, y1: u32, color: [u8; 3]) {
    for y in y0..y1.min(img.height()) {
        for x in x0..x1.min(img.width()) {
            img.put_pixel(x, y, Rgb(color));
        }
    }
}

/// Draw a 1-px border around a rectangle.
fn draw_border(img: &mut RgbImage, x0: u32, y0: u32, x1: u32, y1: u32, color: [u8; 3]) {
    for x in x0..x1.min(img.width()) {
        if y0 < img.height() {
            img.put_pixel(x, y0, Rgb(color));
        }
        let y1c = (y1 - 1).min(img.height() - 1);
        img.put_pixel(x, y1c, Rgb(color));
    }
    for y in y0..y1.min(img.height()) {
        if x0 < img.width() {
            img.put_pixel(x0, y, Rgb(color));
        }
        let x1c = (x1 - 1).min(img.width() - 1);
        img.put_pixel(x1c, y, Rgb(color));
    }
}

/// Draw a filled circle (integer Bresenham).
fn fill_circle(img: &mut RgbImage, cx: i32, cy: i32, r: i32, color: [u8; 3]) {
    let r2 = r * r;
    for dy in -r..=r {
        for dx in -r..=r {
            if dx * dx + dy * dy <= r2 {
                let px = cx + dx;
                let py = cy + dy;
                if px >= 0 && py >= 0 && px < img.width() as i32 && py < img.height() as i32 {
                    img.put_pixel(px as u32, py as u32, Rgb(color));
                }
            }
        }
    }
}

/// Build the 3200×800 synthetic panorama scene.
///
/// The scene is deliberately **non-periodic** so that Harris + normalised
/// patch descriptors produce unambiguous matches across overlapping tiles.
/// It contains:
///  • a diagonal two-tone gradient background (no axis-aligned symmetries)
///  • many randomly placed filled circles with unique colours and radii
///  • random filled rectangles at random orientations-by-position
///  • a scattering of small "chip" clusters (distinctive local neighbourhoods)
///
/// Every visual element is placed at a unique random position, so a patch
/// descriptor at any corner is distinguishable from every other corner.
fn gen_panorama_scene() -> RgbImage {
    let scene_w = 3200u32;
    let scene_h = 800u32;

    let mut img = RgbImage::new(scene_w, scene_h);

    // ── background: diagonal two-tone gradient ──
    // Varies slowly along both axes so every region has a unique tint.
    for y in 0..scene_h {
        for x in 0..scene_w {
            let tx = x as f32 / scene_w as f32;
            let ty = y as f32 / scene_h as f32;
            let r = (40.0 + tx * 60.0 + ty * 30.0) as u8;
            let g = (60.0 + tx * 30.0 + ty * 90.0) as u8;
            let b = (110.0 + (1.0 - tx) * 80.0 + (1.0 - ty) * 40.0) as u8;
            img.put_pixel(x, y, Rgb([r, g, b]));
        }
    }

    // Rich 64-colour palette so nearby shapes rarely share colours.
    let palette: [[u8; 3]; 32] = [
        [220, 60, 60],
        [60, 180, 60],
        [60, 60, 220],
        [220, 180, 40],
        [220, 100, 40],
        [140, 60, 200],
        [40, 200, 200],
        [220, 60, 160],
        [100, 160, 60],
        [60, 120, 200],
        [200, 120, 80],
        [80, 200, 140],
        [180, 60, 100],
        [100, 80, 160],
        [200, 200, 80],
        [80, 160, 180],
        [255, 140, 0],
        [128, 0, 128],
        [0, 128, 128],
        [255, 215, 0],
        [127, 255, 0],
        [255, 20, 147],
        [70, 130, 180],
        [240, 128, 128],
        [34, 139, 34],
        [218, 112, 214],
        [255, 99, 71],
        [0, 191, 255],
        [154, 205, 50],
        [199, 21, 133],
        [47, 79, 79],
        [255, 192, 203],
    ];

    // ── Dense non-periodic circles (~1200 of them) ──
    // Coordinates jittered per-circle; radius, colour, and position all
    // independent so no two local neighbourhoods are identical.
    let mut rng = 0xDEAD_BEEF_1234_5678u64;
    for _ in 0..1200 {
        rng = xs64(rng);
        let cx = (rng as u32 % scene_w) as i32;
        rng = xs64(rng);
        let cy = (rng as u32 % scene_h) as i32;
        rng = xs64(rng);
        let r = (4 + (rng as i32).unsigned_abs() as i32 % 18).min(22);
        rng = xs64(rng);
        let ci = (rng as usize) % palette.len();
        let color = palette[ci];
        fill_circle(&mut img, cx, cy, r, color);
        // Inner highlight — creates a sharp corner pair around the circle.
        let inner = [
            (color[0] as u16 + 60).min(255) as u8,
            (color[1] as u16 + 60).min(255) as u8,
            (color[2] as u16 + 60).min(255) as u8,
        ];
        if r > 5 {
            fill_circle(&mut img, cx - 1, cy - 1, r / 2, inner);
        }
    }

    // ── Random axis-aligned rectangles (~400) ──
    rng = 0xFACEFEED_0BADF00Du64;
    for _ in 0..400 {
        rng = xs64(rng);
        let x0 = (rng as u32) % (scene_w - 40);
        rng = xs64(rng);
        let y0 = (rng as u32) % (scene_h - 40);
        rng = xs64(rng);
        let w = 8 + (rng as u32) % 34;
        rng = xs64(rng);
        let h = 8 + (rng as u32) % 34;
        rng = xs64(rng);
        let ci = (rng as usize) % palette.len();
        let color = palette[ci];
        let x1 = (x0 + w).min(scene_w - 1);
        let y1 = (y0 + h).min(scene_h - 1);
        fill_rect(&mut img, x0, y0, x1, y1, color);
        draw_border(&mut img, x0, y0, x1, y1, [15, 15, 15]);
    }

    // ── Scattered "chip" clusters: a ring of 3–5 small circles around an
    // anchor point.  Each cluster forms a distinctive local pattern that
    // normalised-patch descriptors can match unambiguously.
    rng = 0xABCD_1234_FEED_C0DEu64;
    for _ in 0..120 {
        rng = xs64(rng);
        let ax = 40 + (rng as u32) % (scene_w - 80);
        rng = xs64(rng);
        let ay = 40 + (rng as u32) % (scene_h - 80);
        rng = xs64(rng);
        let n = 3 + (rng as usize) % 3;
        for k in 0..n {
            rng = xs64(rng);
            let angle = (k as f32 / n as f32) * std::f32::consts::TAU;
            let rr = 14.0 + (rng as u32 % 8) as f32;
            let px = ax as f32 + angle.cos() * rr;
            let py = ay as f32 + angle.sin() * rr;
            let ci = (rng as usize) % palette.len();
            fill_circle(&mut img, px as i32, py as i32, 5, palette[ci]);
        }
        // Centre dot in a contrasting colour.
        rng = xs64(rng);
        let ci = (rng as usize) % palette.len();
        fill_circle(&mut img, ax as i32, ay as i32, 7, palette[ci]);
        fill_circle(&mut img, ax as i32, ay as i32, 3, [250, 250, 250]);
    }

    img
}

/// Crop a sub-image from `src`, starting at `x0` with width `w`.
fn crop_x(src: &RgbImage, x0: u32, w: u32) -> RgbImage {
    let h = src.height();
    ImageBuffer::from_fn(w, h, |x, y| *src.get_pixel(x0 + x, y))
}

/// Generate two overlapping tiles (left and right) from the scene.
///
///   pano_left.png  — columns 0   .. 2000  (62.5 % of scene)
///   pano_right.png — columns 1200 .. 3200  (62.5 % of scene)
///   overlap region — columns 1200 .. 2000  (800 px = 25 % of each tile)
fn gen_panorama_2(scene: &RgbImage, dir: &Path) {
    let sw = scene.width(); // 3200
    let tile_w = sw * 5 / 8; // 2000
    let overlap = sw / 4; // 800

    let left = crop_x(scene, 0, tile_w);
    let right = crop_x(scene, sw - tile_w, tile_w);

    save(left, dir, "pano_left.png");
    save(right, dir, "pano_right.png");

    println!(
        "    overlap: {overlap} px ({:.0}% of tile width)",
        overlap as f64 / tile_w as f64 * 100.0
    );
}

/// Generate three overlapping tiles for a 3-image panorama test.
///
///   pano3_a.png — columns 0    .. 1400  (43.75 %)
///   pano3_b.png — columns 900  .. 2300  (43.75 %)
///   pano3_c.png — columns 1800 .. 3200  (43.75 %)
///
///  a↔b overlap 900..1400 = 500 px
///  b↔c overlap 1800..2300 = 500 px
fn gen_panorama_3(scene: &RgbImage, dir: &Path) {
    let sw = scene.width(); // 3200
    let tile_w = sw * 7 / 16; // 1400

    let a = crop_x(scene, 0, tile_w);
    let b = crop_x(scene, 900, tile_w);
    let c = crop_x(scene, sw - tile_w, tile_w);

    save(a, dir, "pano3_a.png");
    save(b, dir, "pano3_b.png");
    save(c, dir, "pano3_c.png");

    let overlap = tile_w + 900 - sw / 2;
    println!(
        "    a↔b / b↔c overlap: ~{overlap} px ({:.0}% of tile width)",
        overlap as f64 / tile_w as f64 * 100.0
    );
}

// ─── focus stacking test images ─────────────────────────────────────────────

/// Build a 1600×900 feature-rich scene used as the "in focus everywhere"
/// reference.  Same non-periodic style as the panorama scene so Modified
/// Laplacian has plenty of high-frequency content to pick up on.
fn gen_focus_scene() -> RgbImage {
    let w = 1600u32;
    let h = 900u32;
    let mut img = RgbImage::new(w, h);

    // Two-tone diagonal gradient background.
    for y in 0..h {
        for x in 0..w {
            let tx = x as f32 / w as f32;
            let ty = y as f32 / h as f32;
            let r = (50.0 + tx * 60.0 + ty * 20.0) as u8;
            let g = (70.0 + tx * 40.0 + ty * 80.0) as u8;
            let b = (100.0 + (1.0 - tx) * 70.0 + (1.0 - ty) * 40.0) as u8;
            img.put_pixel(x, y, Rgb([r, g, b]));
        }
    }

    let palette: [[u8; 3]; 20] = [
        [230, 60, 60],
        [60, 200, 60],
        [60, 60, 220],
        [220, 180, 40],
        [220, 100, 40],
        [140, 60, 200],
        [40, 200, 200],
        [220, 60, 160],
        [100, 160, 60],
        [60, 120, 200],
        [200, 120, 80],
        [80, 200, 140],
        [180, 60, 100],
        [100, 80, 160],
        [200, 200, 80],
        [255, 140, 0],
        [0, 191, 255],
        [154, 205, 50],
        [218, 112, 214],
        [47, 79, 79],
    ];

    // Dense scattering of small high-contrast shapes.  Each has a
    // contrasting inner dot so the Modified Laplacian picks up strong
    // double-edges.
    let mut rng = 0x1234_5678_ABCD_EF00u64;
    for _ in 0..900 {
        rng = xs64(rng);
        let cx = (rng as u32 % w) as i32;
        rng = xs64(rng);
        let cy = (rng as u32 % h) as i32;
        rng = xs64(rng);
        let r = 4 + (rng as i32 & 0xF);
        rng = xs64(rng);
        let ci = (rng as usize) % palette.len();
        let color = palette[ci];
        fill_circle(&mut img, cx, cy, r, color);
        rng = xs64(rng);
        let inner = palette[(rng as usize) % palette.len()];
        fill_circle(&mut img, cx, cy, (r / 2).max(1), inner);
    }

    // Rectangles with sharp borders.
    rng = 0xAAAA_BBBB_CCCC_DDDDu64;
    for _ in 0..300 {
        rng = xs64(rng);
        let x0 = rng as u32 % (w - 40);
        rng = xs64(rng);
        let y0 = rng as u32 % (h - 40);
        rng = xs64(rng);
        let rw = 8 + (rng as u32 % 28);
        rng = xs64(rng);
        let rh = 8 + (rng as u32 % 28);
        rng = xs64(rng);
        let ci = (rng as usize) % palette.len();
        let x1 = (x0 + rw).min(w - 1);
        let y1 = (y0 + rh).min(h - 1);
        fill_rect(&mut img, x0, y0, x1, y1, palette[ci]);
        draw_border(&mut img, x0, y0, x1, y1, [15, 15, 15]);
    }

    img
}

/// Gaussian-blur a rectangular region of `src` in place (padded-reflect
/// boundary handling).  Used to synthesise out-of-focus content.
///
/// `amount` is a unit-less blur strength: 0.0 → no blur, 1.0 → σ = 4 px.
/// Non-affected pixels outside the bands stay at `src` exactly.
fn gaussian_blur_bands(src: &RgbImage, bands: &[(u32, u32, f32)]) -> RgbImage {
    // Each band: (y0, y1, blur_amount).  Pixels outside every band stay
    // sharp.  Implementation: run a separable Gaussian on the full image
    // per unique sigma, then composite the blurred rows back into src.
    let w = src.width() as usize;
    let h = src.height() as usize;

    // Collect unique sigmas.
    let mut sigmas: Vec<f32> = bands.iter().map(|&(_, _, a)| a * 4.0).collect();
    sigmas.sort_by(|a, b| a.partial_cmp(b).unwrap());
    sigmas.dedup_by(|a, b| (*a - *b).abs() < 1e-3);

    // Precompute a blurred buffer per unique sigma.
    let blurred: Vec<(f32, Vec<[f32; 3]>)> = sigmas
        .iter()
        .map(|&sigma| {
            // Source as f32 RGB.
            let mut buf: Vec<[f32; 3]> = Vec::with_capacity(w * h);
            for y in 0..h as u32 {
                for x in 0..w as u32 {
                    let p = src.get_pixel(x, y).0;
                    buf.push([p[0] as f32, p[1] as f32, p[2] as f32]);
                }
            }
            if sigma <= 0.01 {
                return (sigma, buf);
            }
            let kernel = gaussian_kernel(sigma);
            let half = kernel.len() / 2;

            // Horizontal pass.
            let mut tmp = buf.clone();
            for y in 0..h {
                for x in 0..w {
                    let mut acc = [0.0f32; 3];
                    for (k, &kv) in kernel.iter().enumerate() {
                        let xi = (x as isize + k as isize - half as isize).clamp(0, w as isize - 1)
                            as usize;
                        let s = buf[y * w + xi];
                        acc[0] += kv * s[0];
                        acc[1] += kv * s[1];
                        acc[2] += kv * s[2];
                    }
                    tmp[y * w + x] = acc;
                }
            }

            // Vertical pass.
            let mut out = tmp.clone();
            for y in 0..h {
                for x in 0..w {
                    let mut acc = [0.0f32; 3];
                    for (k, &kv) in kernel.iter().enumerate() {
                        let yi = (y as isize + k as isize - half as isize).clamp(0, h as isize - 1)
                            as usize;
                        let s = tmp[yi * w + x];
                        acc[0] += kv * s[0];
                        acc[1] += kv * s[1];
                        acc[2] += kv * s[2];
                    }
                    out[y * w + x] = acc;
                }
            }
            (sigma, out)
        })
        .collect();

    // Composite: start from the original, then for each band replace
    // pixels with the matching blurred buffer.
    let mut dst = src.clone();
    for &(y0, y1, amount) in bands {
        let sigma = amount * 4.0;
        // Find the matching blurred buffer.
        let buf = blurred
            .iter()
            .find(|(s, _)| (*s - sigma).abs() < 1e-3)
            .map(|(_, b)| b)
            .expect("sigma lookup");
        let y0 = y0.min(src.height());
        let y1 = y1.min(src.height());
        for y in y0..y1 {
            for x in 0..src.width() {
                let p = buf[y as usize * w + x as usize];
                dst.put_pixel(
                    x,
                    y,
                    Rgb([
                        p[0].clamp(0.0, 255.0) as u8,
                        p[1].clamp(0.0, 255.0) as u8,
                        p[2].clamp(0.0, 255.0) as u8,
                    ]),
                );
            }
        }
    }
    dst
}

/// Discrete 1-D Gaussian kernel for the given `sigma` (σ).  Kernel radius
/// is `ceil(3 · σ)`, truncated at the tails.
fn gaussian_kernel(sigma: f32) -> Vec<f32> {
    let radius = (sigma * 3.0).ceil().max(1.0) as usize;
    let mut k = vec![0.0f32; 2 * radius + 1];
    let two_sigma_sq = 2.0 * sigma * sigma;
    let mut sum = 0.0f32;
    for (i, v) in k.iter_mut().enumerate() {
        let x = i as f32 - radius as f32;
        *v = (-(x * x) / two_sigma_sq).exp();
        sum += *v;
    }
    for v in k.iter_mut() {
        *v /= sum;
    }
    k
}

/// Build a three-frame focus stack from the reference scene:
///   focus_top.png    — top band sharp,    mid + bottom blurred
///   focus_mid.png    — middle band sharp, top + bottom blurred
///   focus_bot.png    — bottom band sharp, top + middle blurred
///
/// When fused with a focus-stacking algorithm the output should match
/// `focus_scene_full.png` pixel-for-pixel in the sharp regions and very
/// closely everywhere else.
fn gen_focus_stack_3(scene: &RgbImage, dir: &Path) {
    let h = scene.height();
    let t1 = h / 3;
    let t2 = 2 * h / 3;

    // Each frame blurs every band except its "sharp" band.  Use a heavy
    // blur (σ = 4) so the Modified Laplacian signal-to-noise is huge.
    let top = gaussian_blur_bands(scene, &[(t1, t2, 1.0), (t2, h, 1.0)]);
    let mid = gaussian_blur_bands(scene, &[(0, t1, 1.0), (t2, h, 1.0)]);
    let bot = gaussian_blur_bands(scene, &[(0, t1, 1.0), (t1, t2, 1.0)]);

    save(top, dir, "focus_top.png");
    save(mid, dir, "focus_mid.png");
    save(bot, dir, "focus_bot.png");

    println!("    band split: top=0..{t1}, mid={t1}..{t2}, bot={t2}..{h} (blur σ=4 elsewhere)");
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

    println!("\n  [panorama test images]");
    let scene = gen_panorama_scene();
    save(scene.clone(), out_dir, "pano_scene_full.png");
    gen_panorama_2(&scene, out_dir);
    gen_panorama_3(&scene, out_dir);

    println!("\n  [focus stacking test images]");
    let focus_scene = gen_focus_scene();
    save(focus_scene.clone(), out_dir, "focus_scene_full.png");
    gen_focus_stack_3(&focus_scene, out_dir);

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
    println!("\nPanorama test workflow:");
    println!("  pano_scene_full.png  — full 3200×800 reference scene");
    println!("  pano_left / pano_right — 2-tile test (open pano_left, stitch pano_right)");
    println!("  pano3_a / pano3_b / pano3_c — 3-tile test (open pano3_a, stitch b then c)");
    println!("\nFocus stacking test workflow:");
    println!("  focus_scene_full.png       — the all-in-focus reference");
    println!("  focus_top / focus_mid / focus_bot — three frames, one sharp band each");
    println!("  open focus_top, add focus_mid + focus_bot, stack → should match reference");
}
