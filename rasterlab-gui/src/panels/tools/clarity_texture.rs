use rasterlab_core::ops::ClarityTextureOp;

use super::shared::{ParamTool, param_tool_actions};
use super::tool_trait::{Tool, ToolAction, ToolUiCtx};
use crate::state::EditingTool;

pub struct ClarityTextureTool {
    pub clarity: f32,
    pub texture: f32,
    pub preview_active: bool,
}

impl ClarityTextureTool {
    pub fn new() -> Self {
        Self {
            clarity: 0.0,
            texture: 0.0,
            preview_active: false,
        }
    }
}

impl ParamTool for ClarityTextureTool {
    type Op = ClarityTextureOp;

    fn op(&self) -> ClarityTextureOp {
        ClarityTextureOp::new(self.clarity, self.texture)
    }

    fn reset(&mut self) {
        *self = Self::new();
    }

    fn load(&mut self, op: &ClarityTextureOp) {
        self.clarity = op.clarity;
        self.texture = op.texture;
    }

    fn preview_active(&mut self) -> &mut bool {
        &mut self.preview_active
    }
}

impl Tool for ClarityTextureTool {
    fn id(&self) -> &'static str {
        "clarity_texture"
    }
    fn display_name(&self) -> &'static str {
        "◈  Clarity / Texture"
    }
    fn editing_tool(&self) -> Option<EditingTool> {
        Some(EditingTool::ClarityTexture)
    }

    fn render_ui(&mut self, ui: &mut egui::Ui, ctx: &ToolUiCtx<'_>) -> ToolAction {
        let c_changed = ui
            .add(
                egui::Slider::new(&mut self.clarity, -1.0..=1.0)
                    .step_by(0.01)
                    .text("Clarity"),
            )
            .changed();
        let t_changed = ui
            .add(
                egui::Slider::new(&mut self.texture, -1.0..=1.0)
                    .step_by(0.01)
                    .text("Texture"),
            )
            .changed();
        param_tool_actions(ui, ctx, self, c_changed || t_changed)
    }
    super::shared::impl_param_tool!();
}
