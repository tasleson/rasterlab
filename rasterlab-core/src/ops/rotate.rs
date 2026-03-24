use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{
    error::RasterResult,
    image::Image,
    traits::operation::Operation,
};

/// How to rotate the image.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "mode", content = "value")]
pub enum RotateMode {
    /// 90° clockwise fast path (pixel transposition, no interpolation).
    Cw90,
    /// 180° rotation fast path.
    Cw180,
    /// 270° clockwise (= 90° counter-clockwise) fast path.
    Cw270,
    /// Arbitrary angle in degrees (clockwise, positive = CW).
    /// Uses bilinear interpolation; output includes the full rotated bounding box.
    Arbitrary(f32),
}

/// Rotate operation.
///
/// Right-angle rotations use pixel-transposition for maximum speed (no interpolation
/// artefacts, single-pass with rayon).  Arbitrary angles use inverse-mapping with
/// bilinear interpolation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RotateOp {
    pub mode: RotateMode,
    /// Background fill colour (RGBA) for areas outside the source after an
    /// arbitrary rotation.  Defaults to transparent black `[0, 0, 0, 0]`.
    pub background: [u8; 4],
}

impl RotateOp {
    pub fn cw90()  -> Self { Self { mode: RotateMode::Cw90,  background: [0; 4] } }
    pub fn cw180() -> Self { Self { mode: RotateMode::Cw180, background: [0; 4] } }
    pub fn cw270() -> Self { Self { mode: RotateMode::Cw270, background: [0; 4] } }
    pub fn arbitrary(degrees: f32) -> Self {
        Self { mode: RotateMode::Arbitrary(degrees), background: [0; 4] }
    }
}

#[typetag::serde]
impl Operation for RotateOp {
    fn name(&self) -> &'static str { "rotate" }

    fn apply(&self, image: &Image) -> RasterResult<Image> {
        match self.mode {
            RotateMode::Cw90          => rotate_cw90(image),
            RotateMode::Cw180         => rotate_180(image),
            RotateMode::Cw270         => rotate_cw270(image),
            RotateMode::Arbitrary(d)  => rotate_arbitrary(image, d, self.background),
        }
    }

    fn describe(&self) -> String {
        match self.mode {
            RotateMode::Cw90         => "Rotate 90° CW".into(),
            RotateMode::Cw180        => "Rotate 180°".into(),
            RotateMode::Cw270        => "Rotate 270° CW".into(),
            RotateMode::Arbitrary(d) => format!("Rotate {:.2}°", d),
        }
    }
}

// ---------------------------------------------------------------------------
// 90° CW:  new(nx, ny) = old(ny, w-1-nx)
//          output: new_w = old_h,  new_h = old_w
// ---------------------------------------------------------------------------
// 90° CW derivation (input w×h → output h×w):
//   output(nx, ny) = input(ox=ny, oy=h-1-nx)
fn rotate_cw90(src: &Image) -> RasterResult<Image> {
    let (w, h) = (src.width, src.height);
    // After 90° CW: new image is h wide and w tall
    let mut out = Image::new(h, w);
    out.metadata = src.metadata.clone();

    let new_w = h as usize; // output width
    out.data
        .par_chunks_mut(new_w * 4)
        .enumerate()
        .for_each(|(ny, row)| {
            for nx in 0..new_w {
                let ox = ny as u32;              // input col = output row
                let oy = (h - 1) - nx as u32;   // input row = h-1-output_col
                let src_off = (oy as usize * w as usize + ox as usize) * 4;
                let dst_off = nx * 4;
                row[dst_off..dst_off + 4].copy_from_slice(&src.data[src_off..src_off + 4]);
            }
        });

    Ok(out)
}

// ---------------------------------------------------------------------------
// 180°:  new(nx, ny) = old(w-1-nx, h-1-ny)
// ---------------------------------------------------------------------------
fn rotate_180(src: &Image) -> RasterResult<Image> {
    let mut out = src.deep_clone();
    let total = out.width as usize * out.height as usize;
    let half  = total / 2;

    for i in 0..half {
        let j = total - 1 - i;
        let (ai, bi) = (i * 4, j * 4);
        // Swap pixels i and j across all 4 channels
        for c in 0..4 {
            out.data.swap(ai + c, bi + c);
        }
    }
    // handle centre pixel in odd-total case: already in place
    Ok(out)
}

// ---------------------------------------------------------------------------
// 270° CW (= 90° CCW):  new(nx, ny) = old(h-1-ny, nx)
//                        output: new_w = old_h,  new_h = old_w
// ---------------------------------------------------------------------------
// 270° CW (= 90° CCW) derivation (input w×h → output h×w):
//   output(nx, ny) = input(ox=w-1-ny, oy=nx)
fn rotate_cw270(src: &Image) -> RasterResult<Image> {
    let (w, h) = (src.width, src.height);
    let mut out = Image::new(h, w);
    out.metadata = src.metadata.clone();

    let new_w = h as usize;
    out.data
        .par_chunks_mut(new_w * 4)
        .enumerate()
        .for_each(|(ny, row)| {
            for nx in 0..new_w {
                let ox = (w - 1) - ny as u32;  // input col = w-1-output_row
                let oy = nx as u32;             // input row = output_col
                let src_off = (oy as usize * w as usize + ox as usize) * 4;
                let dst_off = nx * 4;
                row[dst_off..dst_off + 4].copy_from_slice(&src.data[src_off..src_off + 4]);
            }
        });

    Ok(out)
}

// ---------------------------------------------------------------------------
// Arbitrary angle — inverse-mapped bilinear interpolation.
// ---------------------------------------------------------------------------
fn rotate_arbitrary(src: &Image, degrees: f32, bg: [u8; 4]) -> RasterResult<Image> {
    let theta = degrees.to_radians();
    let (cos_t, sin_t) = (theta.cos(), theta.sin());

    let w = src.width as f32;
    let h = src.height as f32;

    // Bounding box of rotated image
    let new_w = ((w * cos_t.abs()) + (h * sin_t.abs())).ceil() as u32;
    let new_h = ((w * sin_t.abs()) + (h * cos_t.abs())).ceil() as u32;

    let cx_out = new_w as f32 / 2.0;
    let cy_out = new_h as f32 / 2.0;
    let cx_src = w / 2.0;
    let cy_src = h / 2.0;

    let mut out = Image::new(new_w, new_h);
    out.metadata = src.metadata.clone();

    let nw = new_w as usize;
    out.data
        .par_chunks_mut(nw * 4)
        .enumerate()
        .for_each(|(ny, row)| {
            let dy = ny as f32 - cy_out;
            for nx in 0..nw {
                let dx = nx as f32 - cx_out;
                // Inverse rotation (CW by theta → rotate back CCW)
                let sx = dx * cos_t + dy * sin_t + cx_src;
                let sy = -dx * sin_t + dy * cos_t + cy_src;

                let pixel = if sx >= 0.0 && sx < w && sy >= 0.0 && sy < h {
                    src.sample_bilinear(sx, sy)
                } else {
                    bg
                };

                let off = nx * 4;
                row[off..off + 4].copy_from_slice(&pixel);
            }
        });

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, col: [u8; 4]) -> Image {
        let mut img = Image::new(w, h);
        for y in 0..h {
            for x in 0..w {
                img.set_pixel(x, y, col);
            }
        }
        img
    }

    #[test]
    fn rotate_180_identity_for_solid() {
        let src = solid(6, 4, [100, 150, 200, 255]);
        let out = RotateOp::cw180().apply(&src).unwrap();
        assert_eq!(out.width, 6);
        assert_eq!(out.height, 4);
        assert_eq!(out.pixel(0, 0), [100, 150, 200, 255]);
    }

    #[test]
    fn rotate_cw90_swaps_dimensions() {
        let src = solid(8, 4, [1, 2, 3, 255]);
        let out = RotateOp::cw90().apply(&src).unwrap();
        assert_eq!(out.width,  4);
        assert_eq!(out.height, 8);
    }

    #[test]
    fn rotate_arbitrary_zero() {
        let src = solid(8, 8, [10, 20, 30, 255]);
        let out = RotateOp::arbitrary(0.0).apply(&src).unwrap();
        // 0° rotation: bounding box should remain 8×8
        assert_eq!(out.width,  8);
        assert_eq!(out.height, 8);
        // Centre pixel unchanged
        assert_eq!(out.pixel(4, 4), [10, 20, 30, 255]);
    }
}
