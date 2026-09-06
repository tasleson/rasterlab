use rasterlab_core::ops::SharpenOp;

use super::shared::{ParamTool, param_tool_actions};
use super::tool_trait::{Tool, ToolAction, ToolUiCtx};
use crate::state::EditingTool;

pub struct SharpenTool {
    pub strength: f32,
    pub preview_active: bool,
}

impl SharpenTool {
    pub fn new() -> Self {
        Self {
            strength: 1.0,
            preview_active: false,
        }
    }
}

impl ParamTool for SharpenTool {
    type Op = SharpenOp;

    const APPLY: &'static str = "Apply Sharpen";

    fn op(&self) -> SharpenOp {
        SharpenOp::new(self.strength)
    }

    fn reset(&mut self) {
        *self = Self::new();
    }

    fn load(&mut self, op: &SharpenOp) {
        self.strength = op.strength;
    }

    fn preview_active(&mut self) -> &mut bool {
        &mut self.preview_active
    }
}

impl Tool for SharpenTool {
    fn id(&self) -> &'static str {
        "sharpen"
    }
    fn display_name(&self) -> &'static str {
        "◈  Sharpen"
    }
    fn editing_tool(&self) -> Option<EditingTool> {
        Some(EditingTool::Sharpen)
    }

    fn render_ui(&mut self, ui: &mut egui::Ui, ctx: &ToolUiCtx<'_>) -> ToolAction {
        let changed = ui
            .add(
                egui::Slider::new(&mut self.strength, 0.0..=10.0)
                    .step_by(0.05)
                    .text("Strength"),
            )
            .changed();
        param_tool_actions(ui, ctx, self, changed)
    }

    super::shared::impl_param_tool!();
}
