//! Smoke-test the panorama op from the command line.

use rasterlab_core::formats::FormatRegistry;
use rasterlab_core::image::Image;
use rasterlab_core::ops::PanoramaOp;
use rasterlab_core::traits::format_handler::EncodeOptions;
use rasterlab_core::traits::operation::Operation;

fn main() {
    rayon::ThreadPoolBuilder::new()
        .stack_size(16 * 1024 * 1024)
        .build_global()
        .unwrap();

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("usage: pano_test <img1> <img2> [img3 ...]");
        std::process::exit(1);
    }

    // Report per-image dimensions.
    let reg_probe = FormatRegistry::with_builtins();
    for p in &args {
        match reg_probe.decode_file(std::path::Path::new(p)) {
            Ok(img) => println!("  {} → {}x{}", p, img.width, img.height),
            Err(e) => println!("  {} → load error: {e}", p),
        }
    }

    let op = PanoramaOp::new(args.clone(), 80);
    let dummy = Image::new(1, 1);

    let t0 = std::time::Instant::now();
    match op.apply(dummy) {
        Ok(image) => {
            let ms = t0.elapsed().as_millis();
            println!("OK: {}x{} in {} ms", image.width, image.height, ms);
            let reg = FormatRegistry::with_builtins();
            let out = std::path::Path::new("/Users/tony/rasterlab/pano_out.png");
            let bytes = reg
                .encode_file(&image, out, &EncodeOptions::default())
                .expect("encode failed");
            std::fs::write(out, &bytes).expect("fs write failed");
            println!("Wrote {} ({} bytes)", out.display(), bytes.len());
        }
        Err(e) => {
            eprintln!("FAIL: {}", e);
            std::process::exit(2);
        }
    }
}
