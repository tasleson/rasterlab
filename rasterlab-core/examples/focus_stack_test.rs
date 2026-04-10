//! Smoke-test the focus stacking op from the command line.

use rasterlab_core::formats::FormatRegistry;
use rasterlab_core::image::Image;
use rasterlab_core::ops::FocusStackOp;
use rasterlab_core::traits::format_handler::EncodeOptions;
use rasterlab_core::traits::operation::Operation;

fn main() {
    rayon::ThreadPoolBuilder::new()
        .stack_size(16 * 1024 * 1024)
        .build_global()
        .unwrap();

    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // Optional `--reference <path>` flag: after stacking, compare against
    // the reference image and report RMSE + max per-channel delta.
    let mut reference: Option<String> = None;
    if let Some(i) = args.iter().position(|s| s == "--reference") {
        if i + 1 >= args.len() {
            eprintln!("--reference requires a path");
            std::process::exit(1);
        }
        reference = Some(args.remove(i + 1));
        args.remove(i);
    }
    if args.len() < 2 {
        eprintln!("usage: focus_stack_test [--reference <path>] <img1> <img2> [img3 ...]");
        std::process::exit(1);
    }

    let reg_probe = FormatRegistry::with_builtins();
    for p in &args {
        match reg_probe.decode_file(std::path::Path::new(p)) {
            Ok(img) => println!("  {} → {}x{}", p, img.width, img.height),
            Err(e) => println!("  {} → load error: {e}", p),
        }
    }

    let op = FocusStackOp::new(args.clone());
    let dummy = Image::new(1, 1);

    let t0 = std::time::Instant::now();
    match op.apply(dummy) {
        Ok(image) => {
            let ms = t0.elapsed().as_millis();
            println!("OK: {}x{} in {} ms", image.width, image.height, ms);
            let reg = FormatRegistry::with_builtins();
            let out = std::path::Path::new("/Users/tony/rasterlab/focus_stack_out.png");
            let bytes = reg
                .encode_file(&image, out, &EncodeOptions::default())
                .expect("encode failed");
            std::fs::write(out, &bytes).expect("fs write failed");
            println!("Wrote {} ({} bytes)", out.display(), bytes.len());

            if let Some(ref_path) = reference {
                let reference_img = reg
                    .decode_file(std::path::Path::new(&ref_path))
                    .expect("reference load failed");
                assert_eq!(
                    (image.width, image.height),
                    (reference_img.width, reference_img.height),
                    "reference dimensions differ"
                );
                let n = (image.width as usize) * (image.height as usize);
                let mut se = 0.0f64;
                let mut max_d = 0i32;
                for i in 0..n {
                    for c in 0..3 {
                        let d = image.data[i * 4 + c] as i32 - reference_img.data[i * 4 + c] as i32;
                        se += (d * d) as f64;
                        if d.abs() > max_d {
                            max_d = d.abs();
                        }
                    }
                }
                let rmse = (se / (n as f64 * 3.0)).sqrt();
                println!("vs {ref_path}: RMSE={rmse:.3}  max channel delta={max_d}");
            }
        }
        Err(e) => {
            eprintln!("FAIL: {}", e);
            std::process::exit(2);
        }
    }
}
