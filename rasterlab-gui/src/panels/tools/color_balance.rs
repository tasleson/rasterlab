use std::any::Any;

use rasterlab_core::ops::ColorBalanceOp;
use rasterlab_core::traits::operation::Operation;

use super::tool_trait::{Tool, ToolAction, ToolUiCtx};
use crate::state::EditingTool;

pub struct ColorBalanceTool {
    pub cyan_red: [f32; 3],
    pub magenta_green: [f32; 3],
    pub yellow_blue: [f32; 3],
    pub preview_active: bool,
}

impl ColorBalanceTool {
    pub fn new() -> Self {
        Self {
            cyan_red: [0.0; 3],
            magenta_green: [0.0; 3],
            yellow_blue: [0.0; 3],
            preview_active: false,
        }
    }
}

impl Tool for ColorBalanceTool {
    fn id(&self) -> &'static str {
        "color_balance"
    }
    fn display_name(&self) -> &'static str {
        "⚖  Color Balance"
    }
    fn editing_tool(&self) -> Option<EditingTool> {
        Some(EditingTool::ColorBalance)
    }

    fn render_ui(&mut self, ui: &mut egui::Ui, ctx: &ToolUiCtx<'_>) -> ToolAction {
        let mut changed = false;
        let zone_labels = ["Shadows", "Midtones", "Highlights"];

        ui.label("Cyan ↔ Red");
        egui::Grid::new("cb_cr_grid")
            .num_columns(2)
            .spacing([8.0, 2.0])
            .show(ui, |ui| {
                for (i, zone) in zone_labels.iter().enumerate() {
                    ui.label(*zone);
                    changed |= ui
                        .add(egui::Slider::new(&mut self.cyan_red[i], -1.0..=1.0).step_by(0.01))
                        .changed();
                    ui.end_row();
                }
            });
        ui.add_space(4.0);
        ui.label("Magenta ↔ Green");
        egui::Grid::new("cb_mg_grid")
            .num_columns(2)
            .spacing([8.0, 2.0])
            .show(ui, |ui| {
                for (i, zone) in zone_labels.iter().enumerate() {
                    ui.label(*zone);
                    changed |= ui
                        .add(
                            egui::Slider::new(&mut self.magenta_green[i], -1.0..=1.0).step_by(0.01),
                        )
                        .changed();
                    ui.end_row();
                }
            });
        ui.add_space(4.0);
        ui.label("Yellow ↔ Blue");
        egui::Grid::new("cb_yb_grid")
            .num_columns(2)
            .spacing([8.0, 2.0])
            .show(ui, |ui| {
                for (i, zone) in zone_labels.iter().enumerate() {
                    ui.label(*zone);
                    changed |= ui
                        .add(egui::Slider::new(&mut self.yellow_blue[i], -1.0..=1.0).step_by(0.01))
                        .changed();
                    ui.end_row();
                }
            });
        ui.add_space(4.0);

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
                action = ToolAction::PushOp(Box::new(ColorBalanceOp::new(
                    self.cyan_red,
                    self.magenta_green,
                    self.yellow_blue,
                )));
                self.cyan_red = [0.0; 3];
                self.magenta_green = [0.0; 3];
                self.yellow_blue = [0.0; 3];
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
                self.cyan_red = [0.0; 3];
                self.magenta_green = [0.0; 3];
                self.yellow_blue = [0.0; 3];
                if self.preview_active {
                    self.preview_active = false;
                    action = ToolAction::RequestRender;
                }
            }
        });
        action
    }

    super::shared::impl_preview_controls!();
    fn preview_op(&self) -> Option<Box<dyn Operation>> {
        if self.preview_active {
            Some(Box::new(ColorBalanceOp::new(
                self.cyan_red,
                self.magenta_green,
                self.yellow_blue,
            )))
        } else {
            None
        }
    }
    fn load_from_op(&mut self, op: &dyn Operation) -> bool {
        if let Some(o) = op.as_any().and_then(|a| a.downcast_ref::<ColorBalanceOp>()) {
            self.cyan_red = o.cyan_red;
            self.magenta_green = o.magenta_green;
            self.yellow_blue = o.yellow_blue;
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
