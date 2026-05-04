use std::any::Any;

use rasterlab_core::ops::VibranceOp;
use rasterlab_core::traits::operation::Operation;

use super::tool_trait::{Tool, ToolAction, ToolUiCtx};
use crate::state::EditingTool;

pub struct VibranceTool {
    pub vibrance: f32,
    pub preview_active: bool,
}

impl VibranceTool {
    pub fn new() -> Self {
        Self {
            vibrance: 0.0,
            preview_active: false,
        }
    }
}

impl Tool for VibranceTool {
    fn id(&self) -> &'static str {
        "vibrance"
    }
    fn display_name(&self) -> &'static str {
        "✦  Vibrance"
    }
    fn editing_tool(&self) -> Option<EditingTool> {
        Some(EditingTool::Vibrance)
    }

    fn render_ui(&mut self, ui: &mut egui::Ui, ctx: &ToolUiCtx<'_>) -> ToolAction {
        let changed = ui
            .add(egui::Slider::new(&mut self.vibrance, -1.0..=1.0).step_by(0.01))
            .changed();
        if changed && ctx.has_image {
            self.preview_active = true;
            return ToolAction::RequestRender;
        }
        let mut action = ToolAction::None;
        ui.horizontal(|ui| {
            if ui
                .add_enabled(ctx.has_image, egui::Button::new("Apply"))
                .clicked()
            {
                self.preview_active = false;
                action = ToolAction::PushOp(Box::new(VibranceOp::new(self.vibrance)));
                self.vibrance = 0.0;
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
                self.vibrance = 0.0;
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
            Some(Box::new(VibranceOp::new(self.vibrance)))
        } else {
            None
        }
    }
    fn load_from_op(&mut self, op: &dyn Operation) -> bool {
        if let Some(o) = op.as_any().and_then(|a| a.downcast_ref::<VibranceOp>()) {
            self.vibrance = o.strength;
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
