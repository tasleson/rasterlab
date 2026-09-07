use rasterlab_core::ops::BrightnessContrastOp;

use super::shared::{ParamTool, param_tool_actions};
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

impl ParamTool for BrightnessContrastTool {
    type Op = BrightnessContrastOp;

    fn op(&self) -> BrightnessContrastOp {
        BrightnessContrastOp::new(self.brightness, self.contrast)
    }

    fn reset(&mut self) {
        *self = Self::new();
    }

    fn load(&mut self, op: &BrightnessContrastOp) {
        self.brightness = op.brightness;
        self.contrast = op.contrast;
    }

    fn preview_active(&mut self) -> &mut bool {
        &mut self.preview_active
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
        param_tool_actions(ui, ctx, self, changed)
    }
    super::shared::impl_param_tool!();
}
