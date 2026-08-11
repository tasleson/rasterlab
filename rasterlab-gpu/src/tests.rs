use super::*;
use crate::ops::SUPPORTED_GPU_OP_COUNT;
use rasterlab_core::{
    Image,
    ops::{
        BlackAndWhiteOp, BlurOp, BrightnessContrastOp, BwMode, ClarityTextureOp, ColorBalanceOp,
        ColorSpaceConversion, ColorSpaceOp, CurvesOp, DenoiseOp, FauxHdrOp, HighlightsShadowsOp,
        HslPanelOp, HueShiftOp, LevelsOp, NoiseReductionOp, NrMethod, SaturationOp, SepiaOp,
        ShadowExposureOp, SharpenOp, SplitToneOp, VibranceOp, VignetteOp, WhiteBalanceOp,
    },
    traits::operation::Operation,
};

async fn make_context() -> Option<GpuContext> {
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
            label: Some("rasterlab gpu test device"),
            required_limits: limits.clone(),
            ..Default::default()
        })
        .await
        .ok()?;
    Some(GpuContext::new(device, queue, limits))
}

fn test_image(width: u32, height: u32) -> Image {
    let mut image = Image::new(width, height);
    for (i, pixel) in image.data.chunks_mut(4).enumerate() {
        pixel[0] = (i * 3 % 256) as u8;
        pixel[1] = (i * 5 % 256) as u8;
        pixel[2] = (i * 7 % 256) as u8;
        pixel[3] = (31 + i * 11 % 225) as u8;
    }
    image
}

// Tolerance policy for GPU-versus-CPU comparisons.
//
// The two implementations are separate pieces of arithmetic, so they only
// agree bit-for-bit where no float math survives to the GPU.  Everywhere else
// an adapter is free to contract a multiply-add, approximate a reciprocal, or
// ship its own `exp`/`pow`, and both sides *truncate* the final f32 to 8 bits
// — so a result that lands an ulp below a whole level on one adapter and on
// it on another differs by a whole level.  These budgets are therefore stated
// per shader pass rather than measured off one GPU; the drift on an AMD Radeon
// 780M sits inside them with a level to spare, and anything larger is a bug in
// the kernel, not adapter variation.
//
/// A GPU pass that only applies a lookup table the CPU built: no float math
/// runs on the GPU, so the results are byte-identical on every adapter.
const EXACT: u8 = 0;
/// One point-wise pass of f32 math in the shader, truncated to 8 bits.
const POINT_PASS: u8 = 1;
/// One pass that sums a window of pixels before truncating: ulp differences
/// accumulate across the window first.
const WINDOW_PASS: u8 = 2;

/// Compare a GPU result against the CPU reference for the same operation.
///
/// RGB may deviate by up to `tolerance` levels; alpha must survive exactly,
/// since every op compared this way leaves it alone.  Failures report the
/// worst pixel plus the mean deviation, which is what distinguishes a real
/// regression (a shifted mean) from an adapter rounding a few pixels the
/// other way.
#[track_caller]
fn assert_matches_cpu(actual: &Image, expected: &Image, tolerance: u8, case: &str) {
    assert_eq!(actual.width, expected.width, "{case}: width");
    assert_eq!(actual.height, expected.height, "{case}: height");
    assert_eq!(actual.data.len(), expected.data.len(), "{case}: length");

    let mut max_delta = 0u8;
    let mut worst = 0usize;
    let mut sum_delta = 0u64;
    for (i, (a, b)) in actual
        .data
        .chunks(4)
        .zip(expected.data.chunks(4))
        .enumerate()
    {
        for channel in 0..3 {
            let delta = a[channel].abs_diff(b[channel]);
            if delta > max_delta {
                max_delta = delta;
                worst = i;
            }
            sum_delta += u64::from(delta);
        }
        assert_eq!(a[3], b[3], "{case}: alpha changed at pixel {i}");
    }

    let mean_delta = sum_delta as f64 / (actual.data.len() / 4 * 3) as f64;
    assert!(
        max_delta <= tolerance,
        "{case}: GPU drifted from CPU by {max_delta} levels (tolerance {tolerance}), \
         mean {mean_delta:.3}; worst pixel {worst} gpu={:?} cpu={:?}",
        &actual.data[worst * 4..worst * 4 + 4],
        &expected.data[worst * 4..worst * 4 + 4],
    );
}

#[test]
#[ignore = "requires a working wgpu adapter"]
fn rgba8_upload_readback_exact() {
    let Some(ctx) = pollster::block_on(make_context()) else {
        eprintln!("skipping: no wgpu adapter available");
        return;
    };
    let image = test_image(5, 3);
    let gpu = GpuImage::from_image(&ctx, &image).unwrap();
    let out = gpu.read_rgba8(&ctx).unwrap();
    assert_eq!(out, image.data);
}

#[test]
#[ignore = "requires a working wgpu adapter"]
fn brightness_contrast_matches_cpu() {
    let Some(ctx) = pollster::block_on(make_context()) else {
        eprintln!("skipping: no wgpu adapter available");
        return;
    };
    let cases = [
        (0.0, 0.0, 8, 8),
        (0.25, 0.0, 13, 17),
        (-0.25, 0.0, 13, 17),
        (0.0, 0.45, 19, 11),
        (0.0, -0.45, 19, 11),
        (0.15, -0.2, 257, 3),
    ];
    for (brightness, contrast, width, height) in cases {
        let src = test_image(width, height);
        let op = BrightnessContrastOp::new(brightness, contrast);
        let expected = op.apply(src.deep_clone()).unwrap();
        let gpu = GpuImage::from_image(&ctx, &src).unwrap();
        let actual = apply_one(&ctx, &op, gpu).unwrap().into_image(&ctx).unwrap();
        assert_matches_cpu(
            &actual,
            &expected,
            EXACT,
            &format!("brightness_contrast b={brightness} c={contrast}"),
        );
    }
}

#[test]
#[ignore = "requires a working wgpu adapter"]
fn curves_matches_cpu() {
    let Some(ctx) = pollster::block_on(make_context()) else {
        eprintln!("skipping: no wgpu adapter available");
        return;
    };
    let cases = [
        vec![[0.0, 0.0], [1.0, 1.0]],
        vec![[0.0, 1.0], [1.0, 0.0]],
        vec![[0.0, 0.0], [0.35, 0.2], [0.7, 0.86], [1.0, 1.0]],
        vec![[0.0, 0.08], [0.18, 0.12], [0.62, 0.74], [1.0, 0.95]],
    ];
    for points in cases {
        let src = test_image(31, 17);
        let op = CurvesOp {
            points: points.clone(),
        };
        let expected = op.apply(src.deep_clone()).unwrap();
        let gpu = GpuImage::from_image(&ctx, &src).unwrap();
        let actual = apply_one(&ctx, &op, gpu).unwrap().into_image(&ctx).unwrap();
        assert_matches_cpu(&actual, &expected, EXACT, &format!("curves {points:?}"));
    }
}

#[test]
#[ignore = "requires a working wgpu adapter"]
fn hue_shift_matches_cpu() {
    let Some(ctx) = pollster::block_on(make_context()) else {
        eprintln!("skipping: no wgpu adapter available");
        return;
    };
    for degrees in [0.0, 30.0, -75.0, 120.0, 180.0, 270.0] {
        let src = test_image(31, 17);
        let op = HueShiftOp::new(degrees);
        let expected = op.apply(src.deep_clone()).unwrap();
        let gpu = GpuImage::from_image(&ctx, &src).unwrap();
        let actual = apply_one(&ctx, &op, gpu).unwrap().into_image(&ctx).unwrap();
        // An HSL round trip: `rgb_to_hsl` divides by a channel spread that can
        // be tiny, so an ulp of adapter disagreement is easy to come by.
        assert_matches_cpu(
            &actual,
            &expected,
            POINT_PASS,
            &format!("hue_shift degrees={degrees}"),
        );
    }
}

#[test]
#[ignore = "requires a working wgpu adapter"]
fn saturation_matches_cpu() {
    let Some(ctx) = pollster::block_on(make_context()) else {
        eprintln!("skipping: no wgpu adapter available");
        return;
    };
    for saturation in [0.0, 0.35, 1.0, 1.75, 4.0] {
        let src = test_image(31, 17);
        let op = SaturationOp::new(saturation);
        let expected = op.apply(src.deep_clone()).unwrap();
        let gpu = GpuImage::from_image(&ctx, &src).unwrap();
        let actual = apply_one(&ctx, &op, gpu).unwrap().into_image(&ctx).unwrap();
        assert_matches_cpu(
            &actual,
            &expected,
            POINT_PASS,
            &format!("saturation={saturation}"),
        );
    }
}

#[test]
#[ignore = "requires a working wgpu adapter"]
fn vibrance_matches_cpu() {
    let Some(ctx) = pollster::block_on(make_context()) else {
        eprintln!("skipping: no wgpu adapter available");
        return;
    };
    for strength in [-1.0, -0.35, 0.0, 0.45, 1.0] {
        let src = test_image(31, 17);
        let op = VibranceOp::new(strength);
        let expected = op.apply(src.deep_clone()).unwrap();
        let gpu = GpuImage::from_image(&ctx, &src).unwrap();
        let actual = apply_one(&ctx, &op, gpu).unwrap().into_image(&ctx).unwrap();
        assert_matches_cpu(
            &actual,
            &expected,
            POINT_PASS,
            &format!("vibrance strength={strength}"),
        );
    }
}

#[test]
#[ignore = "requires a working wgpu adapter"]
fn white_balance_matches_cpu() {
    let Some(ctx) = pollster::block_on(make_context()) else {
        eprintln!("skipping: no wgpu adapter available");
        return;
    };
    for (temperature, tint) in [
        (0.0, 0.0),
        (0.5, 0.0),
        (-0.5, 0.0),
        (0.0, 0.5),
        (0.7, -0.4),
        (-1.0, 1.0),
    ] {
        let src = test_image(31, 17);
        let op = WhiteBalanceOp::new(temperature, tint);
        let expected = op.apply(src.deep_clone()).unwrap();
        let gpu = GpuImage::from_image(&ctx, &src).unwrap();
        let actual = apply_one(&ctx, &op, gpu).unwrap().into_image(&ctx).unwrap();
        assert_matches_cpu(
            &actual,
            &expected,
            POINT_PASS,
            &format!("white_balance temperature={temperature} tint={tint}"),
        );
    }
}

#[test]
#[ignore = "requires a working wgpu adapter"]
fn gpu_pipeline_chains_ops_with_single_readback() {
    let Some(ctx) = pollster::block_on(make_context()) else {
        eprintln!("skipping: no wgpu adapter available");
        return;
    };

    let src = test_image(257, 129);
    let op_a = BrightnessContrastOp::new(0.12, -0.18);
    let op_b = BrightnessContrastOp::new(-0.08, 0.22);
    let expected = op_b.apply(op_a.apply(src.deep_clone()).unwrap()).unwrap();

    let mut pipeline = GpuPipeline::from_image(&ctx, &src).unwrap();
    pipeline.apply_op(&ctx, &op_a).unwrap();
    pipeline.apply_op(&ctx, &op_b).unwrap();
    assert_eq!(pipeline.op_count(), 2);
    let (actual, timings) = pipeline.into_image(&ctx).unwrap();

    assert_matches_cpu(&actual, &expected, EXACT, "chained brightness/contrast");
    assert!(timings.upload > Default::default());
    assert!(timings.dispatch > Default::default());
    assert!(timings.readback > Default::default());
}

#[test]
#[ignore = "requires a working wgpu adapter"]
fn gpu_pipeline_chains_brightness_and_curves() {
    let Some(ctx) = pollster::block_on(make_context()) else {
        eprintln!("skipping: no wgpu adapter available");
        return;
    };

    let src = test_image(257, 129);
    let op_a = BrightnessContrastOp::new(0.12, -0.18);
    let op_b = CurvesOp {
        points: vec![[0.0, 0.02], [0.3, 0.18], [0.74, 0.9], [1.0, 1.0]],
    };
    let expected = op_b.apply(op_a.apply(src.deep_clone()).unwrap()).unwrap();

    let mut pipeline = GpuPipeline::from_image(&ctx, &src).unwrap();
    pipeline.apply_op(&ctx, &op_a).unwrap();
    pipeline.apply_op(&ctx, &op_b).unwrap();
    assert_eq!(pipeline.op_count(), 2);
    let (actual, timings) = pipeline.into_image(&ctx).unwrap();

    assert_matches_cpu(&actual, &expected, EXACT, "chained brightness + curves");
    assert!(timings.upload > Default::default());
    assert!(timings.dispatch > Default::default());
    assert!(timings.readback > Default::default());
}

#[test]
#[ignore = "requires a working wgpu adapter"]
fn gpu_pipeline_chains_point_color_ops() {
    let Some(ctx) = pollster::block_on(make_context()) else {
        eprintln!("skipping: no wgpu adapter available");
        return;
    };

    let src = test_image(257, 129);
    let op_a = BrightnessContrastOp::new(0.12, -0.18);
    let op_b = CurvesOp {
        points: vec![[0.0, 0.02], [0.3, 0.18], [0.74, 0.9], [1.0, 1.0]],
    };
    let op_c = HueShiftOp::new(47.0);
    let op_d = SaturationOp::new(1.65);
    let op_e = VibranceOp::new(0.48);
    let op_f = WhiteBalanceOp::new(0.32, -0.22);
    let expected = op_f
        .apply(
            op_e.apply(
                op_d.apply(
                    op_c.apply(op_b.apply(op_a.apply(src.deep_clone()).unwrap()).unwrap())
                        .unwrap(),
                )
                .unwrap(),
            )
            .unwrap(),
        )
        .unwrap();

    let mut pipeline = GpuPipeline::from_image(&ctx, &src).unwrap();
    pipeline.apply_op(&ctx, &op_a).unwrap();
    pipeline.apply_op(&ctx, &op_b).unwrap();
    pipeline.apply_op(&ctx, &op_c).unwrap();
    pipeline.apply_op(&ctx, &op_d).unwrap();
    pipeline.apply_op(&ctx, &op_e).unwrap();
    pipeline.apply_op(&ctx, &op_f).unwrap();
    assert_eq!(pipeline.op_count(), 6);
    let (actual, timings) = pipeline.into_image(&ctx).unwrap();

    // The pipeline keeps its intermediates as RGBA8, so each of the four
    // shader-computed ops in the chain (hue shift, saturation, vibrance,
    // white balance) re-quantizes and can hand the next one a level of drift;
    // the two lookup-table ops contribute none.
    assert_matches_cpu(
        &actual,
        &expected,
        4 * POINT_PASS,
        "chained point colour ops",
    );
    assert!(timings.upload > Default::default());
    assert!(timings.dispatch > Default::default());
    assert!(timings.readback > Default::default());
}

#[test]
#[ignore = "requires a working wgpu adapter"]
fn large_image_dispatch_stays_within_wgpu_limits() {
    let Some(ctx) = pollster::block_on(make_context()) else {
        eprintln!("skipping: no wgpu adapter available");
        return;
    };

    let src = Image::new(4096, 4096);
    let op = BrightnessContrastOp::new(0.0, 0.0);
    let (actual, _) = apply_one_to_image(&ctx, &op, &src).unwrap();

    assert_eq!(actual.width, src.width);
    assert_eq!(actual.height, src.height);
    assert_eq!(actual.data.len(), src.data.len());
}

#[test]
#[ignore = "requires a working wgpu adapter"]
fn noise_reduction_nlm_runs_on_gpu() {
    let Some(ctx) = pollster::block_on(make_context()) else {
        eprintln!("skipping: no wgpu adapter available");
        return;
    };

    let src = test_image(32, 24);
    let op = NoiseReductionOp {
        method: NrMethod::NonLocalMeans,
        luma_strength: 0.5,
        color_strength: 0.5,
        detail_preservation: 0.0,
    };
    let (actual, _) = apply_one_to_image(&ctx, &op, &src).unwrap();

    assert_eq!(actual.width, src.width);
    assert_eq!(actual.height, src.height);
    assert_eq!(actual.data.len(), src.data.len());
    for (input, output) in src.data.chunks(4).zip(actual.data.chunks(4)) {
        assert_eq!(output[3], input[3]);
    }
}

#[test]
#[ignore = "requires a working wgpu adapter"]
fn noise_reduction_nlm_roughly_matches_cpu() {
    let Some(ctx) = pollster::block_on(make_context()) else {
        eprintln!("skipping: no wgpu adapter available");
        return;
    };

    let src = test_image(24, 18);
    let op = NoiseReductionOp {
        method: NrMethod::NonLocalMeans,
        luma_strength: 0.5,
        color_strength: 0.5,
        detail_preservation: 0.5,
    };
    let expected = op.apply(src.deep_clone()).unwrap();
    let (actual, _) = apply_one_to_image(&ctx, &op, &src).unwrap();

    // Two windowed passes: the non-local means average, then the detail blend
    // that reads it back.
    assert_matches_cpu(&actual, &expected, 2 * WINDOW_PASS, "noise_reduction nlm");
}

#[test]
#[ignore = "requires a working wgpu adapter"]
fn black_and_white_runs_on_gpu() {
    let Some(ctx) = pollster::block_on(make_context()) else {
        eprintln!("skipping: no wgpu adapter available");
        return;
    };
    let src = test_image(32, 24);
    let op = BlackAndWhiteOp {
        mode: BwMode::Luminance,
    };
    let (out, _) = apply_one_to_image(&ctx, &op, &src).unwrap();
    assert_eq!(out.width, src.width);
    assert_eq!(out.height, src.height);
    for (i, o) in src.data.chunks(4).zip(out.data.chunks(4)) {
        assert_eq!(o[3], i[3]);
    }
}

#[test]
#[ignore = "requires a working wgpu adapter"]
fn black_and_white_roughly_matches_cpu() {
    let Some(ctx) = pollster::block_on(make_context()) else {
        eprintln!("skipping: no wgpu adapter available");
        return;
    };
    let src = test_image(24, 18);
    let op = BlackAndWhiteOp {
        mode: BwMode::Perceptual,
    };
    let expected = op.apply(src.deep_clone()).unwrap();
    let (actual, _) = apply_one_to_image(&ctx, &op, &src).unwrap();
    assert_matches_cpu(&actual, &expected, POINT_PASS, "black_and_white");
}

#[test]
#[ignore = "requires a working wgpu adapter"]
fn blur_runs_on_gpu() {
    let Some(ctx) = pollster::block_on(make_context()) else {
        eprintln!("skipping: no wgpu adapter available");
        return;
    };
    // Create a bright spot in a dark image
    let mut src = Image::new(32, 32);
    // Set most pixels dark
    for chunk in src.data.chunks_mut(4) {
        chunk[0] = 10;
        chunk[1] = 10;
        chunk[2] = 10;
        chunk[3] = 255;
    }
    // Bright centre pixel
    let cx = 16usize;
    let cy = 16usize;
    let idx = (cy * 32 + cx) * 4;
    src.data[idx] = 255;
    src.data[idx + 1] = 255;
    src.data[idx + 2] = 255;
    src.data[idx + 3] = 255;

    let op = BlurOp::new(2.0);
    let (out, _) = apply_one_to_image(&ctx, &op, &src).unwrap();
    assert_eq!(out.width, src.width);
    assert_eq!(out.height, src.height);
    // The bright spot should be dimmed after blur
    assert!(
        out.data[idx] < 255,
        "bright centre should dim after blur, got {}",
        out.data[idx]
    );
}

#[test]
#[ignore = "requires a working wgpu adapter"]
fn color_balance_runs_on_gpu() {
    let Some(ctx) = pollster::block_on(make_context()) else {
        eprintln!("skipping: no wgpu adapter available");
        return;
    };
    let src = test_image(32, 24);
    let op = ColorBalanceOp::new([0.5, 0.0, -0.5], [0.0, 0.3, 0.0], [-0.2, 0.0, 0.4]);
    let (out, _) = apply_one_to_image(&ctx, &op, &src).unwrap();
    assert_eq!(out.width, src.width);
    assert_eq!(out.height, src.height);
    for (i, o) in src.data.chunks(4).zip(out.data.chunks(4)) {
        assert_eq!(o[3], i[3]);
    }
}

#[test]
#[ignore = "requires a working wgpu adapter"]
fn color_balance_roughly_matches_cpu() {
    let Some(ctx) = pollster::block_on(make_context()) else {
        eprintln!("skipping: no wgpu adapter available");
        return;
    };
    let src = test_image(24, 18);
    let op = ColorBalanceOp::new([0.3, 0.0, -0.2], [0.0, 0.2, 0.0], [-0.1, 0.0, 0.3]);
    let expected = op.apply(src.deep_clone()).unwrap();
    let (actual, _) = apply_one_to_image(&ctx, &op, &src).unwrap();
    assert_matches_cpu(&actual, &expected, POINT_PASS, "color_balance");
}

#[test]
#[ignore = "requires a working wgpu adapter"]
fn color_space_runs_on_gpu() {
    let Some(ctx) = pollster::block_on(make_context()) else {
        eprintln!("skipping: no wgpu adapter available");
        return;
    };
    let src = test_image(32, 24);
    let op = ColorSpaceOp {
        conversion: ColorSpaceConversion::SrgbToDisplayP3,
    };
    let (out, _) = apply_one_to_image(&ctx, &op, &src).unwrap();
    assert_eq!(out.width, src.width);
    assert_eq!(out.height, src.height);
    for (i, o) in src.data.chunks(4).zip(out.data.chunks(4)) {
        assert_eq!(o[3], i[3]);
    }
}

#[test]
#[ignore = "requires a working wgpu adapter"]
fn color_space_roughly_matches_cpu() {
    let Some(ctx) = pollster::block_on(make_context()) else {
        eprintln!("skipping: no wgpu adapter available");
        return;
    };
    let src = test_image(24, 18);
    let op = ColorSpaceOp {
        conversion: ColorSpaceConversion::SrgbToDisplayP3,
    };
    let expected = op.apply(src.deep_clone()).unwrap();
    let (actual, _) = apply_one_to_image(&ctx, &op, &src).unwrap();
    assert_matches_cpu(&actual, &expected, POINT_PASS, "color_space");
}

#[test]
#[ignore = "requires a working wgpu adapter"]
fn denoise_runs_on_gpu() {
    let Some(ctx) = pollster::block_on(make_context()) else {
        eprintln!("skipping: no wgpu adapter available");
        return;
    };
    let src = test_image(32, 24);
    let op = DenoiseOp {
        strength: 0.3,
        radius: 2,
    };
    let (out, _) = apply_one_to_image(&ctx, &op, &src).unwrap();
    assert_eq!(out.width, src.width);
    assert_eq!(out.height, src.height);
    for (i, o) in src.data.chunks(4).zip(out.data.chunks(4)) {
        assert_eq!(o[3], i[3]);
    }
}

#[test]
#[ignore = "requires a working wgpu adapter"]
fn denoise_roughly_matches_cpu() {
    let Some(ctx) = pollster::block_on(make_context()) else {
        eprintln!("skipping: no wgpu adapter available");
        return;
    };
    let src = test_image(24, 18);
    let op = DenoiseOp {
        strength: 0.3,
        radius: 2,
    };
    let expected = op.apply(src.deep_clone()).unwrap();
    let (actual, _) = apply_one_to_image(&ctx, &op, &src).unwrap();
    assert_matches_cpu(&actual, &expected, WINDOW_PASS, "denoise");
}

#[test]
#[ignore = "requires a working wgpu adapter"]
fn hsl_panel_runs_on_gpu() {
    let Some(ctx) = pollster::block_on(make_context()) else {
        eprintln!("skipping: no wgpu adapter available");
        return;
    };
    let src = test_image(32, 24);
    let op = HslPanelOp::new(
        [30.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        [0.0, 0.2, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        [0.0, 0.0, 0.1, 0.0, 0.0, 0.0, 0.0, 0.0],
    );
    let (out, _) = apply_one_to_image(&ctx, &op, &src).unwrap();
    assert_eq!(out.width, src.width);
    assert_eq!(out.height, src.height);
    for (i, o) in src.data.chunks(4).zip(out.data.chunks(4)) {
        assert_eq!(o[3], i[3]);
    }
}

#[test]
#[ignore = "requires a working wgpu adapter"]
fn hsl_panel_roughly_matches_cpu() {
    let Some(ctx) = pollster::block_on(make_context()) else {
        eprintln!("skipping: no wgpu adapter available");
        return;
    };
    let src = test_image(24, 18);
    let op = HslPanelOp::new(
        [20.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        [0.0, 0.1, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        [0.0, 0.0, 0.05, 0.0, 0.0, 0.0, 0.0, 0.0],
    );
    let expected = op.apply(src.deep_clone()).unwrap();
    let (actual, _) = apply_one_to_image(&ctx, &op, &src).unwrap();
    assert_matches_cpu(&actual, &expected, POINT_PASS, "hsl_panel");
}

#[test]
#[ignore = "requires a working wgpu adapter"]
fn sharpen_runs_on_gpu() {
    let Some(ctx) = pollster::block_on(make_context()) else {
        eprintln!("skipping: no wgpu adapter available");
        return;
    };
    let src = test_image(32, 24);
    let op = SharpenOp::new(1.0);
    let (out, _) = apply_one_to_image(&ctx, &op, &src).unwrap();
    assert_eq!(out.width, src.width);
    assert_eq!(out.height, src.height);
    for (i, o) in src.data.chunks(4).zip(out.data.chunks(4)) {
        assert_eq!(o[3], i[3]);
    }
}

#[test]
#[ignore = "requires a working wgpu adapter"]
fn sharpen_roughly_matches_cpu() {
    let Some(ctx) = pollster::block_on(make_context()) else {
        eprintln!("skipping: no wgpu adapter available");
        return;
    };
    let src = test_image(24, 18);
    let op = SharpenOp::new(1.0);
    let expected = op.apply(src.deep_clone()).unwrap();
    let (actual, _) = apply_one_to_image(&ctx, &op, &src).unwrap();
    assert_matches_cpu(&actual, &expected, WINDOW_PASS, "sharpen");
}

#[test]
#[ignore = "requires a working wgpu adapter"]
fn faux_hdr_runs_on_gpu() {
    let Some(ctx) = pollster::block_on(make_context()) else {
        eprintln!("skipping: no wgpu adapter available");
        return;
    };
    let src = test_image(32, 24);
    let op = FauxHdrOp::new(0.8);
    let (out, _) = apply_one_to_image(&ctx, &op, &src).unwrap();
    assert_eq!(out.width, src.width);
    assert_eq!(out.height, src.height);
    for (i, o) in src.data.chunks(4).zip(out.data.chunks(4)) {
        assert_eq!(o[3], i[3]);
    }
}

#[test]
#[ignore = "requires a working wgpu adapter"]
fn faux_hdr_roughly_matches_cpu() {
    let Some(ctx) = pollster::block_on(make_context()) else {
        eprintln!("skipping: no wgpu adapter available");
        return;
    };
    let src = test_image(24, 18);
    let op = FauxHdrOp::new(0.8);
    let expected = op.apply(src.deep_clone()).unwrap();
    let (actual, _) = apply_one_to_image(&ctx, &op, &src).unwrap();
    assert_matches_cpu(&actual, &expected, POINT_PASS, "faux_hdr");
}

#[test]
#[ignore = "requires a working wgpu adapter"]
fn clarity_texture_runs_on_gpu() {
    let Some(ctx) = pollster::block_on(make_context()) else {
        eprintln!("skipping: no wgpu adapter available");
        return;
    };
    let src = test_image(64, 48);
    let op = ClarityTextureOp::new(0.5, 0.3);
    let (out, _) = apply_one_to_image(&ctx, &op, &src).unwrap();
    assert_eq!(out.width, src.width);
    assert_eq!(out.height, src.height);
    for (i, o) in src.data.chunks(4).zip(out.data.chunks(4)) {
        assert_eq!(o[3], i[3]);
    }
}

#[test]
#[ignore = "requires a working wgpu adapter"]
fn clarity_texture_roughly_matches_cpu() {
    let Some(ctx) = pollster::block_on(make_context()) else {
        eprintln!("skipping: no wgpu adapter available");
        return;
    };
    let src = test_image(48, 36);
    let op = ClarityTextureOp::new(0.4, 0.0);
    let expected = op.apply(src.deep_clone()).unwrap();
    let (actual, _) = apply_one_to_image(&ctx, &op, &src).unwrap();
    assert_matches_cpu(&actual, &expected, WINDOW_PASS, "clarity_texture");
}

/// One instance of every op the dispatcher claims to support.  Kept in sync
/// with the `SupportedGpuOp` variants by `every_supported_op_is_dispatchable`.
fn supported_op_samples() -> Vec<Box<dyn Operation>> {
    vec![
        Box::new(BrightnessContrastOp::new(0.1, 0.1)),
        Box::new(CurvesOp {
            points: vec![[0.0, 0.0], [0.5, 0.6], [1.0, 1.0]],
        }),
        Box::new(HueShiftOp::new(30.0)),
        Box::new(SaturationOp::new(1.2)),
        Box::new(VibranceOp::new(0.4)),
        Box::new(WhiteBalanceOp::new(0.2, -0.1)),
        Box::new(NoiseReductionOp {
            method: NrMethod::NonLocalMeans,
            luma_strength: 0.5,
            color_strength: 0.5,
            detail_preservation: 0.5,
        }),
        Box::new(SepiaOp::new(0.8)),
        Box::new(LevelsOp::new(0.05, 0.95, 1.1)),
        Box::new(HighlightsShadowsOp::new(-0.3, 0.4)),
        Box::new(VignetteOp::new(0.5, 0.6, 0.4)),
        Box::new(ShadowExposureOp::new(0.7, 2.0)),
        Box::new(SplitToneOp::new(210.0, 0.3, 40.0, 0.25, 0.0)),
        Box::new(BlackAndWhiteOp {
            mode: BwMode::Luminance,
        }),
        Box::new(BlurOp::new(2.0)),
        Box::new(ColorBalanceOp::new(
            [0.3, 0.0, -0.2],
            [0.0, 0.2, 0.0],
            [-0.1, 0.0, 0.3],
        )),
        Box::new(ColorSpaceOp::new(ColorSpaceConversion::SrgbToDisplayP3)),
        Box::new(DenoiseOp {
            strength: 0.3,
            radius: 2,
        }),
        Box::new(HslPanelOp::new(
            [20.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            [0.0, 0.1, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            [0.0, 0.0, 0.05, 0.0, 0.0, 0.0, 0.0, 0.0],
        )),
        Box::new(SharpenOp::new(1.0)),
        Box::new(FauxHdrOp::new(0.8)),
        Box::new(ClarityTextureOp::new(0.5, 0.3)),
    ]
}

/// Every GPU-backed op must override `Operation::as_any`, otherwise `classify`
/// cannot downcast it and the render pipeline silently falls back to the CPU
/// with its kernel and shader left unreachable.  Needs no adapter, so it runs
/// in the normal test job rather than the scheduled GPU one.
#[test]
fn every_supported_op_is_dispatchable() {
    let samples = supported_op_samples();
    assert_eq!(
        samples.len(),
        SUPPORTED_GPU_OP_COUNT,
        "supported_op_samples() is out of sync with SupportedGpuOp"
    );
    for op in samples {
        assert!(
            op.as_any().is_some(),
            "{} does not override Operation::as_any",
            op.name()
        );
        assert!(
            supports(op.as_ref()),
            "{} is not reachable from the GPU dispatcher",
            op.name()
        );
    }
}
