use std::any::Any;

use egui::DragValue;
use rasterlab_core::ops::DenoiseOp;
use rasterlab_core::traits::operation::Operation;

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
        if changed && ctx.has_image {
            self.preview_active = true;
            return ToolAction::RequestRender;
        }
        let mut action = ToolAction::None;
        ui.horizontal(|ui| {
            if ui
                .add_enabled(ctx.has_image, egui::Button::new("Apply Denoise"))
                .clicked()
            {
                self.preview_active = false;
                action = ToolAction::PushOp(Box::new(DenoiseOp::new(self.strength, self.radius)));
                self.strength = 0.5;
                self.radius = 3;
            }
            if self.preview_active
                && ui
                    .add_enabled(ctx.has_image, egui::Button::new("Cancel"))
                    .clicked()
            {
                self.preview_active = false;
                action = ToolAction::RequestRender;
            }
            if ui.button("Reset").clicked() {
                self.strength = 0.5;
                self.radius = 3;
                if self.preview_active {
                    self.preview_active = false;
                    action = ToolAction::RequestRender;
                }
            }
        });
        action
    }

    fn is_preview_active(&self) -> bool {
        self.preview_active
    }
    fn cancel_preview(&mut self) {
        self.preview_active = false;
    }
    fn activate_preview(&mut self) {
        self.preview_active = true;
    }
    fn preview_op(&self) -> Option<Box<dyn Operation>> {
        if self.preview_active {
            Some(Box::new(DenoiseOp::new(self.strength, self.radius)))
        } else {
            None
        }
    }
    fn load_from_op(&mut self, op: &dyn Operation) -> bool {
        if let Some(o) = op.as_any().and_then(|a| a.downcast_ref::<DenoiseOp>()) {
            self.strength = o.strength;
            self.radius = o.radius;
            true
        } else {
            false
        }
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
