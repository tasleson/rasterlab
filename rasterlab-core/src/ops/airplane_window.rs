use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{error::RasterResult, image::Image, traits::operation::Operation};

/// Reduce haze, cool cast, and coloured reflection patches from photos shot
/// through plexiglass aircraft windows.
///
/// The pass is intentionally conservative: broad tonal/color corrections are
/// estimated from the image, while reflection repair works mostly in chroma so
/// mountain/cloud detail carried by luminance is preserved.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AirplaneWindowCorrectionOp {
    /// Overall blend of corrected result over the source. `0.0..=1.0`.
    pub strength: f32,
    /// Automatic white/cool cast removal. `0.0..=1.0`.
    pub cast_removal: f32,
    /// Luminance dehaze/local contrast amount. `0.0..=1.0`.
    pub haze_reduction: f32,
    /// Chroma-only repair of local magenta/cyan/green reflection patches. `0.0..=1.0`.
    pub reflection_repair: f32,
}

impl AirplaneWindowCorrectionOp {
    pub fn new(
        strength: f32,
        cast_removal: f32,
        haze_reduction: f32,
        reflection_repair: f32,
    ) -> Self {
        Self {
            strength: strength.clamp(0.0, 1.0),
            cast_removal: cast_removal.clamp(0.0, 1.0),
            haze_reduction: haze_reduction.clamp(0.0, 1.0),
            reflection_repair: reflection_repair.clamp(0.0, 1.0),
        }
    }
}

impl Default for AirplaneWindowCorrectionOp {
    fn default() -> Self {
        Self::new(0.85, 0.80, 0.45, 0.85)
    }
}

#[typetag::serde]
impl Operation for AirplaneWindowCorrectionOp {
    fn name(&self) -> &'static str {
        "airplane_window"
    }

    fn clone_box(&self) -> Box<dyn Operation> {
        Box::new(self.clone())
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn apply(&self, image: Image) -> RasterResult<Image> {
        if self.strength <= 0.0 {
            return Ok(image);
        }

        let w = image.width as usize;
        let h = image.height as usize;
        if w == 0 || h == 0 {
            return Ok(image);
        }

        let gains = estimate_cast_gains(&image, self.cast_removal);

        let mut y = vec![0.0f32; w * h];
        let mut u = vec![0.0f32; w * h];
        let mut v = vec![0.0f32; w * h];
        image
            .data
            .par_chunks_exact(4)
            .zip(y.par_iter_mut())
            .zip(u.par_iter_mut())
            .zip(v.par_iter_mut())
            .for_each(|(((px, y_out), u_out), v_out)| {
                let r = (px[0] as f32 / 255.0 * gains[0]).clamp(0.0, 1.0);
                let g = (px[1] as f32 / 255.0 * gains[1]).clamp(0.0, 1.0);
                let b = (px[2] as f32 / 255.0 * gains[2]).clamp(0.0, 1.0);

                *y_out = lum(r, g, b);
                *u_out = r - g;
                *v_out = b - 0.5 * (r + g);
            });

        if self.haze_reduction > 0.0 {
            enhance_luminance(&mut y, w, h, self.haze_reduction);
        }

        if self.reflection_repair > 0.0 {
            repair_reflection_chroma(&mut u, &mut v, &y, w, h, self.reflection_repair);
        }

        let mut out = Image::new(image.width, image.height);
        out.metadata = image.metadata.clone();
        let strength = self.strength;
        out.data
            .par_chunks_exact_mut(4)
            .enumerate()
            .for_each(|(i, px)| {
                let (cr, cg, cb) = opponent_to_rgb(y[i], u[i], v[i]);
                let src = &image.data[i * 4..i * 4 + 4];
                px[0] = blend_u8(src[0], cr, strength);
                px[1] = blend_u8(src[1], cg, strength);
                px[2] = blend_u8(src[2], cb, strength);
                px[3] = src[3];
            });

        Ok(out)
    }

    fn describe(&self) -> String {
        format!(
            "Airplane Window  strength {:.0}%  cast {:.0}%  haze {:.0}%  reflections {:.0}%",
            self.strength * 100.0,
            self.cast_removal * 100.0,
            self.haze_reduction * 100.0,
            self.reflection_repair * 100.0
        )
    }
}

fn estimate_cast_gains(image: &Image, amount: f32) -> [f32; 3] {
    if amount <= 0.0 {
        return [1.0, 1.0, 1.0];
    }

    let mut samples = Vec::new();
    for px in image.data.chunks_exact(4).step_by(8) {
        let r = px[0] as f32 / 255.0;
        let g = px[1] as f32 / 255.0;
        let b = px[2] as f32 / 255.0;
        let l = lum(r, g, b);
        let max_c = r.max(g).max(b).max(1e-5);
        let min_c = r.min(g).min(b);
        let sat = (max_c - min_c) / max_c;
        if l > 0.45 && sat < 0.35 {
            samples.push((l, r, g, b));
        }
    }

    if samples.len() < 64 {
        return [1.0, 1.0, 1.0];
    }

    samples.sort_by(|a, b| a.0.total_cmp(&b.0));
    let start = samples.len() * 55 / 100;
    let end = (samples.len() * 95 / 100).max(start + 1);

    let mut sums = [0.0f32; 3];
    let mut count = 0.0f32;
    for &(_, r, g, b) in &samples[start..end] {
        sums[0] += r;
        sums[1] += g;
        sums[2] += b;
        count += 1.0;
    }
    let avg = [
        sums[0] / count.max(1.0),
        sums[1] / count.max(1.0),
        sums[2] / count.max(1.0),
    ];
    let target = (avg[0] + avg[1] + avg[2]) / 3.0;
    [
        lerp(1.0, (target / avg[0].max(1e-4)).clamp(0.75, 1.35), amount),
        lerp(1.0, (target / avg[1].max(1e-4)).clamp(0.75, 1.35), amount),
        lerp(1.0, (target / avg[2].max(1e-4)).clamp(0.75, 1.35), amount),
    ]
}

fn enhance_luminance(y: &mut [f32], w: usize, h: usize, amount: f32) {
    let (black, white) = luminance_percentiles(y, 0.006, 0.992);
    let range = (white - black).max(0.08);

    y.par_iter_mut().for_each(|l| {
        let stretched = ((*l - black) / range).clamp(0.0, 1.0);
        *l = lerp(*l, stretched, amount * 0.55);
    });

    let radius = ((w.min(h) as f32 * 0.025).round() as usize).max(8);
    let blur = box_blur_1ch(y, w, h, radius);
    y.par_iter_mut().zip(blur.par_iter()).for_each(|(l, b)| {
        let detail = *l - *b;
        let midtone = 4.0 * *l * (1.0 - *l);
        *l = (*l + detail * midtone * amount * 1.15).clamp(0.0, 1.0);
    });
}

fn repair_reflection_chroma(
    u: &mut [f32],
    v: &mut [f32],
    y: &[f32],
    w: usize,
    h: usize,
    amount: f32,
) {
    let radius = ((w.min(h) as f32 * 0.085).round() as usize).max(24);
    let smooth_u = box_blur_1ch(u, w, h, radius);
    let smooth_v = box_blur_1ch(v, w, h, radius);

    u.par_iter_mut()
        .zip(v.par_iter_mut())
        .zip(smooth_u.par_iter())
        .zip(smooth_v.par_iter())
        .zip(y.par_iter())
        .for_each(|((((u_px, v_px), su), sv), l)| {
            let du = *u_px - *su;
            let dv = *v_px - *sv;
            let residual = (du * du + dv * dv).sqrt();
            let chroma = (*u_px * *u_px + *v_px * *v_px).sqrt();

            let anomaly = smoothstep(0.010, 0.075, residual) * smoothstep(0.018, 0.14, chroma);
            let snow_or_haze = smoothstep(0.18, 0.92, *l);
            let repair = (anomaly * snow_or_haze * amount).clamp(0.0, 1.0);

            *u_px = lerp(*u_px, *su, repair);
            *v_px = lerp(*v_px, *sv, repair);

            let colored_window_reflection =
                smoothstep(0.012, 0.075, (*u_px).abs()) * smoothstep(0.025, 0.15, chroma);
            let chroma_damp =
                (colored_window_reflection * snow_or_haze * amount * 0.72).clamp(0.0, 0.72);
            *u_px = lerp(*u_px, 0.0, chroma_damp);
            *v_px = lerp(*v_px, *sv * 0.35, chroma_damp);
        });
}

fn luminance_percentiles(y: &[f32], black_p: f32, white_p: f32) -> (f32, f32) {
    let mut hist = [0u32; 1024];
    for &l in y {
        let idx = (l.clamp(0.0, 1.0) * 1023.0).round() as usize;
        hist[idx] += 1;
    }

    let total = y.len() as u32;
    let black_target = (total as f32 * black_p).round() as u32;
    let white_target = (total as f32 * white_p).round() as u32;

    let mut accum = 0u32;
    let mut black = 0.0;
    let mut white = 1.0;
    for (i, &count) in hist.iter().enumerate() {
        accum += count;
        if accum >= black_target {
            black = i as f32 / 1023.0;
            break;
        }
    }
    accum = 0;
    for (i, &count) in hist.iter().enumerate() {
        accum += count;
        if accum >= white_target {
            white = i as f32 / 1023.0;
            break;
        }
    }
    (black, white)
}

fn box_blur_1ch(src: &[f32], w: usize, h: usize, radius: usize) -> Vec<f32> {
    let mut buf = src.to_vec();
    for _ in 0..3 {
        box_blur_h_1ch(&mut buf, w, radius);
        box_blur_v_1ch(&mut buf, w, h, radius);
    }
    buf
}

fn box_blur_h_1ch(buf: &mut [f32], w: usize, radius: usize) {
    buf.par_chunks_mut(w).for_each(|row| {
        let mut out = vec![0.0f32; w];
        let mut sum = 0.0f32;
        for &v in row.iter().take(radius.min(w - 1) + 1) {
            sum += v;
        }
        let mut count = (radius.min(w - 1) + 1) as f32;
        for x in 0..w {
            out[x] = sum / count;
            if x + radius + 1 < w {
                sum += row[x + radius + 1];
                count += 1.0;
            }
            if x >= radius {
                sum -= row[x - radius];
                count -= 1.0;
            }
        }
        row.copy_from_slice(&out);
    });
}

fn box_blur_v_1ch(buf: &mut [f32], w: usize, h: usize, radius: usize) {
    const STRIP: usize = 16;
    let n_strips = w.div_ceil(STRIP);
    let raw = buf.as_mut_ptr() as usize;

    (0..n_strips).into_par_iter().for_each(|s| {
        let x0 = s * STRIP;
        let sw = STRIP.min(w - x0);
        let p = raw as *mut f32;

        let mut src = vec![0.0f32; h * sw];
        for y in 0..h {
            for dx in 0..sw {
                unsafe {
                    src[y * sw + dx] = *p.add(y * w + x0 + dx);
                }
            }
        }

        let mut out = vec![0.0f32; h * sw];
        let mut sums = vec![0.0f32; sw];
        for y in 0..=radius.min(h - 1) {
            for dx in 0..sw {
                sums[dx] += src[y * sw + dx];
            }
        }
        let mut count = (radius.min(h - 1) + 1) as f32;
        for y in 0..h {
            for dx in 0..sw {
                out[y * sw + dx] = sums[dx] / count;
            }
            if y + radius + 1 < h {
                for dx in 0..sw {
                    sums[dx] += src[(y + radius + 1) * sw + dx];
                }
                count += 1.0;
            }
            if y >= radius {
                for dx in 0..sw {
                    sums[dx] -= src[(y - radius) * sw + dx];
                }
                count -= 1.0;
            }
        }

        for y in 0..h {
            for dx in 0..sw {
                unsafe {
                    *p.add(y * w + x0 + dx) = out[y * sw + dx];
                }
            }
        }
    });
}

#[inline]
fn lum(r: f32, g: f32, b: f32) -> f32 {
    0.299 * r + 0.587 * g + 0.114 * b
}

#[inline]
fn opponent_to_rgb(y: f32, u: f32, v: f32) -> (f32, f32, f32) {
    let g = y - 0.356 * u - 0.114 * v;
    let r = g + u;
    let b = g + 0.5 * u + v;
    (r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0))
}

#[inline]
fn blend_u8(src: u8, corrected: f32, amount: f32) -> u8 {
    let c = corrected * 255.0;
    (src as f32 + (c - src as f32) * amount)
        .round()
        .clamp(0.0, 255.0) as u8
}

#[inline]
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

#[inline]
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_with_zero_strength() {
        let mut src = Image::new(8, 8);
        for (i, p) in src.data.chunks_mut(4).enumerate() {
            p[0] = (i * 3 % 255) as u8;
            p[1] = (i * 7 % 255) as u8;
            p[2] = (i * 11 % 255) as u8;
            p[3] = 99;
        }
        let out = AirplaneWindowCorrectionOp::new(0.0, 1.0, 1.0, 1.0)
            .apply(src.deep_clone())
            .unwrap();
        assert_eq!(out.data, src.data);
    }

    #[test]
    fn alpha_is_preserved() {
        let mut src = Image::new(16, 16);
        for p in src.data.chunks_mut(4) {
            p[0] = 120;
            p[1] = 140;
            p[2] = 180;
            p[3] = 17;
        }
        let out = AirplaneWindowCorrectionOp::default().apply(src).unwrap();
        assert!(out.data.chunks(4).all(|p| p[3] == 17));
    }

    #[test]
    fn blue_cast_on_neutral_patch_is_reduced() {
        let mut src = Image::new(32, 32);
        for p in src.data.chunks_mut(4) {
            p[0] = 140;
            p[1] = 150;
            p[2] = 190;
            p[3] = 255;
        }
        let out = AirplaneWindowCorrectionOp::new(1.0, 1.0, 0.0, 0.0)
            .apply(src)
            .unwrap();
        let p = out.pixel(0, 0);
        assert!(p[2] - p[0] < 50, "blue cast should be reduced: {p:?}");
    }
}
