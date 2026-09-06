use rasterlab_core::ops::VibranceOp;

use super::shared::{ParamTool, param_tool_actions};
use super::tool_trait::{Tool, ToolAction, ToolUiCtx};
use crate::state::EditingTool;

pub struct VibranceTool {
    pub vibrance: f32,
    pub preview_active: bool,
}

impl VibranceTool {
    pub fn new() -> Self {
        Self {
            vibrance: 0.0,
            preview_active: false,
        }
    }
}

impl ParamTool for VibranceTool {
    type Op = VibranceOp;

    fn op(&self) -> VibranceOp {
        VibranceOp::new(self.vibrance)
    }

    fn reset(&mut self) {
        *self = Self::new();
    }

    fn load(&mut self, op: &VibranceOp) {
        self.vibrance = op.strength;
    }

    fn preview_active(&mut self) -> &mut bool {
        &mut self.preview_active
    }
}

impl Tool for VibranceTool {
    fn id(&self) -> &'static str {
        "vibrance"
    }
    fn display_name(&self) -> &'static str {
        "✦  Vibrance"
    }
    fn editing_tool(&self) -> Option<EditingTool> {
        Some(EditingTool::Vibrance)
    }

    fn render_ui(&mut self, ui: &mut egui::Ui, ctx: &ToolUiCtx<'_>) -> ToolAction {
        let changed = ui
            .add(egui::Slider::new(&mut self.vibrance, -1.0..=1.0).step_by(0.01))
            .changed();
        param_tool_actions(ui, ctx, self, changed)
    }
    super::shared::impl_param_tool!();
}
