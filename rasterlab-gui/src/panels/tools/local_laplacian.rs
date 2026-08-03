use std::any::Any;

use rasterlab_core::ops::LocalLaplacianOp;
use rasterlab_core::ops::local_laplacian::DEFAULT_THRESHOLD;
use rasterlab_core::traits::operation::Operation;

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

    fn op(&self) -> LocalLaplacianOp {
        LocalLaplacianOp::new(self.tone, self.detail, self.threshold)
    }

    fn reset(&mut self) {
        self.tone = 0.0;
        self.detail = 0.0;
        self.threshold = DEFAULT_THRESHOLD;
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

        let mut action = ToolAction::None;
        if (tone_changed || detail_changed || threshold_changed) && ctx.has_image {
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
                self.reset();
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
                self.reset();
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
            Some(Box::new(self.op()))
        } else {
            None
        }
    }
    fn load_from_op(&mut self, op: &dyn Operation) -> bool {
        if let Some(o) = op
            .as_any()
            .and_then(|a| a.downcast_ref::<LocalLaplacianOp>())
        {
            self.tone = o.tone;
            self.detail = o.detail;
            self.threshold = o.threshold;
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
