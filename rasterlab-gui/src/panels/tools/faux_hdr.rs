use rasterlab_core::ops::FauxHdrOp;

use super::shared::{ParamTool, param_tool_actions};
use super::tool_trait::{Tool, ToolAction, ToolUiCtx};
use crate::state::EditingTool;

pub struct FauxHdrTool {
    pub strength: f32,
    pub preview_active: bool,
}

impl FauxHdrTool {
    pub fn new() -> Self {
        Self {
            strength: 0.8,
            preview_active: false,
        }
    }
}

impl ParamTool for FauxHdrTool {
    type Op = FauxHdrOp;

    fn op(&self) -> FauxHdrOp {
        FauxHdrOp::new(self.strength)
    }

    fn reset(&mut self) {
        *self = Self::new();
    }

    fn load(&mut self, op: &FauxHdrOp) {
        self.strength = op.strength;
    }

    fn preview_active(&mut self) -> &mut bool {
        &mut self.preview_active
    }
}

impl Tool for FauxHdrTool {
    fn id(&self) -> &'static str {
        "faux_hdr"
    }
    fn display_name(&self) -> &'static str {
        "◈  Faux HDR"
    }
    fn editing_tool(&self) -> Option<EditingTool> {
        Some(EditingTool::FauxHdr)
    }

    fn render_ui(&mut self, ui: &mut egui::Ui, ctx: &ToolUiCtx<'_>) -> ToolAction {
        ui.label(
            egui::RichText::new("Exposure fusion from ±1 stop virtual brackets")
                .small()
                .color(egui::Color32::from_gray(140)),
        );
        ui.add_space(2.0);
        let changed = ui
            .add(
                egui::Slider::new(&mut self.strength, 0.0..=1.0)
                    .text("Strength")
                    .step_by(0.01),
            )
            .changed();
        param_tool_actions(ui, ctx, self, changed)
    }
    super::shared::impl_param_tool!();
}
