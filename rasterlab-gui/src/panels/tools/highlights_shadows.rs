use rasterlab_core::ops::HighlightsShadowsOp;

use super::shared::{ParamTool, param_tool_actions};
use super::tool_trait::{Tool, ToolAction, ToolUiCtx};
use crate::state::EditingTool;

pub struct HighlightsShadowsTool {
    pub highlights: f32,
    pub shadows: f32,
    pub preview_active: bool,
}

impl HighlightsShadowsTool {
    pub fn new() -> Self {
        Self {
            highlights: 0.0,
            shadows: 0.0,
            preview_active: false,
        }
    }
}

impl ParamTool for HighlightsShadowsTool {
    type Op = HighlightsShadowsOp;

    fn op(&self) -> HighlightsShadowsOp {
        HighlightsShadowsOp::new(self.highlights, self.shadows)
    }

    fn reset(&mut self) {
        *self = Self::new();
    }

    fn load(&mut self, op: &HighlightsShadowsOp) {
        self.highlights = op.highlights;
        self.shadows = op.shadows;
    }

    fn preview_active(&mut self) -> &mut bool {
        &mut self.preview_active
    }
}

impl Tool for HighlightsShadowsTool {
    fn id(&self) -> &'static str {
        "highlights_shadows"
    }
    fn display_name(&self) -> &'static str {
        "◑  Highlights / Shadows"
    }
    fn editing_tool(&self) -> Option<EditingTool> {
        Some(EditingTool::HighlightsShadows)
    }

    fn render_ui(&mut self, ui: &mut egui::Ui, ctx: &ToolUiCtx<'_>) -> ToolAction {
        let mut changed = false;
        egui::Grid::new("hl_grid")
            .num_columns(2)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                ui.label("Highlights");
                changed |= ui
                    .add(egui::Slider::new(&mut self.highlights, -1.0..=1.0).step_by(0.01))
                    .changed();
                ui.end_row();
                ui.label("Shadows");
                changed |= ui
                    .add(egui::Slider::new(&mut self.shadows, -1.0..=1.0).step_by(0.01))
                    .changed();
                ui.end_row();
            });
        param_tool_actions(ui, ctx, self, changed)
    }
    super::shared::impl_param_tool!();
}
