//! Parses CLI flag strings into typed `Box<dyn Operation>` values.

use anyhow::{anyhow, bail, Result};
use rasterlab_core::{
    ops::{BlackAndWhiteOp, CropOp, RotateOp, SharpenOp},
    traits::operation::Operation,
};

/// Build an ordered operation list from the structured CLI arguments provided
/// by `ProcessArgs` / `BatchArgs`.
///
/// The order of operations matches the order flags appear on the command line
/// (clap preserves argument order for repeated flags in the future; for now
/// they are applied in a fixed logical sequence).
pub struct PipelineSpec {
    pub crop:    Option<String>,  // "x,y,w,h"
    pub rotate:  Option<String>,  // "90" | "180" | "270" | "<float>"
    pub bw:      Option<String>,  // "luminance" | "average" | "perceptual" | "r,g,b"
    pub sharpen: Option<f32>,
}

impl PipelineSpec {
    pub fn build(self) -> Result<Vec<Box<dyn Operation>>> {
        let mut ops: Vec<Box<dyn Operation>> = Vec::new();

        if let Some(crop_str) = self.crop {
            ops.push(parse_crop(&crop_str)?);
        }
        if let Some(rot_str) = self.rotate {
            ops.push(parse_rotate(&rot_str)?);
        }
        if let Some(bw_str) = self.bw {
            ops.push(parse_bw(&bw_str)?);
        }
        if let Some(strength) = self.sharpen {
            ops.push(Box::new(SharpenOp::new(strength)));
        }

        Ok(ops)
    }
}

fn parse_crop(s: &str) -> Result<Box<dyn Operation>> {
    let parts: Vec<u32> = s
        .split(',')
        .map(|p| p.trim().parse::<u32>().map_err(|e| anyhow!("Invalid crop value: {e}")))
        .collect::<Result<_>>()?;
    if parts.len() != 4 {
        bail!("--crop requires 4 comma-separated integers: x,y,w,h  (got '{}')", s);
    }
    Ok(Box::new(CropOp::new(parts[0], parts[1], parts[2], parts[3])))
}

fn parse_rotate(s: &str) -> Result<Box<dyn Operation>> {
    let op = match s.trim() {
        "90"  => RotateOp::cw90(),
        "180" => RotateOp::cw180(),
        "270" => RotateOp::cw270(),
        other => {
            let deg: f32 = other.parse().map_err(|_| {
                anyhow!("--rotate expects 90|180|270 or a float degrees value, got '{}'", other)
            })?;
            RotateOp::arbitrary(deg)
        }
    };
    Ok(Box::new(op))
}

fn parse_bw(s: &str) -> Result<Box<dyn Operation>> {
    let op = match s.trim().to_lowercase().as_str() {
        "luminance"  => BlackAndWhiteOp::luminance(),
        "average"    => BlackAndWhiteOp::average(),
        "perceptual" => BlackAndWhiteOp::perceptual(),
        other => {
            // Try parsing as "r,g,b" channel mixer weights
            let parts: Vec<f32> = other
                .split(',')
                .map(|p| p.trim().parse::<f32>().map_err(|e| anyhow!("{e}")))
                .collect::<Result<_>>()
                .map_err(|_| {
                    anyhow!(
                        "--bw expects luminance|average|perceptual|r,g,b  (got '{}')",
                        other
                    )
                })?;
            if parts.len() != 3 {
                bail!("--bw channel mixer requires 3 floats: r,g,b");
            }
            BlackAndWhiteOp::channel_mixer(parts[0], parts[1], parts[2])
        }
    };
    Ok(Box::new(op))
}
