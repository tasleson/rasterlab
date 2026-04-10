use serde::{Deserialize, Serialize};

use crate::{error::RasterResult, image::Image, traits::operation::Operation};

/// A single heal/clone-stamp spot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealSpot {
    pub dest_x: i32,
    pub dest_y: i32,
    pub src_x: i32,
    pub src_y: i32,
    pub radius: u32,
}

/// Clone-stamp / spot-heal operation.
///
/// Each [`HealSpot`] copies pixels from a source patch to a destination patch
/// with a cosine-windowed blend so the edges blend smoothly with the
/// surrounding pixels.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HealOp {
    pub spots: Vec<HealSpot>,
}

impl HealOp {
    pub fn new(spots: Vec<HealSpot>) -> Self {
        Self { spots }
    }

    /// Compute the mean squared difference of the border pixels of two
    /// `(2r+1) × (2r+1)` patches centred at `(ax, ay)` and `(bx, by)`.
    ///
    /// Only the perimeter pixels of the square are compared (not the interior),
    /// so this is O(r) rather than O(r²) — fast enough to call for ~48 candidate
    /// positions.
    pub fn patch_border_ssd(image: &Image, ax: i32, ay: i32, bx: i32, by: i32, r: i32) -> f32 {
        let w = image.width as i32;
        let h = image.height as i32;

        let mut sum = 0.0_f32;
        let mut count = 0u32;

        let mut sample = |dx: i32, dy: i32| {
            let pa = image.pixel(
                (ax + dx).clamp(0, w - 1) as u32,
                (ay + dy).clamp(0, h - 1) as u32,
            );
            let pb = image.pixel(
                (bx + dx).clamp(0, w - 1) as u32,
                (by + dy).clamp(0, h - 1) as u32,
            );
            for c in 0..3 {
                let diff = pa[c] as f32 - pb[c] as f32;
                sum += diff * diff;
            }
            count += 1;
        };

        for d in -r..=r {
            // Top and bottom rows
            sample(d, -r);
            sample(d, r);
            // Left and right columns (excluding corners already counted above)
            if d > -r && d < r {
                sample(-r, d);
                sample(r, d);
            }
        }

        if count == 0 {
            return f32::MAX;
        }
        sum / count as f32
    }

    /// Auto-detect a good source patch for a heal spot centred at `(dest_x, dest_y)`.
    ///
    /// Searches a ring of candidate positions at distances `1.5r … 4r` and
    /// returns the one whose border pixels best match the destination patch.
    /// Falls back to `(dest_x + 2*radius, dest_y)` if no valid candidate
    /// exists within image bounds.
    pub fn auto_detect_source(image: &Image, dest_x: i32, dest_y: i32, radius: u32) -> (i32, i32) {
        let r = radius as f32;
        let w = image.width as i32;
        let h = image.height as i32;
        let border_r = radius as i32;

        let mut best_ssd = f32::MAX;
        let mut best = None;

        // 3 distance rings × 16 angular samples = 48 candidates
        for ring in 0..3u32 {
            let dist = r * (1.5 + ring as f32 * 0.833); // 1.5r, 2.333r, 3.167r → ~1.5–4r
            for step in 0..16u32 {
                let angle = std::f32::consts::TAU * step as f32 / 16.0;
                let sx = dest_x + (dist * angle.cos()).round() as i32;
                let sy = dest_y + (dist * angle.sin()).round() as i32;

                // Candidate patch must be fully within image
                if sx - border_r < 0
                    || sy - border_r < 0
                    || sx + border_r >= w
                    || sy + border_r >= h
                {
                    continue;
                }

                let ssd = Self::patch_border_ssd(image, dest_x, dest_y, sx, sy, border_r);
                if ssd < best_ssd {
                    best_ssd = ssd;
                    best = Some((sx, sy));
                }
            }
        }

        best.unwrap_or((dest_x + radius as i32 * 2, dest_y))
    }
}

/// Apply one heal spot: reads from `image_data` (original), writes into `out`.
///
/// Using the original buffer for reads avoids read/write aliasing between
/// overlapping spots when they are applied sequentially.
fn apply_spot(image_data: &[u8], out: &mut [u8], w: usize, h: usize, spot: &HealSpot) {
    let r = spot.radius as i32;
    let r_sq = (spot.radius * spot.radius) as i32;
    let half_pi = std::f32::consts::FRAC_PI_2;

    for dy in -r..=r {
        for dx in -r..=r {
            if dx * dx + dy * dy > r_sq {
                continue;
            }

            let dst_px = spot.dest_x + dx;
            let dst_py = spot.dest_y + dy;
            if dst_px < 0 || dst_py < 0 || dst_px >= w as i32 || dst_py >= h as i32 {
                continue;
            }

            let src_px = (spot.src_x + dx).clamp(0, w as i32 - 1);
            let src_py = (spot.src_y + dy).clamp(0, h as i32 - 1);

            let dist = ((dx * dx + dy * dy) as f32).sqrt();
            let t = dist / spot.radius as f32;
            let cos_val = (half_pi * t).cos();
            let weight = cos_val * cos_val;

            let src_off = (src_py as usize * w + src_px as usize) * 4;
            let dst_off = (dst_py as usize * w + dst_px as usize) * 4;

            // Read destination from the ORIGINAL buffer, not `out`
            for c in 0..3 {
                let src_val = image_data[src_off + c] as f32;
                let dst_val = image_data[dst_off + c] as f32;
                let blended = dst_val + (src_val - dst_val) * weight;
                out[dst_off + c] = blended.round().clamp(0.0, 255.0) as u8;
            }
            // Preserve alpha
            out[dst_off + 3] = image_data[dst_off + 3];
        }
    }
}

#[typetag::serde]
impl Operation for HealOp {
    fn name(&self) -> &'static str {
        "heal"
    }

    fn clone_box(&self) -> Box<dyn Operation> {
        Box::new(self.clone())
    }

    fn apply(&self, image: Image) -> RasterResult<Image> {
        if self.spots.is_empty() {
            return Ok(image);
        }

        let w = image.width as usize;
        let h = image.height as usize;

        // Take a snapshot of the original data so all spots read from the
        // pre-modification image, preventing one spot from influencing another.
        let original = image.data.clone();
        let mut out = image;

        for spot in &self.spots {
            apply_spot(&original, &mut out.data, w, h, spot);
        }

        Ok(out)
    }

    fn describe(&self) -> String {
        format!(
            "Heal  {} spot{}",
            self.spots.len(),
            if self.spots.len() == 1 { "" } else { "s" }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(r: u8, g: u8, b: u8, w: u32, h: u32) -> Image {
        let mut img = Image::new(w, h);
        img.data.chunks_mut(4).for_each(|p| {
            p[0] = r;
            p[1] = g;
            p[2] = b;
            p[3] = 255;
        });
        img
    }

    #[test]
    fn test_heal_copies_source() {
        // Image: left half red, right half blue.
        // Place a heal spot from the blue region onto the red region.
        let mut img = Image::new(100, 100);
        for y in 0..100u32 {
            for x in 0..100u32 {
                let color = if x < 50 {
                    [255u8, 0, 0, 255]
                } else {
                    [0, 0, 255, 255]
                };
                img.set_pixel(x, y, color);
            }
        }

        let op = HealOp::new(vec![HealSpot {
            dest_x: 25,
            dest_y: 50,
            src_x: 75,
            src_y: 50,
            radius: 5,
        }]);

        let out = op.apply(img).unwrap();
        // Centre pixel of the destination should be close to blue (0, 0, 255)
        let centre = out.pixel(25, 50);
        assert!(
            centre[2] > 200,
            "dest centre should be blueish after heal, got {:?}",
            centre
        );
    }

    #[test]
    fn test_auto_detect_returns_valid_coords() {
        let img = solid(128, 128, 128, 200, 200);
        let (sx, sy) = HealOp::auto_detect_source(&img, 100, 100, 15);
        assert!(
            (0..200).contains(&sx) && (0..200).contains(&sy),
            "auto-detect returned out-of-bounds ({sx}, {sy})"
        );
    }

    #[test]
    fn test_alpha_preserved() {
        // Image with alpha = 128 everywhere.
        let mut img = Image::new(60, 60);
        img.data.chunks_mut(4).for_each(|p| {
            p[0] = 200;
            p[1] = 100;
            p[2] = 50;
            p[3] = 128;
        });

        let op = HealOp::new(vec![HealSpot {
            dest_x: 20,
            dest_y: 20,
            src_x: 40,
            src_y: 40,
            radius: 8,
        }]);

        let out = op.apply(img).unwrap();
        // Alpha must be unchanged at the destination centre
        let p = out.pixel(20, 20);
        assert_eq!(p[3], 128, "alpha should be preserved, got {}", p[3]);
    }
}
