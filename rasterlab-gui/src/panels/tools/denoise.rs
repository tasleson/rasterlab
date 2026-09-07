use egui::DragValue;
use rasterlab_core::ops::DenoiseOp;

use super::shared::{ParamTool, param_tool_actions};
use super::tool_trait::{Tool, ToolAction, ToolUiCtx};
use crate::state::EditingTool;

pub struct DenoiseTool {
    pub strength: f32,
    pub radius: u32,
    pub preview_active: bool,
}

impl DenoiseTool {
    pub fn new() -> Self {
        Self {
            strength: 0.5,
            radius: 3,
            preview_active: false,
        }
    }
}

impl ParamTool for DenoiseTool {
    type Op = DenoiseOp;

    const APPLY: &'static str = "Apply Denoise";

    fn op(&self) -> DenoiseOp {
        DenoiseOp::new(self.strength, self.radius)
    }

    fn reset(&mut self) {
        *self = Self::new();
    }

    fn load(&mut self, op: &DenoiseOp) {
        self.strength = op.strength;
        self.radius = op.radius;
    }

    fn preview_active(&mut self) -> &mut bool {
        &mut self.preview_active
    }
}

impl Tool for DenoiseTool {
    fn id(&self) -> &'static str {
        "denoise"
    }
    fn display_name(&self) -> &'static str {
        "◌  Denoise"
    }
    fn editing_tool(&self) -> Option<EditingTool> {
        Some(EditingTool::Denoise)
    }

    fn render_ui(&mut self, ui: &mut egui::Ui, ctx: &ToolUiCtx<'_>) -> ToolAction {
        let mut changed = false;
        egui::Grid::new("denoise_grid")
            .num_columns(2)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                ui.label("Strength:");
                changed |= ui
                    .add(
                        DragValue::new(&mut self.strength)
                            .speed(0.01)
                            .range(0.01..=1.0_f32),
                    )
                    .changed();
                ui.end_row();
                ui.label("Radius:");
                changed |= ui
                    .add(
                        DragValue::new(&mut self.radius)
                            .speed(1)
                            .range(1..=10_u32)
                            .suffix(" px"),
                    )
                    .changed();
                ui.end_row();
            });
        param_tool_actions(ui, ctx, self, changed)
    }
    super::shared::impl_param_tool!();
}
