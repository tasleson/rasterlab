use rasterlab_core::{
    ops::{LinearMask, MaskShape, RadialMask, ResampleMode},
    traits::format_handler::EncodeOptions,
    traits::operation::Operation,
};

use crate::file_chooser::DialogKind;
use crate::panels::tools::{
    blur::BlurTool, brightness_contrast::BrightnessContrastTool, bw::BwTool,
    clarity_texture::ClarityTextureTool, color_balance::ColorBalanceTool,
    color_space::ColorSpaceTool, crop::CropTool, curves::CurvesTool, denoise::DenoiseTool,
    faux_hdr::FauxHdrTool, focus_stack::FocusStackTool, grain::GrainTool, hdr_merge::HdrMergeTool,
    heal::HealTool, highlights_shadows::HighlightsShadowsTool, hsl::HslTool,
    hue_shift::HueShiftTool, levels::LevelsTool, lut::LutTool, noise_reduction::NoiseReductionTool,
    panorama::PanoramaTool, perspective::PerspectiveTool, resize::ResizeTool, rotate::RotateTool,
    saturation::SaturationTool, sepia::SepiaTool, shadow_exposure::ShadowExposureTool,
    sharpen::SharpenTool, split_tone::SplitToneTool, straighten::StraightenTool, tool_trait::Tool,
    vibrance::VibranceTool, vignette::VignetteTool, white_balance::WhiteBalanceTool,
};

/// All tool state: trait-based tools in a Vec, plus masking, export, and dialog fields.
pub struct ToolState {
    pub tools: Vec<Box<dyn Tool>>,

    // ── Masking ───────────────────────────────────────────────────────────
    /// 0 = None, 1 = Linear Gradient, 2 = Radial Gradient.
    pub mask_sel: usize,
    pub mask_lin_cx: f32,
    pub mask_lin_cy: f32,
    pub mask_lin_angle: f32,
    pub mask_lin_feather: f32,
    pub mask_lin_invert: bool,
    pub mask_rad_cx: f32,
    pub mask_rad_cy: f32,
    pub mask_rad_radius: f32,
    pub mask_rad_feather: f32,
    pub mask_rad_invert: bool,

    // ── Export settings ───────────────────────────────────────────────────
    pub encode_opts: EncodeOptions,
    pub export_resize_enabled: bool,
    pub export_resize_w: u32,
    pub export_resize_h: u32,
    pub export_resize_mode: ResampleMode,

    // ── Library batch export dialog ───────────────────────────────────────
    pub export_dialog: crate::panels::export_dialog::ExportDialogState,

    // ── Dialog request flags ─────────────────────────────────────────────
    pub pending_dialog: Option<DialogKind>,
}

impl ToolState {
    pub fn new() -> Self {
        Self {
            tools: Self::build_tools(),
            mask_sel: 0,
            mask_lin_cx: 0.5,
            mask_lin_cy: 0.5,
            mask_lin_angle: 90.0,
            mask_lin_feather: 0.5,
            mask_lin_invert: false,
            mask_rad_cx: 0.5,
            mask_rad_cy: 0.5,
            mask_rad_radius: 0.3,
            mask_rad_feather: 0.5,
            mask_rad_invert: false,
            encode_opts: EncodeOptions::default(),
            export_resize_enabled: false,
            export_resize_w: 0,
            export_resize_h: 0,
            export_resize_mode: ResampleMode::Bicubic,
            export_dialog: crate::panels::export_dialog::ExportDialogState::default(),
            pending_dialog: None,
        }
    }

    fn build_tools() -> Vec<Box<dyn Tool>> {
        vec![
            Box::new(BwTool::new()),
            Box::new(BlurTool::new()),
            Box::new(BrightnessContrastTool::new()),
            Box::new(ClarityTextureTool::new()),
            Box::new(ColorBalanceTool::new()),
            Box::new(ColorSpaceTool::new()),
            Box::new(CropTool::new()),
            Box::new(CurvesTool::new()),
            Box::new(DenoiseTool::new()),
            Box::new(FauxHdrTool::new()),
            Box::new(FocusStackTool::new()),
            Box::new(GrainTool::new()),
            Box::new(HdrMergeTool::new()),
            Box::new(HealTool::new()),
            Box::new(HighlightsShadowsTool::new()),
            Box::new(HslTool::new()),
            Box::new(HueShiftTool::new()),
            Box::new(LevelsTool::new()),
            Box::new(LutTool::new()),
            Box::new(NoiseReductionTool::new()),
            Box::new(PanoramaTool::new()),
            Box::new(PerspectiveTool::new()),
            Box::new(ResizeTool::new()),
            Box::new(RotateTool::new()),
            Box::new(SaturationTool::new()),
            Box::new(SepiaTool::new()),
            Box::new(ShadowExposureTool::new()),
            Box::new(SharpenTool::new()),
            Box::new(SplitToneTool::new()),
            Box::new(StraightenTool::new()),
            Box::new(VibranceTool::new()),
            Box::new(VignetteTool::new()),
            Box::new(WhiteBalanceTool::new()),
        ]
    }

    pub fn find<T: 'static>(&self) -> Option<&T> {
        self.tools
            .iter()
            .find_map(|t| t.as_any().downcast_ref::<T>())
    }

    pub fn find_mut<T: 'static>(&mut self) -> Option<&mut T> {
        self.tools
            .iter_mut()
            .find_map(|t| t.as_any_mut().downcast_mut::<T>())
    }

    pub fn any_preview_active(&self) -> bool {
        self.tools.iter().any(|t| t.is_preview_active())
    }

    pub fn preview_op(&self) -> Option<Box<dyn Operation>> {
        self.tools.iter().find_map(|t| t.preview_op())
    }

    pub fn cancel_all_previews(&mut self) {
        for tool in &mut self.tools {
            tool.cancel_preview();
        }
    }

    pub fn current_mask_shape(&self) -> Option<MaskShape> {
        match self.mask_sel {
            1 => Some(MaskShape::Linear(LinearMask {
                cx: self.mask_lin_cx,
                cy: self.mask_lin_cy,
                angle_deg: self.mask_lin_angle,
                feather: self.mask_lin_feather,
                invert: self.mask_lin_invert,
            })),
            2 => Some(MaskShape::Radial(RadialMask {
                cx: self.mask_rad_cx,
                cy: self.mask_rad_cy,
                radius: self.mask_rad_radius,
                feather: self.mask_rad_feather,
                invert: self.mask_rad_invert,
            })),
            _ => None,
        }
    }

    pub fn crop_aspect_ratio(&self) -> Option<(f32, f32)> {
        let crop = self.find::<CropTool>()?;
        let flip = crop.portrait;
        match crop.aspect_idx {
            1 => {
                let (w, h) = (3.0, 2.0);
                Some(if flip { (h, w) } else { (w, h) })
            }
            2 => {
                let (w, h) = (4.0, 3.0);
                Some(if flip { (h, w) } else { (w, h) })
            }
            3 => Some((1.0, 1.0)),
            4 => {
                let (w, h) = (16.0, 9.0);
                Some(if flip { (h, w) } else { (w, h) })
            }
            5 => Some((9.0, 16.0)),
            6 => Some((crop.custom_ratio, 1.0)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Strip the leading icon/emoji prefix from a display name so the
    /// alphabetical-ordering check compares only the human-readable label.
    ///
    /// Display names follow the convention `"<icon>  Name"` — one icon
    /// codepoint followed by two ASCII spaces. We trim leading non-ASCII
    /// characters and any whitespace that follows.
    fn label(display_name: &str) -> &str {
        let s = display_name.trim_start_matches(|c: char| !c.is_ascii());
        s.trim_start()
    }

    /// CLAUDE.md mandates:
    ///   "All other tools are placed in strict alphabetical order by display
    ///    name after [Auto Enhance and Looks]."
    ///
    /// Verify that the trait-tool registration order in `build_tools` matches
    /// the case-insensitive alphabetical sort of the display name labels.
    /// Adding a new tool out of position fails this test immediately.
    #[test]
    fn tools_are_in_alphabetical_order_by_display_name() {
        let tools = ToolState::build_tools();
        let labels: Vec<String> = tools
            .iter()
            .map(|t| label(t.display_name()).to_string())
            .collect();

        let mut sorted = labels.clone();
        sorted.sort_by_key(|s| s.to_lowercase());

        assert_eq!(
            labels, sorted,
            "Tool order is not strictly alphabetical by display name.\n\
             Current:  {:?}\n\
             Expected: {:?}",
            labels, sorted,
        );
    }

    /// Every tool must have a unique `id()` so prefs/state lookups are
    /// unambiguous; a duplicate id would silently mask one tool's open/closed
    /// state.
    #[test]
    fn tool_ids_are_unique() {
        let tools = ToolState::build_tools();
        let mut ids: Vec<&'static str> = tools.iter().map(|t| t.id()).collect();
        ids.sort();
        let mut deduped = ids.clone();
        deduped.dedup();
        assert_eq!(
            ids, deduped,
            "duplicate tool id found in build_tools(): {:?}",
            ids,
        );
    }

    /// Display labels must not be empty after trimming the icon prefix —
    /// otherwise the tools panel renders an icon with no name.
    #[test]
    fn tool_display_labels_are_non_empty() {
        let tools = ToolState::build_tools();
        for t in &tools {
            let l = label(t.display_name());
            assert!(
                !l.is_empty(),
                "tool {:?} has an empty display label after icon stripping",
                t.id(),
            );
        }
    }
}
