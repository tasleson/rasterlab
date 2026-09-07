use rasterlab_core::ops::SaturationOp;

use super::shared::{ParamTool, param_tool_actions};
use super::tool_trait::{Tool, ToolAction, ToolUiCtx};
use crate::state::EditingTool;

pub struct SaturationTool {
    pub saturation: f32,
    pub preview_active: bool,
}

impl SaturationTool {
    pub fn new() -> Self {
        Self {
            saturation: 1.0,
            preview_active: false,
        }
    }
}

impl ParamTool for SaturationTool {
    type Op = SaturationOp;

    fn op(&self) -> SaturationOp {
        SaturationOp::new(self.saturation)
    }

    fn reset(&mut self) {
        *self = Self::new();
    }

    fn load(&mut self, op: &SaturationOp) {
        self.saturation = op.saturation;
    }

    fn preview_active(&mut self) -> &mut bool {
        &mut self.preview_active
    }
}

impl Tool for SaturationTool {
    fn id(&self) -> &'static str {
        "saturation"
    }
    fn display_name(&self) -> &'static str {
        "🎨  Saturation"
    }
    fn editing_tool(&self) -> Option<EditingTool> {
        Some(EditingTool::Saturation)
    }

    fn render_ui(&mut self, ui: &mut egui::Ui, ctx: &ToolUiCtx<'_>) -> ToolAction {
        let changed = ui
            .add(egui::Slider::new(&mut self.saturation, 0.0..=4.0).step_by(0.01))
            .changed();
        param_tool_actions(ui, ctx, self, changed)
    }

    super::shared::impl_param_tool!();
}
