use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{error::RasterResult, image::Image, traits::operation::Operation};

/// Tone curve adjustment applied equally to all RGB channels.
///
/// Control points are stored as `[input, output]` pairs in `[0.0, 1.0]`.
/// The first point is always at `x = 0.0` and the last at `x = 1.0`.
/// The curve is interpolated using a monotone cubic Hermite spline
/// (Fritsch-Carlson), which guarantees no overshoot between control points.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CurvesOp {
    /// Control points sorted ascending by `[0]` (x/input).
    pub points: Vec<[f32; 2]>,
}

impl CurvesOp {
    /// Identity curve — output equals input.
    pub fn identity() -> Self {
        Self {
            points: vec![[0.0, 0.0], [1.0, 1.0]],
        }
    }

    /// Build the 256-entry lookup table from the current control points.
    /// Values outside the endpoint range are clamped to the endpoint outputs.
    pub fn build_lut(points: &[[f32; 2]]) -> [u8; 256] {
        let mut lut = [0u8; 256];
        if points.is_empty() {
            return lut;
        }
        if points.len() == 1 {
            let v = (points[0][1].clamp(0.0, 1.0) * 255.0).round() as u8;
            return [v; 256];
        }

        let n = points.len() - 1; // number of segments

        // Chord slopes.
        let mut d = vec![0.0f32; n];
        for i in 0..n {
            let dx = points[i + 1][0] - points[i][0];
            d[i] = if dx > 1e-9 {
                (points[i + 1][1] - points[i][1]) / dx
            } else {
                0.0
            };
        }

        // Initial tangents (Catmull-Rom style averages).
        let mut m = vec![0.0f32; n + 1];
        m[0] = d[0];
        m[n] = d[n - 1];
        for i in 1..n {
            m[i] = (d[i - 1] + d[i]) * 0.5;
        }

        // Fritsch-Carlson monotonicity adjustment.
        for i in 0..n {
            if d[i].abs() < 1e-9 {
                m[i] = 0.0;
                m[i + 1] = 0.0;
            } else {
                let alpha = m[i] / d[i];
                let beta = m[i + 1] / d[i];
                let mag_sq = alpha * alpha + beta * beta;
                if mag_sq > 9.0 {
                    let tau = 3.0 / mag_sq.sqrt();
                    m[i] *= tau;
                    m[i + 1] *= tau;
                }
            }
        }

        // Evaluate spline at each LUT index.
        for (idx, v) in lut.iter_mut().enumerate() {
            let x = idx as f32 / 255.0;

            // Find the segment (binary search).
            let seg = match points.windows(2).position(|w| x <= w[1][0]) {
                Some(i) => i,
                None => n - 1,
            }
            .min(n - 1);

            let x0 = points[seg][0];
            let x1 = points[seg + 1][0];
            let dx = x1 - x0;

            let y = if dx < 1e-9 {
                points[seg][1]
            } else {
                let t = (x - x0) / dx;
                // Cubic Hermite basis functions.
                let t2 = t * t;
                let t3 = t2 * t;
                let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
                let h10 = t3 - 2.0 * t2 + t;
                let h01 = -2.0 * t3 + 3.0 * t2;
                let h11 = t3 - t2;
                h00 * points[seg][1]
                    + h10 * m[seg] * dx
                    + h01 * points[seg + 1][1]
                    + h11 * m[seg + 1] * dx
            };

            *v = (y.clamp(0.0, 1.0) * 255.0).round() as u8;
        }

        lut
    }
}

#[typetag::serde]
impl Operation for CurvesOp {
    fn name(&self) -> &'static str {
        "curves"
    }

    fn apply(&self, image: &Image) -> RasterResult<Image> {
        let lut = Self::build_lut(&self.points);
        let mut out = image.deep_clone();

        out.data.par_chunks_mut(4).for_each(|p| {
            p[0] = lut[p[0] as usize];
            p[1] = lut[p[1] as usize];
            p[2] = lut[p[2] as usize];
            // alpha untouched
        });

        Ok(out)
    }

    fn describe(&self) -> String {
        format!("Curves  ({} points)", self.points.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_lut() {
        let lut = CurvesOp::build_lut(&[[0.0, 0.0], [1.0, 1.0]]);
        for (i, &v) in lut.iter().enumerate() {
            assert_eq!(v, i as u8, "identity lut mismatch at {i}");
        }
    }

    #[test]
    fn invert_lut() {
        let lut = CurvesOp::build_lut(&[[0.0, 1.0], [1.0, 0.0]]);
        assert_eq!(lut[0], 255);
        assert_eq!(lut[255], 0);
    }

    #[test]
    fn identity_apply_unchanged() {
        let mut src = Image::new(4, 4);
        src.data.chunks_mut(4).for_each(|p| {
            p[0] = 80;
            p[1] = 160;
            p[2] = 240;
            p[3] = 255;
        });
        let out = CurvesOp::identity().apply(&src).unwrap();
        assert_eq!(out.data, src.data);
    }

    #[test]
    fn alpha_preserved() {
        let mut src = Image::new(2, 2);
        src.data.chunks_mut(4).for_each(|p| {
            p[3] = 99;
        });
        let out = CurvesOp::identity().apply(&src).unwrap();
        out.data.chunks(4).for_each(|p| assert_eq!(p[3], 99));
    }
}
