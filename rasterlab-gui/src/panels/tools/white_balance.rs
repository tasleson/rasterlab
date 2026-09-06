use rasterlab_core::ops::WhiteBalanceOp;

use super::shared::{ParamTool, param_tool_actions};
use super::tool_trait::{Tool, ToolAction, ToolUiCtx};
use crate::state::EditingTool;

pub struct WhiteBalanceTool {
    pub temperature: f32,
    pub tint: f32,
    pub preview_active: bool,
}

impl WhiteBalanceTool {
    pub fn new() -> Self {
        Self {
            temperature: 0.0,
            tint: 0.0,
            preview_active: false,
        }
    }
}

impl ParamTool for WhiteBalanceTool {
    type Op = WhiteBalanceOp;

    fn op(&self) -> WhiteBalanceOp {
        WhiteBalanceOp::new(self.temperature, self.tint)
    }

    fn reset(&mut self) {
        *self = Self::new();
    }

    fn load(&mut self, op: &WhiteBalanceOp) {
        self.temperature = op.temperature;
        self.tint = op.tint;
    }

    fn preview_active(&mut self) -> &mut bool {
        &mut self.preview_active
    }
}

impl Tool for WhiteBalanceTool {
    fn id(&self) -> &'static str {
        "white_balance"
    }
    fn display_name(&self) -> &'static str {
        "🌡  White Balance"
    }
    fn editing_tool(&self) -> Option<EditingTool> {
        Some(EditingTool::WhiteBalance)
    }

    fn render_ui(&mut self, ui: &mut egui::Ui, ctx: &ToolUiCtx<'_>) -> ToolAction {
        let mut changed = false;
        egui::Grid::new("wb_grid")
            .num_columns(2)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                ui.label("Temperature");
                changed |= ui
                    .add(egui::Slider::new(&mut self.temperature, -1.0..=1.0).step_by(0.01))
                    .changed();
                ui.end_row();
                ui.label("Tint");
                changed |= ui
                    .add(egui::Slider::new(&mut self.tint, -1.0..=1.0).step_by(0.01))
                    .changed();
                ui.end_row();
            });
        param_tool_actions(ui, ctx, self, changed)
    }
    super::shared::impl_param_tool!();
}
