use std::any::Any;

use egui::DragValue;
use rasterlab_core::ops::SplitToneOp;
use rasterlab_core::traits::operation::Operation;

use super::tool_trait::{Tool, ToolAction, ToolUiCtx};
use crate::state::EditingTool;

pub struct SplitToneTool {
    pub shadow_hue: f32,
    pub shadow_sat: f32,
    pub highlight_hue: f32,
    pub highlight_sat: f32,
    pub balance: f32,
    pub preview_active: bool,
}

impl SplitToneTool {
    pub fn new() -> Self {
        Self {
            shadow_hue: 30.0,
            shadow_sat: 0.0,
            highlight_hue: 200.0,
            highlight_sat: 0.0,
            balance: 0.0,
            preview_active: false,
        }
    }
}

impl Tool for SplitToneTool {
    fn id(&self) -> &'static str {
        "split_tone"
    }
    fn display_name(&self) -> &'static str {
        "🎨  Split Tone"
    }
    fn editing_tool(&self) -> Option<EditingTool> {
        Some(EditingTool::SplitTone)
    }

    fn render_ui(&mut self, ui: &mut egui::Ui, ctx: &ToolUiCtx<'_>) -> ToolAction {
        let mut changed = false;

        egui::Grid::new("split_tone_grid")
            .num_columns(2)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                ui.label("Shadow hue");
                changed |= ui
                    .add(
                        DragValue::new(&mut self.shadow_hue)
                            .speed(1.0)
                            .range(0.0..=359.9_f32)
                            .suffix("°"),
                    )
                    .changed();
                ui.end_row();

                ui.label("Shadow sat");
                changed |= ui
                    .add(egui::Slider::new(&mut self.shadow_sat, 0.0..=1.0).step_by(0.01))
                    .changed();
                ui.end_row();

                ui.label("Highlight hue");
                changed |= ui
                    .add(
                        DragValue::new(&mut self.highlight_hue)
                            .speed(1.0)
                            .range(0.0..=359.9_f32)
                            .suffix("°"),
                    )
                    .changed();
                ui.end_row();

                ui.label("Highlight sat");
                changed |= ui
                    .add(egui::Slider::new(&mut self.highlight_sat, 0.0..=1.0).step_by(0.01))
                    .changed();
                ui.end_row();

                ui.label("Balance");
                changed |= ui
                    .add(egui::Slider::new(&mut self.balance, -1.0..=1.0).step_by(0.01))
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
                .add_enabled(ctx.has_image, egui::Button::new("Apply"))
                .clicked()
            {
                self.preview_active = false;
                action = ToolAction::PushOp(Box::new(SplitToneOp::new(
                    self.shadow_hue,
                    self.shadow_sat,
                    self.highlight_hue,
                    self.highlight_sat,
                    self.balance,
                )));
                self.shadow_hue = 30.0;
                self.shadow_sat = 0.0;
                self.highlight_hue = 200.0;
                self.highlight_sat = 0.0;
                self.balance = 0.0;
            }
            if ui.button("Reset").clicked() {
                self.shadow_hue = 30.0;
                self.shadow_sat = 0.0;
                self.highlight_hue = 200.0;
                self.highlight_sat = 0.0;
                self.balance = 0.0;
                if self.preview_active {
                    self.preview_active = false;
                    action = ToolAction::RequestRender;
                }
            }
            if self.preview_active
                && ui
                    .add_enabled(ctx.has_image, egui::Button::new("Cancel"))
                    .clicked()
            {
                self.preview_active = false;
                action = ToolAction::RequestRender;
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
            Some(Box::new(SplitToneOp::new(
                self.shadow_hue,
                self.shadow_sat,
                self.highlight_hue,
                self.highlight_sat,
                self.balance,
            )))
        } else {
            None
        }
    }
    fn load_from_op(&mut self, op: &dyn Operation) -> bool {
        if let Some(o) = op.as_any().and_then(|a| a.downcast_ref::<SplitToneOp>()) {
            self.shadow_hue = o.shadow_hue;
            self.shadow_sat = o.shadow_sat;
            self.highlight_hue = o.highlight_hue;
            self.highlight_sat = o.highlight_sat;
            self.balance = o.balance;
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
