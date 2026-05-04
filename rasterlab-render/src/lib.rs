//! # rasterlab-render
//!
//! Background render thread and pipeline execution extracted from the GUI crate.
//! No `egui`/`eframe` dependency — rendering can be tested headlessly.

use std::sync::{Arc, mpsc};

use rasterlab_core::{
    Image, cancel as core_cancel, ops::HistogramData, traits::operation::Operation,
};
#[cfg(feature = "gpu")]
use rasterlab_gpu::GpuContext;
use rayon::prelude::*;

/// Linear scale factor used for the fast downsampled preview.
/// 0.25 = 1/4 width × 1/4 height = 1/16 the pixels → ~16× faster ops.
pub const PREVIEW_SCALE: f32 = 0.25;

/// Result of a completed render operation.
pub enum RenderResult {
    Complete {
        image: Arc<Image>,
        hist: Box<HistogramData>,
        intermediates: Vec<Arc<Image>>,
        start_index: usize,
        cache_gen: u64,
        is_preview: bool,
        overlay_rect: Option<[u32; 4]>,
    },
    Error(String),
    Cancelled,
}

/// Parameters for a render request.
pub struct RenderRequest {
    pub start_image: Arc<Image>,
    pub committed_ops: Vec<Option<Box<dyn Operation>>>,
    pub preview_op: Option<Box<dyn Operation>>,
    pub preview_scale: Option<f32>,
    pub preview_viewport: Option<[u32; 4]>,
    pub overlay_viewport: Option<[u32; 4]>,
    #[cfg(feature = "gpu")]
    pub gpu: Option<Arc<GpuContext>>,
}

/// Metadata carried alongside the render for result routing.
pub struct RenderMeta {
    pub start_index: usize,
    pub cache_gen: u64,
    pub is_preview: bool,
}

/// Spawn a render on a background thread.
///
/// Sends exactly one `RenderResult` on completion via `tx`, then calls
/// `repaint` so the host UI can wake up. The thread is named
/// `"rasterlab-render"` and gets a 32 MiB stack.
pub fn spawn_render<M>(
    request: RenderRequest,
    meta: RenderMeta,
    tx: mpsc::Sender<M>,
    repaint: Arc<dyn Fn() + Send + Sync>,
) where
    M: From<RenderResult> + Send + 'static,
{
    std::thread::Builder::new()
        .name("rasterlab-render".into())
        .stack_size(32 * 1024 * 1024)
        .spawn(move || {
            let result = render_pipeline(
                request.start_image,
                request.committed_ops,
                request.preview_op,
                request.preview_scale,
                request.preview_viewport,
                request.overlay_viewport,
                #[cfg(feature = "gpu")]
                request.gpu,
            );
            let msg = match result {
                Ok((image, hist, intermediates, overlay_rect)) => RenderResult::Complete {
                    image,
                    hist: Box::new(hist),
                    intermediates,
                    start_index: meta.start_index,
                    cache_gen: meta.cache_gen,
                    is_preview: meta.is_preview,
                    overlay_rect,
                },
                Err(e) => {
                    if core_cancel::is_requested() {
                        RenderResult::Cancelled
                    } else {
                        RenderResult::Error(e)
                    }
                }
            };
            let _ = tx.send(M::from(msg));
            repaint();
        })
        .expect("failed to spawn render thread");
}

/// Initialize the global rayon thread pool with 32 MiB stack per worker.
pub fn init_rayon_pool() {
    rayon::ThreadPoolBuilder::new()
        .stack_size(32 * 1024 * 1024)
        .build_global()
        .expect("failed to build rayon thread pool");
}

/// Find the black and white points for auto-levels by clipping the histogram
/// at `lo_pct` and `hi_pct` percentiles of the cumulative pixel count.
/// Returns `(black, white)` as fractions in `[0.0, 1.0]`.
pub fn percentile_levels(hist: &[u64; 256], lo_pct: f64, hi_pct: f64) -> (f32, f32) {
    let total: u64 = hist.iter().sum();
    if total == 0 {
        return (0.0, 1.0);
    }
    let lo_target = ((total as f64 * lo_pct).ceil() as u64).max(1);
    let hi_target = ((total as f64 * (1.0 - hi_pct)).ceil() as u64).max(1);

    let mut black = 0usize;
    let mut cumsum = 0u64;
    for (i, &count) in hist.iter().enumerate() {
        cumsum += count;
        if cumsum >= lo_target {
            black = i;
            break;
        }
    }

    let mut white = 255usize;
    cumsum = 0;
    for (i, &count) in hist.iter().enumerate().rev() {
        cumsum += count;
        if cumsum >= hi_target {
            white = i;
            break;
        }
    }

    if white <= black {
        return (0.0, 1.0);
    }
    (black as f32 / 255.0, white as f32 / 255.0)
}

// ---------------------------------------------------------------------------
// Internal render logic
// ---------------------------------------------------------------------------

type PipelineResult =
    Result<(Arc<Image>, HistogramData, Vec<Arc<Image>>, Option<[u32; 4]>), String>;

fn render_pipeline(
    start_image: Arc<Image>,
    committed_ops: Vec<Option<Box<dyn Operation>>>,
    preview_op: Option<Box<dyn Operation>>,
    preview_scale: Option<f32>,
    preview_viewport: Option<[u32; 4]>,
    overlay_viewport: Option<[u32; 4]>,
    #[cfg(feature = "gpu")] gpu: Option<Arc<GpuContext>>,
) -> PipelineResult {
    // ── Overlay path ─────────────────────────────────────────────────────
    if let (Some(op), Some([vp_x, vp_y, vp_w, vp_h])) = (&preview_op, overlay_viewport) {
        let mut current = start_image;
        for committed in committed_ops.iter().flatten() {
            let img = match Arc::try_unwrap(current) {
                Ok(img) => img,
                Err(a) => a.as_ref().deep_clone(),
            };
            let result = committed
                .apply(img)
                .map_err(|e| format!("Op '{}' failed: {}", committed.name(), e))?;
            debug_validate_image(&result, committed.name());
            current = Arc::new(result);
        }
        let x = vp_x.min(current.width.saturating_sub(1));
        let y = vp_y.min(current.height.saturating_sub(1));
        let w = vp_w.min(current.width.saturating_sub(x)).max(1);
        let h = vp_h.min(current.height.saturating_sub(y)).max(1);

        let crop = extract_region(current.as_ref(), x, y, w, h);
        let processed = op
            .apply(crop)
            .map_err(|e| format!("Op '{}' (overlay) failed: {}", op.name(), e))?;
        debug_validate_image(&processed, op.name());

        let hist = HistogramData::compute(&processed);
        return Ok((Arc::new(processed), hist, Vec::new(), Some([x, y, w, h])));
    }

    // ── Fallback: downsampled-blit path ───────────────────────────────────
    let is_preview = preview_scale.is_some();
    let mut current: Arc<Image> = match preview_scale {
        Some(scale) => Arc::new(downsample_nn(start_image.as_ref(), scale)),
        None => start_image,
    };
    let mut intermediates = if !is_preview {
        Vec::with_capacity(committed_ops.len())
    } else {
        Vec::new()
    };

    apply_committed_ops(
        &mut current,
        committed_ops,
        preview_scale,
        is_preview,
        &mut intermediates,
        #[cfg(feature = "gpu")]
        gpu.as_deref(),
    )?;

    if let Some(op) = preview_op {
        if let Some([vp_x, vp_y, vp_w, vp_h]) = preview_viewport {
            let scale = preview_scale.unwrap_or(1.0);
            let sx = ((vp_x as f32 * scale) as u32).min(current.width.saturating_sub(1));
            let sy = ((vp_y as f32 * scale) as u32).min(current.height.saturating_sub(1));
            let sw = ((vp_w as f32 * scale).ceil() as u32)
                .min(current.width.saturating_sub(sx))
                .max(1);
            let sh = ((vp_h as f32 * scale).ceil() as u32)
                .min(current.height.saturating_sub(sy))
                .max(1);
            let crop = extract_region(current.as_ref(), sx, sy, sw, sh);
            let processed = op
                .apply(crop)
                .map_err(|e| format!("Op '{}' (preview) failed: {}", op.name(), e))?;
            debug_validate_image(&processed, op.name());
            let base = match Arc::try_unwrap(current) {
                Ok(img) => img,
                Err(a) => a.as_ref().deep_clone(),
            };
            if processed.width == sw && processed.height == sh {
                let mut base = base;
                blit_region(&mut base, &processed, sx, sy);
                current = Arc::new(base);
            } else {
                let result = op
                    .apply(base)
                    .map_err(|e| format!("Op '{}' (preview) failed: {}", op.name(), e))?;
                debug_validate_image(&result, op.name());
                current = Arc::new(result);
            }
        } else {
            let img = match Arc::try_unwrap(current) {
                Ok(img) => img,
                Err(a) => a.as_ref().deep_clone(),
            };
            let result = op
                .apply(img)
                .map_err(|e| format!("Op '{}' (preview) failed: {}", op.name(), e))?;
            debug_validate_image(&result, op.name());
            current = Arc::new(result);
        }
    }

    let hist = HistogramData::compute(current.as_ref());
    Ok((current, hist, intermediates, None))
}

fn apply_committed_ops(
    current: &mut Arc<Image>,
    committed_ops: Vec<Option<Box<dyn Operation>>>,
    preview_scale: Option<f32>,
    is_preview: bool,
    intermediates: &mut Vec<Arc<Image>>,
    #[cfg(feature = "gpu")] gpu: Option<&GpuContext>,
) -> Result<(), String> {
    for maybe_op in committed_ops {
        if let Some(op) = maybe_op {
            let op = match preview_scale {
                Some(s) => op.scaled_for_preview(s),
                None => op,
            };
            let img = match Arc::try_unwrap(std::mem::replace(current, Arc::new(Image::new(1, 1))))
            {
                Ok(img) => img,
                Err(a) => a.as_ref().deep_clone(),
            };
            let result = apply_one_with_optional_gpu(
                img,
                op.as_ref(),
                #[cfg(feature = "gpu")]
                gpu,
            )
            .map_err(|e| format!("Op '{}' failed: {}", op.name(), e))?;
            debug_validate_image(&result, op.name());
            *current = Arc::new(result);
        }
        if !is_preview {
            intermediates.push(Arc::clone(current));
        }
    }
    Ok(())
}

fn apply_one_with_optional_gpu(
    image: Image,
    op: &dyn Operation,
    #[cfg(feature = "gpu")] gpu: Option<&GpuContext>,
) -> Result<Image, String> {
    #[cfg(feature = "gpu")]
    {
        if should_try_gpu(op, &image, gpu.is_some()) {
            if let Some(ctx) = gpu {
                match rasterlab_gpu::apply_one_to_image(ctx, op, &image) {
                    Ok((out, timings))
                        if out.width == image.width && out.height == image.height =>
                    {
                        log_gpu(&format!(
                            "op={} pixels={} upload={:?} dispatch={:?} readback={:?}",
                            op.name(),
                            image.pixel_count(),
                            timings.upload,
                            timings.dispatch,
                            timings.readback
                        ));
                        return Ok(out);
                    }
                    Ok((out, _)) => {
                        log_gpu(&format!(
                            "op={} invalid gpu shape {}x{}, falling back to cpu",
                            op.name(),
                            out.width,
                            out.height
                        ));
                    }
                    Err(e) => {
                        log_gpu(&format!(
                            "op={} gpu failed: {}; falling back to cpu",
                            op.name(),
                            e
                        ));
                    }
                }
            }
        }
    }

    #[cfg(feature = "gpu")]
    let cpu_start = std::time::Instant::now();
    let out = op.apply(image).map_err(|e| e.to_string())?;
    #[cfg(feature = "gpu")]
    log_gpu(&format!("op={} cpu={:?}", op.name(), cpu_start.elapsed()));
    Ok(out)
}

#[cfg(feature = "gpu")]
fn should_try_gpu(op: &dyn Operation, image: &Image, has_context: bool) -> bool {
    if !has_context || !rasterlab_gpu::supports(op) {
        return false;
    }
    match std::env::var("RASTERLAB_GPU").as_deref() {
        Ok("0") => false,
        Ok("force") => true,
        Ok("1") => image.pixel_count() >= 2_000_000,
        _ => image.pixel_count() >= 2_000_000,
    }
}

#[cfg(feature = "gpu")]
fn log_gpu(message: &str) {
    if std::env::var("RASTERLAB_GPU_LOG").as_deref() == Ok("1") {
        eprintln!("[rasterlab-gpu] {message}");
    }
}

/// Nearest-neighbour downsample via rayon row-parallel copy.
fn downsample_nn(img: &Image, scale: f32) -> Image {
    let new_w = ((img.width as f32 * scale) as u32).max(1);
    let new_h = ((img.height as f32 * scale) as u32).max(1);
    let mut out = Image::new(new_w, new_h);
    let x_ratio = img.width as f32 / new_w as f32;
    let y_ratio = img.height as f32 / new_h as f32;
    let src_w = img.width as usize;
    out.data
        .par_chunks_mut(new_w as usize * 4)
        .enumerate()
        .for_each(|(y, row)| {
            let src_y = (y as f32 * y_ratio) as usize;
            for x in 0..new_w as usize {
                let src_x = (x as f32 * x_ratio) as usize;
                let src_off = (src_y * src_w + src_x) * 4;
                let dst_off = x * 4;
                row[dst_off..dst_off + 4].copy_from_slice(&img.data[src_off..src_off + 4]);
            }
        });
    out
}

#[inline]
fn debug_validate_image(image: &Image, op_name: &str) {
    debug_assert_eq!(
        image.data.len(),
        image.width as usize * image.height as usize * 4,
        "Operation '{}' returned an Image with mismatched buffer: \
         data.len()={} but {}x{}x4={}",
        op_name,
        image.data.len(),
        image.width,
        image.height,
        image.width as usize * image.height as usize * 4,
    );
}

/// Extract a rectangular region from `src` into a new Image.
fn extract_region(src: &Image, x: u32, y: u32, w: u32, h: u32) -> Image {
    let mut out = Image::new(w, h);
    let src_stride = src.width as usize * 4;
    let x_off = x as usize * 4;
    let row_bytes = w as usize * 4;
    out.data
        .par_chunks_mut(row_bytes)
        .enumerate()
        .for_each(|(dst_y, dst_row)| {
            let src_start = (y as usize + dst_y) * src_stride + x_off;
            dst_row.copy_from_slice(&src.data[src_start..src_start + row_bytes]);
        });
    out
}

/// Copy `src` into `dst` at pixel offset `(x, y)`.
fn blit_region(dst: &mut Image, src: &Image, x: u32, y: u32) {
    let row_bytes = src.width as usize * 4;
    let dst_stride = dst.width as usize * 4;
    let start = y as usize * dst_stride;
    let end = start + src.height as usize * dst_stride;
    let x_off = x as usize * 4;
    dst.data[start..end]
        .par_chunks_mut(dst_stride)
        .enumerate()
        .for_each(|(src_row, dst_row)| {
            let src_off = src_row * row_bytes;
            dst_row[x_off..x_off + row_bytes]
                .copy_from_slice(&src.data[src_off..src_off + row_bytes]);
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use rasterlab_core::ops::BrightnessContrastOp;

    #[cfg(feature = "gpu")]
    async fn make_gpu_context() -> Option<GpuContext> {
        let instance = wgpu::Instance::default();
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
            .ok()?;
        let limits = adapter.limits();
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("rasterlab render gpu test device"),
                required_limits: limits.clone(),
                ..Default::default()
            })
            .await
            .ok()?;
        Some(GpuContext::new(device, queue, limits))
    }

    #[test]
    fn render_empty_pipeline() {
        let img = Image::new(64, 64);
        let request = RenderRequest {
            start_image: Arc::new(img),
            committed_ops: vec![],
            preview_op: None,
            preview_scale: None,
            preview_viewport: None,
            overlay_viewport: None,
            #[cfg(feature = "gpu")]
            gpu: None,
        };
        let (tx, rx) = std::sync::mpsc::channel::<RenderResult>();
        let repaint = Arc::new(|| {});
        let meta = RenderMeta {
            start_index: 0,
            cache_gen: 0,
            is_preview: false,
        };
        spawn_render(request, meta, tx, repaint);
        let result = rx.recv().unwrap();
        match result {
            RenderResult::Complete { image, .. } => {
                assert_eq!(image.width, 64);
                assert_eq!(image.height, 64);
            }
            _ => panic!("expected Complete"),
        }
    }

    #[test]
    fn percentile_levels_uniform() {
        let mut hist = [0u64; 256];
        for h in &mut hist {
            *h = 100;
        }
        let (black, white) = percentile_levels(&hist, 0.01, 0.99);
        assert!(black < 0.05);
        assert!(white > 0.95);
    }

    #[test]
    fn disabled_ops_preserve_intermediate_slots() {
        let mut img = Image::new(2, 1);
        img.data = vec![10, 20, 30, 40, 50, 60, 70, 80];
        let (out, _hist, intermediates, overlay) = render_pipeline(
            Arc::new(img.deep_clone()),
            vec![
                None,
                Some(Box::new(BrightnessContrastOp::new(0.1, 0.0))),
                None,
            ],
            None,
            None,
            None,
            None,
            #[cfg(feature = "gpu")]
            None,
        )
        .unwrap();

        assert_eq!(overlay, None);
        assert_eq!(intermediates.len(), 3);
        assert_eq!(intermediates[0].data, img.data);
        assert_eq!(intermediates[1].data, out.data);
        assert_eq!(intermediates[2].data, out.data);
    }

    #[cfg(feature = "gpu")]
    #[test]
    #[ignore = "requires a working wgpu adapter"]
    fn gpu_enabled_brightness_contrast_matches_cpu_render() {
        let Some(gpu) = pollster::block_on(make_gpu_context()) else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };
        let mut img = Image::new(2048, 1024);
        for (i, pixel) in img.data.chunks_mut(4).enumerate() {
            pixel[0] = (i * 3 % 256) as u8;
            pixel[1] = (i * 5 % 256) as u8;
            pixel[2] = (i * 7 % 256) as u8;
            pixel[3] = (i * 11 % 256) as u8;
        }
        let op = || Some(Box::new(BrightnessContrastOp::new(0.18, -0.22)) as Box<dyn Operation>);

        let (cpu, _, _, _) = render_pipeline(
            Arc::new(img.deep_clone()),
            vec![op()],
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let (gpu_out, _, _, _) = render_pipeline(
            Arc::new(img),
            vec![op()],
            None,
            None,
            None,
            None,
            Some(Arc::new(gpu)),
        )
        .unwrap();

        assert_eq!(gpu_out.data, cpu.data);
    }
}
