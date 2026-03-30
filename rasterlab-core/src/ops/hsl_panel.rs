use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{error::RasterResult, image::Image, traits::operation::Operation};

/// Per-hue-band Hue / Saturation / Luminance panel.
///
/// Eight hue bands — Reds, Oranges, Yellows, Greens, Aquas, Blues, Purples,
/// Magentas — each with independent H / S / L sliders.  Bands overlap
/// smoothly via triangular weighting so there are no hard colour edges.
///
/// Band centres (degrees): 0, 45, 90, 135, 180, 225, 270, 315.
///
/// * `hue`        — shift in degrees, range `[-180, 180]`.
/// * `saturation` — additive shift, range `[-1, 1]` (`+1` → fully saturated).
/// * `luminance`  — additive shift in `[0, 1]` lightness, range `[-0.5, 0.5]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HslPanelOp {
    /// Per-band hue shift in degrees (8 bands: Reds … Magentas).
    pub hue: [f32; 8],
    /// Per-band saturation shift (additive, in `[-1, 1]`).
    pub saturation: [f32; 8],
    /// Per-band luminance shift (additive, in `[-0.5, 0.5]`).
    pub luminance: [f32; 8],
}

impl HslPanelOp {
    pub fn new(hue: [f32; 8], saturation: [f32; 8], luminance: [f32; 8]) -> Self {
        let clamp8 = |a: [f32; 8], lo: f32, hi: f32| {
            let mut out = a;
            for v in &mut out {
                *v = v.clamp(lo, hi);
            }
            out
        };
        Self {
            hue: clamp8(hue, -180.0, 180.0),
            saturation: clamp8(saturation, -1.0, 1.0),
            luminance: clamp8(luminance, -0.5, 0.5),
        }
    }

    /// `true` when every slider is at neutral.
    pub fn is_identity(&self) -> bool {
        let all_zero = |a: &[f32; 8]| a.iter().all(|v| v.abs() < 1e-5);
        all_zero(&self.hue) && all_zero(&self.saturation) && all_zero(&self.luminance)
    }
}

impl Default for HslPanelOp {
    fn default() -> Self {
        Self {
            hue: [0.0; 8],
            saturation: [0.0; 8],
            luminance: [0.0; 8],
        }
    }
}

// Band centres in [0, 1] hue space (0°, 45°, … , 315°).
const BAND_CENTRES: [f32; 8] = [
    0.000, // Reds       0°
    0.125, // Oranges   45°
    0.250, // Yellows   90°
    0.375, // Greens   135°
    0.500, // Aquas    180°
    0.625, // Blues    225°
    0.750, // Purples  270°
    0.875, // Magentas 315°
];

// Half-width of each triangular kernel in [0, 1] hue space.
const HALF_WIDTH: f32 = 0.125; // 1/8 = 45°

/// Triangular weight for band `i` at normalised hue `h` (circular).
#[inline]
fn band_weight(h: f32, centre: f32) -> f32 {
    let raw_d = (h - centre).abs();
    // Wrap: distance on a circle in [0, 1].
    let d = if raw_d > 0.5 { 1.0 - raw_d } else { raw_d };
    (1.0 - d / HALF_WIDTH).max(0.0)
}

#[typetag::serde]
impl Operation for HslPanelOp {
    fn name(&self) -> &'static str {
        "hsl_panel"
    }

    fn clone_box(&self) -> Box<dyn Operation> {
        Box::new(self.clone())
    }

    fn apply(&self, mut image: Image) -> RasterResult<Image> {
        if self.is_identity() {
            return Ok(image);
        }

        let hue = self.hue;
        let sat = self.saturation;
        let lum = self.luminance;

        image.data.par_chunks_mut(4).for_each(|p| {
            let r = p[0] as f32 / 255.0;
            let g = p[1] as f32 / 255.0;
            let b = p[2] as f32 / 255.0;

            let (h, s, l) = rgb_to_hsl(r, g, b);

            // Compute weighted deltas from each band.
            let mut dh = 0.0f32;
            let mut ds = 0.0f32;
            let mut dl = 0.0f32;
            let mut w_sum = 0.0f32;
            for i in 0..8 {
                let w = band_weight(h, BAND_CENTRES[i]);
                dh += w * hue[i];
                ds += w * sat[i];
                dl += w * lum[i];
                w_sum += w;
            }

            // If the pixel hue falls outside all bands (shouldn't happen with
            // triangular kernels, but guard against divide-by-zero).
            let (new_h, new_s, new_l) = if w_sum < 1e-6 {
                (h, s, l)
            } else {
                let new_h = (h + dh / (360.0 * w_sum)).rem_euclid(1.0);
                let new_s = (s + ds / w_sum).clamp(0.0, 1.0);
                let new_l = (l + dl / w_sum).clamp(0.0, 1.0);
                (new_h, new_s, new_l)
            };

            let (ro, go, bo) = hsl_to_rgb(new_h, new_s, new_l);
            p[0] = (ro * 255.0).clamp(0.0, 255.0) as u8;
            p[1] = (go * 255.0).clamp(0.0, 255.0) as u8;
            p[2] = (bo * 255.0).clamp(0.0, 255.0) as u8;
            // alpha unchanged
        });

        Ok(image)
    }

    fn describe(&self) -> String {
        "HSL Panel".into()
    }
}

// ---------------------------------------------------------------------------
// HSL ↔ RGB helpers (same as vibrance.rs / hue_shift.rs)
// ---------------------------------------------------------------------------

fn rgb_to_hsl(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) * 0.5;

    if (max - min).abs() < 1e-9 {
        return (0.0, 0.0, l);
    }

    let d = max - min;
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };

    let h = if (max - r).abs() < 1e-9 {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if (max - g).abs() < 1e-9 {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };

    (h / 6.0, s, l)
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (f32, f32, f32) {
    if s < 1e-9 {
        return (l, l, l);
    }
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    (
        hue_to_rgb(p, q, h + 1.0 / 3.0),
        hue_to_rgb(p, q, h),
        hue_to_rgb(p, q, h - 1.0 / 3.0),
    )
}

fn hue_to_rgb(p: f32, q: f32, mut t: f32) -> f32 {
    if t < 0.0 {
        t += 1.0;
    }
    if t > 1.0 {
        t -= 1.0;
    }
    if t < 1.0 / 6.0 {
        return p + (q - p) * 6.0 * t;
    }
    if t < 0.5 {
        return q;
    }
    if t < 2.0 / 3.0 {
        return p + (q - p) * (2.0 / 3.0 - t) * 6.0;
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(r: u8, g: u8, b: u8) -> Image {
        let mut img = Image::new(4, 4);
        img.data.chunks_mut(4).for_each(|p| {
            p[0] = r;
            p[1] = g;
            p[2] = b;
            p[3] = 255;
        });
        img
    }

    #[test]
    fn identity_unchanged() {
        let src = solid(180, 80, 40);
        let src_data = src.data.clone();
        let out = HslPanelOp::default().apply(src).unwrap();
        assert_eq!(out.data, src_data);
    }

    #[test]
    fn alpha_preserved() {
        let mut src = Image::new(4, 4);
        src.data.chunks_mut(4).for_each(|p| {
            p[0] = 200;
            p[1] = 50;
            p[2] = 50;
            p[3] = 77;
        });
        let mut sat = [0.0f32; 8];
        sat[0] = 0.5; // boost reds saturation
        let out = HslPanelOp::new([0.0; 8], sat, [0.0; 8]).apply(src).unwrap();
        out.data.chunks(4).for_each(|p| assert_eq!(p[3], 77));
    }

    #[test]
    fn red_hue_shift_affects_red_pixel() {
        // Pure red (hue ≈ 0°). Shift reds +90° → should become yellow-ish.
        let src = solid(230, 20, 20);
        let orig_g = src.data[1];
        let mut hue = [0.0f32; 8];
        hue[0] = 90.0;
        let op = HslPanelOp::new(hue, [0.0; 8], [0.0; 8]);
        let out = op.apply(src).unwrap();
        // After +90° shift, R channel should decrease and G should increase.
        assert!(
            out.data[1] > orig_g,
            "G should increase after +90° hue on reds"
        );
    }

    #[test]
    fn green_band_does_not_affect_red_pixel() {
        // Shift greens only — a pure red pixel should be barely affected.
        let src = solid(230, 20, 20);
        let mut hue = [0.0f32; 8];
        hue[3] = 90.0; // greens band
        let op = HslPanelOp::new(hue, [0.0; 8], [0.0; 8]);
        let out = op.apply(src).unwrap();
        // Red pixel: R should be almost unchanged (±5).
        assert!((out.data[0] as i16 - 230i16).abs() <= 5);
    }

    #[test]
    fn saturation_boost_increases_chroma() {
        let src = solid(160, 120, 100); // low-sat warm pixel (reds/oranges)
        let chroma_before = (src.data[0] as i16 - src.data[2] as i16).unsigned_abs();
        let mut sat = [0.0f32; 8];
        sat[0] = 0.8;
        sat[1] = 0.8;
        let op = HslPanelOp::new([0.0; 8], sat, [0.0; 8]);
        let out = op.apply(src).unwrap();
        let chroma_after = (out.data[0] as i16 - out.data[2] as i16).unsigned_abs();
        assert!(chroma_after > chroma_before, "chroma should increase");
    }

    #[test]
    fn luminance_shift_brightens() {
        let src = solid(100, 60, 60); // dark red
        let orig_r = src.data[0];
        let mut lum = [0.0f32; 8];
        lum[0] = 0.3;
        let op = HslPanelOp::new([0.0; 8], [0.0; 8], lum);
        let out = op.apply(src).unwrap();
        assert!(out.data[0] > orig_r, "R should brighten");
    }
}
