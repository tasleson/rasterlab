/// Applies Local Tone to an image and reports what it did to the regional
/// measurements — the tonal spread and vertical skew that Adaptive Enhance's
/// analysis uses to recognise an unevenly-lit frame.
///
/// A successful run on a backlit photograph shows both descriptors shrinking:
/// the sky comes down, the subject comes up, and the frame stops disagreeing
/// with itself about what correct exposure means.
///
/// Usage:
///   cargo run --release --example local_tone -- <image_path> [tone] [detail]
use std::{env, path::PathBuf, time::Instant};

use rasterlab_core::{
    analysis::{ImageStats, RegionalStats},
    formats::FormatRegistry,
    ops::LocalLaplacianOp,
    traits::{format_handler::EncodeOptions, operation::Operation},
};

fn describe(label: &str, regional: &RegionalStats) {
    println!("  {label:8}  tonal spread {:6.1}", regional.tonal_spread());
    if let Some(skew) = regional.vertical_skew() {
        println!("            vertical skew {skew:+6.1}");
    }
}

/// Compact ASCII map of the tile medians, so the effect is visible per region.
fn tile_map(regional: &RegionalStats) {
    const RAMP: &[u8] = b" .:-=+*#%@";
    for row in 0..regional.rows() {
        print!("    |");
        for col in 0..regional.cols() {
            let v = regional.tile(col, row).map_or(0, |t| t.luma_median);
            let idx = (v as usize * (RAMP.len() - 1)) / 255;
            print!("{}", RAMP[idx] as char);
        }
        println!("|");
    }
}

fn main() {
    rayon::ThreadPoolBuilder::new()
        .stack_size(16 * 1024 * 1024)
        .build_global()
        .unwrap();

    let mut args = env::args().skip(1);
    let path = PathBuf::from(
        args.next()
            .expect("usage: local_tone <image_path> [tone] [detail]"),
    );
    let tone: f32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(0.7);
    let detail: f32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(0.2);

    let registry = FormatRegistry::with_builtins();
    let image = registry.decode_file(&path).expect("failed to load image");
    println!(
        "Loaded {} ({}x{})\nApplying tone {tone:+.2}, detail {detail:+.2}\n",
        path.display(),
        image.width,
        image.height
    );

    let before = ImageStats::compute(&image);
    if let Some(r) = &before.regional {
        describe("before", r);
        tile_map(r);
    }

    let t = Instant::now();
    let out = LocalLaplacianOp::with_defaults(tone, detail)
        .apply(image)
        .expect("local tone failed");
    let elapsed = t.elapsed().as_secs_f64() * 1000.0;

    let after = ImageStats::compute(&out);
    println!();
    if let Some(r) = &after.regional {
        describe("after", r);
        tile_map(r);
    }
    println!("\nApplied in {elapsed:.0} ms");

    let out_path = path.with_file_name(format!(
        "{}_localtone.png",
        path.file_stem().unwrap().to_string_lossy()
    ));
    let bytes = registry
        .encode_file(&out, &out_path, &EncodeOptions::default())
        .expect("encode failed");
    std::fs::write(&out_path, bytes).expect("write failed");
    println!("Wrote {}", out_path.display());
}
