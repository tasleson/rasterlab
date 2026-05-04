use std::any::Any;

use rasterlab_core::ops::HighlightsShadowsOp;
use rasterlab_core::traits::operation::Operation;

use super::tool_trait::{Tool, ToolAction, ToolUiCtx};
use crate::state::EditingTool;

pub struct HighlightsShadowsTool {
    pub highlights: f32,
    pub shadows: f32,
    pub preview_active: bool,
}

impl HighlightsShadowsTool {
    pub fn new() -> Self {
        Self {
            highlights: 0.0,
            shadows: 0.0,
            preview_active: false,
        }
    }
}

impl Tool for HighlightsShadowsTool {
    fn id(&self) -> &'static str {
        "highlights_shadows"
    }
    fn display_name(&self) -> &'static str {
        "◑  Highlights / Shadows"
    }
    fn editing_tool(&self) -> Option<EditingTool> {
        Some(EditingTool::HighlightsShadows)
    }

    fn render_ui(&mut self, ui: &mut egui::Ui, ctx: &ToolUiCtx<'_>) -> ToolAction {
        let mut changed = false;
        egui::Grid::new("hl_grid")
            .num_columns(2)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                ui.label("Highlights");
                changed |= ui
                    .add(egui::Slider::new(&mut self.highlights, -1.0..=1.0).step_by(0.01))
                    .changed();
                ui.end_row();
                ui.label("Shadows");
                changed |= ui
                    .add(egui::Slider::new(&mut self.shadows, -1.0..=1.0).step_by(0.01))
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
                action = ToolAction::PushOp(Box::new(HighlightsShadowsOp::new(
                    self.highlights,
                    self.shadows,
                )));
                self.highlights = 0.0;
                self.shadows = 0.0;
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
                self.highlights = 0.0;
                self.shadows = 0.0;
                if self.preview_active {
                    self.preview_active = false;
                    action = ToolAction::RequestRender;
                }
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
            Some(Box::new(HighlightsShadowsOp::new(
                self.highlights,
                self.shadows,
            )))
        } else {
            None
        }
    }
    fn load_from_op(&mut self, op: &dyn Operation) -> bool {
        if let Some(o) = op
            .as_any()
            .and_then(|a| a.downcast_ref::<HighlightsShadowsOp>())
        {
            self.highlights = o.highlights;
            self.shadows = o.shadows;
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
