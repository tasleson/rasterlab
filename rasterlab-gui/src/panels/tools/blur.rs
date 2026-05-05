use std::any::Any;

use egui::DragValue;
use rasterlab_core::ops::BlurOp;
use rasterlab_core::traits::operation::Operation;

use super::tool_trait::{Tool, ToolAction, ToolUiCtx};
use crate::state::EditingTool;

pub struct BlurTool {
    pub radius: f32,
    pub preview_active: bool,
}

impl BlurTool {
    pub fn new() -> Self {
        Self {
            radius: 2.0,
            preview_active: false,
        }
    }
}

impl Tool for BlurTool {
    fn id(&self) -> &'static str {
        "blur"
    }
    fn display_name(&self) -> &'static str {
        "≋  Blur"
    }
    fn editing_tool(&self) -> Option<EditingTool> {
        Some(EditingTool::Blur)
    }

    fn render_ui(&mut self, ui: &mut egui::Ui, ctx: &ToolUiCtx<'_>) -> ToolAction {
        let changed = ui
            .horizontal(|ui| {
                ui.label("Radius (σ):");
                ui.add(
                    DragValue::new(&mut self.radius)
                        .speed(0.1)
                        .range(0.1..=100.0_f32)
                        .suffix(" px"),
                )
                .changed()
            })
            .inner;
        let mut action = ToolAction::None;
        if changed && ctx.has_image {
            self.preview_active = true;
            action = ToolAction::RequestRender;
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(ctx.has_image, egui::Button::new("Apply Blur"))
                .clicked()
            {
                self.preview_active = false;
                action = ToolAction::PushOp(Box::new(BlurOp::new(self.radius)));
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
                self.radius = 2.0;
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
            Some(Box::new(BlurOp::new(self.radius)))
        } else {
            None
        }
    }
    fn load_from_op(&mut self, op: &dyn Operation) -> bool {
        if let Some(o) = op.as_any().and_then(|a| a.downcast_ref::<BlurOp>()) {
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
