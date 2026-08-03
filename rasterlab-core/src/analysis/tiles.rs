//! Regional (tiled) image statistics.
//!
//! Global histograms answer "what tones does this image contain?" but not
//! "*where* are they?".  Two images with identical global histograms — a
//! backlit portrait and an evenly-lit landscape — call for completely
//! different corrections, and no amount of whole-frame math can tell them
//! apart.  This module measures the image over a grid of roughly 8×8 tiles
//! and derives descriptors from the *spread between* tiles:
//!
//! * [`RegionalStats::tonal_spread`] — how unevenly lit the frame is.
//! * [`RegionalStats::vertical_skew`] — bright-top/dark-bottom (backlit).
//! * [`RegionalStats::uniform_border`] — a Polaroid frame or scan margin.
//!
//! The tile pass deliberately rides along on the luma plane the Laplacian
//! variance already builds (see [`super::ImageStats::compute`]), so it costs
//! one extra read of an 8-bit plane rather than a second pass over RGBA.
//! Chroma is the one quantity that genuinely needs RGB, so it is sampled on
//! the same 1-in-4 pixel stride the global chroma measurement uses.

use rayon::prelude::*;

use crate::image::Image;

// ── Grid geometry ─────────────────────────────────────────────────────────────

/// Tiles along each axis for a square image.  8×8 = 64 tiles is the usual
/// choice for local tone operators (it is what Photoshop-era "local contrast"
/// grids and the local Laplacian literature use): fine enough that a sky, a
/// face and a shadow land in different tiles, coarse enough that each tile
/// still holds tens of thousands of pixels and its median is stable.
const TILES_PER_AXIS: f64 = 8.0;

/// Smallest tile edge, in pixels, that still yields a meaningful median.
/// Below this a "tile" is a handful of pixels and its statistics are noise,
/// so small images get fewer, larger tiles instead of a full 8×8 grid.
const MIN_TILE_PX: u32 = 8;

/// Pixels sampled per row when measuring per-tile chroma: one in four,
/// matching `sampled_mean_chroma`'s stride so the two chroma numbers are
/// directly comparable and neither costs a dense RGBA traversal.
const CHROMA_PIXEL_STRIDE: usize = 4;

/// Rows sampled per tile when measuring chroma: one in four.
///
/// The pixel stride alone does *not* save any memory traffic — at 4 bytes per
/// pixel a 64-byte cache line holds 16 pixels, so taking every 4th pixel still
/// touches every line and reads the whole RGBA buffer.  Skipping rows is what
/// actually reduces the bytes fetched.  Together the two strides sample 1 in
/// 16 pixels, which for a tile holding tens of thousands of pixels leaves the
/// mean accurate to well under one level while cutting this pass — measured as
/// the dominant cost of the regional grid — to roughly a quarter.
const CHROMA_ROW_STRIDE: usize = 4;

// ── Border-detection thresholds ───────────────────────────────────────────────

/// Luma variance at or below which a tile counts as *flat*.  Variance 16 is a
/// standard deviation of 4 levels out of 255 — about what scanner noise and
/// paper grain produce on a blank Polaroid margin.  Real photographic content
/// measures in the hundreds to thousands, so the gap is enormous and the
/// threshold does not need to be precise.
const BORDER_FLAT_VARIANCE: f32 = 16.0;

/// Maximum spread (max − min) of the medians of all detected border tiles.
/// A genuine frame is one material under one light, so its tiles agree to
/// within a few levels; 10 leaves room for scanner vignetting across a large
/// margin while rejecting "flat but unrelated" tiles such as a blown sky on
/// one edge and a black shadow on another.
const BORDER_LEVEL_SPREAD: f32 = 10.0;

/// Minimum tonal spread the *interior* must show before we believe the
/// perimeter is a border.  Without this, a photograph of a blank wall would
/// be detected as "all border and no picture" and the planner would end up
/// analysing an arbitrary crop of it.
const BORDER_CONTENT_SPREAD: f32 = 12.0;

/// Smallest grid on which the ring logic is meaningful: excluding one ring
/// from a 4×4 grid still leaves a 2×2 interior.  Anything smaller means the
/// image is too small (or too extreme in aspect) to talk about a border.
const BORDER_MIN_GRID: u32 = 4;

/// The interior must retain at least this fraction of the frame's area.  A
/// "border" that swallows three quarters of the picture is a mis-detection,
/// not a mount; refusing it keeps the failure mode safe.
const BORDER_MIN_CONTENT_FRACTION: f32 = 0.25;

/// How far a pixel may sit from the detected border level and still count as
/// border during the pixel-accurate edge refinement.  ±12 levels absorbs
/// paper grain, scanner vignetting across a large margin and JPEG ringing
/// along the frame edge, while any real picture content differs by far more.
const BORDER_LEVEL_TOLERANCE: f32 = 12.0;

/// Fraction of a scan line's pixels that must match the border level for the
/// whole line to count as border.  Just under 1.0 so a dust speck or a
/// scratch does not stop the refinement one line early, but high enough that
/// the first line containing actual picture ends it.
const BORDER_LINE_PURITY: f32 = 0.98;

/// Fraction of the tile rows treated as "top" and "bottom" when measuring
/// vertical skew.  A third at each end leaves a middle band out of the
/// comparison, so the number reports a genuine top-to-bottom gradient rather
/// than tile-to-tile noise around the centre.
const SKEW_BAND_FRACTION: u32 = 3;

// ── Types ─────────────────────────────────────────────────────────────────────

/// A half-open pixel rectangle: `x .. x + width`, `y .. y + height`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    /// The whole image.
    pub fn whole(image: &Image) -> Self {
        Self {
            x: 0,
            y: 0,
            width: image.width,
            height: image.height,
        }
    }

    pub fn area(&self) -> u64 {
        self.width as u64 * self.height as u64
    }

    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }
}

/// Statistics for a single tile of the regional grid.
///
/// Luma figures are exact (computed from a full 256-bucket histogram of the
/// tile's pixels in the shared luma plane).  The RGB-derived figures are
/// sampled on [`CHROMA_PIXEL_STRIDE`], which is plenty for tile-scale means.
#[derive(Debug, Clone, Copy)]
pub struct TileStats {
    /// The tile's pixel bounds within the image.
    pub rect: Rect,
    /// 10th percentile of luma — the tile's shadow level.
    pub luma_p10: u8,
    /// Median luma — the tile's overall level, and the value the derived
    /// descriptors are built from (medians ignore small bright/dark outliers
    /// such as specular highlights that would drag a mean around).
    pub luma_median: u8,
    /// 90th percentile of luma — the tile's highlight level.
    pub luma_p90: u8,
    /// Mean luma.
    pub luma_mean: f32,
    /// Variance of luma within the tile.  Near zero means the tile is flat
    /// (sky, paper, a blown highlight); large means it holds detail.
    pub luma_variance: f32,
    /// Mean chroma (max − min of RGB, 0–255), the same definition the global
    /// saturation stage uses, sampled on a stride.
    pub mean_chroma: f32,
    /// Mean R, G and B of the sampled pixels.  Cheaper than per-tile channel
    /// histograms (which would need a dense RGB pass) and enough to see a
    /// cast that varies across the frame.
    pub mean_rgb: [f32; 3],
    /// Number of pixels in the tile.
    pub pixels: u32,
}

/// A confidently-detected uniform border: a Polaroid frame, a slide mount, or
/// the flatbed lid showing around a small print.
#[derive(Debug, Clone, Copy)]
pub struct BorderRegion {
    /// The picture area with the border removed — what the planner should
    /// measure instead of the whole frame.
    pub content_rect: Rect,
    /// How many tile rings were excluded.  Detection is tile-granular, so the
    /// excluded margin is a multiple of the tile size and always errs toward
    /// keeping picture rather than cutting into it.
    pub rings: u32,
    /// Mean luma of the border tiles (a white paper frame lands near 240, a
    /// black slide mount near 15).
    pub level: f32,
}

/// Tile grid plus the scene descriptors derived from it.
///
/// Row-major: tile `(col, row)` is at index `row * cols + col`.
#[derive(Debug, Clone)]
pub struct RegionalStats {
    cols: u32,
    rows: u32,
    tiles: Vec<TileStats>,
    border: Option<BorderRegion>,
}

impl RegionalStats {
    /// Number of tile columns.
    pub fn cols(&self) -> u32 {
        self.cols
    }

    /// Number of tile rows.
    pub fn rows(&self) -> u32 {
        self.rows
    }

    /// All tiles, row-major.
    pub fn tiles(&self) -> &[TileStats] {
        &self.tiles
    }

    /// The tile at `(col, row)`, or `None` if out of range.
    pub fn tile(&self, col: u32, row: u32) -> Option<&TileStats> {
        (col < self.cols && row < self.rows).then(|| &self.tiles[(row * self.cols + col) as usize])
    }

    /// Measure `image` over the tile grid, reading luma from `luma` — the
    /// full-resolution 1-byte-per-pixel plane the caller has already built
    /// (see [`super::luma_plane`]).  Returns `None` for degenerate images.
    ///
    /// Parallelism is over *tiles*: each tile reads its own rows of the
    /// shared plane into a private 256-bucket histogram.  That histogram is
    /// built once per tile, never moved through a rayon `fold`, so this does
    /// not repeat the large-accumulator mistake documented in CLAUDE.md.
    pub fn compute(image: &Image, luma: &[u8]) -> Option<Self> {
        let w = image.width as usize;
        let h = image.height as usize;
        if w == 0 || h == 0 || luma.len() < w * h {
            return None;
        }
        let (cols, rows) = grid_dims(image.width, image.height)?;
        let row_stride = image.row_stride();

        let tiles: Vec<TileStats> = (0..(cols as usize * rows as usize))
            .into_par_iter()
            .map(|idx| {
                let col = (idx % cols as usize) as u32;
                let row = (idx / cols as usize) as u32;
                let rect = tile_rect(image.width, image.height, cols, rows, col, row);
                tile_stats(image, luma, row_stride, rect)
            })
            .collect();

        let mut stats = Self {
            cols,
            rows,
            tiles,
            border: None,
        };
        stats.border = stats.detect_border(luma, w, h);
        Some(stats)
    }

    /// The per-tile medians, row-major — the raw material for a local tone
    /// operator and for the descriptors below.
    pub fn tile_medians(&self) -> Vec<u8> {
        self.tiles.iter().map(|t| t.luma_median).collect()
    }

    /// How unevenly lit the frame is: the p90 − p10 spread of the tile
    /// medians, in 0–255 luma units.
    ///
    /// A low value means one global tone curve fits the whole picture; a high
    /// value means regions disagree about what "correct exposure" is and the
    /// image wants local treatment.  Percentiles rather than max − min so a
    /// single specular tile or one dark corner cannot dominate.
    pub fn tonal_spread(&self) -> f32 {
        let mut medians: Vec<u8> = self.tile_medians();
        if medians.is_empty() {
            return 0.0;
        }
        medians.sort_unstable();
        (percentile_of_sorted(&medians, 0.9) - percentile_of_sorted(&medians, 0.1)) as f32
    }

    /// Mean tile median of the top band minus that of the bottom band, in
    /// 0–255 luma units.  `None` when there are too few tile rows to compare.
    ///
    /// Strongly positive is the backlit signature — bright sky over a dark
    /// subject — which a global histogram cannot distinguish from an evenly
    /// lit scene that happens to contain both tones.  Strongly negative is
    /// the reverse (a bright foreground under a dark sky, or a vignetted
    /// top).  Near zero means the frame is vertically even.
    pub fn vertical_skew(&self) -> Option<f32> {
        if self.rows < 2 {
            return None;
        }
        let band = (self.rows / SKEW_BAND_FRACTION).max(1);
        let mean_of_rows = |range: std::ops::Range<u32>| -> f32 {
            let mut sum = 0.0;
            let mut n = 0.0;
            for row in range {
                for col in 0..self.cols {
                    sum += self.tile(col, row).map_or(0.0, |t| t.luma_median as f32);
                    n += 1.0;
                }
            }
            if n > 0.0 { sum / n } else { 0.0 }
        };
        Some(mean_of_rows(0..band) - mean_of_rows(self.rows - band..self.rows))
    }

    /// The uniform border found during [`Self::compute`], if any — a scan
    /// margin or instant-film frame, together with the picture area inside
    /// it.  See [`Self::detect_border`] for how it is established.
    pub fn uniform_border(&self) -> Option<BorderRegion> {
        self.border
    }

    /// Detect a uniform border and locate the picture inside it.
    ///
    /// Two stages, deliberately: **tiles decide whether a border exists**,
    /// **pixels decide where it ends**.
    ///
    /// A ring of perimeter tiles qualifies only if *every* tile in it is flat
    /// ([`BORDER_FLAT_VARIANCE`]) and all border tiles found so far agree in
    /// level to within [`BORDER_LEVEL_SPREAD`]; rings are peeled inward while
    /// they keep qualifying.  That gate is coarse but very hard to trip by
    /// accident, which is what we want — the cost of a false positive
    /// (analysing an arbitrary crop) is far worse than the cost of a miss
    /// (today's behaviour).
    ///
    /// The ring boundary alone is not good enough to *measure* through: it
    /// lands on a tile edge, so up to a tile's worth of margin survives
    /// inside it, and a few percent of frame pixels is all it takes to drag a
    /// 99.5th-percentile white point back to paper white.  So each edge is
    /// then walked inward one scan line at a time and stopped at the first
    /// line that is no longer almost entirely at the border level.
    fn detect_border(&self, luma: &[u8], width: usize, height: usize) -> Option<BorderRegion> {
        if self.cols < BORDER_MIN_GRID || self.rows < BORDER_MIN_GRID {
            return None;
        }

        let mut rings = 0u32;
        let mut lo = u8::MAX;
        let mut hi = u8::MIN;
        let mut level_sum = 0.0f32;
        let mut level_n = 0.0f32;

        // Peel rings while each one is flat and consistent with the border
        // found so far, always leaving at least a 2×2 interior.
        while self.cols >= 2 * rings + BORDER_MIN_GRID && self.rows >= 2 * rings + BORDER_MIN_GRID {
            let ring: Vec<&TileStats> = self.ring_tiles(rings);
            let flat = ring.iter().all(|t| t.luma_variance <= BORDER_FLAT_VARIANCE);
            let ring_lo = ring.iter().map(|t| t.luma_median).min().unwrap_or(0);
            let ring_hi = ring.iter().map(|t| t.luma_median).max().unwrap_or(255);
            let merged_spread = hi.max(ring_hi) as f32 - lo.min(ring_lo) as f32;
            if !flat || merged_spread > BORDER_LEVEL_SPREAD {
                break;
            }
            lo = lo.min(ring_lo);
            hi = hi.max(ring_hi);
            level_sum += ring.iter().map(|t| t.luma_mean).sum::<f32>();
            level_n += ring.len() as f32;
            rings += 1;
        }

        if rings == 0 || level_n <= 0.0 {
            return None;
        }
        let level = level_sum / level_n;

        let coarse = self.sub_grid_rect(rings)?;
        if !self.interior_is_content(rings) {
            return None;
        }

        // Refine each edge to the pixel, never straying more than one tile
        // past the ring boundary — beyond that we would be trusting a
        // scan-line heuristic further than the tile evidence supports.
        let slack = self.tile(rings, rings)?.rect;
        let refined = refine_border_rect(luma, width, height, coarse, slack, level);

        let frame = self.frame_rect();
        if refined.is_empty() || refined == frame {
            return None;
        }
        let frame_area = frame.area() as f32;
        if frame_area <= 0.0 || (refined.area() as f32 / frame_area) < BORDER_MIN_CONTENT_FRACTION {
            return None;
        }

        Some(BorderRegion {
            content_rect: refined,
            rings,
            level,
        })
    }

    /// Tiles forming the perimeter of the sub-grid inset by `ring` tiles.
    fn ring_tiles(&self, ring: u32) -> Vec<&TileStats> {
        let (c0, r0) = (ring, ring);
        let (c1, r1) = (self.cols - ring, self.rows - ring);
        let mut out = Vec::new();
        for row in r0..r1 {
            for col in c0..c1 {
                let edge = row == r0 || row == r1 - 1 || col == c0 || col == c1 - 1;
                if let (true, Some(t)) = (edge, self.tile(col, row)) {
                    out.push(t);
                }
            }
        }
        out
    }

    /// Pixel rectangle covered by the sub-grid inset by `inset` tiles.
    fn sub_grid_rect(&self, inset: u32) -> Option<Rect> {
        if self.cols <= 2 * inset || self.rows <= 2 * inset {
            return None;
        }
        let tl = self.tile(inset, inset)?.rect;
        let br = self
            .tile(self.cols - inset - 1, self.rows - inset - 1)?
            .rect;
        Some(Rect {
            x: tl.x,
            y: tl.y,
            width: br.x + br.width - tl.x,
            height: br.y + br.height - tl.y,
        })
    }

    /// The full frame as a pixel rectangle.
    fn frame_rect(&self) -> Rect {
        self.sub_grid_rect(0).unwrap_or(Rect {
            x: 0,
            y: 0,
            width: 0,
            height: 0,
        })
    }

    /// Does the sub-grid inset by `inset` tiles look like picture rather than
    /// more flat border?
    fn interior_is_content(&self, inset: u32) -> bool {
        let mut medians: Vec<u8> = Vec::new();
        for row in inset..self.rows - inset {
            for col in inset..self.cols - inset {
                if let Some(t) = self.tile(col, row) {
                    medians.push(t.luma_median);
                }
            }
        }
        if medians.is_empty() {
            return false;
        }
        medians.sort_unstable();
        let spread = percentile_of_sorted(&medians, 0.9) - percentile_of_sorted(&medians, 0.1);
        spread as f32 >= BORDER_CONTENT_SPREAD
    }
}

// ── Grid helpers ──────────────────────────────────────────────────────────────

/// Choose a tile grid for a `width` × `height` image.
///
/// Aims for [`TILES_PER_AXIS`]² tiles that stay near-square: for aspect ratio
/// `a`, `cols = 8·√a` and `rows = 8/√a` keeps `cols·rows ≈ 64` while
/// `(w/cols)/(h/rows) ≈ 1`.  Both counts are then clamped so no tile is
/// narrower than [`MIN_TILE_PX`], which is what makes small images degrade to
/// a coarser grid instead of producing zero-size tiles.
fn grid_dims(width: u32, height: u32) -> Option<(u32, u32)> {
    if width == 0 || height == 0 {
        return None;
    }
    let aspect = (width as f64 / height as f64).sqrt();
    let cols = (TILES_PER_AXIS * aspect).round().max(1.0) as u32;
    let rows = (TILES_PER_AXIS / aspect).round().max(1.0) as u32;
    let max_cols = (width / MIN_TILE_PX).max(1);
    let max_rows = (height / MIN_TILE_PX).max(1);
    Some((cols.clamp(1, max_cols), rows.clamp(1, max_rows)))
}

/// Pixel bounds of tile `(col, row)`.
///
/// Boundaries are `col · width / cols`, so the grid partitions the image
/// exactly: remainders are spread over the tiles instead of being dropped or
/// piled onto the last one, and no pixel belongs to two tiles.
fn tile_rect(width: u32, height: u32, cols: u32, rows: u32, col: u32, row: u32) -> Rect {
    let split = |i: u32, n: u32, len: u32| -> u32 { (i as u64 * len as u64 / n as u64) as u32 };
    let x0 = split(col, cols, width);
    let x1 = split(col + 1, cols, width);
    let y0 = split(row, rows, height);
    let y1 = split(row + 1, rows, height);
    Rect {
        x: x0,
        y: y0,
        width: x1 - x0,
        height: y1 - y0,
    }
}

/// Walk each edge of `coarse` inward to the first scan line that is no longer
/// almost entirely at the border `level`, and return the tightened rectangle.
///
/// Scanning starts at the image edge (every line out there is border by the
/// tile evidence, so it costs a quick confirmation) and stops at the latest
/// one tile past the ring boundary — `slack` carries that tile's size.  If a
/// line unexpectedly fails early the walk stops there, which keeps *more*
/// margin: refinement can only ever be more conservative than the caller's
/// coarse rectangle would have been on that edge.
///
/// Cost is bounded by the scanned band, and this runs only on images where a
/// border was already detected, so it never touches the common path.
fn refine_border_rect(
    luma: &[u8],
    width: usize,
    height: usize,
    coarse: Rect,
    slack: Rect,
    level: f32,
) -> Rect {
    let matches = |v: u8| (v as f32 - level).abs() <= BORDER_LEVEL_TOLERANCE;
    let is_border =
        |hits: usize, total: usize| total > 0 && hits as f32 >= total as f32 * BORDER_LINE_PURITY;

    let column_is_border = |x: usize, y0: usize, y1: usize| {
        let hits = (y0..y1).filter(|&y| matches(luma[y * width + x])).count();
        is_border(hits, y1 - y0)
    };
    let row_is_border = |y: usize, x0: usize, x1: usize| {
        let hits = luma[y * width + x0..y * width + x1]
            .iter()
            .filter(|&&v| matches(v))
            .count();
        is_border(hits, x1 - x0)
    };

    // Vertical edges first, over the full image height: the top and bottom
    // margins are themselves border, so they only reinforce the verdict.
    let max_left = (coarse.x + slack.width).min(width as u32 / 2) as usize;
    let mut left = 0usize;
    while left < max_left && column_is_border(left, 0, height) {
        left += 1;
    }

    let min_right = (coarse.x + coarse.width)
        .saturating_sub(slack.width)
        .max(width as u32 / 2) as usize;
    let mut right = width;
    while right > min_right && column_is_border(right - 1, 0, height) {
        right -= 1;
    }

    // Horizontal edges are then measured across the already-narrowed span so
    // the side margins cannot pad their purity.
    let (x0, x1) = if left < right {
        (left, right)
    } else {
        (0, width)
    };
    let max_top = (coarse.y + slack.height).min(height as u32 / 2) as usize;
    let mut top = 0usize;
    while top < max_top && row_is_border(top, x0, x1) {
        top += 1;
    }

    let min_bottom = (coarse.y + coarse.height)
        .saturating_sub(slack.height)
        .max(height as u32 / 2) as usize;
    let mut bottom = height;
    while bottom > min_bottom && row_is_border(bottom - 1, x0, x1) {
        bottom -= 1;
    }

    if left >= right || top >= bottom {
        return coarse;
    }
    Rect {
        x: left as u32,
        y: top as u32,
        width: (right - left) as u32,
        height: (bottom - top) as u32,
    }
}

/// Measure one tile: exact luma statistics from the shared plane plus strided
/// RGB means from the pixel buffer.
fn tile_stats(image: &Image, luma: &[u8], row_stride: usize, rect: Rect) -> TileStats {
    let w = image.width as usize;
    let (x0, x1) = (rect.x as usize, (rect.x + rect.width) as usize);
    let (y0, y1) = (rect.y as usize, (rect.y + rect.height) as usize);

    // Private 1 KiB histogram, built once for this tile.  Exact percentiles,
    // mean and variance all fall out of it without a second read.
    let mut hist = [0u32; 256];
    for y in y0..y1 {
        for &l in &luma[y * w + x0..y * w + x1] {
            hist[l as usize] += 1;
        }
    }

    let mut count = 0u64;
    let mut sum = 0f64;
    let mut sumsq = 0f64;
    for (v, &c) in hist.iter().enumerate() {
        let c = c as u64;
        count += c;
        sum += v as f64 * c as f64;
        sumsq += (v * v) as f64 * c as f64;
    }
    let (mean, var) = if count > 0 {
        let n = count as f64;
        let mean = sum / n;
        (mean, (sumsq / n - mean * mean).max(0.0))
    } else {
        (0.0, 0.0)
    };

    // Strided RGB sample for chroma and per-channel means.
    let mut chroma_sum = 0u64;
    let mut rgb_sum = [0u64; 3];
    let mut samples = 0u64;
    for y in (y0..y1).step_by(CHROMA_ROW_STRIDE) {
        let start = y * row_stride + x0 * 4;
        let end = y * row_stride + x1 * 4;
        for p in image.data[start..end]
            .chunks_exact(4)
            .step_by(CHROMA_PIXEL_STRIDE)
        {
            let (r, g, b) = (p[0], p[1], p[2]);
            chroma_sum += (r.max(g).max(b) - r.min(g).min(b)) as u64;
            rgb_sum[0] += r as u64;
            rgb_sum[1] += g as u64;
            rgb_sum[2] += b as u64;
            samples += 1;
        }
    }
    let inv = if samples > 0 {
        1.0 / samples as f32
    } else {
        0.0
    };

    TileStats {
        rect,
        luma_p10: hist_percentile(&hist, count, 0.1),
        luma_median: hist_percentile(&hist, count, 0.5),
        luma_p90: hist_percentile(&hist, count, 0.9),
        luma_mean: mean as f32,
        luma_variance: var as f32,
        mean_chroma: chroma_sum as f32 * inv,
        mean_rgb: [
            rgb_sum[0] as f32 * inv,
            rgb_sum[1] as f32 * inv,
            rgb_sum[2] as f32 * inv,
        ],
        pixels: count as u32,
    }
}

/// Percentile of a `u32` histogram, matching [`super::percentile`]'s
/// definition (smallest bucket whose cumulative count reaches `pct`).
fn hist_percentile(hist: &[u32; 256], total: u64, pct: f64) -> u8 {
    if total == 0 {
        return 0;
    }
    let target = ((total as f64 * pct).ceil() as u64).clamp(1, total);
    let mut cumsum = 0u64;
    for (i, &count) in hist.iter().enumerate() {
        cumsum += count as u64;
        if cumsum >= target {
            return i as u8;
        }
    }
    255
}

/// Percentile of an already-sorted slice by nearest-rank.
fn percentile_of_sorted(sorted: &[u8], pct: f64) -> u8 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * pct).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

#[cfg(test)]
mod tests {
    use super::super::luma_plane;
    use super::*;

    /// Build an image from a per-pixel closure of `(x, y)`.
    fn image_from(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 3]) -> Image {
        let mut img = Image::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let [r, g, b] = f(x, y);
                let o = img.pixel_offset(x, y);
                img.data[o] = r;
                img.data[o + 1] = g;
                img.data[o + 2] = b;
                img.data[o + 3] = 255;
            }
        }
        img
    }

    fn regional(img: &Image) -> Option<RegionalStats> {
        let luma = luma_plane(img)?;
        RegionalStats::compute(img, &luma)
    }

    /// Deterministic pseudo-random texture so tiles are never flat.
    fn noise(x: u32, y: u32) -> u8 {
        let mut s = x.wrapping_mul(1973) ^ y.wrapping_mul(9277) ^ 0x5bf0_3635;
        s ^= s >> 13;
        s = s.wrapping_mul(0x9e37_79b9);
        (s >> 24) as u8
    }

    #[test]
    fn grid_tiles_partition_every_pixel_exactly_once() {
        for (w, h) in [
            (1, 1),
            (3, 3),
            (17, 23),
            (101, 47),
            (47, 101),
            (8, 4000),
            (640, 480),
        ] {
            let img = image_from(w, h, |x, y| {
                let v = noise(x, y);
                [v, v, v]
            });
            let stats = regional(&img).expect("non-empty image must have a grid");

            let mut covered = vec![0u8; (w * h) as usize];
            for t in stats.tiles() {
                assert!(!t.rect.is_empty(), "{w}x{h}: zero-size tile {:?}", t.rect);
                assert_eq!(
                    t.pixels,
                    t.rect.width * t.rect.height,
                    "{w}x{h}: tile pixel count disagrees with its rect"
                );
                for y in t.rect.y..t.rect.y + t.rect.height {
                    for x in t.rect.x..t.rect.x + t.rect.width {
                        covered[(y * w + x) as usize] += 1;
                    }
                }
            }
            assert!(
                covered.iter().all(|&c| c == 1),
                "{w}x{h}: every pixel must be covered exactly once"
            );
            let total: u32 = stats.tiles().iter().map(|t| t.pixels).sum();
            assert_eq!(total, w * h, "{w}x{h}: tile pixel counts must sum to area");
        }
    }

    #[test]
    fn tiles_stay_near_square_and_bounded() {
        // The grid must keep tiles roughly square (so a tile means the same
        // thing horizontally and vertically) and stay near the 8×8 target
        // even for extreme aspect ratios.
        for (w, h) in [(4000, 3000), (4000, 4000), (8000, 1000), (1000, 8000)] {
            let (cols, rows) = grid_dims(w, h).unwrap();
            let tw = w as f64 / cols as f64;
            let th = h as f64 / rows as f64;
            let aspect = (tw / th).max(th / tw);
            assert!(
                aspect < 1.35,
                "{w}x{h}: tile aspect {aspect:.2} too far from square"
            );
            assert!(
                (16..=200).contains(&(cols * rows)),
                "{w}x{h}: {cols}x{rows} tiles is far from the 8x8 target"
            );
        }
    }

    #[test]
    fn degenerate_sizes_yield_none_not_panic() {
        assert!(grid_dims(0, 0).is_none());
        assert!(grid_dims(10, 0).is_none());
        assert!(grid_dims(0, 10).is_none());
        assert!(regional(&Image::new(0, 0)).is_none());

        // 1x1 must produce a single-tile grid without panicking, and its
        // descriptors must degrade gracefully.
        let one = regional(&image_from(1, 1, |_, _| [7, 7, 7])).expect("1x1 has a grid");
        assert_eq!((one.cols(), one.rows()), (1, 1));
        assert_eq!(one.tiles()[0].luma_median, 7);
        assert!(one.vertical_skew().is_none());
        assert!(one.uniform_border().is_none());
        assert_eq!(one.tonal_spread(), 0.0);
    }

    #[test]
    fn backlit_image_has_high_vertical_skew() {
        // Bright top half over dark bottom half — the backlit signature.
        let backlit = image_from(320, 320, |x, y| {
            let base: i32 = if y < 160 { 220 } else { 40 };
            let v = (base + (noise(x, y) as i32 % 12)).clamp(0, 255) as u8;
            [v, v, v]
        });
        let skew = regional(&backlit).unwrap().vertical_skew().unwrap();
        assert!(skew > 120.0, "backlit skew should be large, got {skew}");

        // A gentle even gradient covering the same tonal range has a much
        // smaller top-to-bottom difference per tile band.
        let flat_lit = image_from(320, 320, |x, _| {
            let v = (40 + x * 180 / 320) as u8;
            [v, v.saturating_add(noise(x, 0) % 3), v]
        });
        let even = regional(&flat_lit).unwrap().vertical_skew().unwrap();
        assert!(
            even.abs() < 5.0,
            "horizontally-graded image should have near-zero vertical skew, got {even}"
        );
    }

    #[test]
    fn tonal_spread_separates_even_from_uneven_lighting() {
        let even = image_from(320, 320, |x, y| {
            let v = 100u8.saturating_add(noise(x, y) % 20);
            [v, v, v]
        });
        let uneven = image_from(320, 320, |x, y| {
            let base = if x < 160 && y < 160 { 20 } else { 210 };
            let v = (base as u32 + (noise(x, y) % 12) as u32).min(255) as u8;
            [v, v, v]
        });
        let a = regional(&even).unwrap().tonal_spread();
        let b = regional(&uneven).unwrap().tonal_spread();
        assert!(a < 15.0, "evenly lit spread should be small, got {a}");
        assert!(b > 100.0, "unevenly lit spread should be large, got {b}");
    }

    /// Picture content whose *tile medians* vary across the frame — a
    /// diagonal gradient with texture, as a real photograph has.
    fn content_pixel(x: u32, y: u32, w: u32, h: u32) -> u8 {
        let base = 20 + (x + y) * 200 / (w + h);
        (base as i32 + (noise(x, y) % 24) as i32 - 12).clamp(0, 255) as u8
    }

    /// A uniform frame around varied content, as a scanned print produces.
    fn bordered_image(w: u32, h: u32, margin: u32, frame: u8) -> Image {
        image_from(w, h, |x, y| {
            if x < margin || y < margin || x >= w - margin || y >= h - margin {
                // Paper grain: flat to within a level or two.
                let v = frame.saturating_sub(noise(x, y) % 3);
                [v, v, v]
            } else {
                let v = content_pixel(x, y, w, h);
                [v, v, v]
            }
        })
    }

    #[test]
    fn uniform_border_detected_only_when_present() {
        let framed = bordered_image(400, 400, 60, 245);
        let b = regional(&framed)
            .unwrap()
            .uniform_border()
            .expect("uniform white frame must be detected");
        assert!(b.rings >= 1);
        assert!(b.level > 200.0, "white frame level: {}", b.level);
        // Edge refinement must land on the actual frame edge, not the tile
        // boundary that first flagged it.
        assert_eq!(
            (
                b.content_rect.x,
                b.content_rect.y,
                b.content_rect.width,
                b.content_rect.height
            ),
            (60, 60, 280, 280),
            "content rect should be the exact picture area"
        );
        assert!(b.content_rect.area() as f32 / (400.0 * 400.0) >= BORDER_MIN_CONTENT_FRACTION);

        // Same picture without a frame: no detection.
        let plain = image_from(400, 400, |x, y| {
            let v = content_pixel(x, y, 400, 400);
            [v, v, v]
        });
        assert!(
            regional(&plain).unwrap().uniform_border().is_none(),
            "unframed image must not report a border"
        );

        // A completely flat image is "all border" by the flatness test alone;
        // the interior-content check must reject it.
        let blank = image_from(400, 400, |x, y| {
            let v = 128u8.saturating_add(noise(x, y) % 3);
            [v, v, v]
        });
        assert!(
            regional(&blank).unwrap().uniform_border().is_none(),
            "a flat image has no picture inside, so no border either"
        );

        // Flat perimeter tiles that disagree in level — dark side bars and a
        // blown top/bottom band — are not one frame, and the level-spread
        // rule must reject them.
        let two_tone = image_from(400, 400, |x, y| {
            let v = if !(60..340).contains(&x) {
                10 + noise(x, y) % 3
            } else if !(60..340).contains(&y) {
                250u8.saturating_sub(noise(x, y) % 3)
            } else {
                content_pixel(x, y, 400, 400)
            };
            [v, v, v]
        });
        assert!(
            regional(&two_tone).unwrap().uniform_border().is_none(),
            "flat but mismatched edges are not a uniform border"
        );
    }

    #[test]
    fn tile_chroma_matches_pixel_content() {
        // Left half neutral grey, right half saturated red: tile chroma must
        // separate them.
        let img = image_from(320, 320, |x, _| {
            if x < 160 {
                [128, 128, 128]
            } else {
                [200, 40, 40]
            }
        });
        let stats = regional(&img).unwrap();
        let left = stats.tile(0, 0).unwrap();
        let right = stats.tile(stats.cols() - 1, 0).unwrap();
        assert!(
            left.mean_chroma < 1.0,
            "grey tile chroma {}",
            left.mean_chroma
        );
        assert!(
            (right.mean_chroma - 160.0).abs() < 1.0,
            "red tile chroma {}",
            right.mean_chroma
        );
        assert!(right.mean_rgb[0] > right.mean_rgb[2]);
    }

    #[test]
    fn tile_luma_statistics_are_exact() {
        // A tile of constant value has that value as median/mean and zero
        // variance; percentiles bracket a known ramp.
        let img = image_from(64, 64, |_, y| {
            let v = (y * 4) as u8;
            [v, v, v]
        });
        let stats = regional(&img).unwrap();
        for t in stats.tiles() {
            assert!(t.luma_p10 <= t.luma_median && t.luma_median <= t.luma_p90);
            assert!(t.luma_variance >= 0.0);
            assert!(
                (t.luma_mean - t.luma_median as f32).abs() < 8.0,
                "ramp tile mean {} vs median {}",
                t.luma_mean,
                t.luma_median
            );
        }
    }
}
