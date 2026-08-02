use rayon::prelude::*;
use serde::{Deserialize, Deserializer, Serialize};
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{error::RasterResult, image::Image, traits::operation::Operation};

// Vertical perforation geometry is shared with the marking layout so text and
// edge codes can be guaranteed to stay in the clear rebate lanes.
const HOLE_HALF_H: f32 = 1.48 / 35.0;
const HOLE_CENTER_Y: f32 = 3.20 / 35.0;

/// Film stocks used for the randomized edge printing.
///
/// The list is intentionally limited to well-known 35 mm stocks that are
/// still sold by their manufacturers. It is edge printing, not a colour-film
/// emulation: choosing HP5, for example, does not silently make the image B&W.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FilmStock {
    #[default]
    KodakPortra400,
    KodakGold200,
    KodakEktar100,
    KodakTriX400,
    IlfordHp5Plus,
    IlfordFp4Plus,
    IlfordDelta400,
    Fujifilm200,
    Fujifilm400,
}

impl FilmStock {
    pub const ALL: [Self; 9] = [
        Self::KodakPortra400,
        Self::KodakGold200,
        Self::KodakEktar100,
        Self::KodakTriX400,
        Self::IlfordHp5Plus,
        Self::IlfordFp4Plus,
        Self::IlfordDelta400,
        Self::Fujifilm200,
        Self::Fujifilm400,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::KodakPortra400 => "KODAK PORTRA 400",
            Self::KodakGold200 => "KODAK GOLD 200",
            Self::KodakEktar100 => "KODAK EKTAR 100",
            Self::KodakTriX400 => "KODAK TRI-X 400",
            Self::IlfordHp5Plus => "ILFORD HP5 PLUS",
            Self::IlfordFp4Plus => "ILFORD FP4 PLUS",
            Self::IlfordDelta400 => "ILFORD DELTA 400",
            Self::Fujifilm200 => "FUJIFILM 200",
            Self::Fujifilm400 => "FUJIFILM 400",
        }
    }

    fn is_bw(self) -> bool {
        matches!(
            self,
            Self::KodakTriX400 | Self::IlfordHp5Plus | Self::IlfordFp4Plus | Self::IlfordDelta400
        )
    }

    fn ink_color(self) -> [u8; 3] {
        if self.is_bw() {
            [225, 228, 218]
        } else {
            [225, 151, 88]
        }
    }
}

/// Render a full-width 35 mm negative scan, including both rows of sprocket
/// holes, edge codes, and stock/frame markings.
///
/// The dimensions are based on standard 35 mm film: perforations repeat every
/// 4.75 mm and the film is 35 mm tall. The treatment is relative to the image
/// height, so it remains consistent at preview and export resolutions.
#[derive(Debug, Clone, Serialize)]
pub struct SprocketFilmOp {
    #[serde(default)]
    pub stock: FilmStock,
    #[serde(default = "default_frame_number")]
    pub frame_number: u8,
    /// Fixes the perforation phase and distressed edge-code pattern so renders
    /// of a saved operation are deterministic.
    #[serde(default)]
    pub marking_seed: u64,
}

fn default_frame_number() -> u8 {
    12
}

impl Default for SprocketFilmOp {
    fn default() -> Self {
        Self {
            stock: FilmStock::default(),
            frame_number: default_frame_number(),
            marking_seed: 0,
        }
    }
}

// A former version of this operation was a unit struct, and therefore appears
// in old internally-tagged pipeline JSON with no fields after the type tag.
// Defaults on every field keep those projects loadable.
impl<'de> Deserialize<'de> for SprocketFilmOp {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Fields {
            #[serde(default)]
            stock: FilmStock,
            #[serde(default = "default_frame_number")]
            frame_number: u8,
            #[serde(default)]
            marking_seed: u64,
        }

        let fields = Fields::deserialize(deserializer)?;
        Ok(Self {
            stock: fields.stock,
            frame_number: fields.frame_number.clamp(1, 36),
            marking_seed: fields.marking_seed,
        })
    }
}

impl SprocketFilmOp {
    /// Choose a manufacturer, one of its current stocks, and a 1–36 frame
    /// number. Manufacturer choice is even, rather than weighting Kodak more
    /// heavily merely because it has more stocks in the list.
    pub fn randomized() -> Self {
        static APPLY_COUNTER: AtomicU64 = AtomicU64::new(0);

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos() as u64);
        let sequence = APPLY_COUNTER.fetch_add(1, Ordering::Relaxed);
        let seed = mix64(nanos ^ sequence.wrapping_mul(0x9e37_79b9_7f4a_7c15));
        let stock_roll = mix64(seed ^ 0xa076_1d64_78bd_642f);
        let stock = match seed % 3 {
            0 => {
                const KODAK: [FilmStock; 4] = [
                    FilmStock::KodakPortra400,
                    FilmStock::KodakGold200,
                    FilmStock::KodakEktar100,
                    FilmStock::KodakTriX400,
                ];
                KODAK[stock_roll as usize % KODAK.len()]
            }
            1 => {
                const ILFORD: [FilmStock; 3] = [
                    FilmStock::IlfordHp5Plus,
                    FilmStock::IlfordFp4Plus,
                    FilmStock::IlfordDelta400,
                ];
                ILFORD[stock_roll as usize % ILFORD.len()]
            }
            _ => {
                const FUJIFILM: [FilmStock; 2] = [FilmStock::Fujifilm200, FilmStock::Fujifilm400];
                FUJIFILM[stock_roll as usize % FUJIFILM.len()]
            }
        };

        Self {
            stock,
            frame_number: (mix64(seed ^ 0xe703_7ed1_a0b4_28db) % 36 + 1) as u8,
            marking_seed: seed,
        }
    }

    /// Use a chosen stock while retaining randomized frame markings.
    pub fn with_random_markings(stock: FilmStock) -> Self {
        Self {
            stock,
            ..Self::randomized()
        }
    }
}

#[typetag::serde]
impl Operation for SprocketFilmOp {
    fn name(&self) -> &'static str {
        "sprocket_film"
    }

    fn clone_box(&self) -> Box<dyn Operation> {
        Box::new(self.clone())
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn apply(&self, mut image: Image) -> RasterResult<Image> {
        let width = image.width as usize;
        let height = image.height as usize;
        if width == 0 || height == 0 {
            return Ok(image);
        }

        // All measurements are fractions of the 35 mm film height. The
        // reference scans use vertically oriented, softly rounded KS-style
        // perforations, rather than the wide slots the first version drew.
        const PERF_PITCH: f32 = 4.75 / 35.0;
        const HOLE_HALF_W: f32 = 1.02 / 35.0;
        const CORNER_RADIUS: f32 = 0.42 / 35.0;
        const HALO_WIDTH: f32 = 0.30 / 35.0;
        const PERFORATION_EFFECT_END: f32 = HOLE_CENTER_Y + HOLE_HALF_H + HALO_WIDTH;

        let film_span = width as f32 / height as f32;
        let hole_count = (film_span / PERF_PITCH).round().max(1.0);
        // Fit a whole number of perforations to the frame while retaining a
        // stable, scan-like phase that can leave a partial hole at a cut end.
        let fitted_pitch = film_span / hole_count;
        let aa = (0.75 / height as f32).max(0.000_1);
        let phase = if self.marking_seed == 0 {
            0.0
        } else {
            unit_hash(self.marking_seed, 17, 41) * fitted_pitch
        };
        let halo_color = if self.stock.is_bw() {
            [150, 153, 148]
        } else {
            [194, 70, 24]
        };
        image
            .data
            .par_chunks_exact_mut(width * 4)
            .enumerate()
            .for_each(|(y, row)| {
                let film_y = (y as f32 + 0.5) / height as f32;
                let edge_y = film_y.min(1.0 - film_y);

                // Leave the photograph completely untouched outside the two
                // perforation rows. In particular, do not add a dark scan or
                // cut-edge treatment around the finished image.
                if edge_y > PERFORATION_EFFECT_END + aa {
                    return;
                }

                let hole_y = if film_y < 0.5 {
                    HOLE_CENTER_Y
                } else {
                    1.0 - HOLE_CENTER_Y
                };

                for x in 0..width {
                    let film_x = (x as f32 + 0.5) / height as f32;
                    let hole_index = ((film_x + phase) / fitted_pitch).floor();
                    let hole_x = (hole_index + 0.5) * fitted_pitch - phase;
                    let sdf = rounded_rect_sdf(
                        film_x,
                        film_y,
                        hole_x,
                        hole_y,
                        HOLE_HALF_W,
                        HOLE_HALF_H,
                        CORNER_RADIUS,
                    );

                    let off = x * 4;
                    let px = &mut row[off..off + 4];

                    // Slight bloom around the clear perforations, warm on
                    // colour-negative stocks and neutral on B&W stock.
                    let hole_coverage = smooth_coverage(sdf, aa);
                    let halo = (1.0 - (sdf.max(0.0) / HALO_WIDTH).clamp(0.0, 1.0))
                        * (1.0 - hole_coverage)
                        * if self.stock.is_bw() { 0.14 } else { 0.34 };
                    blend_rgb(px, halo_color, halo);
                    blend_rgb(px, [1, 1, 1], hole_coverage * 0.995);
                }
            });

        draw_edge_markings(&mut image, self.stock, self.frame_number, self.marking_seed);

        Ok(image)
    }

    fn describe(&self) -> String {
        format!(
            "35mm Sprocket Film — {} — frame {}",
            self.stock.label(),
            self.frame_number
        )
    }
}

#[inline]
fn mix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

#[inline]
fn unit_hash(seed: u64, x: u64, y: u64) -> f32 {
    let bits = mix64(seed ^ x.wrapping_mul(0x517c_c1b7_2722_0a95) ^ y.rotate_left(29));
    (bits >> 40) as f32 / ((1_u32 << 24) - 1) as f32
}

fn draw_edge_markings(image: &mut Image, stock: FilmStock, frame_number: u8, seed: u64) {
    let width = image.width as usize;
    let height = image.height as usize;
    if width == 0 || height == 0 {
        return;
    }

    let ink = stock.ink_color();
    let (top_lane_end, bottom_lane_start) = marking_lane_bounds(height);
    draw_edge_code(image, ink, seed, bottom_lane_start);

    // A deliberately coarse OCR/dot-matrix face resembles exposed film-edge
    // lettering and remains legible even in quarter-resolution previews. Keep
    // it smaller than the original treatment and reserve one scale unit of
    // clear space on both sides so it touches neither the image edge nor a
    // perforation.
    let preferred_scale = (height / 210).max(1);
    let clear_lane_height = top_lane_end.min(height.saturating_sub(bottom_lane_start));
    let scale = preferred_scale.min(clear_lane_height / 9);
    if scale == 0 {
        return;
    }
    let glyph_height = 7 * scale;
    let top_y = ((top_lane_end - glyph_height) / 2) as i32;
    let label = stock.label();
    let label_width = text_width(label, scale);
    let jitter_span = (width / 30).max(1);
    let jitter = (mix64(seed ^ 0x243f_6a88_85a3_08d3) as usize % jitter_span) as i32
        - (jitter_span / 2) as i32;
    let mut label_x = width as i32 / 11 + jitter;
    let repeat = ((width as f32 * 0.56) as usize).max(label_width + width / 12);
    while label_x < width as i32 {
        draw_text(image, label_x, top_y, label, scale, ink, 0.78);
        label_x += repeat as i32;
    }

    let next_frame = if frame_number == 36 {
        1
    } else {
        frame_number + 1
    };
    let labels = [
        frame_number.to_string(),
        format!(">{}A", frame_number),
        next_frame.to_string(),
    ];
    let centers = [width / 5, width / 2, width * 4 / 5];
    let bottom_y = (bottom_lane_start + (height - bottom_lane_start - glyph_height) / 2) as i32;
    for (label, center) in labels.iter().zip(centers) {
        let x = center.saturating_sub(text_width(label, scale) / 2) as i32;
        draw_text(image, x, bottom_y, label, scale, ink, 0.88);
    }
}

/// Pixel bounds of the clear rebate lanes, excluding a small safety gap around
/// the antialiased perforation edge. The top lane is `0..top_end`; the bottom
/// lane is `bottom_start..height`.
fn marking_lane_bounds(height: usize) -> (usize, usize) {
    let hole_top = ((HOLE_CENTER_Y - HOLE_HALF_H) * height as f32).floor() as usize;
    let hole_bottom = ((1.0 - HOLE_CENTER_Y + HOLE_HALF_H) * height as f32).ceil() as usize;
    let safety_gap = (height / 400).max(1);
    (
        hole_top.saturating_sub(safety_gap),
        hole_bottom.saturating_add(safety_gap).min(height),
    )
}

/// Draw irregular exposed manufacturer/data coding along the bottom cut edge.
fn draw_edge_code(image: &mut Image, color: [u8; 3], seed: u64, lane_start: usize) {
    let width = image.width as usize;
    let height = image.height as usize;
    let max_bar_height = height.saturating_sub(lane_start);
    if max_bar_height == 0 {
        return;
    }
    let unit = (height / 205).max(1);
    let mut x = 0usize;
    let mut index = 0u64;

    while x < width {
        let random = mix64(seed ^ index.wrapping_mul(0xd6e8_feb8_6659_fd93));
        let bar_width = ((random & 0x3) as usize + 1) * unit;
        let gap = (((random >> 5) & 0x3) as usize + 1) * unit;
        let bar_height = ((((random >> 9) & 0x7) as usize + 5) * unit).min(max_bar_height);
        let alpha = 0.38 + ((random >> 16) & 0xff) as f32 / 255.0 * 0.24;
        fill_rect_blended(
            image,
            x as i32,
            height.saturating_sub(bar_height) as i32,
            bar_width,
            bar_height,
            color,
            alpha,
        );
        x = x.saturating_add(bar_width + gap);
        index += 1;
    }
}

fn text_width(text: &str, scale: usize) -> usize {
    text.chars()
        .count()
        .saturating_mul(6 * scale)
        .saturating_sub(scale)
}

fn draw_text(
    image: &mut Image,
    x: i32,
    y: i32,
    text: &str,
    scale: usize,
    color: [u8; 3],
    alpha: f32,
) {
    let mut cursor = x;
    for character in text.chars() {
        let rows = glyph_rows(character.to_ascii_uppercase());
        for (row, bits) in rows.into_iter().enumerate() {
            for column in 0..5 {
                if bits & (1 << (4 - column)) != 0 {
                    fill_rect_blended(
                        image,
                        cursor + (column * scale) as i32,
                        y + (row * scale) as i32,
                        scale,
                        scale,
                        color,
                        alpha,
                    );
                }
            }
        }
        cursor += (6 * scale) as i32;
    }
}

fn fill_rect_blended(
    image: &mut Image,
    x: i32,
    y: i32,
    width: usize,
    height: usize,
    color: [u8; 3],
    alpha: f32,
) {
    let x0 = x.max(0) as usize;
    let y0 = y.max(0) as usize;
    let x1 = (x.saturating_add(width as i32)).max(0) as usize;
    let y1 = (y.saturating_add(height as i32)).max(0) as usize;
    let x1 = x1.min(image.width as usize);
    let y1 = y1.min(image.height as usize);
    if x0 >= x1 || y0 >= y1 {
        return;
    }

    let stride = image.width as usize * 4;
    for py in y0..y1 {
        let row = &mut image.data[py * stride..(py + 1) * stride];
        for px in x0..x1 {
            blend_rgb(&mut row[px * 4..px * 4 + 4], color, alpha);
        }
    }
}

/// Five-by-seven bitmap glyphs used by film stock and frame markings.
fn glyph_rows(character: char) -> [u8; 7] {
    match character {
        'A' => [0x0e, 0x11, 0x11, 0x1f, 0x11, 0x11, 0x11],
        'B' => [0x1e, 0x11, 0x11, 0x1e, 0x11, 0x11, 0x1e],
        'C' => [0x0f, 0x10, 0x10, 0x10, 0x10, 0x10, 0x0f],
        'D' => [0x1e, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1e],
        'E' => [0x1f, 0x10, 0x10, 0x1e, 0x10, 0x10, 0x1f],
        'F' => [0x1f, 0x10, 0x10, 0x1e, 0x10, 0x10, 0x10],
        'G' => [0x0f, 0x10, 0x10, 0x13, 0x11, 0x11, 0x0f],
        'H' => [0x11, 0x11, 0x11, 0x1f, 0x11, 0x11, 0x11],
        'I' => [0x1f, 0x04, 0x04, 0x04, 0x04, 0x04, 0x1f],
        'J' => [0x07, 0x02, 0x02, 0x02, 0x12, 0x12, 0x0c],
        'K' => [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11],
        'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1f],
        'M' => [0x11, 0x1b, 0x15, 0x15, 0x11, 0x11, 0x11],
        'N' => [0x11, 0x19, 0x15, 0x13, 0x11, 0x11, 0x11],
        'O' => [0x0e, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0e],
        'P' => [0x1e, 0x11, 0x11, 0x1e, 0x10, 0x10, 0x10],
        'Q' => [0x0e, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0d],
        'R' => [0x1e, 0x11, 0x11, 0x1e, 0x14, 0x12, 0x11],
        'S' => [0x0f, 0x10, 0x10, 0x0e, 0x01, 0x01, 0x1e],
        'T' => [0x1f, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0e],
        'V' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x0a, 0x04],
        'W' => [0x11, 0x11, 0x11, 0x15, 0x15, 0x15, 0x0a],
        'X' => [0x11, 0x11, 0x0a, 0x04, 0x0a, 0x11, 0x11],
        'Y' => [0x11, 0x11, 0x0a, 0x04, 0x04, 0x04, 0x04],
        'Z' => [0x1f, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1f],
        '0' => [0x0e, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0e],
        '1' => [0x04, 0x0c, 0x14, 0x04, 0x04, 0x04, 0x1f],
        '2' => [0x0e, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1f],
        '3' => [0x1e, 0x01, 0x01, 0x0e, 0x01, 0x01, 0x1e],
        '4' => [0x02, 0x06, 0x0a, 0x12, 0x1f, 0x02, 0x02],
        '5' => [0x1f, 0x10, 0x10, 0x1e, 0x01, 0x01, 0x1e],
        '6' => [0x0e, 0x10, 0x10, 0x1e, 0x11, 0x11, 0x0e],
        '7' => [0x1f, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0e, 0x11, 0x11, 0x0e, 0x11, 0x11, 0x0e],
        '9' => [0x0e, 0x11, 0x11, 0x0f, 0x01, 0x01, 0x0e],
        '-' => [0x00, 0x00, 0x00, 0x1f, 0x00, 0x00, 0x00],
        '+' => [0x00, 0x04, 0x04, 0x1f, 0x04, 0x04, 0x00],
        '>' => [0x10, 0x08, 0x04, 0x02, 0x04, 0x08, 0x10],
        ' ' => [0; 7],
        _ => [0x0e, 0x11, 0x01, 0x02, 0x04, 0x00, 0x04],
    }
}

/// Signed distance to a rounded rectangle. Negative values are inside.
#[inline]
fn rounded_rect_sdf(
    x: f32,
    y: f32,
    cx: f32,
    cy: f32,
    half_w: f32,
    half_h: f32,
    radius: f32,
) -> f32 {
    let qx = (x - cx).abs() - (half_w - radius);
    let qy = (y - cy).abs() - (half_h - radius);
    let outside = qx.max(0.0).hypot(qy.max(0.0));
    let inside = qx.max(qy).min(0.0);
    outside + inside - radius
}

#[inline]
fn smooth_coverage(signed_distance: f32, aa: f32) -> f32 {
    ((aa - signed_distance) / (2.0 * aa)).clamp(0.0, 1.0)
}

#[inline]
fn blend_rgb(pixel: &mut [u8], color: [u8; 3], amount: f32) {
    if amount <= 0.0 {
        return;
    }
    let inv = 1.0 - amount;
    for channel in 0..3 {
        pixel[channel] =
            (pixel[channel] as f32 * inv + color[channel] as f32 * amount).round() as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid_image(width: u32, height: u32, color: [u8; 4]) -> Image {
        let mut image = Image::new(width, height);
        image
            .data
            .chunks_exact_mut(4)
            .for_each(|pixel| pixel.copy_from_slice(&color));
        image
    }

    #[test]
    fn preserves_dimensions_and_center_pixels() {
        let source = solid_image(700, 350, [80, 140, 210, 173]);
        let output = SprocketFilmOp::default().apply(source).unwrap();

        assert_eq!((output.width, output.height), (700, 350));
        assert_eq!(output.pixel(350, 175), [80, 140, 210, 173]);
        assert!(output.data.chunks_exact(4).all(|pixel| pixel[3] == 173));
    }

    #[test]
    fn renders_black_perforations_in_both_rebates() {
        let output = SprocketFilmOp::default()
            .apply(solid_image(700, 350, [220, 220, 220, 255]))
            .unwrap();

        // A 70 mm-wide frame fits 15 holes. Check every hole centre: edge
        // printing is drawn last, so this also guards against markings being
        // painted back over any perforation.
        for index in 0..15 {
            let x = (((index as f32 + 0.5) * 700.0 / 15.0).floor() as u32).min(699);
            for y in [32, 317] {
                let pixel = output.pixel(x, y);
                assert!(
                    pixel[0] < 15 && pixel[1] < 15 && pixel[2] < 15,
                    "hole {index} at ({x}, {y}) was overwritten: {pixel:?}"
                );
            }
        }
    }

    #[test]
    fn preserves_image_brightness_between_perforations() {
        let source_color = [120, 160, 200, 255];
        let output = SprocketFilmOp::default()
            .apply(solid_image(700, 350, source_color))
            .unwrap();

        // Midway between adjacent holes, on the same rows as their centres.
        // These pixels used to be darkened by the broad rebate overlay.
        assert_eq!(output.pixel(47, 32), source_color);
        assert_eq!(output.pixel(47, 317), source_color);
    }

    #[test]
    fn does_not_add_an_outer_border() {
        let source_color = [120, 160, 200, 255];
        let output = SprocketFilmOp::default()
            .apply(solid_image(700, 350, source_color))
            .unwrap();

        // The old scan-edge treatment darkened both vertical sides and the
        // full top edge. These locations are outside all printed markings.
        assert_eq!(output.pixel(0, 175), source_color);
        assert_eq!(output.pixel(699, 175), source_color);
        assert_eq!(output.pixel(350, 0), source_color);
    }

    #[test]
    fn handles_tiny_and_empty_images() {
        assert_eq!(
            SprocketFilmOp::default()
                .apply(Image::new(0, 0))
                .unwrap()
                .data
                .len(),
            0
        );
        let tiny = SprocketFilmOp::default().apply(Image::new(1, 1)).unwrap();
        assert_eq!(tiny.data.len(), 4);
    }

    #[test]
    fn serializes_as_pipeline_operation() {
        let op: Box<dyn Operation> = Box::new(SprocketFilmOp {
            stock: FilmStock::IlfordHp5Plus,
            frame_number: 27,
            marking_seed: 1234,
        });
        let json = serde_json::to_value(&op).unwrap();
        let restored: Box<dyn Operation> = serde_json::from_value(json).unwrap();
        assert_eq!(restored.name(), "sprocket_film");
        assert!(restored.describe().contains("ILFORD HP5 PLUS"));
        assert!(restored.describe().contains("frame 27"));
    }

    #[test]
    fn loads_fieldless_legacy_pipeline_operation() {
        let restored: Box<dyn Operation> =
            serde_json::from_value(serde_json::json!({ "type": "SprocketFilmOp" })).unwrap();
        assert_eq!(restored.name(), "sprocket_film");
        assert!(restored.describe().contains("KODAK PORTRA 400"));
    }

    #[test]
    fn randomized_markings_are_valid_and_stable_when_cloned() {
        let op = SprocketFilmOp::randomized();
        assert!((1..=36).contains(&op.frame_number));
        let clone = op.clone();
        assert_eq!(clone.stock, op.stock);
        assert_eq!(clone.frame_number, op.frame_number);
        assert_eq!(clone.marking_seed, op.marking_seed);
    }

    #[test]
    fn chosen_stock_keeps_random_frame_markings() {
        let op = SprocketFilmOp::with_random_markings(FilmStock::Fujifilm400);
        assert_eq!(op.stock, FilmStock::Fujifilm400);
        assert!((1..=36).contains(&op.frame_number));
        assert!(op.describe().contains("FUJIFILM 400"));
    }

    #[test]
    fn marking_lanes_never_reach_perforations() {
        for height in 1..=4096 {
            let (top_end, bottom_start) = marking_lane_bounds(height);
            let hole_top = ((HOLE_CENTER_Y - HOLE_HALF_H) * height as f32).floor() as usize;
            let hole_bottom = ((1.0 - HOLE_CENTER_Y + HOLE_HALF_H) * height as f32).ceil() as usize;
            assert!(top_end <= hole_top, "height {height}");
            assert!(bottom_start >= hole_bottom.min(height), "height {height}");

            let lane_height = top_end.min(height.saturating_sub(bottom_start));
            let scale = (height / 210).max(1).min(lane_height / 9);
            assert!(9 * scale <= top_end, "height {height}");
            assert!(9 * scale <= height - bottom_start, "height {height}");
        }
    }
}
