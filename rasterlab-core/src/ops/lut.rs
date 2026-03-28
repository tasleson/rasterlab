use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{error::RasterResult, image::Image, traits::operation::Operation};

/// 3D LUT color grading operation.
///
/// Applies a CUBE-format 3-dimensional lookup table to re-map the RGB values
/// of every pixel.  The LUT is stored as a flat `Vec<f32>` in R-fastest order
/// matching the `.cube` file spec.  Size must be a cube root (e.g. 17, 33, 65).
///
/// # `.cube` format
/// Lines beginning with `#` or keywords like `TITLE`, `LUT_3D_SIZE`,
/// `DOMAIN_MIN/MAX` are parsed; the remaining float-triplet lines are the table
/// entries in order `R(0,0,0) … R(N-1,0,0)  R(0,1,0) … R(N-1,N-1,N-1)`.
///
/// # Usage
/// Create via [`LutOp::from_cube_str`] which parses a `.cube` file string.
/// The `strength` parameter linearly blends between the original colour (0.0)
/// and the full LUT output (1.0).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LutOp {
    /// Number of entries per axis.  Must satisfy `data.len() == size^3 * 3`.
    pub size: u32,
    /// Flat RGB float data: `[r0,g0,b0, r1,g1,b1, …]` in R-fastest order.
    pub data: Vec<f32>,
    /// Blend: `0.0` = identity, `1.0` = full LUT.  Range `[0.0, 1.0]`.
    pub strength: f32,
}

impl LutOp {
    /// Parse a `.cube` file string and return a ready-to-use `LutOp`.
    ///
    /// Returns `Err` if the file is malformed or the size is unsupported
    /// (must be in `[2, 65]`).
    pub fn from_cube_str(src: &str, strength: f32) -> Result<Self, String> {
        let mut size: Option<u32> = None;
        let mut data: Vec<f32> = Vec::new();

        for line in src.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line.starts_with("LUT_3D_SIZE") {
                let n: u32 = line
                    .split_whitespace()
                    .nth(1)
                    .and_then(|s| s.parse().ok())
                    .ok_or("LUT_3D_SIZE parse error")?;
                if !(2..=65).contains(&n) {
                    return Err(format!("LUT_3D_SIZE {} out of supported range [2, 65]", n));
                }
                size = Some(n);
                continue;
            }
            // Skip other keywords.
            if line.starts_with(|c: char| c.is_alphabetic()) {
                continue;
            }
            // Try to parse as a float triplet.
            let parts: Vec<f32> = line
                .split_whitespace()
                .filter_map(|s| s.parse().ok())
                .collect();
            if parts.len() == 3 {
                data.extend_from_slice(&parts);
            }
        }

        let n = size.ok_or("LUT_3D_SIZE keyword not found")?;
        let expected = (n as usize).pow(3) * 3;
        if data.len() != expected {
            return Err(format!(
                "Expected {} floats for {}^3 LUT, got {}",
                expected,
                n,
                data.len()
            ));
        }

        Ok(Self {
            size: n,
            data,
            strength: strength.clamp(0.0, 1.0),
        })
    }

    /// Build an identity LUT of the given size.  Useful as a starting point
    /// or for testing.
    pub fn identity(size: u32) -> Self {
        let n = size as usize;
        let mut data = Vec::with_capacity(n * n * n * 3);
        for b in 0..n {
            for g in 0..n {
                for r in 0..n {
                    data.push(r as f32 / (n as f32 - 1.0));
                    data.push(g as f32 / (n as f32 - 1.0));
                    data.push(b as f32 / (n as f32 - 1.0));
                }
            }
        }
        Self {
            size,
            data,
            strength: 1.0,
        }
    }
}

/// Trilinear interpolation into the 3D LUT.
///
/// `r`, `g`, `b` are in `[0, 1]`; returns interpolated `(r', g', b')`.
#[inline]
fn lut_sample(data: &[f32], size: usize, r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let sz = size as f32 - 1.0;
    let ri = (r * sz).clamp(0.0, sz);
    let gi = (g * sz).clamp(0.0, sz);
    let bi = (b * sz).clamp(0.0, sz);

    let r0 = ri.floor() as usize;
    let g0 = gi.floor() as usize;
    let b0 = bi.floor() as usize;
    let r1 = (r0 + 1).min(size - 1);
    let g1 = (g0 + 1).min(size - 1);
    let b1 = (b0 + 1).min(size - 1);

    let tr = ri - ri.floor();
    let tg = gi - gi.floor();
    let tb = bi - bi.floor();

    let idx = |ri: usize, gi: usize, bi: usize| (bi * size * size + gi * size + ri) * 3;

    // Trilinear interpolation (8 corners).
    let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;

    let mut out = [0.0f32; 3];
    for c in 0..3 {
        let c000 = data[idx(r0, g0, b0) + c];
        let c100 = data[idx(r1, g0, b0) + c];
        let c010 = data[idx(r0, g1, b0) + c];
        let c110 = data[idx(r1, g1, b0) + c];
        let c001 = data[idx(r0, g0, b1) + c];
        let c101 = data[idx(r1, g0, b1) + c];
        let c011 = data[idx(r0, g1, b1) + c];
        let c111 = data[idx(r1, g1, b1) + c];

        let c00 = lerp(c000, c100, tr);
        let c10 = lerp(c010, c110, tr);
        let c01 = lerp(c001, c101, tr);
        let c11 = lerp(c011, c111, tr);
        let c0 = lerp(c00, c10, tg);
        let c1 = lerp(c01, c11, tg);
        out[c] = lerp(c0, c1, tb);
    }
    (out[0], out[1], out[2])
}

#[typetag::serde]
impl Operation for LutOp {
    fn name(&self) -> &'static str {
        "lut"
    }

    fn apply(&self, mut image: Image) -> RasterResult<Image> {
        if self.data.is_empty() {
            return Ok(image);
        }

        let size = self.size as usize;
        let strength = self.strength;
        let data = &self.data;

        image.data.par_chunks_mut(4).for_each(|p| {
            let r = p[0] as f32 / 255.0;
            let g = p[1] as f32 / 255.0;
            let b = p[2] as f32 / 255.0;

            let (lr, lg, lb) = lut_sample(data, size, r, g, b);

            // Blend with original.
            p[0] = ((r + (lr - r) * strength) * 255.0).clamp(0.0, 255.0) as u8;
            p[1] = ((g + (lg - g) * strength) * 255.0).clamp(0.0, 255.0) as u8;
            p[2] = ((b + (lb - b) * strength) * 255.0).clamp(0.0, 255.0) as u8;
            // alpha unchanged
        });

        Ok(image)
    }

    fn describe(&self) -> String {
        format!("LUT  {}³  {:.0}%", self.size, self.strength * 100.0)
    }
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
    fn identity_lut_unchanged() {
        let src = solid(100, 150, 200);
        let src_data = src.data.clone();
        let op = LutOp::identity(17);
        let out = op.apply(src).unwrap();
        for (a, b) in src_data.chunks(4).zip(out.data.chunks(4)) {
            assert!((a[0] as i16 - b[0] as i16).abs() <= 1);
            assert!((a[1] as i16 - b[1] as i16).abs() <= 1);
            assert!((a[2] as i16 - b[2] as i16).abs() <= 1);
        }
    }

    #[test]
    fn zero_strength_is_identity() {
        let src = solid(80, 120, 160);
        let orig = [src.data[0], src.data[2]];
        // Build a swapped-channels LUT (R=B, G=G, B=R) but at strength=0 → unchanged.
        let mut op = LutOp::identity(17);
        // Swap R and B in the LUT data.
        for triplet in op.data.chunks_mut(3) {
            triplet.swap(0, 2);
        }
        op.strength = 0.0;
        let out = op.apply(src).unwrap();
        assert_eq!(out.data[0], orig[0]);
        assert_eq!(out.data[2], orig[1]);
    }

    #[test]
    fn cube_parse_round_trip() {
        // Build a minimal 2^3 identity cube string and parse it.
        let mut cube = "LUT_3D_SIZE 2\n".to_string();
        for b in 0..2 {
            for g in 0..2 {
                for r in 0..2 {
                    cube.push_str(&format!("{} {} {}\n", r, g, b));
                }
            }
        }
        let op = LutOp::from_cube_str(&cube, 1.0).expect("parse");
        assert_eq!(op.size, 2);
        // Apply to a mid-grey; identity LUT → unchanged.
        let src = solid(128, 128, 128);
        let out = op.apply(src).unwrap();
        assert!((out.data[0] as i16 - 128).abs() <= 2);
    }

    #[test]
    fn alpha_preserved() {
        let mut src = Image::new(4, 4);
        src.data.chunks_mut(4).for_each(|p| {
            p[0] = 100;
            p[1] = 150;
            p[2] = 200;
            p[3] = 88;
        });
        let op = LutOp::identity(17);
        let out = op.apply(src).unwrap();
        out.data.chunks(4).for_each(|p| assert_eq!(p[3], 88));
    }
}
