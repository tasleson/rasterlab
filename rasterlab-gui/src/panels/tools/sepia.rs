use rasterlab_core::ops::SepiaOp;

use super::shared::{ParamTool, param_tool_actions};
use super::tool_trait::{Tool, ToolAction, ToolUiCtx};
use crate::state::EditingTool;

pub struct SepiaTool {
    pub strength: f32,
    pub preview_active: bool,
}

impl SepiaTool {
    pub fn new() -> Self {
        Self {
            strength: 1.0,
            preview_active: false,
        }
    }
}

impl ParamTool for SepiaTool {
    type Op = SepiaOp;

    const APPLY: &'static str = "Apply Sepia";

    fn op(&self) -> SepiaOp {
        SepiaOp::new(self.strength)
    }

    fn reset(&mut self) {
        *self = Self::new();
    }

    fn load(&mut self, op: &SepiaOp) {
        self.strength = op.strength;
    }

    fn preview_active(&mut self) -> &mut bool {
        &mut self.preview_active
    }
}

impl Tool for SepiaTool {
    fn id(&self) -> &'static str {
        "sepia"
    }
    fn display_name(&self) -> &'static str {
        "🟫  Sepia"
    }
    fn editing_tool(&self) -> Option<EditingTool> {
        Some(EditingTool::Sepia)
    }

    fn render_ui(&mut self, ui: &mut egui::Ui, ctx: &ToolUiCtx<'_>) -> ToolAction {
        let changed = ui
            .add(egui::Slider::new(&mut self.strength, 0.0..=1.0).step_by(0.01))
            .changed();
        param_tool_actions(ui, ctx, self, changed)
    }

    super::shared::impl_param_tool!();
}
