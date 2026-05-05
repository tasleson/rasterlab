use std::any::Any;

use rasterlab_core::ops::ClarityTextureOp;
use rasterlab_core::traits::operation::Operation;

use super::tool_trait::{Tool, ToolAction, ToolUiCtx};
use crate::state::EditingTool;

pub struct ClarityTextureTool {
    pub clarity: f32,
    pub texture: f32,
    pub preview_active: bool,
}

impl ClarityTextureTool {
    pub fn new() -> Self {
        Self {
            clarity: 0.0,
            texture: 0.0,
            preview_active: false,
        }
    }
}

impl Tool for ClarityTextureTool {
    fn id(&self) -> &'static str {
        "clarity_texture"
    }
    fn display_name(&self) -> &'static str {
        "◈  Clarity / Texture"
    }
    fn editing_tool(&self) -> Option<EditingTool> {
        Some(EditingTool::ClarityTexture)
    }

    fn render_ui(&mut self, ui: &mut egui::Ui, ctx: &ToolUiCtx<'_>) -> ToolAction {
        let c_changed = ui
            .add(
                egui::Slider::new(&mut self.clarity, -1.0..=1.0)
                    .step_by(0.01)
                    .text("Clarity"),
            )
            .changed();
        let t_changed = ui
            .add(
                egui::Slider::new(&mut self.texture, -1.0..=1.0)
                    .step_by(0.01)
                    .text("Texture"),
            )
            .changed();
        let mut action = ToolAction::None;
        if (c_changed || t_changed) && ctx.has_image {
            self.preview_active = true;
            action = ToolAction::RequestRender;
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(ctx.has_image, egui::Button::new("Apply"))
                .clicked()
            {
                self.preview_active = false;
                action =
                    ToolAction::PushOp(Box::new(ClarityTextureOp::new(self.clarity, self.texture)));
                self.clarity = 0.0;
                self.texture = 0.0;
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
                self.clarity = 0.0;
                self.texture = 0.0;
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
            Some(Box::new(ClarityTextureOp::new(self.clarity, self.texture)))
        } else {
            None
        }
    }
    fn load_from_op(&mut self, op: &dyn Operation) -> bool {
        if let Some(o) = op
            .as_any()
            .and_then(|a| a.downcast_ref::<ClarityTextureOp>())
        {
            self.clarity = o.clarity;
            self.texture = o.texture;
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
