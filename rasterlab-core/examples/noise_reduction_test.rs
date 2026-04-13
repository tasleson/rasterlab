/// Visual / numeric sanity check for the Noise Reduction operation.
///
/// Loads a clean image, adds Gaussian noise, runs each NR method, and
/// writes `noisy.png` / `denoised_wavelet.png` / `denoised_nlm.png` alongside.
/// Prints MSE and PSNR vs. the clean original for each output so you can
/// confirm NR actually moves the image *closer* to the ground truth.
///
/// Usage:
///   cargo run --release --example noise_reduction_test -- <clean_image> [sigma]
use std::{env, fs, path::PathBuf};

use rasterlab_core::{
    formats::FormatRegistry,
    image::Image,
    ops::noise_reduction::{NoiseReductionOp, NrMethod},
    traits::{format_handler::EncodeOptions, operation::Operation},
};

fn xorshift(state: &mut u32) -> u32 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *state = x;
    x
}

fn gauss(state: &mut u32) -> f32 {
    let mut s = 0.0f32;
    for _ in 0..12 {
        s += (xorshift(state) as f32) / (u32::MAX as f32);
    }
    s - 6.0
}

fn add_gaussian_noise(img: &Image, sigma: f32, seed: u32) -> Image {
    let mut out = img.deep_clone();
    let mut state = seed;
    for px in out.data.chunks_mut(4) {
        for ch in px.iter_mut().take(3) {
            let n = gauss(&mut state) * sigma;
            *ch = (*ch as f32 + n).clamp(0.0, 255.0) as u8;
        }
    }
    out
}

fn mse(a: &[u8], b: &[u8]) -> f64 {
    let mut sum = 0.0f64;
    let mut n = 0usize;
    for (pa, pb) in a.chunks(4).zip(b.chunks(4)) {
        for c in 0..3 {
            let d = pa[c] as f64 - pb[c] as f64;
            sum += d * d;
            n += 1;
        }
    }
    sum / n.max(1) as f64
}

fn psnr(mse: f64) -> f64 {
    if mse <= 0.0 {
        f64::INFINITY
    } else {
        10.0 * (255.0f64 * 255.0 / mse).log10()
    }
}

fn save_png(reg: &FormatRegistry, img: &Image, path: &PathBuf) {
    let opts = EncodeOptions::default();
    let bytes = reg.encode_file(img, path, &opts).expect("encode");
    fs::write(path, bytes).expect("write png");
    println!("  wrote {}", path.display());
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: noise_reduction_test <clean_image> [sigma=15]");
        std::process::exit(1);
    }
    let input = PathBuf::from(&args[1]);
    let sigma: f32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(15.0);

    let reg = FormatRegistry::with_builtins();
    let clean = reg.decode_file(&input).expect("decode input");
    println!(
        "clean: {} ({}x{})",
        input.display(),
        clean.width,
        clean.height
    );

    let noisy = add_gaussian_noise(&clean, sigma, 0xC0FFEE);
    let mse_noisy = mse(&clean.data, &noisy.data);
    println!(
        "noisy (σ={sigma}):              MSE={mse_noisy:8.2}  PSNR={:.2} dB",
        psnr(mse_noisy)
    );

    let out_dir = input.parent().unwrap_or_else(|| std::path::Path::new("."));

    let noisy_path = out_dir.join("noisy.png");
    save_png(&reg, &noisy, &noisy_path);

    for (label, method, out_name) in [
        ("wavelet", NrMethod::Wavelet, "denoised_wavelet.png"),
        ("nlm    ", NrMethod::NonLocalMeans, "denoised_nlm.png"),
    ] {
        let op = NoiseReductionOp {
            method,
            luma_strength: 0.5,
            color_strength: 0.5,
            detail_preservation: 0.3,
        };
        let t = std::time::Instant::now();
        let denoised = op.apply(noisy.deep_clone()).expect("apply NR");
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        let m = mse(&clean.data, &denoised.data);
        println!(
            "denoised ({label}):           MSE={m:8.2}  PSNR={:.2} dB  ({ms:.0} ms)",
            psnr(m)
        );
        save_png(&reg, &denoised, &out_dir.join(out_name));
    }
}
