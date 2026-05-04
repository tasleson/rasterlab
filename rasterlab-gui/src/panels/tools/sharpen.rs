use std::any::Any;

use rasterlab_core::ops::SharpenOp;
use rasterlab_core::traits::operation::Operation;

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
        if changed && ctx.has_image {
            self.preview_active = true;
            return ToolAction::RequestRender;
        }
        let mut action = ToolAction::None;
        ui.horizontal(|ui| {
            if ui
                .add_enabled(ctx.has_image, egui::Button::new("Apply Sharpen"))
                .clicked()
            {
                self.preview_active = false;
                action = ToolAction::PushOp(Box::new(SharpenOp::new(self.strength)));
                self.strength = 1.0;
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
                self.strength = 1.0;
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
            Some(Box::new(SharpenOp::new(self.strength)))
        } else {
            None
        }
    }
    fn load_from_op(&mut self, op: &dyn Operation) -> bool {
        if let Some(o) = op.as_any().and_then(|a| a.downcast_ref::<SharpenOp>()) {
            self.strength = o.strength;
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
