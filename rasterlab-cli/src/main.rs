//! # rasterlab CLI
//!
//! Batch-capable command-line image processor built on `rasterlab-core`.
//!
//! ## Examples
//!
//! ```bash
//! # Single operation
//! rasterlab process photo.jpg -o out.png --crop 100,100,800,600
//!
//! # Pipeline chaining
//! rasterlab process photo.nef -o bw.jpg \
//!     --crop 0,0,2000,1500 \
//!     --rotate 90 \
//!     --bw luminance \
//!     --sharpen 0.8
//!
//! # Save/load pipeline (edit session)
//! rasterlab process photo.jpg -o preview.jpg --save-pipeline edits.json
//! rasterlab process photo.jpg -o final.tiff  --load-pipeline edits.json
//!
//! # Batch processing (all JPEGs in a folder → grayscale PNGs)
//! rasterlab batch ./photos -o ./output --ext jpg --bw luminance
//!
//! # Print image metadata and histogram
//! rasterlab info photo.jpg
//! ```

mod commands;
mod pipeline_builder;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "rasterlab",
    about = "High-performance non-destructive image processor",
    version,
    author
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Process a single image through a pipeline of operations.
    Process(commands::process::ProcessArgs),

    /// Apply the same pipeline to all matching files in a directory.
    Batch(commands::batch::BatchArgs),

    /// Print metadata and channel histograms for an image.
    Info(commands::info::InfoArgs),
}

fn main() -> Result<()> {
    // Rayon worker threads default to 8 MiB — insufficient for image processing.
    rayon::ThreadPoolBuilder::new()
        .stack_size(32 * 1024 * 1024)
        .build_global()
        .expect("failed to build rayon thread pool");

    let cli = Cli::parse();
    match cli.command {
        Commands::Process(args) => commands::process::run(args),
        Commands::Batch(args) => commands::batch::run(args),
        Commands::Info(args) => commands::info::run(args),
    }
}
