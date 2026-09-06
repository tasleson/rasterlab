use rasterlab_core::ops::ColorBalanceOp;

use super::shared::{ParamTool, param_tool_actions};
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

impl ParamTool for ColorBalanceTool {
    type Op = ColorBalanceOp;

    fn op(&self) -> ColorBalanceOp {
        ColorBalanceOp::new(self.cyan_red, self.magenta_green, self.yellow_blue)
    }

    fn reset(&mut self) {
        *self = Self::new();
    }

    fn load(&mut self, op: &ColorBalanceOp) {
        self.cyan_red = op.cyan_red;
        self.magenta_green = op.magenta_green;
        self.yellow_blue = op.yellow_blue;
    }

    fn preview_active(&mut self) -> &mut bool {
        &mut self.preview_active
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

        param_tool_actions(ui, ctx, self, changed)
    }
    super::shared::impl_param_tool!();
}
