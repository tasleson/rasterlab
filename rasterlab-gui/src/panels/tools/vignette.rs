use std::any::Any;

use egui::DragValue;
use rasterlab_core::ops::VignetteOp;
use rasterlab_core::traits::operation::Operation;

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
        let mut action = ToolAction::None;
        if changed && ctx.has_image {
            self.preview_active = true;
            action = ToolAction::RequestRender;
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(ctx.has_image, egui::Button::new("Apply Vignette"))
                .clicked()
            {
                self.preview_active = false;
                action = ToolAction::PushOp(Box::new(VignetteOp::new(
                    self.strength,
                    self.radius,
                    self.feather,
                )));
                self.strength = 0.5;
                self.radius = 0.7;
                self.feather = 0.3;
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
                self.radius = 0.7;
                self.feather = 0.3;
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
            Some(Box::new(VignetteOp::new(
                self.strength,
                self.radius,
                self.feather,
            )))
        } else {
            None
        }
    }
    fn load_from_op(&mut self, op: &dyn Operation) -> bool {
        if let Some(o) = op.as_any().and_then(|a| a.downcast_ref::<VignetteOp>()) {
            self.strength = o.strength;
            self.radius = o.radius;
            self.feather = o.feather;
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
