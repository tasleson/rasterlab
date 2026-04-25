use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{error::RasterResult, image::Image, traits::operation::Operation};

/// Apply an exposure-stops gain **only to the shadows** of an image.
///
/// Unlike [`HighlightsShadowsOp`](super::highlights_shadows::HighlightsShadowsOp),
/// which applies an additive luminance delta, this op applies a
/// multiplicative gain in **linear light** — i.e. it models what happens
/// when the sensor collects more (or fewer) photons in the dark regions,
/// the same way a camera's exposure-stop adjustment does.
///
/// The gain is masked by a luma-driven weight so highlights are left
/// untouched:
///
/// ```text
///   weight(L) = (1 - L)^falloff
///   gain(L)   = 2^(ev * weight(L))
///   rgb_out   = srgb_from_linear( srgb_to_linear(rgb_in) * gain(L) )
/// ```
///
/// * `ev`      — exposure adjustment in stops, range `[-3.0, 3.0]`.
///   Positive lifts the shadows, negative crushes them.
/// * `falloff` — controls how tightly the mask hugs the blacks.
///   Range `[0.5, 4.0]`.  Lower = wider effect that reaches into
///   midtones; higher = narrower effect that only touches deep shadows.
///
/// The same gain is applied to R, G and B so hue is preserved.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShadowExposureOp {
    pub ev: f32,
    pub falloff: f32,
}

impl ShadowExposureOp {
    pub fn new(ev: f32, falloff: f32) -> Self {
        Self {
            ev: ev.clamp(-3.0, 3.0),
            falloff: falloff.clamp(0.5, 4.0),
        }
    }
}

#[typetag::serde]
impl Operation for ShadowExposureOp {
    fn name(&self) -> &'static str {
        "shadow_exposure"
    }

    fn clone_box(&self) -> Box<dyn Operation> {
        Box::new(self.clone())
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn apply(&self, mut image: Image) -> RasterResult<Image> {
        if self.ev.abs() < 1e-5 {
            return Ok(image);
        }

        let ev = self.ev;
        let falloff = self.falloff;

        image.data.par_chunks_mut(4).for_each(|p| {
            let r = p[0] as f32 / 255.0;
            let g = p[1] as f32 / 255.0;
            let b = p[2] as f32 / 255.0;

            // Luma drives the mask in perceptual (sRGB-encoded) space so the
            // slider feels linear to the eye.
            let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
            let weight = (1.0 - luma).clamp(0.0, 1.0).powf(falloff);
            let gain = (ev * weight).exp2();

            // Apply the gain in linear light so it behaves like real exposure.
            let rl = super::srgb_to_linear(r) * gain;
            let gl = super::srgb_to_linear(g) * gain;
            let bl = super::srgb_to_linear(b) * gain;

            p[0] = (super::linear_to_srgb(rl.clamp(0.0, 1.0)) * 255.0).round() as u8;
            p[1] = (super::linear_to_srgb(gl.clamp(0.0, 1.0)) * 255.0).round() as u8;
            p[2] = (super::linear_to_srgb(bl.clamp(0.0, 1.0)) * 255.0).round() as u8;
            // alpha unchanged
        });

        Ok(image)
    }

    fn describe(&self) -> String {
        format!(
            "Shadow Exposure  ev={:+.2}  falloff={:.2}",
            self.ev, self.falloff
        )
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
    fn identity_when_ev_zero() {
        let src = grey(100);
        let src_data = src.data.clone();
        let out = ShadowExposureOp::new(0.0, 2.0).apply(src).unwrap();
        assert_eq!(out.data, src_data);
    }

    #[test]
    fn positive_ev_lifts_shadows() {
        let src = grey(25);
        let orig = src.data[0];
        let out = ShadowExposureOp::new(2.0, 2.0).apply(src).unwrap();
        assert!(
            out.data[0] > orig,
            "dark pixel should brighten, got {} → {}",
            orig,
            out.data[0]
        );
    }

    #[test]
    fn negative_ev_crushes_shadows() {
        let src = grey(40);
        let orig = src.data[0];
        let out = ShadowExposureOp::new(-2.0, 2.0).apply(src).unwrap();
        assert!(
            out.data[0] < orig,
            "dark pixel should darken, got {} → {}",
            orig,
            out.data[0]
        );
    }

    #[test]
    fn highlights_left_alone() {
        // Near-white should barely move because weight ≈ 0 there.
        let src = grey(245);
        let orig = src.data[0];
        let out = ShadowExposureOp::new(3.0, 2.0).apply(src).unwrap();
        let diff = (out.data[0] as i16 - orig as i16).unsigned_abs();
        assert!(
            diff <= 2,
            "highlight moved too much: {} → {}",
            orig,
            out.data[0]
        );
    }

    #[test]
    fn falloff_tightens_the_mask() {
        // With a larger falloff, midtones should be less affected.
        let src_low = grey(80);
        let src_high = grey(80);
        let out_wide = ShadowExposureOp::new(2.0, 1.0).apply(src_low).unwrap();
        let out_narrow = ShadowExposureOp::new(2.0, 4.0).apply(src_high).unwrap();
        assert!(
            out_wide.data[0] > out_narrow.data[0],
            "wide falloff should lift midtones more than narrow falloff"
        );
    }

    #[test]
    fn alpha_preserved() {
        let mut src = Image::new(4, 4);
        src.data.chunks_mut(4).for_each(|p| {
            p[0] = 30;
            p[1] = 30;
            p[2] = 30;
            p[3] = 77;
        });
        let out = ShadowExposureOp::new(1.5, 2.0).apply(src).unwrap();
        out.data.chunks(4).for_each(|p| assert_eq!(p[3], 77));
    }

    #[test]
    fn hue_preserved() {
        // Same gain applied to all channels, so the R:G ratio should be stable
        // (within u8 rounding).
        let mut src = Image::new(1, 1);
        src.data[0] = 60;
        src.data[1] = 30;
        src.data[2] = 20;
        src.data[3] = 255;
        let rg_before = src.data[0] as f32 / src.data[1] as f32;
        let out = ShadowExposureOp::new(1.5, 2.0).apply(src).unwrap();
        let rg_after = out.data[0] as f32 / out.data[1] as f32;
        assert!(
            (rg_after / rg_before - 1.0).abs() < 0.1,
            "R:G ratio shifted: {:.2} → {:.2}",
            rg_before,
            rg_after
        );
    }
}
