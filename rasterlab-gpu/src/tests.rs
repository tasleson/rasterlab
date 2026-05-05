use super::*;
use rasterlab_core::{
    Image,
    ops::{
        BlackAndWhiteOp, BlurOp, BrightnessContrastOp, BwMode, ClarityTextureOp, ColorBalanceOp,
        ColorSpaceConversion, ColorSpaceOp, CurvesOp, DenoiseOp, FauxHdrOp, HslPanelOp, HueShiftOp,
        NoiseReductionOp, NrMethod, SaturationOp, SharpenOp, VibranceOp, WhiteBalanceOp,
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
        assert_eq!(actual.data, expected.data);
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
        let op = CurvesOp { points };
        let expected = op.apply(src.deep_clone()).unwrap();
        let gpu = GpuImage::from_image(&ctx, &src).unwrap();
        let actual = apply_one(&ctx, &op, gpu).unwrap().into_image(&ctx).unwrap();
        assert_eq!(actual.data, expected.data);
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
        assert_eq!(actual.data, expected.data, "degrees={degrees}");
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
        assert_eq!(actual.data, expected.data, "saturation={saturation}");
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
        assert_eq!(actual.data, expected.data, "strength={strength}");
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
        assert_eq!(
            actual.data, expected.data,
            "temperature={temperature} tint={tint}"
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

    assert_eq!(actual.data, expected.data);
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

    assert_eq!(actual.data, expected.data);
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

    assert_eq!(actual.data, expected.data);
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

    let mut max_delta = 0u8;
    let mut sum_delta = 0u64;
    let mut count = 0u64;
    for (a, b) in actual.data.chunks(4).zip(expected.data.chunks(4)) {
        for channel in 0..3 {
            let delta = a[channel].abs_diff(b[channel]);
            max_delta = max_delta.max(delta);
            sum_delta += u64::from(delta);
            count += 1;
        }
        assert_eq!(a[3], b[3]);
    }
    let mean_delta = sum_delta as f64 / count as f64;
    assert!(
        mean_delta <= 3.0 && max_delta <= 16,
        "GPU NLM drifted too far from CPU: mean_delta={mean_delta:.2} max_delta={max_delta}"
    );
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
    let mut max_delta = 0u8;
    for (a, b) in actual.data.chunks(4).zip(expected.data.chunks(4)) {
        for ch in 0..3 {
            max_delta = max_delta.max(a[ch].abs_diff(b[ch]));
        }
        assert_eq!(a[3], b[3]);
    }
    assert!(max_delta <= 1, "black_and_white max_delta={max_delta}");
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
    let mut max_delta = 0u8;
    for (a, b) in actual.data.chunks(4).zip(expected.data.chunks(4)) {
        for ch in 0..3 {
            max_delta = max_delta.max(a[ch].abs_diff(b[ch]));
        }
        assert_eq!(a[3], b[3]);
    }
    assert!(max_delta <= 2, "color_balance max_delta={max_delta}");
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
    let mut max_delta = 0u8;
    for (a, b) in actual.data.chunks(4).zip(expected.data.chunks(4)) {
        for ch in 0..3 {
            max_delta = max_delta.max(a[ch].abs_diff(b[ch]));
        }
        assert_eq!(a[3], b[3]);
    }
    assert!(max_delta <= 2, "color_space max_delta={max_delta}");
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
    let mut max_delta = 0u8;
    for (a, b) in actual.data.chunks(4).zip(expected.data.chunks(4)) {
        for ch in 0..3 {
            max_delta = max_delta.max(a[ch].abs_diff(b[ch]));
        }
        assert_eq!(a[3], b[3]);
    }
    assert!(max_delta <= 3, "denoise max_delta={max_delta}");
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
    let mut max_delta = 0u8;
    for (a, b) in actual.data.chunks(4).zip(expected.data.chunks(4)) {
        for ch in 0..3 {
            max_delta = max_delta.max(a[ch].abs_diff(b[ch]));
        }
        assert_eq!(a[3], b[3]);
    }
    assert!(max_delta <= 2, "hsl_panel max_delta={max_delta}");
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
    let mut max_delta = 0u8;
    for (a, b) in actual.data.chunks(4).zip(expected.data.chunks(4)) {
        for ch in 0..3 {
            max_delta = max_delta.max(a[ch].abs_diff(b[ch]));
        }
        assert_eq!(a[3], b[3]);
    }
    assert!(max_delta <= 2, "sharpen max_delta={max_delta}");
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
    let mut max_delta = 0u8;
    for (a, b) in actual.data.chunks(4).zip(expected.data.chunks(4)) {
        for ch in 0..3 {
            max_delta = max_delta.max(a[ch].abs_diff(b[ch]));
        }
        assert_eq!(a[3], b[3]);
    }
    assert!(max_delta <= 1, "faux_hdr max_delta={max_delta}");
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
    let mut max_delta = 0u8;
    for (a, b) in actual.data.chunks(4).zip(expected.data.chunks(4)) {
        for ch in 0..3 {
            max_delta = max_delta.max(a[ch].abs_diff(b[ch]));
        }
        assert_eq!(a[3], b[3]);
    }
    assert!(max_delta <= 2, "clarity_texture max_delta={max_delta}");
}
