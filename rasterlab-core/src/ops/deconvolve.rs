use std::sync::Mutex;

use num_complex::Complex;
use rayon::prelude::*;
use rustfft::FftPlanner;
use serde::{Deserialize, Serialize};

use crate::{
    cancel,
    error::{RasterError, RasterResult},
    image::Image,
    traits::operation::Operation,
};

// ── Kernel visualisation side-channel ────────────────────────────────────────

pub struct KernelVizData {
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

static LAST_KERNEL: Mutex<Option<KernelVizData>> = Mutex::new(None);

#[inline]
fn check_cancelled() -> RasterResult<()> {
    if cancel::is_requested() {
        Err(RasterError::Cancelled)
    } else {
        Ok(())
    }
}

pub fn take_last_kernel_viz() -> Option<KernelVizData> {
    LAST_KERNEL.lock().ok().and_then(|mut guard| guard.take())
}

fn store_kernel_viz(kernel: &Kernel) {
    let max_val = kernel.data.iter().cloned().fold(0.0f32, f32::max);
    if max_val <= 0.0 {
        return;
    }
    let pixels: Vec<u8> = kernel
        .data
        .iter()
        .map(|&v| (v / max_val * 255.0).clamp(0.0, 255.0) as u8)
        .collect();
    if let Ok(mut guard) = LAST_KERNEL.lock() {
        *guard = Some(KernelVizData {
            pixels,
            width: kernel.width as u32,
            height: kernel.height as u32,
        });
    }
}

// ── Internal buffer types ────────────────────────────────────────────────────

struct GrayBuf {
    data: Vec<f32>,
    width: usize,
    height: usize,
}

impl GrayBuf {
    fn new(width: usize, height: usize) -> Self {
        Self {
            data: vec![0.0; width * height],
            width,
            height,
        }
    }

    #[inline]
    #[allow(dead_code)]
    fn get(&self, x: usize, y: usize) -> f32 {
        self.data[y * self.width + x]
    }

    #[inline]
    fn get_clamped(&self, x: isize, y: isize) -> f32 {
        let cx = x.clamp(0, self.width as isize - 1) as usize;
        let cy = y.clamp(0, self.height as isize - 1) as usize;
        self.data[cy * self.width + cx]
    }

    #[inline]
    #[allow(dead_code)]
    fn set(&mut self, x: usize, y: usize, v: f32) {
        self.data[y * self.width + x] = v;
    }

    fn clone_buf(&self) -> Self {
        Self {
            data: self.data.clone(),
            width: self.width,
            height: self.height,
        }
    }
}

struct GradientPair {
    x: GrayBuf,
    y: GrayBuf,
}

impl GradientPair {
    fn new(x: GrayBuf, y: GrayBuf) -> Self {
        debug_assert_eq!(x.width, y.width);
        debug_assert_eq!(x.height, y.height);
        Self { x, y }
    }

    fn from_image(img: &GrayBuf) -> Self {
        Self::new(grad_x(img), grad_y(img))
    }

    #[inline]
    fn width(&self) -> usize {
        self.x.width
    }

    #[inline]
    fn height(&self) -> usize {
        self.x.height
    }
}

#[allow(dead_code)]
struct ConfidenceMap {
    data: Vec<f32>,
    width: usize,
    height: usize,
}

#[allow(dead_code)]
impl ConfidenceMap {
    fn new(width: usize, height: usize) -> Self {
        Self {
            data: vec![0.0; width * height],
            width,
            height,
        }
    }

    #[inline]
    fn get(&self, x: usize, y: usize) -> f32 {
        debug_assert!(x < self.width);
        debug_assert!(y < self.height);
        self.data[y * self.width + x]
    }
}

struct EdgeSelection {
    gradients: GradientPair,
    selected_count: usize,
}

#[allow(dead_code)]
struct KernelSupport {
    mask: Vec<bool>,
    width: usize,
    height: usize,
}

#[allow(dead_code)]
impl KernelSupport {
    fn new(width: usize, height: usize) -> Self {
        Self {
            mask: vec![false; width * height],
            width,
            height,
        }
    }

    fn selected_count(&self) -> usize {
        self.mask.iter().filter(|&&v| v).count()
    }
}

struct Kernel {
    data: Vec<f32>,
    width: usize,
    height: usize,
}

impl Kernel {
    fn new(width: usize, height: usize) -> Self {
        Self {
            data: vec![0.0; width * height],
            width,
            height,
        }
    }

    fn delta(width: usize, height: usize) -> Self {
        let mut k = Self::new(width, height);
        k.data[(height / 2) * width + width / 2] = 1.0;
        k
    }

    #[inline]
    fn get(&self, x: usize, y: usize) -> f32 {
        self.data[y * self.width + x]
    }

    fn normalize(&mut self) {
        let sum: f32 = self.data.iter().sum();
        if sum > 1e-10 {
            for v in &mut self.data {
                *v /= sum;
            }
        }
    }

    fn threshold_negative(&mut self) {
        for v in &mut self.data {
            if *v < 0.0 {
                *v = 0.0;
            }
        }
    }

    fn max_val(&self) -> f32 {
        self.data.iter().cloned().fold(0.0f32, f32::max)
    }

    fn clone_kernel(&self) -> Self {
        Self {
            data: self.data.clone(),
            width: self.width,
            height: self.height,
        }
    }
}

// ── 2D FFT via row-then-column 1D transforms ────────────────────────────────

fn fft_pad_size(signal: usize, kernel: usize) -> usize {
    (signal + kernel - 1).next_power_of_two()
}

fn gray_to_complex_padded(buf: &GrayBuf, pad_w: usize, pad_h: usize) -> Vec<Complex<f32>> {
    let mut out = vec![Complex::new(0.0, 0.0); pad_w * pad_h];
    for y in 0..buf.height.min(pad_h) {
        let src_row = &buf.data[y * buf.width..][..buf.width.min(pad_w)];
        let dst_row = &mut out[y * pad_w..][..src_row.len()];
        for (d, &s) in dst_row.iter_mut().zip(src_row) {
            *d = Complex::new(s, 0.0);
        }
    }
    out
}

fn kernel_to_complex_shifted(kernel: &Kernel, pad_w: usize, pad_h: usize) -> Vec<Complex<f32>> {
    let mut buf = vec![Complex::new(0.0, 0.0); pad_w * pad_h];
    let cx = kernel.width / 2;
    let cy = kernel.height / 2;
    for ky in 0..kernel.height {
        for kx in 0..kernel.width {
            let py = ((ky as isize - cy as isize) + pad_h as isize) as usize % pad_h;
            let px = ((kx as isize - cx as isize) + pad_w as isize) as usize % pad_w;
            buf[py * pad_w + px] = Complex::new(kernel.get(kx, ky), 0.0);
        }
    }
    buf
}

fn fft2_inplace(data: &mut [Complex<f32>], w: usize, h: usize) {
    let mut planner = FftPlanner::<f32>::new();
    let fft_row = planner.plan_fft_forward(w);
    let fft_col = planner.plan_fft_forward(h);

    // Row-wise FFT
    data.par_chunks_mut(w).for_each(|row| {
        let mut scratch = vec![Complex::new(0.0, 0.0); fft_row.get_inplace_scratch_len()];
        fft_row.process_with_scratch(row, &mut scratch);
    });

    // Column-wise FFT via transpose → row FFT → transpose
    let mut transposed = vec![Complex::new(0.0, 0.0); w * h];
    transpose(data, h, w, &mut transposed);

    transposed.par_chunks_mut(h).for_each(|row| {
        let mut scratch = vec![Complex::new(0.0, 0.0); fft_col.get_inplace_scratch_len()];
        fft_col.process_with_scratch(row, &mut scratch);
    });

    transpose(&transposed, w, h, data);
}

fn ifft2_inplace(data: &mut [Complex<f32>], w: usize, h: usize) {
    let mut planner = FftPlanner::<f32>::new();
    let ifft_row = planner.plan_fft_inverse(w);
    let ifft_col = planner.plan_fft_inverse(h);
    let norm = 1.0 / (w * h) as f32;

    // Row-wise IFFT
    data.par_chunks_mut(w).for_each(|row| {
        let mut scratch = vec![Complex::new(0.0, 0.0); ifft_row.get_inplace_scratch_len()];
        ifft_row.process_with_scratch(row, &mut scratch);
    });

    // Column-wise IFFT via transpose → row IFFT → transpose
    let mut transposed = vec![Complex::new(0.0, 0.0); w * h];
    transpose(data, h, w, &mut transposed);

    transposed.par_chunks_mut(h).for_each(|row| {
        let mut scratch = vec![Complex::new(0.0, 0.0); ifft_col.get_inplace_scratch_len()];
        ifft_col.process_with_scratch(row, &mut scratch);
    });

    transpose(&transposed, w, h, data);

    // Normalize
    data.par_iter_mut().for_each(|v| *v *= norm);
}

fn transpose(src: &[Complex<f32>], rows: usize, cols: usize, dst: &mut [Complex<f32>]) {
    // dst is cols×rows (transposed layout)
    dst.par_chunks_mut(rows)
        .enumerate()
        .for_each(|(x, out_col)| {
            for y in 0..rows {
                out_col[y] = src[y * cols + x];
            }
        });
}

// ── Image ↔ grayscale conversion ─────────────────────────────────────────────

fn image_to_gray(img: &Image) -> GrayBuf {
    let w = img.width as usize;
    let h = img.height as usize;
    let mut buf = GrayBuf::new(w, h);
    buf.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            let off = (y * w + x) * 4;
            let r = img.data[off] as f32;
            let g = img.data[off + 1] as f32;
            let b = img.data[off + 2] as f32;
            row[x] = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        }
    });
    buf
}

fn image_channel(img: &Image, ch: usize) -> GrayBuf {
    let w = img.width as usize;
    let h = img.height as usize;
    let mut buf = GrayBuf::new(w, h);
    buf.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            row[x] = img.data[(y * w + x) * 4 + ch] as f32;
        }
    });
    buf
}

fn channels_to_image(r: &GrayBuf, g: &GrayBuf, b: &GrayBuf, src: &Image) -> Image {
    let w = r.width;
    let h = r.height;
    let mut out = Image::new(w as u32, h as u32);
    out.metadata = src.metadata.clone();
    out.data
        .par_chunks_mut(w * 4)
        .enumerate()
        .for_each(|(y, row)| {
            for x in 0..w {
                let off = x * 4;
                row[off] = r.data[y * w + x].clamp(0.0, 255.0) as u8;
                row[off + 1] = g.data[y * w + x].clamp(0.0, 255.0) as u8;
                row[off + 2] = b.data[y * w + x].clamp(0.0, 255.0) as u8;
                row[off + 3] = src.data[(y * w + x) * 4 + 3];
            }
        });
    out
}

// ── Gaussian blur & pyramid ──────────────────────────────────────────────────

fn gaussian_kernel_1d(sigma: f32) -> Vec<f32> {
    let radius = (3.0 * sigma).ceil() as usize;
    let size = 2 * radius + 1;
    let mut kernel: Vec<f32> = (0..size)
        .map(|i| {
            let x = i as f32 - radius as f32;
            (-x * x / (2.0 * sigma * sigma)).exp()
        })
        .collect();
    let sum: f32 = kernel.iter().sum();
    for v in &mut kernel {
        *v /= sum;
    }
    kernel
}

fn gaussian_blur_gray(img: &GrayBuf, sigma: f32) -> GrayBuf {
    let kernel = gaussian_kernel_1d(sigma);
    let radius = kernel.len() / 2;
    let w = img.width;
    let h = img.height;

    // Horizontal pass
    let mut h_blur = GrayBuf::new(w, h);
    h_blur
        .data
        .par_chunks_mut(w)
        .enumerate()
        .for_each(|(y, row)| {
            for x in 0..w {
                let mut val = 0.0;
                for (i, &kv) in kernel.iter().enumerate() {
                    let sx = (x as isize + i as isize - radius as isize).clamp(0, w as isize - 1)
                        as usize;
                    val += img.data[y * w + sx] * kv;
                }
                row[x] = val;
            }
        });

    // Vertical pass
    let mut result = GrayBuf::new(w, h);
    result
        .data
        .par_chunks_mut(w)
        .enumerate()
        .for_each(|(y, row)| {
            for x in 0..w {
                let mut val = 0.0;
                for (i, &kv) in kernel.iter().enumerate() {
                    let sy = (y as isize + i as isize - radius as isize).clamp(0, h as isize - 1)
                        as usize;
                    val += h_blur.data[sy * w + x] * kv;
                }
                row[x] = val;
            }
        });

    result
}

fn downsample_2x(img: &GrayBuf) -> GrayBuf {
    let blurred = gaussian_blur_gray(img, 1.0);
    let new_w = img.width.div_ceil(2);
    let new_h = img.height.div_ceil(2);
    let mut out = GrayBuf::new(new_w, new_h);
    out.data
        .par_chunks_mut(new_w)
        .enumerate()
        .for_each(|(y, row)| {
            for x in 0..new_w {
                let sx = (x * 2).min(blurred.width - 1);
                let sy = (y * 2).min(blurred.height - 1);
                row[x] = blurred.data[sy * blurred.width + sx];
            }
        });
    out
}

fn build_pyramid(img: &GrayBuf, num_levels: usize) -> Vec<GrayBuf> {
    let mut levels = Vec::with_capacity(num_levels);
    let mut current = img.clone_buf();
    for _ in 0..num_levels - 1 {
        let down = downsample_2x(&current);
        levels.push(current);
        current = down;
    }
    levels.push(current);
    levels.reverse(); // index 0 = coarsest
    levels
}

fn kernel_estimation_working_image(
    img: &GrayBuf,
    kernel_size: usize,
    max_long_edge: usize,
) -> (GrayBuf, usize, usize) {
    let mut current = img.clone_buf();
    let mut scale_divisor = 1usize;
    let mut working_kernel_size = kernel_size | 1;

    while current.width.max(current.height) > max_long_edge && working_kernel_size > 3 {
        current = downsample_2x(&current);
        scale_divisor *= 2;
        working_kernel_size = ((kernel_size / scale_divisor) | 1).max(3);
    }

    (current, working_kernel_size, scale_divisor)
}

// ── Gradient computation ─────────────────────────────────────────────────────

fn grad_x(img: &GrayBuf) -> GrayBuf {
    let w = img.width;
    let h = img.height;
    let mut out = GrayBuf::new(w, h);
    out.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            let xp = (x + 1).min(w - 1);
            row[x] = img.data[y * w + xp] - img.data[y * w + x];
        }
    });
    out
}

fn grad_y(img: &GrayBuf) -> GrayBuf {
    let w = img.width;
    let h = img.height;
    let mut out = GrayBuf::new(w, h);
    out.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let yp = (y + 1).min(h - 1);
        for x in 0..w {
            row[x] = img.data[yp * w + x] - img.data[y * w + x];
        }
    });
    out
}

// ── Shock filter ─────────────────────────────────────────────────────────────

fn shock_filter(img: &GrayBuf, iterations: usize) -> GrayBuf {
    let w = img.width;
    let h = img.height;
    let mut current = img.clone_buf();

    for _ in 0..iterations {
        let mut next = GrayBuf::new(w, h);
        next.data
            .par_chunks_mut(w)
            .enumerate()
            .for_each(|(y, row)| {
                for x in 0..w {
                    let c = current.get_clamped(x as isize, y as isize);
                    let l = current.get_clamped(x as isize - 1, y as isize);
                    let r = current.get_clamped(x as isize + 1, y as isize);
                    let t = current.get_clamped(x as isize, y as isize - 1);
                    let b = current.get_clamped(x as isize, y as isize + 1);

                    let laplacian = l + r + t + b - 4.0 * c;

                    let gx = (r - l) * 0.5;
                    let gy = (b - t) * 0.5;
                    let grad_mag = (gx * gx + gy * gy).sqrt();

                    let dt = 0.5;
                    let sign = if laplacian > 0.0 {
                        1.0
                    } else if laplacian < 0.0 {
                        -1.0
                    } else {
                        0.0
                    };
                    row[x] = c - dt * sign * grad_mag;
                }
            });
        current = next;
    }
    current
}

// ── Edge selection ───────────────────────────────────────────────────────────

const EDGE_CONFIDENCE_WINDOW: usize = 7;
const EDGE_CONFIDENCE_TAU_R: f32 = 0.78;
const EDGE_DIRECTION_BINS: usize = 8;
const PHASE_ONE_ITERATIONS: usize = 5;
const PHASE_ONE_GAMMA: f32 = 10.0;
const ISD_GAMMA: f32 = 1.0;
const SPATIAL_PRIOR_LAMBDA: f32 = 2e-3;
const FAST_KERNEL_ESTIMATION_LONG_EDGE: usize = 1800;
const BALANCED_KERNEL_ESTIMATION_LONG_EDGE: usize = 3600;
const LARGE_IMAGE_PIXELS: usize = 12_000_000;
const HUGE_IMAGE_PIXELS: usize = 24_000_000;

fn integral_image(values: &[f32], width: usize, height: usize) -> Vec<f32> {
    let stride = width + 1;
    let mut integral = vec![0.0; stride * (height + 1)];
    for y in 0..height {
        let mut row_sum = 0.0;
        for x in 0..width {
            row_sum += values[y * width + x];
            let dst = (y + 1) * stride + x + 1;
            integral[dst] = integral[y * stride + x + 1] + row_sum;
        }
    }
    integral
}

#[inline]
fn integral_rect_sum(
    integral: &[f32],
    width: usize,
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
) -> f32 {
    let stride = width + 1;
    integral[y1 * stride + x1] - integral[y0 * stride + x1] - integral[y1 * stride + x0]
        + integral[y0 * stride + x0]
}

fn box_sum(values: &[f32], width: usize, height: usize, window: usize) -> Vec<f32> {
    let radius = window / 2;
    let integral = integral_image(values, width, height);
    let mut out = vec![0.0; width * height];
    out.par_chunks_mut(width).enumerate().for_each(|(y, row)| {
        let y0 = y.saturating_sub(radius);
        let y1 = (y + radius + 1).min(height);
        for x in 0..width {
            let x0 = x.saturating_sub(radius);
            let x1 = (x + radius + 1).min(width);
            row[x] = integral_rect_sum(&integral, width, x0, y0, x1, y1);
        }
    });
    out
}

fn confidence_map(blurred_gradients: &GradientPair, window: usize) -> ConfidenceMap {
    let w = blurred_gradients.width();
    let h = blurred_gradients.height();
    let gx = &blurred_gradients.x.data;
    let gy = &blurred_gradients.y.data;
    let magnitudes: Vec<f32> = gx
        .iter()
        .zip(gy.iter())
        .map(|(&x, &y)| (x * x + y * y).sqrt())
        .collect();

    let sum_x = box_sum(gx, w, h, window);
    let sum_y = box_sum(gy, w, h, window);
    let sum_mag = box_sum(&magnitudes, w, h, window);

    let mut map = ConfidenceMap::new(w, h);
    let full_window_area = (window * window) as f32;
    map.data.par_iter_mut().enumerate().for_each(|(i, r)| {
        let signed_sum = (sum_x[i] * sum_x[i] + sum_y[i] * sum_y[i]).sqrt();
        let flat_stabilizer = 1e-3 * full_window_area;
        *r = signed_sum / (sum_mag[i] + flat_stabilizer);
    });
    map
}

#[inline]
fn gradient_direction_bin(gx: f32, gy: f32) -> usize {
    let angle = gy.atan2(gx).rem_euclid(std::f32::consts::PI);
    let scaled = angle / std::f32::consts::PI * EDGE_DIRECTION_BINS as f32;
    (scaled.floor() as usize).min(EDGE_DIRECTION_BINS - 1)
}

fn percentile_threshold(mut values: Vec<f32>, keep_pct: f32) -> f32 {
    if values.is_empty() {
        return f32::INFINITY;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let keep_fraction = (keep_pct / 100.0).clamp(0.001, 1.0);
    let threshold_idx = ((1.0 - keep_fraction) * values.len() as f32) as usize;
    values[threshold_idx.min(values.len() - 1)]
}

fn direction_grouped_strength_thresholds(
    gradients: &GradientPair,
    keep_pct: f32,
    relaxation_step: usize,
) -> [f32; EDGE_DIRECTION_BINS] {
    let relaxation = 1.1f32.powi(relaxation_step as i32);
    let mut grouped: Vec<Vec<f32>> = (0..EDGE_DIRECTION_BINS).map(|_| Vec::new()).collect();
    let mut all = Vec::new();

    for (&gx, &gy) in gradients.x.data.iter().zip(gradients.y.data.iter()) {
        let mag_sq = gx * gx + gy * gy;
        if mag_sq <= 1e-8 {
            continue;
        }
        grouped[gradient_direction_bin(gx, gy)].push(mag_sq);
        all.push(mag_sq);
    }

    let global = percentile_threshold(all, keep_pct) / relaxation;
    let mut thresholds = [global; EDGE_DIRECTION_BINS];
    for (idx, values) in grouped.into_iter().enumerate() {
        if !values.is_empty() {
            thresholds[idx] = percentile_threshold(values, keep_pct) / relaxation;
        }
    }
    thresholds
}

fn select_edges(
    blurred_gradients: &GradientPair,
    latent: &GrayBuf,
    strength_keep_pct: f32,
    relaxation_step: usize,
) -> EdgeSelection {
    debug_assert_eq!(blurred_gradients.width(), latent.width);
    debug_assert_eq!(blurred_gradients.height(), latent.height);

    let w = latent.width;
    let h = latent.height;
    let confidence = confidence_map(blurred_gradients, EDGE_CONFIDENCE_WINDOW);
    let tau_r = (EDGE_CONFIDENCE_TAU_R / 1.1f32.powi(relaxation_step as i32)).clamp(0.35, 0.95);

    let shocked = shock_filter(latent, 3);
    let gradients = GradientPair::from_image(&shocked);
    let strength_thresholds =
        direction_grouped_strength_thresholds(&gradients, strength_keep_pct, relaxation_step);

    let mut gx_out = GrayBuf::new(w, h);
    let mut gy_out = GrayBuf::new(w, h);
    let mut selected_count = 0;

    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if confidence.get(x, y) < tau_r {
                continue;
            }

            let gx = gradients.x.data[i];
            let gy = gradients.y.data[i];
            let mag_sq = gx * gx + gy * gy;
            let direction = gradient_direction_bin(gx, gy);
            if mag_sq >= strength_thresholds[direction] {
                gx_out.data[i] = gx;
                gy_out.data[i] = gy;
                selected_count += 1;
            }
        }
    }

    EdgeSelection {
        gradients: GradientPair::new(gx_out, gy_out),
        selected_count,
    }
}

// ── Phase 1: kernel estimation in frequency domain ───────────────────────────

fn estimate_kernel_fft(
    grad_bx: &GrayBuf,
    grad_by: &GrayBuf,
    grad_lx: &GrayBuf,
    grad_ly: &GrayBuf,
    kernel_w: usize,
    kernel_h: usize,
    gamma: f32,
) -> Kernel {
    let img_w = grad_bx.width;
    let img_h = grad_bx.height;
    let pad_w = fft_pad_size(img_w, kernel_w);
    let pad_h = fft_pad_size(img_h, kernel_h);

    // Forward FFT all four gradient images
    let mut bx_f = gray_to_complex_padded(grad_bx, pad_w, pad_h);
    let mut by_f = gray_to_complex_padded(grad_by, pad_w, pad_h);
    let mut lx_f = gray_to_complex_padded(grad_lx, pad_w, pad_h);
    let mut ly_f = gray_to_complex_padded(grad_ly, pad_w, pad_h);

    fft2_inplace(&mut bx_f, pad_w, pad_h);
    fft2_inplace(&mut by_f, pad_w, pad_h);
    fft2_inplace(&mut lx_f, pad_w, pad_h);
    fft2_inplace(&mut ly_f, pad_w, pad_h);

    // K = (conj(Lx)*Bx + conj(Ly)*By) / (|Lx|^2 + |Ly|^2 + gamma)
    let mut k_f: Vec<Complex<f32>> = (0..pad_w * pad_h)
        .into_par_iter()
        .map(|i| {
            let num = lx_f[i].conj() * bx_f[i] + ly_f[i].conj() * by_f[i];
            let denom = lx_f[i].norm_sqr() + ly_f[i].norm_sqr() + gamma;
            num / denom
        })
        .collect();

    // Inverse FFT to get spatial kernel
    ifft2_inplace(&mut k_f, pad_w, pad_h);

    // Extract center region with fftshift
    let mut kernel = Kernel::new(kernel_w, kernel_h);
    let cx = kernel_w / 2;
    let cy = kernel_h / 2;
    for ky in 0..kernel_h {
        for kx in 0..kernel_w {
            let py = ((ky as isize - cy as isize) + pad_h as isize) as usize % pad_h;
            let px = ((kx as isize - cx as isize) + pad_w as isize) as usize % pad_w;
            kernel.data[ky * kernel_w + kx] = k_f[py * pad_w + px].re;
        }
    }

    kernel.threshold_negative();
    kernel.normalize();
    kernel
}

fn spatial_prior_latent_solve(
    blurry: &GrayBuf,
    kernel: &Kernel,
    selected_gradients: &GradientPair,
    lambda: f32,
) -> GrayBuf {
    debug_assert_eq!(blurry.width, selected_gradients.width());
    debug_assert_eq!(blurry.height, selected_gradients.height());

    let pad_w = fft_pad_size(blurry.width, kernel.width);
    let pad_h = fft_pad_size(blurry.height, kernel.height);

    let mut b_f = gray_to_complex_padded(blurry, pad_w, pad_h);
    let mut k_f = kernel_to_complex_shifted(kernel, pad_w, pad_h);
    let mut sx_f = gray_to_complex_padded(&selected_gradients.x, pad_w, pad_h);
    let mut sy_f = gray_to_complex_padded(&selected_gradients.y, pad_w, pad_h);

    fft2_inplace(&mut b_f, pad_w, pad_h);
    fft2_inplace(&mut k_f, pad_w, pad_h);
    fft2_inplace(&mut sx_f, pad_w, pad_h);
    fft2_inplace(&mut sy_f, pad_w, pad_h);

    let mut latent_f: Vec<Complex<f32>> = (0..pad_w * pad_h)
        .into_par_iter()
        .map(|i| {
            let u = i % pad_w;
            let v = i / pad_w;
            let x_phase = 2.0 * std::f32::consts::PI * u as f32 / pad_w as f32;
            let y_phase = 2.0 * std::f32::consts::PI * v as f32 / pad_h as f32;
            let dx = Complex::new(x_phase.cos() - 1.0, -x_phase.sin());
            let dy = Complex::new(y_phase.cos() - 1.0, -y_phase.sin());

            let num = k_f[i].conj() * b_f[i] + lambda * (dx.conj() * sx_f[i] + dy.conj() * sy_f[i]);
            let denom = k_f[i].norm_sqr() + lambda * (dx.norm_sqr() + dy.norm_sqr()) + 1e-6;
            num / denom
        })
        .collect();

    ifft2_inplace(&mut latent_f, pad_w, pad_h);

    let mut out = GrayBuf::new(blurry.width, blurry.height);
    out.data
        .par_chunks_mut(blurry.width)
        .enumerate()
        .for_each(|(y, row)| {
            for x in 0..blurry.width {
                row[x] = latent_f[y * pad_w + x].re;
            }
        });
    out
}

fn phase_one_kernel_initialization(
    blurry: &GrayBuf,
    initial_latent: &GrayBuf,
    initial_kernel: &Kernel,
    kernel_size: usize,
    params: &DeconvolveOp,
) -> RasterResult<(Kernel, GrayBuf)> {
    let blurry_gradients = GradientPair::from_image(blurry);
    let mut latent = initial_latent.clone_buf();
    let mut kernel = if initial_kernel.width == kernel_size && initial_kernel.height == kernel_size
    {
        initial_kernel.clone_kernel()
    } else {
        upsample_kernel(initial_kernel, kernel_size, kernel_size)
    };

    for iteration in 0..PHASE_ONE_ITERATIONS {
        check_cancelled()?;

        let edge_selection =
            select_edges(&blurry_gradients, &latent, params.edge_threshold, iteration);
        debug_assert!(edge_selection.selected_count <= blurry.width * blurry.height);

        if edge_selection.selected_count > 0 {
            kernel = estimate_kernel_fft(
                &blurry_gradients.x,
                &blurry_gradients.y,
                &edge_selection.gradients.x,
                &edge_selection.gradients.y,
                kernel_size,
                kernel_size,
                params.phase_one_gamma(),
            );
        }
        check_cancelled()?;

        latent = spatial_prior_latent_solve(
            blurry,
            &kernel,
            &edge_selection.gradients,
            SPATIAL_PRIOR_LAMBDA,
        );
    }

    Ok((kernel, latent))
}

// ── Phase 2: Iterative Support Detection ─────────────────────────────────────

fn convolve_same(signal: &GrayBuf, kernel: &Kernel) -> GrayBuf {
    let pad_w = fft_pad_size(signal.width, kernel.width);
    let pad_h = fft_pad_size(signal.height, kernel.height);
    let mut signal_f = gray_to_complex_padded(signal, pad_w, pad_h);
    let mut kernel_f = kernel_to_complex_shifted(kernel, pad_w, pad_h);

    fft2_inplace(&mut signal_f, pad_w, pad_h);
    fft2_inplace(&mut kernel_f, pad_w, pad_h);

    let mut out_f: Vec<Complex<f32>> = signal_f
        .iter()
        .zip(kernel_f.iter())
        .map(|(&a, &b)| a * b)
        .collect();
    ifft2_inplace(&mut out_f, pad_w, pad_h);

    let mut out = GrayBuf::new(signal.width, signal.height);
    out.data
        .par_chunks_mut(signal.width)
        .enumerate()
        .for_each(|(y, row)| {
            for x in 0..signal.width {
                row[x] = out_f[y * pad_w + x].re;
            }
        });
    out
}

fn kernel_cross_correlation_fft(
    grad_lx: &GrayBuf,
    grad_ly: &GrayBuf,
    grad_rx: &GrayBuf,
    grad_ry: &GrayBuf,
    kernel_w: usize,
    kernel_h: usize,
) -> Kernel {
    let pad_w = fft_pad_size(grad_lx.width, kernel_w);
    let pad_h = fft_pad_size(grad_lx.height, kernel_h);

    let mut lx_f = gray_to_complex_padded(grad_lx, pad_w, pad_h);
    let mut ly_f = gray_to_complex_padded(grad_ly, pad_w, pad_h);
    let mut rx_f = gray_to_complex_padded(grad_rx, pad_w, pad_h);
    let mut ry_f = gray_to_complex_padded(grad_ry, pad_w, pad_h);

    fft2_inplace(&mut lx_f, pad_w, pad_h);
    fft2_inplace(&mut ly_f, pad_w, pad_h);
    fft2_inplace(&mut rx_f, pad_w, pad_h);
    fft2_inplace(&mut ry_f, pad_w, pad_h);

    let mut corr_f: Vec<Complex<f32>> = (0..pad_w * pad_h)
        .into_par_iter()
        .map(|i| lx_f[i].conj() * rx_f[i] + ly_f[i].conj() * ry_f[i])
        .collect();
    ifft2_inplace(&mut corr_f, pad_w, pad_h);

    let mut kernel = Kernel::new(kernel_w, kernel_h);
    let cx = kernel_w / 2;
    let cy = kernel_h / 2;
    for ky in 0..kernel_h {
        for kx in 0..kernel_w {
            let py = ((ky as isize - cy as isize) + pad_h as isize) as usize % pad_h;
            let px = ((kx as isize - cx as isize) + pad_w as isize) as usize % pad_w;
            kernel.data[ky * kernel_w + kx] = corr_f[py * pad_w + px].re;
        }
    }
    kernel
}

fn detect_isd_support(kernel: &Kernel) -> KernelSupport {
    let mut values: Vec<f32> = kernel.data.iter().map(|v| v.max(0.0)).collect();
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let max = values.last().copied().unwrap_or(0.0);
    if max <= 1e-10 || values.len() < 2 {
        return KernelSupport::new(kernel.width, kernel.height);
    }

    let diffs: Vec<f32> = values.windows(2).map(|w| w[1] - w[0]).collect();
    let mut positive_diffs: Vec<f32> = diffs.iter().copied().filter(|d| *d > 1e-12).collect();
    positive_diffs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let floor_diff = positive_diffs.first().copied().unwrap_or(0.0);
    let significant_jump = (0.5 * floor_diff).max(max * 0.001).max(1e-7);

    let threshold = diffs
        .iter()
        .position(|&d| d >= significant_jump)
        .map(|idx| values[idx])
        .unwrap_or(max * 0.05);

    let mut support = KernelSupport::new(kernel.width, kernel.height);
    for (i, &v) in kernel.data.iter().enumerate() {
        support.mask[i] = v > threshold;
    }
    support
}

fn isd_weights(kernel: &Kernel, support: &KernelSupport) -> Vec<f32> {
    debug_assert_eq!(kernel.width, support.width);
    debug_assert_eq!(kernel.height, support.height);

    let max = kernel.max_val().max(1e-6);
    let eps = max * 1e-3;
    kernel
        .data
        .iter()
        .zip(support.mask.iter())
        .map(|(&v, &supported)| {
            if supported {
                0.0
            } else {
                1.0 / (v.abs() + eps)
            }
        })
        .collect()
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

fn apply_isd_normal_equation(
    candidate: &[f32],
    selected_gradients: &GradientPair,
    kernel_w: usize,
    kernel_h: usize,
    weights: &[f32],
    gamma: f32,
) -> Vec<f32> {
    let kernel = Kernel {
        data: candidate.to_vec(),
        width: kernel_w,
        height: kernel_h,
    };
    let pred_x = convolve_same(&selected_gradients.x, &kernel);
    let pred_y = convolve_same(&selected_gradients.y, &kernel);
    let data_grad = kernel_cross_correlation_fft(
        &selected_gradients.x,
        &selected_gradients.y,
        &pred_x,
        &pred_y,
        kernel_w,
        kernel_h,
    );

    data_grad
        .data
        .iter()
        .zip(candidate.iter())
        .zip(weights.iter())
        .map(|((&data, &k), &w)| data + gamma * w * k)
        .collect()
}

fn conjugate_gradient_isd(
    rhs: &[f32],
    initial: &[f32],
    selected_gradients: &GradientPair,
    kernel_w: usize,
    kernel_h: usize,
    weights: &[f32],
    gamma: f32,
    max_iterations: usize,
) -> RasterResult<Vec<f32>> {
    let mut x = initial.to_vec();
    let ax = apply_isd_normal_equation(&x, selected_gradients, kernel_w, kernel_h, weights, gamma);
    let mut r: Vec<f32> = rhs.iter().zip(ax.iter()).map(|(&b, &a)| b - a).collect();
    let mut p = r.clone();
    let mut rs_old = dot(&r, &r);

    if rs_old.sqrt() < 1e-6 {
        return Ok(x);
    }

    for _ in 0..max_iterations {
        check_cancelled()?;
        let ap =
            apply_isd_normal_equation(&p, selected_gradients, kernel_w, kernel_h, weights, gamma);
        let denom = dot(&p, &ap);
        if denom.abs() < 1e-12 {
            break;
        }
        let alpha = rs_old / denom;
        for i in 0..x.len() {
            x[i] += alpha * p[i];
            r[i] -= alpha * ap[i];
        }

        let rs_new = dot(&r, &r);
        if rs_new.sqrt() < 1e-4 {
            break;
        }

        let beta = rs_new / rs_old;
        for i in 0..p.len() {
            p[i] = r[i] + beta * p[i];
        }
        rs_old = rs_new;
    }

    Ok(x)
}

fn relative_kernel_change(a: &Kernel, b: &Kernel) -> f32 {
    let diff = a
        .data
        .iter()
        .zip(b.data.iter())
        .map(|(&x, &y)| {
            let d = x - y;
            d * d
        })
        .sum::<f32>()
        .sqrt();
    let norm = a.data.iter().map(|v| v * v).sum::<f32>().sqrt().max(1e-6);
    diff / norm
}

fn refine_kernel_isd(
    kernel: &Kernel,
    blurry: &GrayBuf,
    gamma: f32,
    noise_power: f32,
    edge_threshold: f32,
    iterations: usize,
) -> RasterResult<Kernel> {
    let mut k = kernel.clone_kernel();
    let kw = k.width;
    let kh = k.height;

    for iteration in 0..iterations {
        check_cancelled()?;

        let support = detect_isd_support(&k);
        if support.selected_count() == 0 {
            break;
        }
        let weights = isd_weights(&k, &support);

        // Deconvolve with the current support-aware kernel to refresh the edge set.
        let latent = wiener_deconvolve(blurry, &k, noise_power);
        check_cancelled()?;

        let blurry_gradients = GradientPair::from_image(blurry);
        let edge_selection = select_edges(&blurry_gradients, &latent, edge_threshold, iteration);
        debug_assert!(edge_selection.selected_count <= blurry.width * blurry.height);
        if edge_selection.selected_count == 0 {
            break;
        }

        let rhs = kernel_cross_correlation_fft(
            &edge_selection.gradients.x,
            &edge_selection.gradients.y,
            &blurry_gradients.x,
            &blurry_gradients.y,
            kw,
            kh,
        );

        let solved = conjugate_gradient_isd(
            &rhs.data,
            &k.data,
            &edge_selection.gradients,
            kw,
            kh,
            &weights,
            gamma,
            12,
        )?;

        let previous = k.clone_kernel();
        k.data = solved;
        k.threshold_negative();
        k.normalize();

        if relative_kernel_change(&previous, &k) < 1e-3 {
            break;
        }
    }

    k.threshold_negative();
    k.normalize();

    Ok(k)
}

// ── Wiener deconvolution (non-blind, per channel) ────────────────────────────

fn wiener_deconvolve(channel: &GrayBuf, kernel: &Kernel, noise_power: f32) -> GrayBuf {
    let pad_w = fft_pad_size(channel.width, kernel.width);
    let pad_h = fft_pad_size(channel.height, kernel.height);

    let mut b_f = gray_to_complex_padded(channel, pad_w, pad_h);
    let mut k_f = kernel_to_complex_shifted(kernel, pad_w, pad_h);

    fft2_inplace(&mut b_f, pad_w, pad_h);
    fft2_inplace(&mut k_f, pad_w, pad_h);

    // L = conj(K) * B / (|K|^2 + noise_power)
    let mut l_f: Vec<Complex<f32>> = (0..pad_w * pad_h)
        .into_par_iter()
        .map(|i| {
            let num = k_f[i].conj() * b_f[i];
            let denom = k_f[i].norm_sqr() + noise_power;
            num / denom
        })
        .collect();

    ifft2_inplace(&mut l_f, pad_w, pad_h);

    // Extract real part, crop to original size
    let mut out = GrayBuf::new(channel.width, channel.height);
    for y in 0..channel.height {
        for x in 0..channel.width {
            out.data[y * channel.width + x] = l_f[y * pad_w + x].re;
        }
    }
    out
}

// ── TV-L1 non-blind deconvolution ────────────────────────────────────────────

const TV_L1_LAMBDA: f32 = 2e-2;
const TV_L1_BETA0: f32 = 1.0;
const TV_L1_THETA0: f32 = 1.0 / TV_L1_LAMBDA;

#[inline]
fn shrink_scalar(v: f32, threshold: f32) -> f32 {
    v.signum() * (v.abs() - threshold).max(0.0)
}

fn shrink_gradient_pair(gradients: &GradientPair, threshold: f32) -> GradientPair {
    let mut wx = GrayBuf::new(gradients.width(), gradients.height());
    let mut wy = GrayBuf::new(gradients.width(), gradients.height());

    wx.data
        .par_iter_mut()
        .zip(wy.data.par_iter_mut())
        .enumerate()
        .for_each(|(i, (ox, oy))| {
            let gx = gradients.x.data[i];
            let gy = gradients.y.data[i];
            let mag = (gx * gx + gy * gy).sqrt();
            if mag > threshold {
                let scale = (mag - threshold) / mag;
                *ox = gx * scale;
                *oy = gy * scale;
            }
        });

    GradientPair::new(wx, wy)
}

fn solve_tv_l1_image(
    blurry: &GrayBuf,
    kernel: &Kernel,
    v: &GrayBuf,
    w: &GradientPair,
    beta: f32,
    theta: f32,
) -> GrayBuf {
    let pad_w = fft_pad_size(blurry.width, kernel.width);
    let pad_h = fft_pad_size(blurry.height, kernel.height);

    let mut b_plus_v = GrayBuf::new(blurry.width, blurry.height);
    b_plus_v
        .data
        .par_iter_mut()
        .enumerate()
        .for_each(|(i, out)| *out = blurry.data[i] + v.data[i]);

    let mut bv_f = gray_to_complex_padded(&b_plus_v, pad_w, pad_h);
    let mut k_f = kernel_to_complex_shifted(kernel, pad_w, pad_h);
    let mut wx_f = gray_to_complex_padded(&w.x, pad_w, pad_h);
    let mut wy_f = gray_to_complex_padded(&w.y, pad_w, pad_h);

    fft2_inplace(&mut bv_f, pad_w, pad_h);
    fft2_inplace(&mut k_f, pad_w, pad_h);
    fft2_inplace(&mut wx_f, pad_w, pad_h);
    fft2_inplace(&mut wy_f, pad_w, pad_h);

    let mut i_f: Vec<Complex<f32>> = (0..pad_w * pad_h)
        .into_par_iter()
        .map(|idx| {
            let u = idx % pad_w;
            let y = idx / pad_w;
            let x_phase = 2.0 * std::f32::consts::PI * u as f32 / pad_w as f32;
            let y_phase = 2.0 * std::f32::consts::PI * y as f32 / pad_h as f32;
            let dx = Complex::new(x_phase.cos() - 1.0, -x_phase.sin());
            let dy = Complex::new(y_phase.cos() - 1.0, -y_phase.sin());

            let num = beta * k_f[idx].conj() * bv_f[idx]
                + theta * (dx.conj() * wx_f[idx] + dy.conj() * wy_f[idx]);
            let denom = beta * k_f[idx].norm_sqr() + theta * (dx.norm_sqr() + dy.norm_sqr()) + 1e-6;
            num / denom
        })
        .collect();

    ifft2_inplace(&mut i_f, pad_w, pad_h);

    let mut out = GrayBuf::new(blurry.width, blurry.height);
    out.data
        .par_chunks_mut(blurry.width)
        .enumerate()
        .for_each(|(y, row)| {
            for x in 0..blurry.width {
                row[x] = i_f[y * pad_w + x].re;
            }
        });
    out
}

fn update_tv_l1_residual(
    latent: &GrayBuf,
    blurry: &GrayBuf,
    kernel: &Kernel,
    beta: f32,
) -> GrayBuf {
    let predicted = convolve_same(latent, kernel);
    let mut v = GrayBuf::new(blurry.width, blurry.height);
    let threshold = TV_L1_LAMBDA / beta;
    v.data.par_iter_mut().enumerate().for_each(|(i, out)| {
        let residual = predicted.data[i] - blurry.data[i];
        *out = shrink_scalar(residual, threshold);
    });
    v
}

fn tv_l1_deconvolve(
    channel: &GrayBuf,
    kernel: &Kernel,
    beta_steps: usize,
    theta_steps: usize,
) -> RasterResult<GrayBuf> {
    let mut latent = channel.clone_buf();
    let mut v = GrayBuf::new(channel.width, channel.height);
    let mut beta = TV_L1_BETA0;

    for beta_step in 0..beta_steps {
        check_cancelled()?;
        let mut theta = TV_L1_THETA0;
        for _ in 0..theta_steps {
            check_cancelled()?;
            let gradients = GradientPair::from_image(&latent);
            let w = shrink_gradient_pair(&gradients, 1.0 / theta);
            latent = solve_tv_l1_image(channel, kernel, &v, &w, beta, theta);
            theta *= 0.5;
        }
        if beta_step + 1 < beta_steps {
            v = update_tv_l1_residual(&latent, channel, kernel, beta);
        }
        beta *= 0.5;
    }

    Ok(latent)
}

// ── Edge taper (Tukey window on borders) ─────────────────────────────────────

fn edge_taper(img: &GrayBuf, taper_width: usize) -> GrayBuf {
    if taper_width == 0 {
        return img.clone_buf();
    }
    let w = img.width;
    let h = img.height;
    let mut out = img.clone_buf();
    let tw = taper_width as f32;

    out.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            let dx = (x as f32).min((w - 1 - x) as f32).min(tw);
            let dy = (y as f32).min((h - 1 - y) as f32).min(tw);
            let wx = 0.5 * (1.0 - (std::f32::consts::PI * dx / tw).cos());
            let wy = 0.5 * (1.0 - (std::f32::consts::PI * dy / tw).cos());
            row[x] *= wx * wy;
        }
    });
    out
}

// ── Kernel upsampling ────────────────────────────────────────────────────────

fn upsample_gray(img: &GrayBuf, new_w: usize, new_h: usize) -> GrayBuf {
    let mut out = GrayBuf::new(new_w, new_h);
    let sx = img.width as f32 / new_w as f32;
    let sy = img.height as f32 / new_h as f32;

    out.data
        .par_chunks_mut(new_w)
        .enumerate()
        .for_each(|(y, row)| {
            for x in 0..new_w {
                let src_x = (x as f32 + 0.5) * sx - 0.5;
                let src_y = (y as f32 + 0.5) * sy - 0.5;

                let x0 = src_x.floor() as isize;
                let y0 = src_y.floor() as isize;
                let x1 = x0 + 1;
                let y1 = y0 + 1;
                let tx = src_x - x0 as f32;
                let ty = src_y - y0 as f32;

                let sample = |sx: isize, sy: isize| -> f32 {
                    img.get_clamped(
                        sx.clamp(0, img.width as isize - 1),
                        sy.clamp(0, img.height as isize - 1),
                    )
                };

                row[x] = (1.0 - tx) * (1.0 - ty) * sample(x0, y0)
                    + tx * (1.0 - ty) * sample(x1, y0)
                    + (1.0 - tx) * ty * sample(x0, y1)
                    + tx * ty * sample(x1, y1);
            }
        });
    out
}

fn upsample_kernel(k: &Kernel, new_w: usize, new_h: usize) -> Kernel {
    let mut out = Kernel::new(new_w, new_h);
    let sx = k.width as f32 / new_w as f32;
    let sy = k.height as f32 / new_h as f32;

    for y in 0..new_h {
        for x in 0..new_w {
            let src_x = (x as f32 + 0.5) * sx - 0.5;
            let src_y = (y as f32 + 0.5) * sy - 0.5;

            let x0 = src_x.floor() as isize;
            let y0 = src_y.floor() as isize;
            let x1 = x0 + 1;
            let y1 = y0 + 1;
            let tx = src_x - x0 as f32;
            let ty = src_y - y0 as f32;

            let sample = |sx: isize, sy: isize| -> f32 {
                if sx < 0 || sy < 0 || sx >= k.width as isize || sy >= k.height as isize {
                    0.0
                } else {
                    k.get(sx as usize, sy as usize)
                }
            };

            let v = (1.0 - tx) * (1.0 - ty) * sample(x0, y0)
                + tx * (1.0 - ty) * sample(x1, y0)
                + (1.0 - tx) * ty * sample(x0, y1)
                + tx * ty * sample(x1, y1);
            out.data[y * new_w + x] = v.max(0.0);
        }
    }
    out.normalize();
    out
}

// ── Top-level blind deblur ───────────────────────────────────────────────────

fn blind_deblur(image: &Image, params: &DeconvolveOp) -> RasterResult<Image> {
    if (image.width as usize) < 4 || (image.height as usize) < 4 {
        return Ok(image.deep_clone());
    }

    let ks = params.kernel_size as usize | 1; // force odd
    let full_pixels = image.width as usize * image.height as usize;
    let full_gray = image_to_gray(image);
    let (gray, estimation_ks, estimation_scale) =
        kernel_estimation_working_image(&full_gray, ks, params.kernel_estimation_long_edge());

    // Determine pyramid levels
    let num_levels = ((estimation_ks as f32).log2().floor() as usize).max(1);

    check_cancelled()?;

    // Build Gaussian pyramid (index 0 = coarsest)
    let pyramid = build_pyramid(&gray, num_levels);
    check_cancelled()?;

    // Kernel sizes at each level (coarsest to finest)
    let kernel_sizes: Vec<usize> = (0..num_levels)
        .map(|level| {
            let scale = 1usize << (num_levels - 1 - level);
            let k = (estimation_ks / scale) | 1;
            k.max(3)
        })
        .collect();

    // Start with a delta kernel and the blurred image as the coarsest latent.
    let mut kernel = Kernel::delta(kernel_sizes[0], kernel_sizes[0]);
    let mut latent = pyramid[0].clone_buf();

    for level in 0..num_levels {
        check_cancelled()?;

        let blurry = &pyramid[level];
        let ksize = kernel_sizes[level];

        if latent.width != blurry.width || latent.height != blurry.height {
            latent = upsample_gray(&latent, blurry.width, blurry.height);
        }

        // Phase 1: run the Xu-Jia coarse estimation loop at this pyramid level.
        let phase_one = phase_one_kernel_initialization(blurry, &latent, &kernel, ksize, params)?;
        kernel = phase_one.0;
        latent = phase_one.1;
        check_cancelled()?;

        // Phase 2: ISD refinement (alternates kernel sparsification with
        // latent re-estimation so each iteration is genuinely productive)
        let tapered_for_isd = edge_taper(blurry, ksize / 2);
        kernel = refine_kernel_isd(
            &kernel,
            &tapered_for_isd,
            params.isd_gamma(),
            params.noise_power,
            params.edge_threshold,
            params.isd_iterations as usize,
        )?;

        // Upsample kernel to next level
        if level + 1 < num_levels {
            let next_ks = kernel_sizes[level + 1];
            kernel = upsample_kernel(&kernel, next_ks, next_ks);
            let next = &pyramid[level + 1];
            latent = upsample_gray(&latent, next.width, next.height);
        }
    }

    check_cancelled()?;

    if estimation_scale > 1 {
        kernel = upsample_kernel(&kernel, ks, ks);
        check_cancelled()?;
    }

    // Store kernel for visualisation
    store_kernel_viz(&kernel);

    // Final TV-L1 non-blind deconvolution per channel
    let taper_w = ks / 2;
    let ch_r = edge_taper(&image_channel(image, 0), taper_w);
    let ch_g = edge_taper(&image_channel(image, 1), taper_w);
    let ch_b = edge_taper(&image_channel(image, 2), taper_w);
    let (beta_steps, theta_steps) = params.tv_l1_schedule_for_pixels(full_pixels);

    let r = tv_l1_deconvolve(&ch_r, &kernel, beta_steps, theta_steps)?;
    check_cancelled()?;
    let g = tv_l1_deconvolve(&ch_g, &kernel, beta_steps, theta_steps)?;
    check_cancelled()?;
    let b = tv_l1_deconvolve(&ch_b, &kernel, beta_steps, theta_steps)?;
    check_cancelled()?;

    Ok(channels_to_image(&r, &g, &b, image))
}

// ── Operation ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeconvolveOp {
    pub kernel_size: u32,
    pub regularization: f32,
    pub noise_power: f32,
    pub edge_threshold: f32,
    pub isd_iterations: u32,
}

impl DeconvolveOp {
    pub fn new(
        kernel_size: u32,
        regularization: f32,
        noise_power: f32,
        edge_threshold: f32,
        isd_iterations: u32,
    ) -> Self {
        Self {
            kernel_size: (kernel_size.clamp(3, 101)) | 1,
            regularization: regularization.clamp(0.001, 10.0),
            noise_power: noise_power.clamp(0.001, 0.1),
            edge_threshold: edge_threshold.clamp(0.5, 5.0),
            isd_iterations: isd_iterations.clamp(1, 5),
        }
    }

    fn deblur_strength(&self) -> f32 {
        self.regularization.clamp(0.1, 3.0)
    }

    fn phase_one_gamma(&self) -> f32 {
        (PHASE_ONE_GAMMA / self.deblur_strength()).clamp(0.001, 50.0)
    }

    fn isd_gamma(&self) -> f32 {
        (ISD_GAMMA / self.deblur_strength()).clamp(0.001, 10.0)
    }

    fn tv_l1_schedule(&self) -> (usize, usize) {
        match self.isd_iterations {
            0 | 1 => (1, 2),
            2 | 3 => (2, 3),
            _ => (3, 4),
        }
    }

    fn tv_l1_schedule_for_pixels(&self, pixels: usize) -> (usize, usize) {
        if pixels >= HUGE_IMAGE_PIXELS && self.isd_iterations <= 1 {
            return (1, 1);
        }
        if pixels >= LARGE_IMAGE_PIXELS && self.isd_iterations <= 1 {
            return (1, 2);
        }
        self.tv_l1_schedule()
    }

    fn kernel_estimation_long_edge(&self) -> usize {
        match self.isd_iterations {
            0 | 1 => FAST_KERNEL_ESTIMATION_LONG_EDGE,
            2 | 3 => BALANCED_KERNEL_ESTIMATION_LONG_EDGE,
            _ => usize::MAX,
        }
    }
}

#[typetag::serde]
impl Operation for DeconvolveOp {
    fn name(&self) -> &'static str {
        "deconvolve"
    }

    fn clone_box(&self) -> Box<dyn Operation> {
        Box::new(self.clone())
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn apply(&self, image: Image) -> RasterResult<Image> {
        if self.kernel_size < 3 {
            return Ok(image);
        }
        blind_deblur(&image, self)
    }

    fn describe(&self) -> String {
        format!(
            "Deblur  k={} strength={:.2} noise={:.3}",
            self.kernel_size,
            self.deblur_strength(),
            self.noise_power
        )
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn small_gray(w: usize, h: usize, fill: f32) -> GrayBuf {
        GrayBuf {
            data: vec![fill; w * h],
            width: w,
            height: h,
        }
    }

    #[test]
    fn fft2_roundtrip() {
        let mut buf = GrayBuf::new(8, 8);
        for (i, v) in buf.data.iter_mut().enumerate() {
            *v = (i as f32) / 64.0;
        }
        let original = buf.data.clone();

        let pad_w = 8;
        let pad_h = 8;
        let mut freq = gray_to_complex_padded(&buf, pad_w, pad_h);
        fft2_inplace(&mut freq, pad_w, pad_h);
        ifft2_inplace(&mut freq, pad_w, pad_h);

        for (i, &expected) in original.iter().enumerate() {
            let got = freq[i].re;
            assert!(
                (got - expected).abs() < 1e-4,
                "pixel {i}: expected {expected}, got {got}"
            );
        }
    }

    #[test]
    fn fft_convolution_matches_spatial() {
        // Create a small image
        let mut img = GrayBuf::new(16, 16);
        img.data[8 * 16 + 8] = 1.0; // impulse at (8,8)

        // Create a 3x3 box kernel
        let mut kernel = Kernel::new(3, 3);
        for v in &mut kernel.data {
            *v = 1.0 / 9.0;
        }

        let pad_w = fft_pad_size(16, 3);
        let pad_h = fft_pad_size(16, 3);

        let mut img_f = gray_to_complex_padded(&img, pad_w, pad_h);
        let mut k_f = kernel_to_complex_shifted(&kernel, pad_w, pad_h);
        fft2_inplace(&mut img_f, pad_w, pad_h);
        fft2_inplace(&mut k_f, pad_w, pad_h);

        let mut result_f: Vec<Complex<f32>> =
            img_f.iter().zip(k_f.iter()).map(|(&a, &b)| a * b).collect();
        ifft2_inplace(&mut result_f, pad_w, pad_h);

        // The impulse response should be the kernel itself, centered at (8,8)
        let center_val = result_f[8 * pad_w + 8].re;
        assert!(
            (center_val - 1.0 / 9.0).abs() < 1e-4,
            "center should be ~1/9, got {center_val}"
        );
    }

    #[test]
    fn gradient_of_constant_is_zero() {
        let img = small_gray(8, 8, 42.0);
        let gx = grad_x(&img);
        let gy = grad_y(&img);
        for v in &gx.data {
            assert!(v.abs() < 1e-6);
        }
        for v in &gy.data {
            assert!(v.abs() < 1e-6);
        }
    }

    #[test]
    fn kernel_normalization() {
        let mut k = Kernel::new(5, 5);
        k.data[6] = 3.0;
        k.data[12] = 7.0;
        k.data[18] = 1.0;
        k.normalize();
        let sum: f32 = k.data.iter().sum();
        assert!((sum - 1.0).abs() < 1e-6);
        assert!(k.data.iter().all(|&v| v >= 0.0));
    }

    #[test]
    fn pyramid_dimensions() {
        let img = small_gray(64, 32, 1.0);
        let pyramid = build_pyramid(&img, 3);
        assert_eq!(pyramid.len(), 3);
        // Index 0 = coarsest
        assert_eq!(pyramid[0].width, 16);
        assert_eq!(pyramid[0].height, 8);
        assert_eq!(pyramid[1].width, 32);
        assert_eq!(pyramid[1].height, 16);
        assert_eq!(pyramid[2].width, 64);
        assert_eq!(pyramid[2].height, 32);
    }

    #[test]
    fn large_images_use_bounded_kernel_estimation_size() {
        let img = small_gray(FAST_KERNEL_ESTIMATION_LONG_EDGE + 1, 64, 128.0);
        let (working, working_kernel, scale) =
            kernel_estimation_working_image(&img, 25, FAST_KERNEL_ESTIMATION_LONG_EDGE);

        assert_eq!(scale, 2);
        assert_eq!(working.width, 901);
        assert_eq!(working.height, 32);
        assert_eq!(working_kernel, 13);
        assert!(working.width.max(working.height) <= FAST_KERNEL_ESTIMATION_LONG_EDGE);
    }

    #[test]
    fn huge_images_use_short_tv_l1_schedule() {
        let fast = DeconvolveOp::new(25, 1.0, 0.01, 1.5, 1);
        let op = DeconvolveOp::new(25, 1.0, 0.01, 2.0, 3);
        assert_eq!(fast.tv_l1_schedule_for_pixels(6970 * 4640), (1, 1));
        assert_eq!(fast.tv_l1_schedule_for_pixels(16_000_000), (1, 2));
        assert_eq!(
            op.tv_l1_schedule_for_pixels(6970 * 4640),
            op.tv_l1_schedule()
        );
        assert_eq!(op.tv_l1_schedule_for_pixels(4_000_000), op.tv_l1_schedule());
    }

    #[test]
    fn default_quality_uses_less_aggressive_kernel_downscale() {
        let op = DeconvolveOp::new(25, 1.0, 0.01, 2.0, 3);
        let img = small_gray(6970, 64, 128.0);
        let (working, working_kernel, scale) =
            kernel_estimation_working_image(&img, 25, op.kernel_estimation_long_edge());

        assert_eq!(scale, 2);
        assert_eq!(working.width, 3485);
        assert_eq!(working_kernel, 13);
    }

    #[test]
    fn shock_filter_preserves_flat() {
        let img = small_gray(16, 16, 100.0);
        let filtered = shock_filter(&img, 3);
        for v in &filtered.data {
            assert!((v - 100.0).abs() < 1e-3);
        }
    }

    #[test]
    fn confidence_selection_rejects_thin_repeated_structures() {
        let w = 72;
        let h = 32;
        let mut img = GrayBuf::new(w, h);
        for y in 0..h {
            for x in 0..w {
                img.data[y * w + x] = if x < 16 {
                    20.0
                } else if x < 34 {
                    220.0
                } else if x < 42 {
                    20.0
                } else if x % 2 == 0 {
                    220.0
                } else {
                    20.0
                };
            }
        }

        let blurred_gradients = GradientPair::from_image(&img);
        let selected = select_edges(&blurred_gradients, &img, 25.0, 0);

        let mut stable_edge_count = 0;
        let mut repeated_edge_count = 0;
        for y in 4..h - 4 {
            for x in 0..w {
                let i = y * w + x;
                let selected_here = selected.gradients.x.data[i].abs() > 1e-4
                    || selected.gradients.y.data[i].abs() > 1e-4;
                if !selected_here {
                    continue;
                }
                if (14..=17).contains(&x) {
                    stable_edge_count += 1;
                } else if (44..=66).contains(&x) {
                    repeated_edge_count += 1;
                }
            }
        }

        assert!(
            stable_edge_count > 0,
            "the large stable step edge should survive confidence selection"
        );
        assert!(
            repeated_edge_count * 2 < stable_edge_count,
            "thin repeated stripe edges should be rejected more aggressively: stable={stable_edge_count}, repeated={repeated_edge_count}"
        );
    }

    #[test]
    fn edge_threshold_relaxation_selects_more_gradients() {
        let w = 40;
        let h = 24;
        let mut img = GrayBuf::new(w, h);
        for y in 0..h {
            for x in 0..w {
                img.data[y * w + x] = if x < 12 {
                    20.0
                } else if x < 24 {
                    180.0
                } else {
                    220.0
                };
            }
        }

        let blurred_gradients = GradientPair::from_image(&img);
        let initial = select_edges(&blurred_gradients, &img, 2.0, 0);
        let relaxed = select_edges(&blurred_gradients, &img, 2.0, 3);

        assert!(
            relaxed.selected_count >= initial.selected_count,
            "dividing tau_r and tau_s by 1.1 each iteration should not make selection stricter"
        );
    }

    #[test]
    fn phase_one_initialization_preserves_kernel_and_latent_shapes() {
        let w = 24;
        let h = 24;
        let mut blurry = GrayBuf::new(w, h);
        for y in 0..h {
            for x in 0..w {
                blurry.data[y * w + x] = if x < w / 2 { 30.0 } else { 210.0 };
            }
        }

        let params = DeconvolveOp::new(5, 2.0, 0.01, 5.0, 1);
        let initial_kernel = Kernel::delta(5, 5);
        let (kernel, latent) =
            phase_one_kernel_initialization(&blurry, &blurry, &initial_kernel, 5, &params).unwrap();

        assert_eq!(kernel.width, 5);
        assert_eq!(kernel.height, 5);
        assert_eq!(latent.width, w);
        assert_eq!(latent.height, h);
        assert!(kernel.data.iter().all(|v| v.is_finite() && *v >= 0.0));

        let kernel_sum: f32 = kernel.data.iter().sum();
        assert!(
            (kernel_sum - 1.0).abs() < 1e-3,
            "phase-one kernel should stay normalized, got sum {kernel_sum}"
        );
    }

    #[test]
    fn spatial_prior_solve_preserves_selected_step_edge_better_than_wiener() {
        let w = 64;
        let h = 32;
        let mut clean = GrayBuf::new(w, h);
        for y in 0..h {
            for x in 0..w {
                clean.data[y * w + x] = if x < w / 2 { 25.0 } else { 225.0 };
            }
        }

        let mut kernel = Kernel::new(7, 7);
        for x in 1..6 {
            kernel.data[3 * 7 + x] = 1.0;
        }
        kernel.normalize();

        let pad_w = fft_pad_size(w, kernel.width);
        let pad_h = fft_pad_size(h, kernel.height);
        let mut clean_f = gray_to_complex_padded(&clean, pad_w, pad_h);
        let mut k_f = kernel_to_complex_shifted(&kernel, pad_w, pad_h);
        fft2_inplace(&mut clean_f, pad_w, pad_h);
        fft2_inplace(&mut k_f, pad_w, pad_h);
        let mut blurred_f: Vec<Complex<f32>> = clean_f
            .iter()
            .zip(k_f.iter())
            .map(|(&a, &b)| a * b)
            .collect();
        ifft2_inplace(&mut blurred_f, pad_w, pad_h);

        let mut blurred = GrayBuf::new(w, h);
        for y in 0..h {
            for x in 0..w {
                blurred.data[y * w + x] = blurred_f[y * pad_w + x].re;
            }
        }

        let selected_gradients = GradientPair::from_image(&clean);
        let spatial = spatial_prior_latent_solve(
            &blurred,
            &kernel,
            &selected_gradients,
            SPATIAL_PRIOR_LAMBDA,
        );
        let wiener = wiener_deconvolve(&blurred, &kernel, 0.05);

        let mean_contrast = |img: &GrayBuf| -> f32 {
            let mut left = 0.0;
            let mut right = 0.0;
            let mut count = 0;
            for y in 6..h - 6 {
                left += img.data[y * w + (w / 2 - 1)];
                right += img.data[y * w + (w / 2 + 1)];
                count += 1;
            }
            (right - left).abs() / count as f32
        };

        let spatial_contrast = mean_contrast(&spatial);
        let wiener_contrast = mean_contrast(&wiener);
        assert!(
            spatial_contrast > wiener_contrast,
            "selected-edge spatial prior should preserve the step edge better than plain Wiener: spatial={spatial_contrast}, wiener={wiener_contrast}"
        );
    }

    #[test]
    fn tv_l1_deconvolution_reduces_impulse_noise_amplification() {
        let w = 32;
        let h = 32;
        let mut noisy = GrayBuf::new(w, h);
        noisy.data.fill(100.0);
        noisy.data[(h / 2) * w + w / 2] = 255.0;

        let kernel = Kernel::delta(3, 3);
        let wiener = wiener_deconvolve(&noisy, &kernel, 0.001);
        let tv_l1 = tv_l1_deconvolve(&noisy, &kernel, 2, 3).unwrap();

        let max_deviation = |img: &GrayBuf| -> f32 {
            img.data
                .iter()
                .map(|v| (v - 100.0).abs())
                .fold(0.0f32, f32::max)
        };

        assert!(
            max_deviation(&tv_l1) < max_deviation(&wiener),
            "TV-L1 should damp impulse noise more than plain Wiener"
        );
    }

    #[test]
    fn isd_support_detection_uses_first_significant_jump() {
        let mut kernel = Kernel::new(7, 7);
        for v in &mut kernel.data {
            *v = 0.0001;
        }
        kernel.data[3 * 7 + 2] = 0.0040;
        kernel.data[3 * 7 + 3] = 0.0045;
        kernel.data[3 * 7 + 4] = 0.08;

        let support = detect_isd_support(&kernel);

        assert!(support.mask[3 * 7 + 2]);
        assert!(support.mask[3 * 7 + 3]);
        assert!(support.mask[3 * 7 + 4]);
        assert!(
            support.selected_count() < kernel.data.len() / 2,
            "support should remain sparse"
        );
    }

    #[test]
    fn isd_support_preserves_small_coherent_values_hard_threshold_would_drop() {
        let mut kernel = Kernel::new(5, 5);
        for v in &mut kernel.data {
            *v = 0.00005;
        }
        kernel.data[12] = 0.1;
        kernel.data[13] = 0.004;

        let hard_threshold = kernel.max_val() * 0.05;
        assert!(
            kernel.data[13] < hard_threshold,
            "fixture should put the coherent small value below the old hard threshold"
        );

        let support = detect_isd_support(&kernel);
        assert!(
            support.mask[13],
            "ISD support should keep the small coherent value"
        );
    }

    #[test]
    fn delta_kernel_preserves_image() {
        let mut src = Image::new(16, 16);
        for (i, chunk) in src.data.chunks_mut(4).enumerate() {
            let v = (i % 256) as u8;
            chunk[0] = v;
            chunk[1] = v;
            chunk[2] = v;
            chunk[3] = 255;
        }

        let op = DeconvolveOp::new(3, 2.0, 0.01, 2.0, 1);
        let result = op.apply(src.deep_clone()).unwrap();
        assert_eq!(result.width, src.width);
        assert_eq!(result.height, src.height);
    }

    #[test]
    fn deconvolve_op_constructor_preserves_public_contract() {
        let op = DeconvolveOp::new(24, 100.0, 100.0, 100.0, 100);
        assert_eq!(op.kernel_size, 25);
        assert_eq!(op.regularization, 10.0);
        assert_eq!(op.noise_power, 0.1);
        assert_eq!(op.edge_threshold, 5.0);
        assert_eq!(op.isd_iterations, 5);

        let op = DeconvolveOp::new(0, 0.0, 0.0, 0.0, 0);
        assert_eq!(op.kernel_size, 3);
        assert_eq!(op.regularization, 0.001);
        assert_eq!(op.noise_power, 0.001);
        assert_eq!(op.edge_threshold, 0.5);
        assert_eq!(op.isd_iterations, 1);
    }

    #[test]
    fn deconvolve_op_serializes_through_operation_trait() {
        let op: Box<dyn Operation> = Box::new(DeconvolveOp::new(25, 2.5, 0.02, 1.5, 4));
        assert_eq!(op.name(), "deconvolve");

        let json = serde_json::to_string(&op).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(value.get("type").and_then(|v| v.as_str()).is_some());

        let restored: Box<dyn Operation> = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.name(), "deconvolve");
        let restored = restored
            .as_any()
            .and_then(|a| a.downcast_ref::<DeconvolveOp>())
            .unwrap();
        assert_eq!(restored.kernel_size, 25);
        assert_eq!(restored.regularization, 2.5);
        assert_eq!(restored.noise_power, 0.02);
        assert_eq!(restored.edge_threshold, 1.5);
        assert_eq!(restored.isd_iterations, 4);
    }

    #[test]
    fn deconvolve_op_clone_box_preserves_parameters() {
        let op = DeconvolveOp::new(31, 1.25, 0.03, 2.5, 3);
        let cloned = op.clone_box();
        let cloned = cloned
            .as_any()
            .and_then(|a| a.downcast_ref::<DeconvolveOp>())
            .unwrap();
        assert_eq!(cloned.kernel_size, 31);
        assert_eq!(cloned.regularization, 1.25);
        assert_eq!(cloned.noise_power, 0.03);
        assert_eq!(cloned.edge_threshold, 2.5);
        assert_eq!(cloned.isd_iterations, 3);
    }

    #[test]
    fn alpha_preserved() {
        let mut src = Image::new(8, 8);
        for chunk in src.data.chunks_mut(4) {
            chunk[0] = 128;
            chunk[1] = 128;
            chunk[2] = 128;
            chunk[3] = 42;
        }
        let op = DeconvolveOp::new(3, 2.0, 0.01, 2.0, 1);
        let result = op.apply(src).unwrap();
        for chunk in result.data.chunks(4) {
            assert_eq!(chunk[3], 42);
        }
    }

    #[test]
    fn output_dimensions_match() {
        let src = Image::new(37, 23);
        let op = DeconvolveOp::new(5, 2.0, 0.01, 2.0, 1);
        let result = op.apply(src).unwrap();
        assert_eq!(result.width, 37);
        assert_eq!(result.height, 23);
    }

    #[test]
    fn wiener_with_known_kernel() {
        // Use a Gaussian blur kernel (no zeros in its spectrum, so Wiener
        // deconvolution can recover the signal cleanly).
        let w = 64;
        let h = 64;
        let mut clean = GrayBuf::new(w, h);
        for y in 0..h {
            for x in 0..w {
                clean.data[y * w + x] = (x as f32 / w as f32) * 200.0 + 28.0;
            }
        }

        let mut kernel = Kernel::new(5, 5);
        for ky in 0..5 {
            for kx in 0..5 {
                let dx = kx as f32 - 2.0;
                let dy = ky as f32 - 2.0;
                kernel.data[ky * 5 + kx] = (-0.5 * (dx * dx + dy * dy)).exp();
            }
        }
        kernel.normalize();

        // Blur via FFT convolution
        let pad_w = fft_pad_size(w, 5);
        let pad_h = fft_pad_size(h, 5);
        let mut clean_f = gray_to_complex_padded(&clean, pad_w, pad_h);
        let mut k_f = kernel_to_complex_shifted(&kernel, pad_w, pad_h);
        fft2_inplace(&mut clean_f, pad_w, pad_h);
        fft2_inplace(&mut k_f, pad_w, pad_h);

        let mut blurred_f: Vec<Complex<f32>> = clean_f
            .iter()
            .zip(k_f.iter())
            .map(|(&a, &b)| a * b)
            .collect();
        ifft2_inplace(&mut blurred_f, pad_w, pad_h);

        let mut blurred = GrayBuf::new(w, h);
        for y in 0..h {
            for x in 0..w {
                blurred.data[y * w + x] = blurred_f[y * pad_w + x].re;
            }
        }

        // Deconvolve with the known kernel
        let recovered = wiener_deconvolve(&blurred, &kernel, 0.001);

        // Interior pixels should be close to original (avoid borders)
        let mut total_err = 0.0f32;
        let mut count = 0;
        for y in 5..h - 5 {
            for x in 5..w - 5 {
                let err = (recovered.data[y * w + x] - clean.data[y * w + x]).abs();
                total_err += err;
                count += 1;
            }
        }
        let mean_err = total_err / count as f32;
        assert!(
            mean_err < 5.0,
            "mean absolute error should be small, got {mean_err}"
        );
    }
}
