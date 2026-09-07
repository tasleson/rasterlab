use rasterlab_core::ops::LocalLaplacianOp;
use rasterlab_core::ops::local_laplacian::DEFAULT_THRESHOLD;

use super::shared::{ParamTool, param_tool_actions};
use super::tool_trait::{Tool, ToolAction, ToolUiCtx};
use crate::state::EditingTool;

pub struct LocalLaplacianTool {
    pub tone: f32,
    pub detail: f32,
    pub threshold: f32,
    pub preview_active: bool,
}

impl LocalLaplacianTool {
    pub fn new() -> Self {
        Self {
            tone: 0.0,
            detail: 0.0,
            threshold: DEFAULT_THRESHOLD,
            preview_active: false,
        }
    }
}

impl ParamTool for LocalLaplacianTool {
    type Op = LocalLaplacianOp;

    fn op(&self) -> LocalLaplacianOp {
        LocalLaplacianOp::new(self.tone, self.detail, self.threshold)
    }

    fn reset(&mut self) {
        *self = Self::new();
    }

    fn load(&mut self, op: &LocalLaplacianOp) {
        self.tone = op.tone;
        self.detail = op.detail;
        self.threshold = op.threshold;
    }

    fn preview_active(&mut self) -> &mut bool {
        &mut self.preview_active
    }
}

impl Tool for LocalLaplacianTool {
    fn id(&self) -> &'static str {
        "local_laplacian"
    }
    fn display_name(&self) -> &'static str {
        "◑  Local Tone"
    }
    fn editing_tool(&self) -> Option<EditingTool> {
        Some(EditingTool::LocalLaplacian)
    }

    fn render_ui(&mut self, ui: &mut egui::Ui, ctx: &ToolUiCtx<'_>) -> ToolAction {
        let tone_changed = ui
            .add(
                egui::Slider::new(&mut self.tone, -1.0..=1.0)
                    .step_by(0.01)
                    .text("Tone"),
            )
            .on_hover_text(
                "Compresses large-scale contrast: lifts shadows and holds highlights \
                 without flattening detail.  Negative expands instead.",
            )
            .changed();
        let detail_changed = ui
            .add(
                egui::Slider::new(&mut self.detail, -1.0..=1.0)
                    .step_by(0.01)
                    .text("Detail"),
            )
            .on_hover_text("Small-scale texture contrast.  Noise is left alone either way.")
            .changed();
        let threshold_changed = ui
            .add(
                egui::Slider::new(&mut self.threshold, 0.02..=0.5)
                    .step_by(0.01)
                    .text("Threshold"),
            )
            .on_hover_text(
                "Where texture ends and scene structure begins.  Differences below this \
                 follow Detail; larger ones follow Tone.",
            )
            .changed();

        param_tool_actions(
            ui,
            ctx,
            self,
            tone_changed || detail_changed || threshold_changed,
        )
    }
    super::shared::impl_param_tool!();
}
