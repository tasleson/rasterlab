//! `rasterlab info` — print image metadata and histograms.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;
use rasterlab_core::{formats::FormatRegistry, ops::HistogramData};

#[derive(Debug, Args)]
pub struct InfoArgs {
    /// Image file to inspect.
    pub input: PathBuf,

    /// Print a simple ASCII bar chart of the histogram.
    #[arg(long, default_value_t = true)]
    pub histogram: bool,
}

pub fn run(args: InfoArgs) -> Result<()> {
    let registry = FormatRegistry::with_builtins();
    let image = registry
        .decode_file(&args.input)
        .with_context(|| format!("Cannot load '{}'", args.input.display()))?;

    println!("File     : {}", args.input.display());
    println!("Size     : {}×{} pixels", image.width, image.height);
    println!("Channels : RGBA8");
    println!(
        "Memory   : {:.2} MiB",
        image.data.len() as f64 / 1_048_576.0
    );

    if let Some(model) = &image.metadata.camera_model {
        println!("Camera   : {}", model);
    }
    if let Some(iso) = image.metadata.iso {
        println!("ISO      : {}", iso);
    }
    if let Some(ss) = &image.metadata.shutter_speed {
        println!("Shutter  : {}", ss);
    }
    if let Some(ap) = image.metadata.aperture {
        println!("Aperture : f/{:.1}", ap);
    }

    if args.histogram {
        let hist = HistogramData::compute(&image);
        println!();
        print_histogram_chart("Red", &hist.red, 'R');
        print_histogram_chart("Green", &hist.green, 'G');
        print_histogram_chart("Blue", &hist.blue, 'B');
        print_histogram_chart("Luma", &hist.luma, 'Y');
    }

    Ok(())
}

fn print_histogram_chart(label: &str, data: &[u64; 256], ch: char) {
    const BARS: usize = 64; // number of columns in the chart
    const HEIGHT: usize = 8; // rows

    // Downsample 256 buckets → BARS buckets
    let bucket_size = 256 / BARS;
    let mut buckets = vec![0u64; BARS];
    for (i, &count) in data.iter().enumerate() {
        buckets[i / bucket_size] += count;
    }

    let max = buckets.iter().copied().max().unwrap_or(1).max(1);

    println!("{} channel ({}):", label, ch);
    for row in (0..HEIGHT).rev() {
        let threshold = max as f64 * (row + 1) as f64 / HEIGHT as f64;
        let line: String = buckets
            .iter()
            .map(|&b| if b as f64 >= threshold { '█' } else { ' ' })
            .collect();
        println!("│{}│", line);
    }
    println!(" 0{}", " ".repeat(BARS - 3) + "255");
    println!();
}
