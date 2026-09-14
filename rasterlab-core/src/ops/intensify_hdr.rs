use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{error::RasterResult, image::Image, traits::operation::Operation};

use super::{CurvesOp, box_blur_1ch, luma_f32};

/// An Intensify-inspired, single-image HDR look.
///
/// # What the reference captures say the effect is
///
/// The legacy plugin's exports in `test_images/plugin_reference_kit` pin the
/// effect down as three separable pieces, measured as follows.
///
/// **A fixed, strongly compressive tone curve applied to a blurred base.**  The
/// neutral wedge is a linear ramp, so a symmetric blur leaves it untouched and
/// its 100 % export reads the curve off directly: deep shadows are thrown open
/// (input 20 → 68), the midtones are flattened to a slope near 0.28 (input
/// 24–136 → 66–105), and the highlights steepen again to about 1.25.  That same
/// curve independently predicts the band means of `local_contrast.png` to
/// within three levels — dark band 29.5 → 62.5, mid 117.5 → 99.4, bright
/// 204.5 → 172.3 — so it is a property of the effect, not of the wedge.
///
/// **A large-radius unsharp mask over it, which deliberately halos.**  In
/// `context_dark.png` a flat grey field of 128 next to a black surround exports
/// at 155 hard against the border and settles to 118 about 130 px away; with a
/// white surround the same pixels read 88 at the border rising to 148.  Both
/// sides of an edge are pushed away from each other, over a distance of roughly
/// 3 % of the short side.  This is the opposite of what [`LocalLaplacianOp`]
/// does: that filter separates detail from structure *by amplitude* precisely
/// so it will not halo, and it therefore cannot produce this look.
///
/// **A plain saturation multiply of about 1.8, with no vivid-colour
/// protection.**  Patch saturation in `color_patches.png` goes 0.28 → 0.46,
/// 0.53 → 0.75, 0.69 → 0.92, and everything past 0.79 clips a channel flat.
/// [`VibranceOp`](super::VibranceOp) is the wrong instrument for that: it
/// deliberately spares colours that are already vivid, which is where most of
/// this effect's movement is.
///
/// # What this does not reproduce
///
/// The plugin also has a global term that reacts to the frame as a whole.  The
/// two context images share an identical centre region, and far enough from the
/// border for the unsharp mask to have died away completely their grey fields
/// still differ by 30 levels (118 against 148), neither of which is the 103 the
/// tone curve alone predicts.  A single extra capture cannot separate the rule
/// behind that from the ones above; the kit's README asks for the exports that
/// would.  Those two synthetic probes are where nearly all of the residual
/// error against the reference sits.
///
/// `amount` blends the finished look back over the untouched source in encoded
/// sRGB, which is what the supplied 50 % exports are closest to.
///
/// [`LocalLaplacianOp`]: super::LocalLaplacianOp
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntensifyHdrOp {
    /// Overall effect amount. `0.0` leaves every source byte unchanged;
    /// `1.0` applies the complete Intensify-inspired look.
    pub amount: f32,
}

/// Tone curve for the blurred base, as control points in `[0, 1]`.
///
/// Fitted to the 256 neutral levels of `outputs/luminance_wedge_100.png`; the
/// monotone spline through these reproduces that measurement to a mean of 1.0
/// levels out of 255.  The clustering below 0.13 is not decoration — the
/// measured curve rises from 0 to 68 over sixteen input levels, and fewer
/// points there round the corner off and close the shadows back up.
const TONE_POINTS: [[f32; 2]; 14] = [
    [0.000, 0.000],
    [0.016, 0.000],
    [0.031, 0.082],
    [0.063, 0.216],
    [0.078, 0.267],
    [0.125, 0.282],
    [0.251, 0.352],
    [0.376, 0.384],
    [0.502, 0.404],
    [0.627, 0.486],
    [0.753, 0.627],
    [0.878, 0.780],
    [0.941, 0.875],
    [1.000, 1.000],
];

/// Blur radius for the base layer, as a fraction of the short side.
///
/// Set by the halo width in the context exports: the overshoot there decays
/// over about 130 px on a 1024 px short side.  Because the radius is relative,
/// the look survives resizing — the same scene at half resolution halos over
/// half as many pixels, which is what the plugin's own resolution probe
/// (`local_contrast_2x.png`) exists to confirm once it is captured.
const BASE_RADIUS_FRAC: f32 = 0.03;

/// Gain on everything the base layer does not contain.
///
/// The tone curve flattens the midtones to a slope near 0.28; this is what puts
/// the texture back on top of it, and it is the whole reason the look reads as
/// "HDR" rather than as a washed-out frame.
const DETAIL_GAIN: f32 = 1.77;

/// Chroma multiplier about the retoned luminance.
const SATURATION: f32 = 1.78;

/// Luminance below which the tonal move is applied as an offset rather than a
/// ratio.  A ratio is what preserves hue, but near black the denominator stops
/// being meaningful and a one-level input difference would swing the output.
const RATIO_FLOOR: f32 = 1.0;

impl IntensifyHdrOp {
    pub fn new(amount: f32) -> Self {
        Self {
            amount: amount.clamp(0.0, 1.0),
        }
    }

    /// The full-strength look, before `amount` blends it back over the source.
    fn render(&self, image: &Image) -> Image {
        let (w, h) = (image.width as usize, image.height as usize);
        let luma = luma_f32(image);
        let radius = ((BASE_RADIUS_FRAC * w.min(h) as f32).round() as usize).max(1);
        let base = box_blur_1ch(&luma, w, h, radius);

        let tone = CurvesOp::build_lut(&TONE_POINTS);
        let tone_at = |v: f32| -> f32 {
            let v = v.clamp(0.0, 255.0);
            let i = v.floor() as usize;
            if i >= 255 {
                return tone[255] as f32;
            }
            let f = v - i as f32;
            tone[i] as f32 * (1.0 - f) + tone[i + 1] as f32 * f
        };

        let mut out = image.deep_clone();
        let stride = out.row_stride();
        out.data
            .par_chunks_mut(stride)
            .enumerate()
            .for_each(|(y, row)| {
                let start = y * w;
                for (x, pixel) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                    let i = start + x;
                    let l0 = luma[i];
                    let l1 = (tone_at(base[i]) + DETAIL_GAIN * (l0 - base[i])).clamp(0.0, 255.0);

                    let mut rgb = [0f32; 3];
                    if l0 > RATIO_FLOOR {
                        let gain = l1 / l0;
                        for c in 0..3 {
                            rgb[c] = pixel[c] as f32 * gain;
                        }
                    } else {
                        let offset = l1 - l0;
                        for c in 0..3 {
                            rgb[c] = pixel[c] as f32 + offset;
                        }
                    }

                    let grey = 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];
                    for c in 0..3 {
                        pixel[c] = (grey + (rgb[c] - grey) * SATURATION)
                            .round()
                            .clamp(0.0, 255.0) as u8;
                    }
                }
            });
        out
    }
}

#[typetag::serde]
impl Operation for IntensifyHdrOp {
    fn name(&self) -> &'static str {
        "intensify_hdr"
    }

    fn clone_box(&self) -> Box<dyn Operation> {
        Box::new(self.clone())
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn apply(&self, image: Image) -> RasterResult<Image> {
        if self.amount <= f32::EPSILON {
            return Ok(image);
        }

        let mut rendered = self.render(&image);
        if self.amount >= 1.0 {
            return Ok(rendered);
        }

        let amount = self.amount;
        let stride = rendered.row_stride();
        rendered
            .data
            .par_chunks_mut(stride)
            .zip(image.data.par_chunks(stride))
            .for_each(|(out_row, src_row)| {
                for (out, src) in out_row
                    .as_chunks_mut::<4>()
                    .0
                    .iter_mut()
                    .zip(src_row.as_chunks::<4>().0)
                {
                    for c in 0..3 {
                        out[c] = (src[c] as f32 + (out[c] as f32 - src[c] as f32) * amount)
                            .round()
                            .clamp(0.0, 255.0) as u8;
                    }
                    out[3] = src[3];
                }
            });
        Ok(rendered)
    }

    fn describe(&self) -> String {
        format!("Intensify HDR  {:.0}%", self.amount * 100.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scene() -> Image {
        let mut image = Image::new(96, 64);
        for y in 0..image.height {
            for x in 0..image.width {
                let base = if x < 48 { 35 } else { 210 };
                let detail = ((x.wrapping_mul(17) ^ y.wrapping_mul(29)) % 34) as i32 - 17;
                let v = (base + detail).clamp(0, 255) as u8;
                let offset = image.pixel_offset(x, y);
                image.data[offset..offset + 4].copy_from_slice(&[v, v / 2, 255 - v, 73]);
            }
        }
        image
    }

    fn flat(v: u8, w: u32, h: u32) -> Image {
        let mut image = Image::new(w, h);
        image
            .data
            .chunks_mut(4)
            .for_each(|p| p.copy_from_slice(&[v, v, v, 255]));
        image
    }

    #[test]
    fn zero_amount_is_byte_exact_identity() {
        let source = scene();
        let expected = source.data.clone();
        let output = IntensifyHdrOp::new(0.0).apply(source).unwrap();
        assert_eq!(output.data, expected);
    }

    #[test]
    fn constructor_clamps_amount() {
        assert_eq!(IntensifyHdrOp::new(-0.1).amount, 0.0);
        assert_eq!(IntensifyHdrOp::new(1.1).amount, 1.0);
    }

    #[test]
    fn half_amount_is_the_midpoint_of_source_and_full_result() {
        let source = scene();
        let full = IntensifyHdrOp::new(1.0).apply(source.deep_clone()).unwrap();
        let half = IntensifyHdrOp::new(0.5).apply(source.deep_clone()).unwrap();
        for ((input, full), half) in source
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .zip(full.data.as_chunks::<4>().0)
            .zip(half.data.as_chunks::<4>().0)
        {
            for channel in 0..3 {
                let expected = ((input[channel] as f32 + full[channel] as f32) * 0.5).round() as u8;
                assert_eq!(half[channel], expected);
            }
            assert_eq!(half[3], input[3]);
        }
    }

    #[test]
    fn alpha_survives_the_full_effect() {
        let source = scene();
        let output = IntensifyHdrOp::new(1.0).apply(source.deep_clone()).unwrap();
        assert_ne!(output.data, source.data);
        for (input, output) in source
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .zip(output.data.as_chunks::<4>().0)
        {
            assert_eq!(output[3], input[3]);
        }
    }

    /// A flat field carries no detail, so it lands wherever the measured tone
    /// curve puts it.  These are the wedge's own numbers, and they are the
    /// reason the effect reads as HDR: the shadow opens by 48 levels and the
    /// highlight comes down by 17.
    #[test]
    fn flat_fields_follow_the_measured_tone_curve() {
        for (input, expected) in [(20u8, 68u8), (128, 103), (240, 223)] {
            let output = IntensifyHdrOp::new(1.0).apply(flat(input, 64, 64)).unwrap();
            let got = output.pixel(32, 32)[0];
            assert!(
                got.abs_diff(expected) <= 3,
                "grey {input} → {got}, expected about {expected}"
            );
        }
    }

    /// The look halos on purpose: both sides of a hard edge are pushed away
    /// from each other, which is what separates it from `LocalLaplacianOp`.
    #[test]
    fn an_edge_overshoots_on_both_sides() {
        let mut source = Image::new(256, 256);
        for y in 0..256 {
            for x in 0..256 {
                let v = if x < 128 { 32u8 } else { 200 };
                source.set_pixel(x, y, [v, v, v, 255]);
            }
        }
        let out = IntensifyHdrOp::new(1.0).apply(source).unwrap();
        let dark_edge = out.pixel(127, 128)[0];
        let dark_far = out.pixel(8, 128)[0];
        let bright_edge = out.pixel(128, 128)[0];
        let bright_far = out.pixel(247, 128)[0];
        assert!(
            dark_edge < dark_far,
            "dark side should undershoot at the edge: {dark_edge} vs {dark_far}"
        );
        assert!(
            bright_edge > bright_far,
            "bright side should overshoot at the edge: {bright_edge} vs {bright_far}"
        );
    }

    /// Saturation is a plain multiply, so an already-vivid colour keeps moving
    /// rather than being protected the way vibrance protects it.
    #[test]
    fn vivid_colours_keep_saturating() {
        let mut source = Image::new(64, 64);
        source
            .data
            .chunks_mut(4)
            .for_each(|p| p.copy_from_slice(&[190, 96, 72, 255]));
        let out = IntensifyHdrOp::new(1.0).apply(source).unwrap();
        let p = out.pixel(32, 32);
        let sat = |c: [u8; 4]| {
            let hi = c[0].max(c[1]).max(c[2]) as f32;
            let lo = c[0].min(c[1]).min(c[2]) as f32;
            if hi == 0.0 { 0.0 } else { (hi - lo) / hi }
        };
        assert!(
            sat(p) > sat([190, 96, 72, 255]) + 0.1,
            "saturation should climb well past the source: {:?}",
            p
        );
    }
}
