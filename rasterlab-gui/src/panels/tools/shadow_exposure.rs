use rasterlab_core::ops::ShadowExposureOp;

use super::shared::{ParamTool, param_tool_actions};
use super::tool_trait::{Tool, ToolAction, ToolUiCtx};
use crate::state::EditingTool;

pub struct ShadowExposureTool {
    pub ev: f32,
    pub falloff: f32,
    pub preview_active: bool,
}

impl ShadowExposureTool {
    pub fn new() -> Self {
        Self {
            ev: 0.0,
            falloff: 2.0,
            preview_active: false,
        }
    }
}

impl ParamTool for ShadowExposureTool {
    type Op = ShadowExposureOp;

    fn op(&self) -> ShadowExposureOp {
        ShadowExposureOp::new(self.ev, self.falloff)
    }

    fn reset(&mut self) {
        *self = Self::new();
    }

    fn load(&mut self, op: &ShadowExposureOp) {
        self.ev = op.ev;
        self.falloff = op.falloff;
    }

    fn preview_active(&mut self) -> &mut bool {
        &mut self.preview_active
    }
}

impl Tool for ShadowExposureTool {
    fn id(&self) -> &'static str {
        "shadow_exposure"
    }
    fn display_name(&self) -> &'static str {
        "🌑  Shadow Exposure"
    }
    fn editing_tool(&self) -> Option<EditingTool> {
        Some(EditingTool::ShadowExposure)
    }

    fn render_ui(&mut self, ui: &mut egui::Ui, ctx: &ToolUiCtx<'_>) -> ToolAction {
        let mut changed = false;
        egui::Grid::new("shadow_exp_grid")
            .num_columns(2)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                ui.label("EV");
                changed |= ui
                    .add(
                        egui::Slider::new(&mut self.ev, -3.0..=3.0)
                            .step_by(0.05)
                            .suffix(" stops"),
                    )
                    .on_hover_text("Exposure adjustment applied only in the shadows")
                    .changed();
                ui.end_row();
                ui.label("Falloff");
                changed |= ui
                    .add(egui::Slider::new(&mut self.falloff, 0.5..=4.0).step_by(0.05))
                    .on_hover_text(
                        "Higher values restrict the effect to deeper shadows;\n\
                             lower values reach further into the midtones",
                    )
                    .changed();
                ui.end_row();
            });
        param_tool_actions(ui, ctx, self, changed)
    }
    super::shared::impl_param_tool!();
}
