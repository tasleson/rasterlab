//! `rasterlab process` — single-file pipeline.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;
use rasterlab_core::{
    formats::FormatRegistry,
    pipeline::{EditPipeline, PipelineState},
    traits::format_handler::EncodeOptions,
};

use crate::pipeline_builder::PipelineSpec;

#[derive(Debug, Args)]
pub struct ProcessArgs {
    /// Input image file (JPEG, PNG, NEF).
    pub input: PathBuf,

    /// Output file path.  Format is inferred from the extension.
    #[arg(short, long)]
    pub output: PathBuf,

    /// Crop: x,y,width,height  (e.g. --crop 0,0,800,600)
    #[arg(long)]
    pub crop: Option<String>,

    /// Rotate: 90 | 180 | 270 | <degrees>  (e.g. --rotate 45.5)
    #[arg(long)]
    pub rotate: Option<String>,

    /// Black and white conversion mode: luminance | average | perceptual | r,g,b
    #[arg(long)]
    pub bw: Option<String>,

    /// Correct aircraft-window haze/cast/reflections: default or strength,cast,haze,reflections.
    #[arg(long, num_args = 0..=1, default_missing_value = "default")]
    pub airplane_window: Option<String>,

    /// Sharpening strength 0.0–10.0  (e.g. --sharpen 1.5)
    #[arg(long)]
    pub sharpen: Option<f32>,

    /// JPEG output quality 1–100 [default: 90]
    #[arg(long, default_value_t = 90)]
    pub jpeg_quality: u8,

    /// PNG compression level 0–9 [default: 6]
    #[arg(long, default_value_t = 6)]
    pub png_compression: u8,

    /// Save the edit pipeline to a JSON file (for reuse with --load-pipeline).
    #[arg(long)]
    pub save_pipeline: Option<PathBuf>,

    /// Load a previously saved pipeline JSON and apply it (ignores other op flags).
    #[arg(long)]
    pub load_pipeline: Option<PathBuf>,
}

pub fn run(args: ProcessArgs) -> Result<()> {
    let registry = FormatRegistry::with_builtins();

    eprintln!("Loading: {}", args.input.display());
    let image = registry
        .decode_file(&args.input)
        .with_context(|| format!("Failed to decode '{}'", args.input.display()))?;

    eprintln!("  {}×{} pixels", image.width, image.height);

    let mut pipeline = EditPipeline::new(image);

    if let Some(pipeline_path) = &args.load_pipeline {
        // Load saved pipeline
        let json = std::fs::read_to_string(pipeline_path)
            .with_context(|| format!("Cannot read pipeline '{}'", pipeline_path.display()))?;
        let mut state: PipelineState =
            serde_json::from_str(&json).context("Failed to parse pipeline JSON")?;
        state.resolve_relative_paths(
            pipeline_path
                .parent()
                .unwrap_or_else(|| std::path::Path::new(".")),
        );
        pipeline
            .load_state(state)
            .context("Failed to restore pipeline")?;
        eprintln!("Pipeline loaded from '{}'", pipeline_path.display());
    } else {
        // Build pipeline from flags
        let spec = PipelineSpec {
            crop: args.crop,
            rotate: args.rotate,
            bw: args.bw,
            airplane_window: args.airplane_window,
            sharpen: args.sharpen,
        };
        for op in spec.build()? {
            let name = op.name().to_owned();
            pipeline.push_op(op);
            eprintln!("  + {}", name);
        }
    }

    // Optionally save the pipeline
    if let Some(save_path) = &args.save_pipeline {
        let state = pipeline
            .save_state()
            .context("Failed to serialise pipeline")?;
        let json = serde_json::to_string_pretty(&state).context("JSON serialisation failed")?;
        std::fs::write(save_path, json)
            .with_context(|| format!("Cannot write pipeline to '{}'", save_path.display()))?;
        eprintln!("Pipeline saved to '{}'", save_path.display());
    }

    // Render
    eprintln!("Rendering…");
    let rendered = pipeline.render().context("Render failed")?;

    // Encode and write output
    let options = EncodeOptions {
        jpeg_quality: args.jpeg_quality,
        png_compression: args.png_compression,
        preserve_metadata: true,
    };
    let bytes = registry
        .encode_file(&rendered, &args.output, &options)
        .with_context(|| format!("Failed to encode output '{}'", args.output.display()))?;

    std::fs::write(&args.output, &bytes)
        .with_context(|| format!("Cannot write '{}'", args.output.display()))?;

    eprintln!("Written {} bytes → {}", bytes.len(), args.output.display());
    Ok(())
}
