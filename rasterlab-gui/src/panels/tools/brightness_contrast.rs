use std::any::Any;

use rasterlab_core::ops::BrightnessContrastOp;
use rasterlab_core::traits::operation::Operation;

use super::tool_trait::{Tool, ToolAction, ToolUiCtx};
use crate::state::EditingTool;

pub struct BrightnessContrastTool {
    pub brightness: f32,
    pub contrast: f32,
    pub preview_active: bool,
}

impl BrightnessContrastTool {
    pub fn new() -> Self {
        Self {
            brightness: 0.0,
            contrast: 0.0,
            preview_active: false,
        }
    }
}

impl Tool for BrightnessContrastTool {
    fn id(&self) -> &'static str {
        "brightness_contrast"
    }
    fn display_name(&self) -> &'static str {
        "☀  Brightness / Contrast"
    }
    fn editing_tool(&self) -> Option<EditingTool> {
        Some(EditingTool::BrightnessContrast)
    }

    fn render_ui(&mut self, ui: &mut egui::Ui, ctx: &ToolUiCtx<'_>) -> ToolAction {
        let mut changed = false;
        egui::Grid::new("bc_grid")
            .num_columns(2)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                ui.label("Brightness");
                changed |= ui
                    .add(egui::Slider::new(&mut self.brightness, -1.0..=1.0).step_by(0.01))
                    .changed();
                ui.end_row();
                ui.label("Contrast");
                changed |= ui
                    .add(egui::Slider::new(&mut self.contrast, -1.0..=1.0).step_by(0.01))
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
                .add_enabled(ctx.has_image, egui::Button::new("Apply"))
                .clicked()
            {
                self.preview_active = false;
                action = ToolAction::PushOp(Box::new(BrightnessContrastOp::new(
                    self.brightness,
                    self.contrast,
                )));
                self.brightness = 0.0;
                self.contrast = 0.0;
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
                self.brightness = 0.0;
                self.contrast = 0.0;
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
            Some(Box::new(BrightnessContrastOp::new(
                self.brightness,
                self.contrast,
            )))
        } else {
            None
        }
    }
    fn load_from_op(&mut self, op: &dyn Operation) -> bool {
        if let Some(o) = op
            .as_any()
            .and_then(|a| a.downcast_ref::<BrightnessContrastOp>())
        {
            self.brightness = o.brightness;
            self.contrast = o.contrast;
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
