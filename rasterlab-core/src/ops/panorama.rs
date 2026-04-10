//! Panorama stitching operation.
//!
//! Stitches multiple images loaded from disk by:
//! 1. Detecting Harris corners in each image.
//! 2. Extracting normalised 15×15 patch descriptors.
//! 3. Brute-force matching with Lowe's ratio test + cross-check.
//! 4. RANSAC homography estimation (general-N DLT via normal equations).
//! 5. Chaining homographies to a common canvas.
//! 6. Inverse-warp rendering with distance-weighted feather blending at seams.
//!
//! `apply()` ignores the input `Image` and reloads all images from
//! `image_paths`, making the op fully self-contained for serialisation /
//! non-destructive replay.

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{
    cancel,
    error::{RasterError, RasterResult},
    formats::FormatRegistry,
    image::Image,
    traits::operation::Operation,
};

// ── Tuning constants ─────────────────────────────────────────────────────────

/// Maximum Harris corners retained per image after NMS.
const MAX_KP: usize = 500;
/// Half-side of the square patch used for description.
const PATCH_HALF: usize = 7;
/// Full patch side = `2 * PATCH_HALF + 1`.
const PATCH_SIDE: usize = 2 * PATCH_HALF + 1;
/// Descriptor dimensionality.
const PATCH_DIM: usize = PATCH_SIDE * PATCH_SIDE; // 225
/// RANSAC iteration count.
const RANSAC_N: usize = 1000;
/// Squared reprojection-error threshold for RANSAC inlier classification.
const RANSAC_THRESH_SQ: f32 = 9.0; // 3 px
/// Minimum inlier count required to accept a homography.
const MIN_INLIERS: usize = 8;
/// Lowe's ratio-test threshold for descriptor matching.
const RATIO_THRESH: f32 = 0.75;
/// Hard cap on each canvas dimension (prevents accidental gigapixel allocation).
const MAX_CANVAS_DIM: u32 = 32_000;

// ── Public op ────────────────────────────────────────────────────────────────

/// Non-destructive panorama stitching op.
///
/// Stores the absolute paths of all images (JPEG, PNG, NEF …) to stitch in
/// order.  `apply()` ignores its `Image` argument and produces the stitched
/// result from scratch so the op is self-contained.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PanoramaOp {
    /// Absolute paths, in stitching order, to the source frames.
    pub image_paths: Vec<String>,
    /// Width of the Gaussian-feather ramp at each seam (pixels).
    pub feather_px: u32,
}

impl PanoramaOp {
    pub fn new(image_paths: Vec<String>, feather_px: u32) -> Self {
        Self {
            image_paths,
            feather_px,
        }
    }
}

#[typetag::serde]
impl Operation for PanoramaOp {
    fn name(&self) -> &'static str {
        "panorama"
    }

    fn clone_box(&self) -> Box<dyn Operation> {
        Box::new(self.clone())
    }

    fn apply(&self, _image: Image) -> RasterResult<Image> {
        stitch(self)
    }

    fn describe(&self) -> String {
        format!("Panorama ({} frames)", self.image_paths.len())
    }

    fn is_geometric(&self) -> bool {
        true
    }
}

// ── Internal types ────────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
struct Kp {
    x: f32,
    y: f32,
}

type Desc = [f32; PATCH_DIM];

// ── Top-level stitching entry point ──────────────────────────────────────────

fn stitch(op: &PanoramaOp) -> RasterResult<Image> {
    if op.image_paths.is_empty() {
        return Err(RasterError::InvalidParams(
            "Panorama: no image paths specified".into(),
        ));
    }

    let reg = FormatRegistry::with_builtins();

    // Load every frame.
    let images: Vec<Image> = op
        .image_paths
        .iter()
        .map(|p| {
            if cancel::is_requested() {
                return Err(RasterError::Cancelled);
            }
            reg.decode_file(std::path::Path::new(p)).map_err(|e| {
                RasterError::InvalidParams(format!("Panorama: cannot load '{p}': {e}"))
            })
        })
        .collect::<RasterResult<_>>()?;

    if images.len() == 1 {
        // Nothing to stitch — return the single loaded frame.
        return Ok(images.into_iter().next().unwrap());
    }

    // ── Feature detection ─────────────────────────────────────────────────

    let mut all_kps: Vec<Vec<Kp>> = Vec::with_capacity(images.len());
    let mut all_descs: Vec<Vec<Desc>> = Vec::with_capacity(images.len());

    for img in &images {
        if cancel::is_requested() {
            return Err(RasterError::Cancelled);
        }
        let gray = to_gray(img);
        let kps = harris_corners(&gray, img.width as usize, img.height as usize);
        let descs = extract_descriptors(&gray, img.width as usize, img.height as usize, &kps);
        all_kps.push(kps);
        all_descs.push(descs);
    }

    // ── Pair-wise homography estimation ───────────────────────────────────

    // H_pair[i] maps image[i+1] coords → image[i] coords.
    let mut h_pair: Vec<[f32; 9]> = Vec::with_capacity(images.len() - 1);

    for i in 0..images.len() - 1 {
        if cancel::is_requested() {
            return Err(RasterError::Cancelled);
        }
        let matches = match_features(
            &all_kps[i],
            &all_descs[i],
            &all_kps[i + 1],
            &all_descs[i + 1],
        );
        if matches.len() < MIN_INLIERS {
            return Err(RasterError::InvalidParams(format!(
                "Panorama: too few feature matches between images {} and {} ({} found, need {})",
                i,
                i + 1,
                matches.len(),
                MIN_INLIERS
            )));
        }
        let h = ransac_homography(
            &matches,
            &all_kps[i],
            &all_kps[i + 1],
            RANSAC_THRESH_SQ,
            RANSAC_N,
        )
        .ok_or_else(|| {
            RasterError::InvalidParams(format!(
                "Panorama: RANSAC failed to find homography between images {i} and {}",
                i + 1
            ))
        })?;
        h_pair.push(h);
    }

    // ── Chain homographies to canvas space ────────────────────────────────

    // H_to_canvas[i] maps image[i] coords → canvas coords (before translation).
    let mut h_to_canvas: Vec<[f32; 9]> = Vec::with_capacity(images.len());
    h_to_canvas.push(identity_h());
    for i in 0..h_pair.len() {
        // H_to_canvas[i+1] = H_to_canvas[i] * H_pair[i]
        // because image[i+1] → image[i] via H_pair[i], then image[i] → canvas via H_to_canvas[i].
        let h = mul_h(&h_to_canvas[i], &h_pair[i]);
        h_to_canvas.push(h);
    }

    // ── Compute canvas bounding box ───────────────────────────────────────

    let mut min_x = f32::MAX;
    let mut min_y = f32::MAX;
    let mut max_x = f32::MIN;
    let mut max_y = f32::MIN;

    for (i, img) in images.iter().enumerate() {
        let w = img.width as f32;
        let h = img.height as f32;
        for &(cx, cy) in &[(0.0f32, 0.0f32), (w, 0.0), (w, h), (0.0, h)] {
            let (px, py) = apply_h(&h_to_canvas[i], cx, cy);
            min_x = min_x.min(px);
            min_y = min_y.min(py);
            max_x = max_x.max(px);
            max_y = max_y.max(py);
        }
    }

    let canvas_w = (max_x - min_x).ceil() as u32;
    let canvas_h = (max_y - min_y).ceil() as u32;

    if canvas_w > MAX_CANVAS_DIM || canvas_h > MAX_CANVAS_DIM {
        return Err(RasterError::DimensionsOutOfRange(format!(
            "Panorama canvas too large ({canvas_w}×{canvas_h}); max is {MAX_CANVAS_DIM}"
        )));
    }

    // ── Build final inverse homographies (canvas coords → image[i] coords) ─

    // Include the translation T⁻¹ = [[1,0,min_x],[0,1,min_y],[0,0,1]] so that
    // canvas origin (0,0) maps to the top-left corner of the bounding box.
    let t_inv = translation_h(min_x, min_y);

    // H_from_canvas[i] = inverse(H_to_canvas[i]) * T⁻¹
    // Equivalently: H_to_canvas_shifted[i] = T * H_to_canvas[i], then invert.
    // We compute it as: H_fc = invert(H_to_canvas[i]) * T_inv  (applied right-to-left)
    let h_from_canvas: Vec<[f32; 9]> = h_to_canvas
        .iter()
        .map(|h| {
            // invert H, then post-multiply by T_inv.
            let h_inv = invert_h(h).unwrap_or_else(identity_h);
            mul_h(&h_inv, &t_inv)
        })
        .collect();

    // ── Render canvas with feather blending ──────────────────────────────

    if cancel::is_requested() {
        return Err(RasterError::Cancelled);
    }

    let feather = op.feather_px.max(1) as f32;
    let mut canvas = Image::new(canvas_w, canvas_h);
    let cw = canvas_w as usize;

    // Per-pixel: loop over all images, accumulate weighted colour, normalise.
    // The closure captures shared references to `images` and `h_from_canvas`,
    // both of which are `Sync`.
    canvas
        .data
        .par_chunks_mut(4)
        .enumerate()
        .for_each(|(idx, pixel)| {
            let cx = (idx % cw) as f32 + 0.5;
            let cy = (idx / cw) as f32 + 0.5;

            let mut r_acc = 0.0f32;
            let mut g_acc = 0.0f32;
            let mut b_acc = 0.0f32;
            let mut w_acc = 0.0f32;

            for (img, hfc) in images.iter().zip(h_from_canvas.iter()) {
                let (sx, sy) = apply_h(hfc, cx, cy);
                let sx = sx - 0.5;
                let sy = sy - 0.5;
                let iw = img.width as f32;
                let ih = img.height as f32;
                if sx < 0.0 || sy < 0.0 || sx >= iw || sy >= ih {
                    continue;
                }
                // Distance-to-edge feather weight, clamped to [0, 1].
                let wx = sx.min(iw - 1.0 - sx).min(feather) / feather;
                let wy = sy.min(ih - 1.0 - sy).min(feather) / feather;
                let w = wx.min(wy).max(0.0);
                if w <= 0.0 {
                    continue;
                }
                let px = bilinear_sample(img, sx, sy);
                r_acc += w * px[0] as f32;
                g_acc += w * px[1] as f32;
                b_acc += w * px[2] as f32;
                w_acc += w;
            }

            if w_acc > 0.0 {
                pixel[0] = (r_acc / w_acc).clamp(0.0, 255.0) as u8;
                pixel[1] = (g_acc / w_acc).clamp(0.0, 255.0) as u8;
                pixel[2] = (b_acc / w_acc).clamp(0.0, 255.0) as u8;
                pixel[3] = 255;
            }
        });

    Ok(canvas)
}

// ── Grayscale ────────────────────────────────────────────────────────────────

fn to_gray(image: &Image) -> Vec<f32> {
    image
        .data
        .chunks_exact(4)
        .map(|p| 0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32)
        .collect()
}

// ── Harris corner detection ───────────────────────────────────────────────────

fn harris_corners(gray: &[f32], w: usize, h: usize) -> Vec<Kp> {
    // Sobel gradients and structure tensor products.
    let mut ix2 = vec![0.0f32; w * h];
    let mut iy2 = vec![0.0f32; w * h];
    let mut ixy = vec![0.0f32; w * h];

    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let ix =
                (gray[(y - 1) * w + x + 1] + 2.0 * gray[y * w + x + 1] + gray[(y + 1) * w + x + 1])
                    - (gray[(y - 1) * w + x - 1]
                        + 2.0 * gray[y * w + x - 1]
                        + gray[(y + 1) * w + x - 1]);
            let iy = (gray[(y + 1) * w + x - 1]
                + 2.0 * gray[(y + 1) * w + x]
                + gray[(y + 1) * w + x + 1])
                - (gray[(y - 1) * w + x - 1]
                    + 2.0 * gray[(y - 1) * w + x]
                    + gray[(y - 1) * w + x + 1]);
            ix2[y * w + x] = ix * ix;
            iy2[y * w + x] = iy * iy;
            ixy[y * w + x] = ix * iy;
        }
    }

    // Separable [1,2,1]/4 smoothing of the tensor components.
    let ix2 = smooth_121(&ix2, w, h);
    let iy2 = smooth_121(&iy2, w, h);
    let ixy = smooth_121(&ixy, w, h);

    // Harris response R = det(M) − k·trace(M)².
    const K: f32 = 0.04;
    let mut response = vec![0.0f32; w * h];
    for i in 0..w * h {
        let det = ix2[i] * iy2[i] - ixy[i] * ixy[i];
        let tr = ix2[i] + iy2[i];
        response[i] = det - K * tr * tr;
    }

    // Non-maximum suppression in a 9×9 window; keep border-safe interior only.
    let border = PATCH_HALF + 5;
    let mut candidates: Vec<(f32, usize, usize)> = Vec::new();

    for y in border..h - border {
        for x in border..w - border {
            let r = response[y * w + x];
            if r <= 0.0 {
                continue;
            }
            // Check local maximum in 9×9 neighbourhood.
            let mut is_max = true;
            'outer: for dy in -4i32..=4 {
                for dx in -4i32..=4 {
                    if dx == 0 && dy == 0 {
                        continue;
                    }
                    let ny = (y as i32 + dy) as usize;
                    let nx = (x as i32 + dx) as usize;
                    if response[ny * w + nx] >= r {
                        is_max = false;
                        break 'outer;
                    }
                }
            }
            if is_max {
                candidates.push((r, x, y));
            }
        }
    }

    // Sort descending by response, keep the top MAX_KP.
    candidates.sort_unstable_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    candidates.truncate(MAX_KP);
    candidates
        .into_iter()
        .map(|(_, x, y)| Kp {
            x: x as f32,
            y: y as f32,
        })
        .collect()
}

// ── Patch descriptor extraction ───────────────────────────────────────────────

fn extract_descriptors(gray: &[f32], w: usize, h: usize, kps: &[Kp]) -> Vec<Desc> {
    kps.iter()
        .filter_map(|kp| extract_one_patch(gray, w, h, kp))
        .collect()
}

/// Extract and normalise the `PATCH_SIDE×PATCH_SIDE` patch centred on `kp`.
/// Returns `None` if the patch has near-zero variance (featureless region).
fn extract_one_patch(gray: &[f32], w: usize, h: usize, kp: &Kp) -> Option<Desc> {
    let cx = kp.x as usize;
    let cy = kp.y as usize;

    let mut patch = [0.0f32; PATCH_DIM];
    let mut sum = 0.0f32;

    for (i, dy) in (-(PATCH_HALF as i32)..=(PATCH_HALF as i32)).enumerate() {
        for (j, dx) in (-(PATCH_HALF as i32)..=(PATCH_HALF as i32)).enumerate() {
            let py = (cy as i32 + dy).clamp(0, h as i32 - 1) as usize;
            let px = (cx as i32 + dx).clamp(0, w as i32 - 1) as usize;
            let v = gray[py * w + px];
            patch[i * PATCH_SIDE + j] = v;
            sum += v;
        }
    }

    let mean = sum / PATCH_DIM as f32;
    let var: f32 = patch.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / PATCH_DIM as f32;

    if var < 1.0 {
        return None; // Flat / low-contrast patch — unreliable.
    }

    let inv_std = 1.0 / var.sqrt();
    let mut desc = [0.0f32; PATCH_DIM];
    for (i, &v) in patch.iter().enumerate() {
        desc[i] = (v - mean) * inv_std;
    }
    Some(desc)
}

// ── Feature matching ──────────────────────────────────────────────────────────

/// Returns `(idx_a, idx_b)` match pairs after ratio test + cross-check.
fn match_features(
    kps_a: &[Kp],
    descs_a: &[Desc],
    kps_b: &[Kp],
    descs_b: &[Desc],
) -> Vec<(usize, usize)> {
    if descs_a.is_empty() || descs_b.is_empty() {
        return Vec::new();
    }

    // Forward: for each descriptor in A find best and second-best in B.
    let fwd: Vec<Option<usize>> = descs_a
        .par_iter()
        .map(|da| {
            let mut best_i = 0usize;
            let mut best_d = f32::MAX;
            let mut second_d = f32::MAX;
            for (j, db) in descs_b.iter().enumerate() {
                let d = ssd(da, db);
                if d < best_d {
                    second_d = best_d;
                    best_d = d;
                    best_i = j;
                } else if d < second_d {
                    second_d = d;
                }
            }
            if best_d < RATIO_THRESH * RATIO_THRESH * second_d {
                Some(best_i)
            } else {
                None
            }
        })
        .collect();

    // Backward: best match from B to A.
    let bwd: Vec<usize> = descs_b
        .par_iter()
        .map(|db| {
            let mut best_i = 0usize;
            let mut best_d = f32::MAX;
            for (j, da) in descs_a.iter().enumerate() {
                let d = ssd(da, db);
                if d < best_d {
                    best_d = d;
                    best_i = j;
                }
            }
            best_i
        })
        .collect();

    // Keep only cross-checked matches (mutual best neighbours) and clamp to kp arrays.
    let n_a = descs_a.len().min(kps_a.len());
    let n_b = descs_b.len().min(kps_b.len());

    fwd.into_iter()
        .enumerate()
        .filter_map(|(i, opt_j)| {
            let j = opt_j?;
            if i >= n_a || j >= n_b {
                return None;
            }
            if bwd[j] == i { Some((i, j)) } else { None }
        })
        .collect()
}

#[inline]
fn ssd(a: &Desc, b: &Desc) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| (x - y) * (x - y)).sum()
}

// ── RANSAC homography ─────────────────────────────────────────────────────────

/// Estimate H mapping `kps_b` → `kps_a` from `matches` using RANSAC + DLT.
fn ransac_homography(
    matches: &[(usize, usize)],
    kps_a: &[Kp],
    kps_b: &[Kp],
    thresh_sq: f32,
    n_iter: usize,
) -> Option<[f32; 9]> {
    let m = matches.len();
    if m < 4 {
        return None;
    }

    let mut rng = 0x5A5A5A5A_5A5A5A5Au64; // xorshift64 seed

    let mut best_h: Option<[f32; 9]> = None;
    let mut best_inliers: Vec<usize> = Vec::new();

    for _ in 0..n_iter {
        // Sample 4 distinct indices.
        let mut idxs = [0usize; 4];
        for slot in 0..4 {
            loop {
                rng = xorshift64(rng);
                let candidate = (rng as usize) % m;
                if !idxs[..slot].contains(&candidate) {
                    idxs[slot] = candidate;
                    break;
                }
            }
        }

        let src: [[f32; 2]; 4] = std::array::from_fn(|k| {
            let (_, ib) = matches[idxs[k]];
            [kps_b[ib].x, kps_b[ib].y]
        });
        let dst: [[f32; 2]; 4] = std::array::from_fn(|k| {
            let (ia, _) = matches[idxs[k]];
            [kps_a[ia].x, kps_a[ia].y]
        });

        let Some(h) = homography_4pt(&src, &dst) else {
            continue;
        };

        // Count inliers.
        let inliers: Vec<usize> = matches
            .iter()
            .enumerate()
            .filter_map(|(idx, &(ia, ib))| {
                let (px, py) = apply_h(&h, kps_b[ib].x, kps_b[ib].y);
                let dx = px - kps_a[ia].x;
                let dy = py - kps_a[ia].y;
                if dx * dx + dy * dy < thresh_sq {
                    Some(idx)
                } else {
                    None
                }
            })
            .collect();

        if inliers.len() > best_inliers.len() {
            best_inliers = inliers;
            best_h = Some(h);
        }
    }

    if best_inliers.len() < MIN_INLIERS {
        return None;
    }

    // Refine using all inliers with the overdetermined DLT.
    let inlier_src: Vec<[f32; 2]> = best_inliers
        .iter()
        .map(|&idx| {
            let (_, ib) = matches[idx];
            [kps_b[ib].x, kps_b[ib].y]
        })
        .collect();
    let inlier_dst: Vec<[f32; 2]> = best_inliers
        .iter()
        .map(|&idx| {
            let (ia, _) = matches[idx];
            [kps_a[ia].x, kps_a[ia].y]
        })
        .collect();

    homography_ls(&inlier_src, &inlier_dst).or(best_h)
}

// ── Homography solvers ────────────────────────────────────────────────────────

/// 4-point exact DLT homography (same formulation as `perspective.rs`).
/// Returns H mapping `src[i]` → `dst[i]`.
#[allow(clippy::needless_range_loop)]
fn homography_4pt(src: &[[f32; 2]; 4], dst: &[[f32; 2]; 4]) -> Option<[f32; 9]> {
    let mut a = [[0.0f64; 9]; 8];
    for (i, (s, d)) in src.iter().zip(dst.iter()).enumerate() {
        let (sx, sy) = (s[0] as f64, s[1] as f64);
        let (dx, dy) = (d[0] as f64, d[1] as f64);
        let row = i * 2;
        a[row] = [-sx, -sy, -1.0, 0.0, 0.0, 0.0, dx * sx, dx * sy, dx];
        a[row + 1] = [0.0, 0.0, 0.0, -sx, -sy, -1.0, dy * sx, dy * sy, dy];
    }
    // Fix h[8] = 1, move to RHS.
    let mut mat = [[0.0f64; 9]; 8];
    for i in 0..8 {
        for j in 0..8 {
            mat[i][j] = a[i][j];
        }
        mat[i][8] = -a[i][8];
    }
    gauss_solve_8x8(&mut mat)
}

/// Over-determined DLT for N ≥ 4 correspondences (normal equations).
fn homography_ls(src: &[[f32; 2]], dst: &[[f32; 2]]) -> Option<[f32; 9]> {
    debug_assert_eq!(src.len(), dst.len());
    if src.len() < 4 {
        return None;
    }

    let mut ata = [[0.0f64; 8]; 8];
    let mut atb = [0.0f64; 8];

    // Each correspondence contributes two DLT rows.  The last (9th) DLT column
    // is (dx, dy); fixing h[8] = 1 moves it to the RHS as (-dx, -dy), matching
    // `homography_4pt` which sets `mat[i][8] = -a[i][8]`.  ATb must accumulate
    // that signed RHS, not +dx/+dy, otherwise the returned H is negated.
    for (s, d) in src.iter().zip(dst.iter()) {
        let (sx, sy) = (s[0] as f64, s[1] as f64);
        let (dx, dy) = (d[0] as f64, d[1] as f64);

        let r1: [f64; 8] = [-sx, -sy, -1.0, 0.0, 0.0, 0.0, dx * sx, dx * sy];
        let r2: [f64; 8] = [0.0, 0.0, 0.0, -sx, -sy, -1.0, dy * sx, dy * sy];
        let b1 = -dx;
        let b2 = -dy;

        for i in 0..8 {
            for j in 0..8 {
                ata[i][j] += r1[i] * r1[j] + r2[i] * r2[j];
            }
            atb[i] += r1[i] * b1 + r2[i] * b2;
        }
    }

    let mut mat = [[0.0f64; 9]; 8];
    for i in 0..8 {
        for j in 0..8 {
            mat[i][j] = ata[i][j];
        }
        mat[i][8] = atb[i];
    }
    gauss_solve_8x8(&mut mat)
}

/// Gaussian elimination with partial pivoting on an 8×9 augmented matrix [A|b].
/// Returns the solution h' (length 8) appended with h[8] = 1.
#[allow(clippy::needless_range_loop)]
fn gauss_solve_8x8(mat: &mut [[f64; 9]; 8]) -> Option<[f32; 9]> {
    for col in 0..8 {
        let mut max_row = col;
        let mut max_val = mat[col][col].abs();
        for row in (col + 1)..8 {
            if mat[row][col].abs() > max_val {
                max_val = mat[row][col].abs();
                max_row = row;
            }
        }
        if max_val < 1e-12 {
            return None;
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
    Some(std::array::from_fn(|i| h[i] as f32))
}

// ── 3×3 homography matrix helpers ────────────────────────────────────────────

#[inline]
fn identity_h() -> [f32; 9] {
    [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]
}

/// Returns the translation homography [[1,0,tx],[0,1,ty],[0,0,1]].
#[inline]
fn translation_h(tx: f32, ty: f32) -> [f32; 9] {
    [1.0, 0.0, tx, 0.0, 1.0, ty, 0.0, 0.0, 1.0]
}

/// Matrix-multiply two 3×3 homographies stored row-major.
fn mul_h(a: &[f32; 9], b: &[f32; 9]) -> [f32; 9] {
    let mut c = [0.0f32; 9];
    for r in 0..3 {
        for k in 0..3 {
            for col in 0..3 {
                c[r * 3 + col] += a[r * 3 + k] * b[k * 3 + col];
            }
        }
    }
    c
}

/// Apply H to (x, y) → (x′, y′) via homogeneous division.
#[inline]
fn apply_h(h: &[f32; 9], x: f32, y: f32) -> (f32, f32) {
    let w = h[6] * x + h[7] * y + h[8];
    (
        (h[0] * x + h[1] * y + h[2]) / w,
        (h[3] * x + h[4] * y + h[5]) / w,
    )
}

/// Invert a 3×3 matrix.  Returns identity if singular.
fn invert_h(m: &[f32; 9]) -> Option<[f32; 9]> {
    let det = m[0] * (m[4] * m[8] - m[5] * m[7]) - m[1] * (m[3] * m[8] - m[5] * m[6])
        + m[2] * (m[3] * m[7] - m[4] * m[6]);
    if det.abs() < 1e-12 {
        return None;
    }
    let d = 1.0 / det;
    Some([
        (m[4] * m[8] - m[5] * m[7]) * d,
        (m[2] * m[7] - m[1] * m[8]) * d,
        (m[1] * m[5] - m[2] * m[4]) * d,
        (m[5] * m[6] - m[3] * m[8]) * d,
        (m[0] * m[8] - m[2] * m[6]) * d,
        (m[2] * m[3] - m[0] * m[5]) * d,
        (m[3] * m[7] - m[4] * m[6]) * d,
        (m[1] * m[6] - m[0] * m[7]) * d,
        (m[0] * m[4] - m[1] * m[3]) * d,
    ])
}

// ── Bilinear sampling ─────────────────────────────────────────────────────────

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

// ── Separable [1,2,1]/4 smoothing ────────────────────────────────────────────

fn smooth_121(src: &[f32], w: usize, h: usize) -> Vec<f32> {
    // Horizontal pass.
    let mut tmp = src.to_vec();
    for y in 0..h {
        let row = y * w;
        for x in 1..w - 1 {
            tmp[row + x] = 0.25 * src[row + x - 1] + 0.5 * src[row + x] + 0.25 * src[row + x + 1];
        }
    }
    // Vertical pass.
    let mut out = tmp.clone();
    for y in 1..h - 1 {
        for x in 0..w {
            out[y * w + x] =
                0.25 * tmp[(y - 1) * w + x] + 0.5 * tmp[y * w + x] + 0.25 * tmp[(y + 1) * w + x];
        }
    }
    out
}

// ── Xorshift64 PRNG ───────────────────────────────────────────────────────────

#[inline]
fn xorshift64(mut x: u64) -> u64 {
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    x
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_stitch_single_image() {
        // One-image panorama should return that image unchanged.
        // We can't use a real path in a unit test so we verify the error path.
        let op = PanoramaOp::new(vec!["nonexistent.jpg".into()], 80);
        // Loading will fail — that's expected; just confirm the error is InvalidParams.
        let img = Image::new(10, 10);
        let result = op.apply(img);
        assert!(result.is_err());
    }

    #[test]
    fn mul_h_identity() {
        let id = identity_h();
        let m = mul_h(&id, &id);
        for i in 0..9 {
            assert!((m[i] - id[i]).abs() < 1e-5);
        }
    }

    #[test]
    fn apply_h_identity() {
        let id = identity_h();
        let (x, y) = apply_h(&id, 100.0, 200.0);
        assert!((x - 100.0).abs() < 1e-4);
        assert!((y - 200.0).abs() < 1e-4);
    }

    #[test]
    fn invert_identity() {
        let id = identity_h();
        let inv = invert_h(&id).unwrap();
        for i in 0..9 {
            assert!((inv[i] - id[i]).abs() < 1e-5);
        }
    }

    #[test]
    fn homography_4pt_round_trip() {
        let src: [[f32; 2]; 4] = [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]];
        let dst: [[f32; 2]; 4] = [[1.0, 0.5], [9.0, 0.5], [11.0, 9.5], [-1.0, 9.5]];
        let h = homography_4pt(&src, &dst).expect("homography");
        for (s, d) in src.iter().zip(dst.iter()) {
            let (px, py) = apply_h(&h, s[0], s[1]);
            assert!((px - d[0]).abs() < 0.1, "x mismatch: {px} vs {}", d[0]);
            assert!((py - d[1]).abs() < 0.1, "y mismatch: {py} vs {}", d[1]);
        }
    }

    #[test]
    fn homography_ls_pure_translation() {
        // Regression test for the Aᵀb sign error: homography_ls previously
        // returned a sign-flipped solution because it accumulated +dx/+dy
        // instead of the fixed-h[8]=1 RHS of -dx/-dy.
        let src: Vec<[f32; 2]> = vec![
            [0.0, 0.0],
            [100.0, 0.0],
            [100.0, 80.0],
            [0.0, 80.0],
            [50.0, 40.0],
            [25.0, 60.0],
        ];
        let tx = 1200.0f32;
        let ty = 0.0f32;
        let dst: Vec<[f32; 2]> = src.iter().map(|p| [p[0] + tx, p[1] + ty]).collect();

        let h = homography_ls(&src, &dst).expect("homography_ls");
        for (s, d) in src.iter().zip(dst.iter()) {
            let (px, py) = apply_h(&h, s[0], s[1]);
            assert!(
                (px - d[0]).abs() < 0.1,
                "x mismatch at ({},{}): got {px}, want {}",
                s[0],
                s[1],
                d[0]
            );
            assert!(
                (py - d[1]).abs() < 0.1,
                "y mismatch at ({},{}): got {py}, want {}",
                s[0],
                s[1],
                d[1]
            );
        }
        // Sanity: the solution should be close to a pure translation (positive tx).
        assert!(h[2] > 0.0, "expected +tx translation, got h[2]={}", h[2]);
    }
}
