use egui::DragValue;
use rasterlab_core::ops::VignetteOp;

use super::shared::{ParamTool, param_tool_actions};
use super::tool_trait::{Tool, ToolAction, ToolUiCtx};
use crate::state::EditingTool;

pub struct VignetteTool {
    pub strength: f32,
    pub radius: f32,
    pub feather: f32,
    pub preview_active: bool,
}

impl VignetteTool {
    pub fn new() -> Self {
        Self {
            strength: 0.5,
            radius: 0.7,
            feather: 0.3,
            preview_active: false,
        }
    }
}

impl ParamTool for VignetteTool {
    type Op = VignetteOp;

    const APPLY: &'static str = "Apply Vignette";

    fn op(&self) -> VignetteOp {
        VignetteOp::new(self.strength, self.radius, self.feather)
    }

    fn reset(&mut self) {
        *self = Self::new();
    }

    fn load(&mut self, op: &VignetteOp) {
        self.strength = op.strength;
        self.radius = op.radius;
        self.feather = op.feather;
    }

    fn preview_active(&mut self) -> &mut bool {
        &mut self.preview_active
    }
}

impl Tool for VignetteTool {
    fn id(&self) -> &'static str {
        "vignette"
    }
    fn display_name(&self) -> &'static str {
        "◎  Vignette"
    }
    fn editing_tool(&self) -> Option<EditingTool> {
        Some(EditingTool::Vignette)
    }

    fn render_ui(&mut self, ui: &mut egui::Ui, ctx: &ToolUiCtx<'_>) -> ToolAction {
        let mut changed = false;
        egui::Grid::new("vignette_grid")
            .num_columns(2)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                ui.label("Strength");
                changed |= ui
                    .add(
                        DragValue::new(&mut self.strength)
                            .speed(0.01)
                            .range(0.0..=1.0),
                    )
                    .changed();
                ui.end_row();
                ui.label("Radius");
                changed |= ui
                    .add(
                        DragValue::new(&mut self.radius)
                            .speed(0.01)
                            .range(0.0..=1.0),
                    )
                    .changed();
                ui.end_row();
                ui.label("Feather");
                changed |= ui
                    .add(
                        DragValue::new(&mut self.feather)
                            .speed(0.01)
                            .range(0.0..=1.0),
                    )
                    .changed();
                ui.end_row();
            });
        param_tool_actions(ui, ctx, self, changed)
    }
    super::shared::impl_param_tool!();
}
