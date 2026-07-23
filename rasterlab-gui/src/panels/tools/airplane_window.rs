use std::any::Any;

use rasterlab_core::ops::AirplaneWindowCorrectionOp;
use rasterlab_core::traits::operation::Operation;

use super::tool_trait::{Tool, ToolAction, ToolUiCtx};
use crate::state::EditingTool;

pub struct AirplaneWindowTool {
    pub strength: f32,
    pub cast_removal: f32,
    pub haze_reduction: f32,
    pub reflection_repair: f32,
    pub preview_active: bool,
}

impl AirplaneWindowTool {
    pub fn new() -> Self {
        let defaults = AirplaneWindowCorrectionOp::default();
        Self {
            strength: defaults.strength,
            cast_removal: defaults.cast_removal,
            haze_reduction: defaults.haze_reduction,
            reflection_repair: defaults.reflection_repair,
            preview_active: false,
        }
    }
}

impl Tool for AirplaneWindowTool {
    fn id(&self) -> &'static str {
        "airplane_window"
    }

    fn display_name(&self) -> &'static str {
        "✈  Airplane Window"
    }

    fn editing_tool(&self) -> Option<EditingTool> {
        Some(EditingTool::AirplaneWindow)
    }

    fn render_ui(&mut self, ui: &mut egui::Ui, ctx: &ToolUiCtx<'_>) -> ToolAction {
        let mut changed = false;
        changed |= ui
            .add(
                egui::Slider::new(&mut self.strength, 0.0..=1.0)
                    .step_by(0.01)
                    .text("Strength"),
            )
            .changed();
        changed |= ui
            .add(
                egui::Slider::new(&mut self.cast_removal, 0.0..=1.0)
                    .step_by(0.01)
                    .text("Cast"),
            )
            .changed();
        changed |= ui
            .add(
                egui::Slider::new(&mut self.haze_reduction, 0.0..=1.0)
                    .step_by(0.01)
                    .text("Haze"),
            )
            .changed();
        changed |= ui
            .add(
                egui::Slider::new(&mut self.reflection_repair, 0.0..=1.0)
                    .step_by(0.01)
                    .text("Reflections"),
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
                action = ToolAction::PushOp(Box::new(self.op()));
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
                *self = Self::new();
                action = ToolAction::RequestRender;
            }
        });

        action
    }

    super::shared::impl_preview_controls!();

    fn preview_op(&self) -> Option<Box<dyn Operation>> {
        if self.preview_active {
            Some(Box::new(self.op()))
        } else {
            None
        }
    }

    fn load_from_op(&mut self, op: &dyn Operation) -> bool {
        if let Some(o) = op
            .as_any()
            .and_then(|a| a.downcast_ref::<AirplaneWindowCorrectionOp>())
        {
            self.strength = o.strength;
            self.cast_removal = o.cast_removal;
            self.haze_reduction = o.haze_reduction;
            self.reflection_repair = o.reflection_repair;
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

impl AirplaneWindowTool {
    fn op(&self) -> AirplaneWindowCorrectionOp {
        AirplaneWindowCorrectionOp::new(
            self.strength,
            self.cast_removal,
            self.haze_reduction,
            self.reflection_repair,
        )
    }
}
