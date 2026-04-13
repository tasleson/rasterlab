use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{error::RasterResult, image::Image, traits::operation::Operation};

/// Adjust colour balance independently in shadows, midtones and highlights.
///
/// Three tonal zones (shadows / midtones / highlights) each expose three
/// axes:
///
/// * **Cyan ↔ Red**     — negative = cyan tint, positive = red/warm tint
/// * **Magenta ↔ Green** — negative = magenta tint, positive = green tint
/// * **Yellow ↔ Blue**  — negative = yellow tint, positive = blue/cool tint
///
/// Zone weights are luma-based and smooth:
/// * shadow weight    = `(1 − luma)²`  — peaks at black, zero at white
/// * midtone weight   = `4 · luma · (1 − luma)` — peaks at mid-grey, zero at extremes
/// * highlight weight = `luma²`        — peaks at white, zero at black
///
/// Each parameter is in `[-1.0, 1.0]`; the maximum additive delta per zone
/// is ±40 % of the full [0, 255] range.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColorBalanceOp {
    /// `[shadows, midtones, highlights]` on the cyan(−1) ↔ red(+1) axis.
    pub cyan_red: [f32; 3],
    /// `[shadows, midtones, highlights]` on the magenta(−1) ↔ green(+1) axis.
    pub magenta_green: [f32; 3],
    /// `[shadows, midtones, highlights]` on the yellow(−1) ↔ blue(+1) axis.
    pub yellow_blue: [f32; 3],
}

impl ColorBalanceOp {
    pub fn new(cyan_red: [f32; 3], magenta_green: [f32; 3], yellow_blue: [f32; 3]) -> Self {
        let clamp3 = |a: [f32; 3]| {
            [
                a[0].clamp(-1.0, 1.0),
                a[1].clamp(-1.0, 1.0),
                a[2].clamp(-1.0, 1.0),
            ]
        };
        Self {
            cyan_red: clamp3(cyan_red),
            magenta_green: clamp3(magenta_green),
            yellow_blue: clamp3(yellow_blue),
        }
    }

    /// Returns `true` if every parameter is at neutral (zero).
    pub fn is_identity(&self) -> bool {
        let all_zero = |a: &[f32; 3]| a.iter().all(|v| v.abs() < 1e-5);
        all_zero(&self.cyan_red) && all_zero(&self.magenta_green) && all_zero(&self.yellow_blue)
    }
}

impl Default for ColorBalanceOp {
    fn default() -> Self {
        Self {
            cyan_red: [0.0; 3],
            magenta_green: [0.0; 3],
            yellow_blue: [0.0; 3],
        }
    }
}

#[typetag::serde]
impl Operation for ColorBalanceOp {
    fn name(&self) -> &'static str {
        "color_balance"
    }

    fn clone_box(&self) -> Box<dyn Operation> {
        Box::new(self.clone())
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn apply(&self, mut image: Image) -> RasterResult<Image> {
        if self.is_identity() {
            return Ok(image);
        }

        let cr = self.cyan_red;
        let mg = self.magenta_green;
        let yb = self.yellow_blue;

        // Maximum additive delta in [0,1] per zone at slider = ±1.
        const SCALE: f32 = 0.4;

        image.data.par_chunks_mut(4).for_each(|p| {
            let r = p[0] as f32 / 255.0;
            let g = p[1] as f32 / 255.0;
            let b = p[2] as f32 / 255.0;

            // BT.709 luminance drives the zone weights.
            let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;

            // Zone weights — sum > 1 is intentional (zones overlap slightly
            // to avoid abrupt transitions; combined effect is bounded by SCALE).
            let sh = (1.0 - luma).powi(2); // shadow
            let mt = 4.0 * luma * (1.0 - luma); // midtone
            let hl = luma.powi(2); // highlight

            let dr = (cr[0] * sh + cr[1] * mt + cr[2] * hl) * SCALE;
            let dg = (mg[0] * sh + mg[1] * mt + mg[2] * hl) * SCALE;
            let db = (yb[0] * sh + yb[1] * mt + yb[2] * hl) * SCALE;

            p[0] = ((r + dr) * 255.0).clamp(0.0, 255.0) as u8;
            p[1] = ((g + dg) * 255.0).clamp(0.0, 255.0) as u8;
            p[2] = ((b + db) * 255.0).clamp(0.0, 255.0) as u8;
            // alpha unchanged
        });

        Ok(image)
    }

    fn describe(&self) -> String {
        "Color Balance".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grey(v: u8) -> Image {
        let mut img = Image::new(4, 4);
        img.data.chunks_mut(4).for_each(|p| {
            p[0] = v;
            p[1] = v;
            p[2] = v;
            p[3] = 255;
        });
        img
    }

    #[test]
    fn identity() {
        let src = grey(128);
        let src_data = src.data.clone();
        let out = ColorBalanceOp::default().apply(src).unwrap();
        assert_eq!(out.data, src_data);
    }

    #[test]
    fn red_shift_in_shadows_brightens_dark_red() {
        // Pushing shadows toward red should increase R on a dark pixel.
        let src = grey(20);
        let orig = [src.data[0], src.data[1], src.data[2]];
        let op = ColorBalanceOp::new([1.0, 0.0, 0.0], [0.0; 3], [0.0; 3]);
        let out = op.apply(src).unwrap();
        assert!(out.data[0] > orig[0], "R should increase in shadows");
        // G and B should be less affected (cyan-red axis only touches R).
        assert_eq!(out.data[1], orig[1], "G should be unchanged");
        assert_eq!(out.data[2], orig[2], "B should be unchanged");
    }

    #[test]
    fn shadow_control_has_no_effect_on_highlights() {
        // Full shadow-red boost should leave a near-white pixel unchanged.
        let src = grey(250);
        let op = ColorBalanceOp::new([1.0, 0.0, 0.0], [0.0; 3], [0.0; 3]);
        let out = op.apply(src).unwrap();
        assert!((out.data[0] as i16 - 250i16).abs() <= 2);
    }

    #[test]
    fn highlight_control_has_no_effect_on_shadows() {
        let src = grey(5);
        let op = ColorBalanceOp::new([0.0, 0.0, 1.0], [0.0; 3], [0.0; 3]);
        let out = op.apply(src).unwrap();
        assert!((out.data[0] as i16 - 5i16).abs() <= 2);
    }

    #[test]
    fn midtone_control_peaks_at_mid_grey() {
        // Midtone boost should affect 128 more than 20 or 240.
        let op = ColorBalanceOp::new([0.0; 3], [0.0; 3], [0.0, 1.0, 0.0]);

        let apply_delta = |v: u8| {
            let src = grey(v);
            let orig = src.data[2];
            let out = op.apply(src).unwrap();
            (out.data[2] as i16 - orig as i16).abs()
        };

        let d_dark = apply_delta(20);
        let d_mid = apply_delta(128);
        let d_bright = apply_delta(240);

        assert!(
            d_mid > d_dark,
            "midtone delta {} should exceed shadow delta {}",
            d_mid,
            d_dark
        );
        assert!(
            d_mid > d_bright,
            "midtone delta {} should exceed highlight delta {}",
            d_mid,
            d_bright
        );
    }

    #[test]
    fn alpha_preserved() {
        let mut src = Image::new(4, 4);
        src.data.chunks_mut(4).for_each(|p| {
            p[0] = 100;
            p[1] = 100;
            p[2] = 100;
            p[3] = 33;
        });
        let op = ColorBalanceOp::new([0.5, 0.0, 0.0], [0.0; 3], [0.0; 3]);
        let out = op.apply(src).unwrap();
        out.data.chunks(4).for_each(|p| assert_eq!(p[3], 33));
    }
}
