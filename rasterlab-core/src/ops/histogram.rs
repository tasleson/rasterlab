use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{error::RasterResult, image::Image, traits::operation::Operation};

/// Per-channel histogram data (256 buckets each).
#[derive(Debug, Clone)]
pub struct HistogramData {
    /// Count of pixels with each red value 0–255.
    pub red:   [u64; 256],
    /// Count of pixels with each green value 0–255.
    pub green: [u64; 256],
    /// Count of pixels with each blue value 0–255.
    pub blue:  [u64; 256],
    /// BT.709 luma (`0.2126R + 0.7152G + 0.0722B`) histogram.
    pub luma:  [u64; 256],
}

impl HistogramData {
    /// Peak count across all channels (useful for normalising bar heights).
    pub fn peak(&self) -> u64 {
        let ch_max = |arr: &[u64; 256]| arr.iter().copied().max().unwrap_or(0);
        ch_max(&self.red)
            .max(ch_max(&self.green))
            .max(ch_max(&self.blue))
            .max(ch_max(&self.luma))
    }

    /// Compute histogram from an image using rayon for parallel accumulation.
    pub fn compute(image: &Image) -> Self {
        type Acc = ([u64; 256], [u64; 256], [u64; 256], [u64; 256]);

        let zero: fn() -> Acc =
            || ([0u64; 256], [0u64; 256], [0u64; 256], [0u64; 256]);

        let (red, green, blue, luma) = image
            .data
            .par_chunks(4)
            .fold(zero, |mut acc, pixel| {
                let (r, g, b) = (pixel[0] as usize, pixel[1] as usize, pixel[2] as usize);
                acc.0[r] += 1;
                acc.1[g] += 1;
                acc.2[b] += 1;
                let l = (0.2126 * pixel[0] as f64
                    + 0.7152 * pixel[1] as f64
                    + 0.0722 * pixel[2] as f64)
                    .round() as usize;
                acc.3[l.min(255)] += 1;
                acc
            })
            .reduce(zero, |mut a, b| {
                for i in 0..256 {
                    a.0[i] += b.0[i];
                    a.1[i] += b.1[i];
                    a.2[i] += b.2[i];
                    a.3[i] += b.3[i];
                }
                a
            });

        HistogramData { red, green, blue, luma }
    }
}

/// A pass-through operation that also exposes [`HistogramData`] computation.
///
/// When inserted in the pipeline this op does not alter pixels.  The GUI and
/// CLI use [`HistogramData::compute`] directly on any rendered image; this op
/// exists so the edit stack can show "histogram checkpoint" entries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistogramOp;

#[typetag::serde]
impl Operation for HistogramOp {
    fn name(&self) -> &'static str { "histogram" }

    fn apply(&self, image: &Image) -> RasterResult<Image> {
        Ok(image.deep_clone()) // pass-through
    }

    fn describe(&self) -> String {
        "Histogram checkpoint".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_red_histogram() {
        let mut img = Image::new(4, 4);
        img.data.chunks_mut(4).for_each(|p| { p[0]=200; p[1]=0; p[2]=0; p[3]=255; });
        let h = HistogramData::compute(&img);
        assert_eq!(h.red[200],   16);
        assert_eq!(h.green[0],   16);
        assert_eq!(h.blue[0],    16);
    }

    #[test]
    fn histogram_op_is_passthrough() {
        let src = Image::new(8, 8);
        let out = HistogramOp.apply(&src).unwrap();
        assert_eq!(out.data, src.data);
    }
}
