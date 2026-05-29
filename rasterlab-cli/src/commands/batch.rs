//! `rasterlab batch` — parallel directory processing.

use std::path::Path;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;
use rasterlab_core::{
    formats::FormatRegistry,
    pipeline::{EditEntry, EditPipeline, PipelineState},
    traits::format_handler::EncodeOptions,
};
use rayon::prelude::*;

use crate::pipeline_builder::PipelineSpec;

#[derive(Debug, Args)]
pub struct BatchArgs {
    /// Directory containing input images.
    pub input_dir: PathBuf,

    /// Output directory (created if it doesn't exist).
    #[arg(short, long)]
    pub output: PathBuf,

    /// Only process files with this extension (e.g. --ext jpg).
    #[arg(long)]
    pub ext: Option<String>,

    /// Crop: x,y,width,height
    #[arg(long)]
    pub crop: Option<String>,

    /// Rotate: 90 | 180 | 270 | <degrees>
    #[arg(long)]
    pub rotate: Option<String>,

    /// B&W mode: luminance | average | perceptual | r,g,b
    #[arg(long)]
    pub bw: Option<String>,

    /// Sharpen strength 0.0–10.0
    #[arg(long)]
    pub sharpen: Option<f32>,

    /// Load a previously saved pipeline JSON and apply it (ignores other op flags).
    #[arg(long)]
    pub load_pipeline: Option<PathBuf>,

    /// Output file extension (infers format).  Defaults to input extension.
    #[arg(long)]
    pub output_ext: Option<String>,

    /// JPEG quality [default: 90]
    #[arg(long, default_value_t = 90)]
    pub jpeg_quality: u8,

    /// PNG compression [default: 6]
    #[arg(long, default_value_t = 6)]
    pub png_compression: u8,
}

pub fn run(args: BatchArgs) -> Result<()> {
    std::fs::create_dir_all(&args.output)
        .with_context(|| format!("Cannot create output dir '{}'", args.output.display()))?;

    // Collect input files
    let filter_ext = args.ext.as_deref().map(|e| e.to_lowercase());
    let mut files: Vec<PathBuf> = std::fs::read_dir(&args.input_dir)
        .with_context(|| format!("Cannot read directory '{}'", args.input_dir.display()))?
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            if !p.is_file() {
                return None;
            }
            if let Some(ref want_ext) = filter_ext {
                let actual_ext = p
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.to_lowercase())
                    .unwrap_or_default();
                if &actual_ext != want_ext {
                    return None;
                }
            }
            Some(p)
        })
        .collect();

    files.sort();

    if files.is_empty() {
        eprintln!("No matching files found in '{}'", args.input_dir.display());
        return Ok(());
    }

    eprintln!(
        "Processing {} file(s) with {} rayon threads…",
        files.len(),
        rayon::current_num_threads()
    );

    let registry = FormatRegistry::with_builtins();

    // Build the operation list once; each thread deserialises its own copies.
    let entries_json: Vec<serde_json::Value> = if let Some(pipeline_path) = &args.load_pipeline {
        let json = std::fs::read_to_string(pipeline_path)
            .with_context(|| format!("Cannot read pipeline '{}'", pipeline_path.display()))?;
        let state: PipelineState =
            serde_json::from_str(&json).context("Failed to parse pipeline JSON")?;
        // Only include entries up to the saved cursor (respects undo state).
        let active = state.entries[..state.cursor.min(state.entries.len())].to_vec();
        eprintln!(
            "Pipeline loaded from '{}' ({} op(s))",
            pipeline_path.display(),
            active.len()
        );
        active
    } else {
        let spec = PipelineSpec {
            crop: args.crop.clone(),
            rotate: args.rotate.clone(),
            bw: args.bw.clone(),
            sharpen: args.sharpen,
        };
        let ops = spec.build()?;
        ops.into_iter()
            .enumerate()
            .map(|(idx, op)| {
                serde_json::to_value(EditEntry {
                    id: idx as u64 + 1,
                    enabled: true,
                    operation: op,
                })
                .context("Serialising operation")
            })
            .collect::<Result<_>>()?
    };

    let options = EncodeOptions {
        jpeg_quality: args.jpeg_quality,
        png_compression: args.png_compression,
        preserve_metadata: true,
    };

    // Process files in parallel
    let results: Vec<(PathBuf, Result<()>)> = files
        .par_iter()
        .map(|input_path| {
            let result = process_one(
                input_path,
                &args.output,
                args.output_ext.as_deref(),
                &registry,
                &entries_json,
                &options,
            );
            (input_path.clone(), result)
        })
        .collect();

    // Report
    let mut ok = 0usize;
    let mut err = 0usize;
    for (path, result) in results {
        match result {
            Ok(()) => {
                ok += 1;
                eprintln!("  ✓  {}", path.display());
            }
            Err(e) => {
                err += 1;
                eprintln!("  ✗  {}  — {}", path.display(), e);
            }
        }
    }

    eprintln!("\nDone: {} succeeded, {} failed", ok, err);
    if err > 0 {
        std::process::exit(1);
    }
    Ok(())
}

fn process_one(
    input: &Path,
    output_dir: &Path,
    output_ext: Option<&str>,
    registry: &FormatRegistry,
    entries_json: &[serde_json::Value],
    options: &EncodeOptions,
) -> Result<()> {
    let image = registry
        .decode_file(input)
        .with_context(|| format!("Decode failed: {}", input.display()))?;

    let mut pipeline = EditPipeline::new(image);

    // Deserialise the active edit entries per thread. Loading the full state
    // preserves entry metadata such as the enabled flag.
    pipeline
        .load_state(PipelineState {
            entries: entries_json.to_vec(),
            cursor: entries_json.len(),
        })
        .context("Deserialising pipeline")?;

    let rendered = pipeline.render().context("Render failed")?;

    // Determine output path
    let stem = input.file_stem().unwrap_or_default();
    let ext = output_ext
        .or_else(|| input.extension().and_then(|e| e.to_str()))
        .unwrap_or("png");
    let out_name = format!("{}.{}", stem.to_string_lossy(), ext);
    let out_path = output_dir.join(&out_name);

    let bytes = registry
        .encode_file(&rendered, &out_path, options)
        .with_context(|| format!("Encode failed for '{}'", out_path.display()))?;

    std::fs::write(&out_path, bytes)
        .with_context(|| format!("Write failed for '{}'", out_path.display()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rasterlab_core::{image::Image, ops::SepiaOp, traits::operation::Operation};

    fn edit_entry_json(id: u64, enabled: bool, operation: Box<dyn Operation>) -> serde_json::Value {
        serde_json::to_value(EditEntry {
            id,
            enabled,
            operation,
        })
        .unwrap()
    }

    #[test]
    fn flag_pipeline_entries_deserialise_as_edit_entries() {
        let spec = PipelineSpec {
            crop: None,
            rotate: None,
            bw: Some("luminance".into()),
            sharpen: Some(1.25),
        };
        let entries: Vec<serde_json::Value> = spec
            .build()
            .unwrap()
            .into_iter()
            .enumerate()
            .map(|(idx, op)| edit_entry_json(idx as u64 + 1, true, op))
            .collect();

        assert_eq!(entries.len(), 2);
        for entry_json in entries {
            let entry: EditEntry = serde_json::from_value(entry_json).unwrap();
            assert!(entry.enabled);
        }
    }

    #[test]
    fn batch_loaded_pipeline_respects_disabled_entries() {
        let entries = vec![edit_entry_json(1, false, Box::new(SepiaOp::new(1.0)))];
        let mut img = Image::new(1, 1);
        img.set_pixel(0, 0, [255, 0, 0, 255]);

        let mut pipeline = EditPipeline::new(img);
        pipeline
            .load_state(PipelineState {
                cursor: entries.len(),
                entries,
            })
            .unwrap();

        let rendered = pipeline.render().unwrap();
        assert_eq!(rendered.pixel(0, 0), [255, 0, 0, 255]);
    }
}
