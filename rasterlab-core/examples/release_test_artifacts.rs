use std::{
    fs,
    path::{Path, PathBuf},
};

use rasterlab_core::{
    formats::FormatRegistry,
    image::Image,
    ops::{
        BlackAndWhiteOp, BlurOp, BrightnessContrastOp, ClarityTextureOp, ColorBalanceOp,
        ColorSpaceConversion, ColorSpaceOp, CropOp, CurvesOp, DenoiseOp, FauxHdrOp, FlipOp,
        FocusStackOp, GrainOp, HdrMergeOp, HealOp, HealSpot, HighlightsShadowsOp, HistogramOp,
        HslPanelOp, HueShiftOp, LevelsOp, LinearMask, LutOp, MaskShape, MaskedOp, NoiseReductionOp,
        NrMethod, PanoramaOp, PerspectiveOp, ResampleMode, ResizeOp, RotateOp, SaturationOp,
        SepiaOp, ShadowExposureOp, SharpenOp, SplitToneOp, VibranceOp, VignetteOp, WhiteBalanceOp,
    },
    pipeline::EditPipeline,
    project::{RlabFile, RlabMeta, SavedCopy},
    traits::{format_handler::EncodeOptions, operation::Operation},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .to_path_buf();
    let out_dir = workspace.join("release_artifacts");
    fs::create_dir_all(&out_dir)?;

    let registry = FormatRegistry::with_builtins();
    let options = EncodeOptions {
        jpeg_quality: 92,
        png_compression: 6,
        preserve_metadata: true,
    };

    let source = release_source_image(640, 360);
    let source_path = out_dir.join("all_tools_source.png");
    let source_bytes = registry.encode_file(&source, &source_path, &options)?;
    fs::write(&source_path, &source_bytes)?;

    let mut pipeline = EditPipeline::new(source);
    for op in release_stack_ops(&workspace)? {
        pipeline.push_op(op);
    }

    let rendered = pipeline.render()?;
    let rendered_path = out_dir.join("all_tools_rendered.png");
    fs::write(
        &rendered_path,
        registry.encode_file(&rendered, &rendered_path, &options)?,
    )?;

    let state = pipeline.save_state()?;
    let stack_path = out_dir.join("all_tools_stack.json");
    fs::write(&stack_path, serde_json::to_string_pretty(&state)?)?;

    let project_path = out_dir.join("all_tools_release_test.rlab");
    let meta = RlabMeta::new(
        env!("CARGO_PKG_VERSION"),
        Some(source_path.display().to_string()),
        pipeline.source().width,
        pipeline.source().height,
    );
    let project = RlabFile::new(
        meta,
        source_bytes,
        vec![SavedCopy {
            name: "All tools release test".to_owned(),
            pipeline_state: state,
        }],
        0,
        None,
    );
    project.write_v4(&project_path)?;

    println!("Wrote {}", source_path.display());
    println!("Wrote {}", rendered_path.display());
    println!("Wrote {}", stack_path.display());
    println!("Wrote {}", project_path.display());
    Ok(())
}

fn release_source_image(width: u32, height: u32) -> Image {
    let mut image = Image::new(width, height);
    for y in 0..height {
        for x in 0..width {
            let nx = x as f32 / (width - 1) as f32;
            let ny = y as f32 / (height - 1) as f32;
            let checker = if ((x / 32) + (y / 32)) % 2 == 0 {
                22.0
            } else {
                -18.0
            };
            let r = (255.0 * nx + checker).clamp(0.0, 255.0) as u8;
            let g = (255.0 * ny).clamp(0.0, 255.0) as u8;
            let b = (210.0 * (1.0 - nx) + 45.0 * ny).clamp(0.0, 255.0) as u8;
            image.set_pixel(x, y, [r, g, b, 255]);
        }
    }
    image
}

fn release_stack_ops(
    workspace: &Path,
) -> Result<Vec<Box<dyn Operation>>, Box<dyn std::error::Error>> {
    let path = |rel: &str| workspace.join(rel).display().to_string();

    let mut hue = [0.0; 8];
    hue[0] = 8.0;
    hue[3] = -10.0;
    let mut sat = [0.0; 8];
    sat[1] = 0.12;
    sat[5] = -0.10;
    let mut lum = [0.0; 8];
    lum[2] = 0.08;
    lum[6] = -0.06;

    let lut = LutOp::from_cube_str(
        &fs::read_to_string(workspace.join("luts/warm_vintage.cube"))?,
        0.25,
    )
    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    Ok(vec![
        Box::new(HdrMergeOp::new(vec![
            path("test_images/hdr_bracket_under.png"),
            path("test_images/hdr_bracket_mid.png"),
            path("test_images/hdr_bracket_over.png"),
        ])),
        Box::new(FocusStackOp::new(vec![
            path("test_images/focus_top.png"),
            path("test_images/focus_mid.png"),
            path("test_images/focus_bot.png"),
        ])),
        Box::new(PanoramaOp::new(
            vec![
                path("test_images/pano3_a.png"),
                path("test_images/pano3_b.png"),
                path("test_images/pano3_c.png"),
            ],
            40,
        )),
        Box::new(CropOp::new(40, 30, 1180, 680)),
        Box::new(PerspectiveOp::new([
            [0.025, 0.015],
            [-0.020, 0.010],
            [0.015, -0.020],
            [-0.010, -0.015],
        ])),
        Box::new(ResizeOp::new(420, 240, ResampleMode::Bicubic)),
        Box::new(RotateOp::arbitrary(1.4)),
        Box::new(RotateOp::cw90()),
        Box::new(FlipOp::horizontal()),
        Box::new(ColorSpaceOp::new(ColorSpaceConversion::SrgbToDisplayP3)),
        Box::new(WhiteBalanceOp::new(0.18, -0.08)),
        Box::new(LevelsOp::new(0.03, 0.96, 1.05)),
        Box::new(CurvesOp {
            points: vec![[0.0, 0.0], [0.25, 0.22], [0.72, 0.78], [1.0, 1.0]],
        }),
        Box::new(BrightnessContrastOp::new(0.04, 0.12)),
        Box::new(HighlightsShadowsOp::new(-0.12, 0.18)),
        Box::new(ShadowExposureOp::new(0.22, 0.55)),
        Box::new(SaturationOp::new(0.08)),
        Box::new(VibranceOp::new(0.20)),
        Box::new(HueShiftOp::new(6.0)),
        Box::new(ColorBalanceOp::new(
            [0.05, 0.00, -0.03],
            [-0.02, 0.04, 0.01],
            [0.01, -0.03, 0.05],
        )),
        Box::new(HslPanelOp::new(hue, sat, lum)),
        Box::new(SplitToneOp::new(225.0, 0.14, 42.0, 0.12, -0.08)),
        Box::new(BlackAndWhiteOp::perceptual()),
        Box::new(SepiaOp::new(0.18)),
        Box::new(FauxHdrOp::new(0.22)),
        Box::new(SharpenOp::new(0.55)),
        Box::new(ClarityTextureOp::new(0.18, 0.12)),
        Box::new(BlurOp::new(0.65)),
        Box::new(DenoiseOp::new(0.08, 2)),
        Box::new(NoiseReductionOp {
            method: NrMethod::Wavelet,
            luma_strength: 0.10,
            color_strength: 0.16,
            detail_preservation: 0.70,
        }),
        Box::new(GrainOp::new(0.07, 1.4, 0x5eed_2026)),
        Box::new(VignetteOp::new(0.18, 0.72, 0.42)),
        Box::new(HealOp::new(vec![HealSpot {
            dest_x: 180,
            dest_y: 110,
            src_x: 220,
            src_y: 118,
            radius: 10,
        }])),
        Box::new(lut),
        Box::new(MaskedOp {
            inner: Box::new(BrightnessContrastOp::new(0.10, 0.08)),
            mask: MaskShape::Linear(LinearMask {
                cx: 0.52,
                cy: 0.48,
                angle_deg: 35.0,
                feather: 0.65,
                invert: false,
            }),
        }),
        Box::new(HistogramOp),
    ])
}
