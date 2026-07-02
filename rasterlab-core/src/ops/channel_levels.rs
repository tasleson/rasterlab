use serde::{Deserialize, Serialize};

use crate::{error::RasterResult, image::Image, traits::operation::Operation};

use super::for_each_pixel_row_parallel;

/// Levels curve for a single channel.
///
/// Same semantics as [`super::LevelsOp`]: values are normalised into
/// `[black, white]` → `[0, 1]`, gamma-corrected via `v ^ (1 / gamma)`, then
/// scaled back to `0–255`.  `black` and `white` are fractions of 255;
/// `gamma > 1.0` brightens midtones, `< 1.0` darkens them.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ChannelRange {
    pub black: f32,
    pub white: f32,
    pub gamma: f32,
}

impl ChannelRange {
    pub fn new(black: f32, white: f32, gamma: f32) -> Self {
        Self {
            black: black.clamp(0.0, 1.0),
            white: white.clamp(0.0, 1.0),
            gamma: gamma.clamp(0.01, 10.0),
        }
    }

    /// Identity mapping — leaves the channel untouched.
    pub fn identity() -> Self {
        Self {
            black: 0.0,
            white: 1.0,
            gamma: 1.0,
        }
    }

    pub fn is_identity(&self) -> bool {
        self.black.abs() < 1e-4
            && (self.white - 1.0).abs() < 1e-4
            && (self.gamma - 1.0).abs() < 1e-3
    }

    /// Build the 256-entry lookup table for this channel's curve.
    pub fn build_lut(&self) -> [u8; 256] {
        let gamma = 1.0 / self.gamma.max(0.01);
        let range = (self.white - self.black).abs().max(1.0 / 255.0);

        let mut lut = [0u8; 256];
        for (i, entry) in lut.iter_mut().enumerate() {
            let v = i as f32 / 255.0;
            let normalized = ((v - self.black) / range).clamp(0.0, 1.0);
            let corrected = normalized.powf(gamma);
            *entry = (corrected * 255.0).round().clamp(0.0, 255.0) as u8;
        }
        lut
    }
}

/// Independent levels adjustment per RGB channel.
///
/// Unlike [`super::LevelsOp`], which applies one curve to all three channels,
/// this op remaps each channel with its own black point, white point, and
/// gamma.  Stretching each channel to fill its own range neutralises colour
/// casts (the same principle as Photoshop's "Auto Color"), which makes this
/// the workhorse of Smart Enhance's cast removal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelLevelsOp {
    pub red: ChannelRange,
    pub green: ChannelRange,
    pub blue: ChannelRange,
}

impl ChannelLevelsOp {
    pub fn new(red: ChannelRange, green: ChannelRange, blue: ChannelRange) -> Self {
        Self { red, green, blue }
    }

    pub fn is_identity(&self) -> bool {
        self.red.is_identity() && self.green.is_identity() && self.blue.is_identity()
    }
}

#[typetag::serde]
impl Operation for ChannelLevelsOp {
    fn name(&self) -> &'static str {
        "channel_levels"
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

        let r_lut = self.red.build_lut();
        let g_lut = self.green.build_lut();
        let b_lut = self.blue.build_lut();

        for_each_pixel_row_parallel(&mut image, |p| {
            p[0] = r_lut[p[0] as usize];
            p[1] = g_lut[p[1] as usize];
            p[2] = b_lut[p[2] as usize];
            // alpha unchanged
        });

        Ok(image)
    }

    fn describe(&self) -> String {
        let fmt = |c: &ChannelRange| format!("{:.2}/{:.2}/{:.2}", c.black, c.gamma, c.white);
        format!(
            "Channel Levels  R {}  G {}  B {}",
            fmt(&self.red),
            fmt(&self.green),
            fmt(&self.blue)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::test_utils::solid;

    #[test]
    fn identity_leaves_pixels_unchanged() {
        let src = solid(30, 120, 200);
        let op = ChannelLevelsOp::new(
            ChannelRange::identity(),
            ChannelRange::identity(),
            ChannelRange::identity(),
        );
        assert!(op.is_identity());
        let out = op.apply(src.deep_clone()).unwrap();
        assert_eq!(out.data, src.data);
    }

    #[test]
    fn channels_are_independent() {
        // Stretch only the red channel; green and blue must be untouched.
        let src = solid(100, 100, 100);
        let op = ChannelLevelsOp::new(
            ChannelRange::new(50.0 / 255.0, 150.0 / 255.0, 1.0),
            ChannelRange::identity(),
            ChannelRange::identity(),
        );
        let out = op.apply(src).unwrap();
        let p = &out.data[..4];
        // 100 is halfway between black=50 and white=150 → ~128.
        assert!((p[0] as i32 - 128).abs() <= 1, "R stretched, got {}", p[0]);
        assert_eq!(p[1], 100, "G unchanged");
        assert_eq!(p[2], 100, "B unchanged");
    }

    #[test]
    fn gamma_brightens_midtones() {
        let src = solid(128, 128, 128);
        let op = ChannelLevelsOp::new(
            ChannelRange::new(0.0, 1.0, 1.5),
            ChannelRange::identity(),
            ChannelRange::identity(),
        );
        let out = op.apply(src).unwrap();
        assert!(out.data[0] > 128, "gamma > 1 should brighten");
        assert_eq!(out.data[1], 128);
    }

    #[test]
    fn matches_levels_op_when_channels_equal() {
        // With identical ranges on all channels the result must match LevelsOp.
        use crate::ops::LevelsOp;
        let range = ChannelRange::new(0.1, 0.9, 1.2);
        let ch_op = ChannelLevelsOp::new(range, range, range);
        let lv_op = LevelsOp::new(0.1, 0.9, 1.2);

        let mut src = Image::new(16, 1);
        for (i, p) in src.data.chunks_mut(4).enumerate() {
            let v = (i * 16) as u8;
            p[0] = v;
            p[1] = v;
            p[2] = v;
            p[3] = 255;
        }
        let a = ch_op.apply(src.deep_clone()).unwrap();
        let b = lv_op.apply(src).unwrap();
        assert_eq!(a.data, b.data);
    }

    #[test]
    fn alpha_preserved() {
        let mut src = Image::new(4, 4);
        src.data.chunks_mut(4).for_each(|p| {
            p[0] = 60;
            p[1] = 60;
            p[2] = 60;
            p[3] = 42;
        });
        let op = ChannelLevelsOp::new(
            ChannelRange::new(0.1, 0.8, 1.3),
            ChannelRange::new(0.05, 0.9, 0.9),
            ChannelRange::new(0.0, 0.7, 1.1),
        );
        let out = op.apply(src).unwrap();
        out.data.chunks(4).for_each(|p| assert_eq!(p[3], 42));
    }
}
