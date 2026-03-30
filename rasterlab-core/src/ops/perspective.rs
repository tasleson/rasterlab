use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{error::RasterResult, image::Image, traits::operation::Operation};

/// Perspective (keystone) correction via a projective homography.
///
/// The four corner offsets describe how much each corner of the source image
/// is displaced **as a fraction of the image dimensions** before mapping back
/// to a rectangular output.  All eight values are in `[-1.0, 1.0]`.
///
/// Corners in order: **top-left**, **top-right**, **bottom-right**, **bottom-left**.
/// Each corner has an `(x, y)` offset where positive-x moves right and
/// positive-y moves down.
///
/// A homography `H` is computed from the four source-to-destination corner
/// correspondences and applied in **inverse-warp** fashion: for every output
/// pixel the inverse `H⁻¹` is used to sample the source image (bilinear
/// interpolation, clamp-to-edge).
///
/// # Typical use — keystone removal
/// If the image was photographed at an angle (converging verticals / horizontals)
/// pull the nearer edge corners inward:
/// * Top-left  `x = +0.1, y = 0`
/// * Top-right `x = -0.1, y = 0`
///
/// (all other corners zero) — this converges the top inward, straightening verticals.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerspectiveOp {
    /// Fractional x/y offset of each corner: `[[tl_x, tl_y], [tr_x, tr_y], [br_x, br_y], [bl_x, bl_y]]`.
    pub corners: [[f32; 2]; 4],
}

impl PerspectiveOp {
    /// `corners` — `[[tl_x, tl_y], [tr_x, tr_y], [br_x, br_y], [bl_x, bl_y]]`,
    /// all values in `[-1, 1]` (fraction of image width/height respectively).
    pub fn new(corners: [[f32; 2]; 4]) -> Self {
        let mut c = corners;
        for corner in &mut c {
            corner[0] = corner[0].clamp(-1.0, 1.0);
            corner[1] = corner[1].clamp(-1.0, 1.0);
        }
        Self { corners: c }
    }

    /// Returns `true` when all corner offsets are at neutral (identity).
    pub fn is_identity(&self) -> bool {
        self.corners
            .iter()
            .all(|c| c[0].abs() < 1e-5 && c[1].abs() < 1e-5)
    }
}

impl Default for PerspectiveOp {
    fn default() -> Self {
        Self {
            corners: [[0.0; 2]; 4],
        }
    }
}

/// Compute the 3×3 homography matrix that maps `src` points to `dst` points.
/// Returns `None` if the system is degenerate.
///
/// Uses the Direct Linear Transform (DLT): build an 8×8 linear system from the
/// four point correspondences, then solve via Gaussian elimination.
#[allow(clippy::needless_range_loop)]
fn homography(src: &[[f32; 2]; 4], dst: &[[f32; 2]; 4]) -> Option<[f32; 9]> {
    // Build 8×9 matrix A for the DLT.
    let mut a = [[0.0f64; 9]; 8];
    for (i, (s, d)) in src.iter().zip(dst.iter()).enumerate() {
        let (sx, sy) = (s[0] as f64, s[1] as f64);
        let (dx, dy) = (d[0] as f64, d[1] as f64);
        let row = i * 2;
        a[row] = [-sx, -sy, -1.0, 0.0, 0.0, 0.0, dx * sx, dx * sy, dx];
        a[row + 1] = [0.0, 0.0, 0.0, -sx, -sy, -1.0, dy * sx, dy * sy, dy];
    }
    // Solve using Gaussian elimination with partial pivoting for the 8×8 system
    // (we fix h[8] = 1 and bring it to the RHS).
    let mut mat = [[0.0f64; 9]; 8]; // [A | b] where b is -a_col8 * 1
    for i in 0..8 {
        for j in 0..8 {
            mat[i][j] = a[i][j];
        }
        mat[i][8] = -a[i][8]; // rhs: move h8 term
    }
    // Forward elimination
    for col in 0..8 {
        // Find pivot
        let mut max_row = col;
        let mut max_val = mat[col][col].abs();
        for row in (col + 1)..8 {
            if mat[row][col].abs() > max_val {
                max_val = mat[row][col].abs();
                max_row = row;
            }
        }
        if max_val < 1e-12 {
            return None; // degenerate
        }
        mat.swap(col, max_row);
        let pivot = mat[col][col];
        for row in (col + 1)..8 {
            let factor = mat[row][col] / pivot;
            for k in col..9 {
                mat[row][k] -= factor * mat[col][k];
            }
        }
    }
    // Back-substitution
    let mut h = [0.0f64; 9];
    h[8] = 1.0;
    for i in (0..8).rev() {
        let mut sum = mat[i][8];
        for j in (i + 1)..8 {
            sum -= mat[i][j] * h[j];
        }
        if mat[i][i].abs() < 1e-12 {
            return None;
        }
        h[i] = sum / mat[i][i];
    }
    Some([
        h[0] as f32,
        h[1] as f32,
        h[2] as f32,
        h[3] as f32,
        h[4] as f32,
        h[5] as f32,
        h[6] as f32,
        h[7] as f32,
        h[8] as f32,
    ])
}

/// Invert a 3×3 matrix.  Returns `None` if singular.
#[cfg(test)]
fn invert3x3(m: &[f32; 9]) -> Option<[f32; 9]> {
    let det = m[0] * (m[4] * m[8] - m[5] * m[7]) - m[1] * (m[3] * m[8] - m[5] * m[6])
        + m[2] * (m[3] * m[7] - m[4] * m[6]);
    if det.abs() < 1e-12 {
        return None;
    }
    let inv_det = 1.0 / det;
    Some([
        (m[4] * m[8] - m[5] * m[7]) * inv_det,
        (m[2] * m[7] - m[1] * m[8]) * inv_det,
        (m[1] * m[5] - m[2] * m[4]) * inv_det,
        (m[5] * m[6] - m[3] * m[8]) * inv_det,
        (m[0] * m[8] - m[2] * m[6]) * inv_det,
        (m[2] * m[3] - m[0] * m[5]) * inv_det,
        (m[3] * m[7] - m[4] * m[6]) * inv_det,
        (m[1] * m[6] - m[0] * m[7]) * inv_det,
        (m[0] * m[4] - m[1] * m[3]) * inv_det,
    ])
}

/// Apply homography `h` to point `(x, y)` → `(x', y')`.
#[inline]
fn apply_h(h: &[f32; 9], x: f32, y: f32) -> (f32, f32) {
    let w = h[6] * x + h[7] * y + h[8];
    (
        (h[0] * x + h[1] * y + h[2]) / w,
        (h[3] * x + h[4] * y + h[5]) / w,
    )
}

/// Bilinear sample from `image` at float coordinates `(sx, sy)`, clamped to border.
#[inline]
fn bilinear_sample(image: &Image, sx: f32, sy: f32) -> [u8; 4] {
    let w = image.width as usize;
    let h = image.height as usize;
    let x0 = (sx.floor() as isize).clamp(0, w as isize - 1) as usize;
    let y0 = (sy.floor() as isize).clamp(0, h as isize - 1) as usize;
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let tx = (sx - sx.floor()).clamp(0.0, 1.0);
    let ty = (sy - sy.floor()).clamp(0.0, 1.0);

    let p00 = &image.data[(y0 * w + x0) * 4..][..4];
    let p10 = &image.data[(y0 * w + x1) * 4..][..4];
    let p01 = &image.data[(y1 * w + x0) * 4..][..4];
    let p11 = &image.data[(y1 * w + x1) * 4..][..4];

    let mut out = [0u8; 4];
    for i in 0..4 {
        let top = p00[i] as f32 + (p10[i] as f32 - p00[i] as f32) * tx;
        let bot = p01[i] as f32 + (p11[i] as f32 - p01[i] as f32) * tx;
        out[i] = (top + (bot - top) * ty).clamp(0.0, 255.0) as u8;
    }
    out
}

#[typetag::serde]
impl Operation for PerspectiveOp {
    fn name(&self) -> &'static str {
        "perspective"
    }

    fn clone_box(&self) -> Box<dyn Operation> {
        Box::new(self.clone())
    }

    fn apply(&self, image: Image) -> RasterResult<Image> {
        if self.is_identity() {
            return Ok(image);
        }

        let w = image.width as f32;
        let h = image.height as f32;

        // Destination rectangle corners (the output frame, in pixel coords).
        let dst_pts: [[f32; 2]; 4] = [[0.0, 0.0], [w, 0.0], [w, h], [0.0, h]];

        // Source corners derived from corner offsets (fractional displacement).
        let src_pts: [[f32; 2]; 4] = [
            [self.corners[0][0] * w, self.corners[0][1] * h], // tl
            [w + self.corners[1][0] * w, self.corners[1][1] * h], // tr
            [w + self.corners[2][0] * w, h + self.corners[2][1] * h], // br
            [self.corners[3][0] * w, h + self.corners[3][1] * h], // bl
        ];

        // H maps dst → src (used in inverse warp).
        use crate::error::RasterError;
        let h_mat = homography(&dst_pts, &src_pts).ok_or_else(|| {
            RasterError::InvalidParams("degenerate perspective homography".into())
        })?;

        let out_w = image.width as usize;
        let mut out = Image::new(image.width, image.height);

        out.data
            .par_chunks_mut(4)
            .enumerate()
            .for_each(|(idx, dst)| {
                let dx = idx % out_w;
                let dy = idx / out_w;
                let (sx, sy) = apply_h(&h_mat, dx as f32 + 0.5, dy as f32 + 0.5);
                let px = bilinear_sample(&image, sx - 0.5, sy - 0.5);
                dst.copy_from_slice(&px);
            });

        Ok(out)
    }

    fn describe(&self) -> String {
        "Perspective".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(r: u8, g: u8, b: u8) -> Image {
        let mut img = Image::new(32, 32);
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
        let src = solid(100, 150, 200);
        let src_data = src.data.clone();
        let out = PerspectiveOp::default().apply(src).unwrap();
        assert_eq!(out.data, src_data);
    }

    #[test]
    fn homography_round_trip() {
        // A homography followed by its inverse should return to origin.
        let src_pts: [[f32; 2]; 4] = [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]];
        let dst_pts: [[f32; 2]; 4] = [[1.0, 1.0], [9.0, 0.0], [10.0, 10.0], [0.0, 10.0]];
        let h = homography(&src_pts, &dst_pts).expect("homography");
        let h_inv = invert3x3(&h).expect("invert");
        let (x, y) = apply_h(&h, 5.0, 5.0);
        let (x2, y2) = apply_h(&h_inv, x, y);
        assert!((x2 - 5.0).abs() < 1e-3, "round-trip x off: {}", x2);
        assert!((y2 - 5.0).abs() < 1e-3, "round-trip y off: {}", y2);
    }

    #[test]
    fn alpha_preserved() {
        let mut src = Image::new(16, 16);
        src.data.chunks_mut(4).for_each(|p| {
            p[0] = 100;
            p[1] = 100;
            p[2] = 100;
            p[3] = 42;
        });
        let op = PerspectiveOp::new([[0.05, 0.0], [-0.05, 0.0], [0.0, 0.0], [0.0, 0.0]]);
        let out = op.apply(src).unwrap();
        out.data.chunks(4).for_each(|p| assert_eq!(p[3], 42));
    }

    #[test]
    fn small_correction_preserves_image() {
        // A tiny perspective correction on a uniform image should leave pixel
        // values basically unchanged (they are all the same colour, so sampling
        // anywhere in the source gives the same result).
        let src = solid(200, 100, 50);
        let op = PerspectiveOp::new([[0.05, 0.05], [-0.05, 0.05], [-0.05, -0.05], [0.05, -0.05]]);
        let out = op.apply(src).unwrap();
        for p in out.data.chunks(4) {
            assert!((p[0] as i16 - 200).abs() <= 2);
        }
    }
}
