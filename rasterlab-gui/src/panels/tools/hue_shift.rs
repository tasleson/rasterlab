use std::any::Any;

use rasterlab_core::ops::HueShiftOp;
use rasterlab_core::traits::operation::Operation;

use super::tool_trait::{Tool, ToolAction, ToolUiCtx};
use crate::state::EditingTool;

pub struct HueShiftTool {
    pub degrees: f32,
    pub preview_active: bool,
}

impl HueShiftTool {
    pub fn new() -> Self {
        Self {
            degrees: 0.0,
            preview_active: false,
        }
    }
}

impl Tool for HueShiftTool {
    fn id(&self) -> &'static str {
        "hue_shift"
    }
    fn display_name(&self) -> &'static str {
        "🎡  Hue Shift"
    }
    fn editing_tool(&self) -> Option<EditingTool> {
        Some(EditingTool::HueShift)
    }

    fn render_ui(&mut self, ui: &mut egui::Ui, ctx: &ToolUiCtx<'_>) -> ToolAction {
        let changed = ui
            .add(
                egui::Slider::new(&mut self.degrees, -180.0..=180.0)
                    .text("Degrees")
                    .step_by(1.0),
            )
            .changed();
        let mut action = ToolAction::None;
        if changed && ctx.has_image {
            self.preview_active = true;
            action = ToolAction::RequestRender;
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(ctx.has_image, egui::Button::new("Apply"))
                .clicked()
            {
                self.preview_active = false;
                action = ToolAction::PushOp(Box::new(HueShiftOp::new(self.degrees)));
                self.degrees = 0.0;
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
                self.degrees = 0.0;
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
            Some(Box::new(HueShiftOp::new(self.degrees)))
        } else {
            None
        }
    }
    fn load_from_op(&mut self, op: &dyn Operation) -> bool {
        if let Some(o) = op.as_any().and_then(|a| a.downcast_ref::<HueShiftOp>()) {
            self.degrees = o.degrees;
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
