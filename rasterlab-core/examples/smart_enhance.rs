/// Runs the Smart Enhance analysis + planner on an image, prints the
/// measured statistics and the resulting plan, applies it, and writes the
/// corrected image next to the input as `<name>_smart.png`.
///
/// Usage:
///   cargo run --release --example smart_enhance -- <image_path>
use std::{env, path::PathBuf, time::Instant};

use rasterlab_core::{
    analysis::{self, ImageStats, RegionalStats},
    formats::FormatRegistry,
    traits::format_handler::EncodeOptions,
};

/// Dark-to-light ramp used for the ASCII tile maps.
const RAMP: &[u8] = b" .:-=+*#%@";

/// Map a 0–255 value onto [`RAMP`].
fn ramp_char(v: f32, max: f32) -> char {
    let t = (v / max).clamp(0.0, 1.0);
    let i = ((t * (RAMP.len() - 1) as f32).round() as usize).min(RAMP.len() - 1);
    RAMP[i] as char
}

/// Print the tile grid as an ASCII map beside the numeric values, so the
/// shape of the lighting is visible at a glance and the actual measurements
/// are there to check it against.
fn print_tile_map(regional: &RegionalStats, label: &str, max: f32, value: impl Fn(usize) -> f32) {
    let (cols, rows) = (regional.cols() as usize, regional.rows() as usize);
    println!("\n  {label}  (ramp ' ' = 0 … '@' = {max:.0})");
    for row in 0..rows {
        let mut art = String::new();
        let mut nums = String::new();
        for col in 0..cols {
            let v = value(row * cols + col);
            art.push(ramp_char(v, max));
            nums.push_str(&format!("{:4.0}", v));
        }
        println!("    |{art}|{nums}");
    }
}

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

    if let Some(regional) = &stats.regional {
        let tiles = regional.tiles();
        println!(
            "\nRegional stats ({} x {} tiles of ~{}x{} px):",
            regional.cols(),
            regional.rows(),
            tiles[0].rect.width,
            tiles[0].rect.height,
        );
        println!(
            "  tonal spread      {:.1}   (p90−p10 of tile medians; high ⇒ wants local tone)",
            regional.tonal_spread()
        );
        match regional.vertical_skew() {
            Some(s) => println!(
                "  vertical skew     {s:+.1}   (top − bottom band; + ⇒ bright sky over dark subject)"
            ),
            None => println!("  vertical skew     n/a   (too few tile rows)"),
        }
        match regional.uniform_border() {
            Some(b) => println!(
                "  uniform border    {} ring(s) at level {:.0} → content {}x{} at ({},{})",
                b.rings,
                b.level,
                b.content_rect.width,
                b.content_rect.height,
                b.content_rect.x,
                b.content_rect.y,
            ),
            None => println!("  uniform border    none detected — planner uses the whole frame"),
        }

        print_tile_map(regional, "tile luma medians", 255.0, |i| {
            tiles[i].luma_median as f32
        });
        let chroma_max = tiles
            .iter()
            .map(|t| t.mean_chroma)
            .fold(1.0f32, f32::max)
            .max(30.0);
        print_tile_map(regional, "tile mean chroma", chroma_max, |i| {
            tiles[i].mean_chroma
        });
    }

    if let Some(content) = &stats.content {
        println!(
            "\nBorder excluded from the planner histograms: luma median {} → {}",
            analysis::median(&stats.hist.luma),
            analysis::median(&content.hist.luma),
        );
    }

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
