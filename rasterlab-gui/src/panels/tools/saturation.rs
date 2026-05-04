use std::any::Any;

use rasterlab_core::ops::SaturationOp;
use rasterlab_core::traits::operation::Operation;

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
                action = ToolAction::PushOp(Box::new(SaturationOp::new(self.saturation)));
                self.saturation = 1.0;
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
                self.saturation = 1.0;
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
            Some(Box::new(SaturationOp::new(self.saturation)))
        } else {
            None
        }
    }
    fn load_from_op(&mut self, op: &dyn Operation) -> bool {
        if let Some(o) = op.as_any().and_then(|a| a.downcast_ref::<SaturationOp>()) {
            self.saturation = o.saturation;
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
