//! Generate synthetic bracketed exposures for validating HDR merge.
//!
//! Writes three PNG files representing the same scene captured at
//! `0.25×`, `1×`, and `4×` exposure (–2 EV, 0 EV, +2 EV).  The scene is
//! a 10-stop linear-radiance gradient — no single LDR frame can capture
//! it without clipping, so a successful merge must pull detail from
//! different frames.
//!
//! Usage:
//!   cargo run --release --example hdr_synthetic -- [output_dir]
//!
//! Output:
//!   hdr_bracket_under.png   –2 EV (clips blacks)
//!   hdr_bracket_mid.png      0 EV (clips both ends)
//!   hdr_bracket_over.png    +2 EV (clips highlights)
//!
//! Then feed the three PNGs into the HDR Merge tool in the GUI, or:
//!   cargo run --release --example hdr_merge_test -- \
//!       hdr_bracket_under.png hdr_bracket_mid.png hdr_bracket_over.png

use std::{env, path::Path};

use image::{ImageBuffer, Rgb, RgbImage};

const WIDTH: u32 = 1024;
const HEIGHT: u32 = 512;
/// Dynamic range of the underlying scene, in stops.  2^10 = 1024×
/// between the darkest and brightest pixel.
const STOPS: f32 = 10.0;

#[inline]
fn linear_to_srgb8(c: f32) -> u8 {
    let c = c.clamp(0.0, 1.0);
    let s = if c <= 0.0031308 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (s * 255.0).round() as u8
}

/// Build a 2-D linear-radiance map.
///
/// * Horizontal axis: overall brightness, spanning `STOPS` stops from
///   `2^(−STOPS/2)` to `2^(STOPS/2)`.
/// * Vertical axis: adds a coloured component so the merge has real
///   chroma to preserve (pure greys would mask many bugs).  The top
///   half leans red, the bottom half leans blue.
fn radiance_at(x: u32, y: u32) -> [f32; 3] {
    let tx = x as f32 / (WIDTH - 1) as f32;
    let ty = y as f32 / (HEIGHT - 1) as f32;
    let base = (-STOPS * 0.5 + STOPS * tx).exp2();
    // Colour tint: red emphasis in the upper half, blue in the lower.
    let r_tint = 0.7 + 0.6 * (1.0 - ty);
    let b_tint = 0.7 + 0.6 * ty;
    [base * r_tint, base, base * b_tint]
}

fn render_exposure(exposure: f32) -> RgbImage {
    let mut img: RgbImage = ImageBuffer::new(WIDTH, HEIGHT);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let rad = radiance_at(x, y);
            let pixel = Rgb([
                linear_to_srgb8(rad[0] * exposure),
                linear_to_srgb8(rad[1] * exposure),
                linear_to_srgb8(rad[2] * exposure),
            ]);
            img.put_pixel(x, y, pixel);
        }
    }
    img
}

fn main() {
    let out_dir = env::args().nth(1).unwrap_or_else(|| ".".to_string());
    let dir = Path::new(&out_dir);
    std::fs::create_dir_all(dir).expect("create output dir");

    let brackets = [
        ("hdr_bracket_under.png", 0.25, "−2 EV"),
        ("hdr_bracket_mid.png", 1.0, "0 EV"),
        ("hdr_bracket_over.png", 4.0, "+2 EV"),
    ];

    for (name, exp, label) in brackets {
        let img = render_exposure(exp);
        let path = dir.join(name);
        img.save(&path).expect("save png");
        println!("wrote {} ({})", path.display(), label);
    }

    println!(
        "\nscene: {w}×{h}, {s} stops of dynamic range",
        w = WIDTH,
        h = HEIGHT,
        s = STOPS
    );
}
