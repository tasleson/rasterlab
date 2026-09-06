use egui::DragValue;
use rasterlab_core::ops::BlurOp;

use super::shared::{ParamTool, param_tool_actions};
use super::tool_trait::{Tool, ToolAction, ToolUiCtx};
use crate::state::EditingTool;

pub struct BlurTool {
    pub radius: f32,
    pub preview_active: bool,
}

impl BlurTool {
    pub fn new() -> Self {
        Self {
            radius: 2.0,
            preview_active: false,
        }
    }
}

impl ParamTool for BlurTool {
    type Op = BlurOp;

    const APPLY: &'static str = "Apply Blur";

    fn op(&self) -> BlurOp {
        BlurOp::new(self.radius)
    }

    fn reset(&mut self) {
        *self = Self::new();
    }

    fn load(&mut self, op: &BlurOp) {
        self.radius = op.radius;
    }

    fn preview_active(&mut self) -> &mut bool {
        &mut self.preview_active
    }
}

impl Tool for BlurTool {
    fn id(&self) -> &'static str {
        "blur"
    }
    fn display_name(&self) -> &'static str {
        "≋  Blur"
    }
    fn editing_tool(&self) -> Option<EditingTool> {
        Some(EditingTool::Blur)
    }

    fn render_ui(&mut self, ui: &mut egui::Ui, ctx: &ToolUiCtx<'_>) -> ToolAction {
        let changed = ui
            .horizontal(|ui| {
                ui.label("Radius (σ):");
                ui.add(
                    DragValue::new(&mut self.radius)
                        .speed(0.1)
                        .range(0.1..=100.0_f32)
                        .suffix(" px"),
                )
                .changed()
            })
            .inner;
        param_tool_actions(ui, ctx, self, changed)
    }
    super::shared::impl_param_tool!();
}
