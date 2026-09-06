use rasterlab_core::ops::HueShiftOp;

use super::shared::{ParamTool, param_tool_actions};
use super::tool_trait::{Tool, ToolAction, ToolUiCtx};
use crate::state::EditingTool;

pub struct HueShiftTool {
    pub degrees: f32,
    pub preview_active: bool,
}

impl HueShiftTool {
    pub fn new() -> Self {
        Self {
            degrees: 0.0,
            preview_active: false,
        }
    }
}

impl ParamTool for HueShiftTool {
    type Op = HueShiftOp;

    fn op(&self) -> HueShiftOp {
        HueShiftOp::new(self.degrees)
    }

    fn reset(&mut self) {
        *self = Self::new();
    }

    fn load(&mut self, op: &HueShiftOp) {
        self.degrees = op.degrees;
    }

    fn preview_active(&mut self) -> &mut bool {
        &mut self.preview_active
    }
}

impl Tool for HueShiftTool {
    fn id(&self) -> &'static str {
        "hue_shift"
    }
    fn display_name(&self) -> &'static str {
        "🎡  Hue Shift"
    }
    fn editing_tool(&self) -> Option<EditingTool> {
        Some(EditingTool::HueShift)
    }

    fn render_ui(&mut self, ui: &mut egui::Ui, ctx: &ToolUiCtx<'_>) -> ToolAction {
        let changed = ui
            .add(
                egui::Slider::new(&mut self.degrees, -180.0..=180.0)
                    .text("Degrees")
                    .step_by(1.0),
            )
            .changed();
        param_tool_actions(ui, ctx, self, changed)
    }
    super::shared::impl_param_tool!();
}
