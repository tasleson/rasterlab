use rasterlab_core::ops::IntensifyHdrOp;

use super::shared::{ParamTool, param_tool_actions};
use super::tool_trait::{Tool, ToolAction, ToolUiCtx};
use crate::state::EditingTool;

pub struct IntensifyHdrTool {
    pub effect_percent: f32,
    pub preview_active: bool,
}

impl IntensifyHdrTool {
    pub fn new() -> Self {
        Self {
            effect_percent: 50.0,
            preview_active: false,
        }
    }
}

impl ParamTool for IntensifyHdrTool {
    type Op = IntensifyHdrOp;

    fn op(&self) -> IntensifyHdrOp {
        IntensifyHdrOp::new(self.effect_percent / 100.0)
    }

    fn reset(&mut self) {
        *self = Self::new();
    }

    fn load(&mut self, op: &IntensifyHdrOp) {
        self.effect_percent = op.amount * 100.0;
    }

    fn preview_active(&mut self) -> &mut bool {
        &mut self.preview_active
    }
}

impl Tool for IntensifyHdrTool {
    fn id(&self) -> &'static str {
        "intensify_hdr"
    }

    fn display_name(&self) -> &'static str {
        "◈  Intensify HDR"
    }

    fn editing_tool(&self) -> Option<EditingTool> {
        Some(EditingTool::IntensifyHdr)
    }

    fn render_ui(&mut self, ui: &mut egui::Ui, ctx: &ToolUiCtx<'_>) -> ToolAction {
        ui.label(
            egui::RichText::new(
                "Compressed tone, boosted local contrast and saturation, matched to Intensify HDR",
            )
            .small()
            .color(egui::Color32::from_gray(140)),
        );
        ui.add_space(2.0);
        let changed = ui
            .add(
                egui::Slider::new(&mut self.effect_percent, 0.0..=100.0)
                    .suffix("%")
                    .step_by(1.0)
                    .text("Effect"),
            )
            .changed();
        param_tool_actions(ui, ctx, self, changed)
    }

    super::shared::impl_param_tool!();
}
