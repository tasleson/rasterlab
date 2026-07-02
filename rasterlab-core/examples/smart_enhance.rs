/// Runs the Smart Enhance analysis + planner on an image, prints the
/// measured statistics and the resulting plan, applies it, and writes the
/// corrected image next to the input as `<name>_smart.png`.
///
/// Usage:
///   cargo run --release --example smart_enhance -- <image_path>
use std::{env, path::PathBuf, time::Instant};

use rasterlab_core::{
    analysis::{self, ImageStats},
    formats::FormatRegistry,
    traits::format_handler::EncodeOptions,
};

fn main() {
    // Large rayon fold accumulators need more than the macOS 512 KiB default.
    rayon::ThreadPoolBuilder::new()
        .stack_size(16 * 1024 * 1024)
        .build_global()
        .unwrap();

    let path = PathBuf::from(
        env::args()
            .nth(1)
            .expect("usage: smart_enhance <image_path>"),
    );

    let registry = FormatRegistry::with_builtins();
    let image = registry.decode_file(&path).expect("failed to load image");
    println!(
        "Loaded {} ({}x{})",
        path.display(),
        image.width,
        image.height
    );

    let t = Instant::now();
    let stats = ImageStats::compute(&image);
    println!("\nStats ({:.1} ms):", t.elapsed().as_secs_f64() * 1000.0);
    println!(
        "  channel medians   R={} G={} B={}",
        analysis::median(&stats.hist.red),
        analysis::median(&stats.hist.green),
        analysis::median(&stats.hist.blue),
    );
    println!("  luma variance     {:.1}", stats.luma_variance);
    println!(
        "  laplacian var     {:.1}",
        stats.laplacian_variance.unwrap_or(0.0)
    );
    println!(
        "  sharpness score   {:.3}",
        stats.sharpness().unwrap_or(0.0)
    );

    let t = Instant::now();
    let plan = analysis::plan_from_stats(&image, &stats);
    println!(
        "\nPlan ({:.1} ms): {}",
        t.elapsed().as_secs_f64() * 1000.0,
        plan.summary()
    );
    for op in plan.clone().into_ops() {
        println!("  {}", op.describe());
    }

    let t = Instant::now();
    let mut out = image;
    for op in plan.into_ops() {
        out = op.apply(out).expect("op failed");
    }
    println!("\nApplied in {:.1} ms", t.elapsed().as_secs_f64() * 1000.0);

    let out_path = path.with_file_name(format!(
        "{}_smart.png",
        path.file_stem().unwrap().to_string_lossy()
    ));
    let bytes = registry
        .encode_file(&out, &out_path, &EncodeOptions::default())
        .expect("encode failed");
    std::fs::write(&out_path, bytes).expect("write failed");
    println!("Wrote {}", out_path.display());
}
