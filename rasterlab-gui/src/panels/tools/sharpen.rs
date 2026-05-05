use rasterlab_core::ops::SharpenOp;
use rasterlab_core::traits::operation::Operation;

use super::shared::{PreviewButtonAction, preview_buttons};
use super::tool_trait::{Tool, ToolAction, ToolUiCtx};
use crate::state::EditingTool;

pub struct SharpenTool {
    pub strength: f32,
    pub preview_active: bool,
}

impl SharpenTool {
    pub fn new() -> Self {
        Self {
            strength: 1.0,
            preview_active: false,
        }
    }
}

impl Tool for SharpenTool {
    fn id(&self) -> &'static str {
        "sharpen"
    }
    fn display_name(&self) -> &'static str {
        "◈  Sharpen"
    }
    fn editing_tool(&self) -> Option<EditingTool> {
        Some(EditingTool::Sharpen)
    }

    fn render_ui(&mut self, ui: &mut egui::Ui, ctx: &ToolUiCtx<'_>) -> ToolAction {
        let changed = ui
            .add(
                egui::Slider::new(&mut self.strength, 0.0..=10.0)
                    .step_by(0.05)
                    .text("Strength"),
            )
            .changed();
        let mut action = ToolAction::None;
        if changed && ctx.has_image {
            self.preview_active = true;
            action = ToolAction::RequestRender;
        }
        if let Some(button_action) =
            preview_buttons(ui, ctx.has_image, &mut self.preview_active, "Apply Sharpen")
        {
            match button_action {
                PreviewButtonAction::Apply => {
                    action = ToolAction::PushOp(Box::new(SharpenOp::new(self.strength)));
                    self.strength = 1.0;
                }
                PreviewButtonAction::Cancel => action = ToolAction::RequestRender,
                PreviewButtonAction::Reset { request_render } => {
                    self.strength = 1.0;
                    if request_render {
                        action = ToolAction::RequestRender;
                    }
                }
            }
        }
        action
    }

    super::shared::impl_preview_tool!(tool => SharpenOp::new(tool.strength));

    fn load_from_op(&mut self, op: &dyn Operation) -> bool {
        if let Some(o) = op.as_any().and_then(|a| a.downcast_ref::<SharpenOp>()) {
            self.strength = o.strength;
            true
        } else {
            false
        }
    }
}
