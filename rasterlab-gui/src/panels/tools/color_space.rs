use std::any::Any;

use rasterlab_core::ops::{ColorSpaceConversion, ColorSpaceOp};
use rasterlab_core::traits::operation::Operation;

use super::tool_trait::{Tool, ToolAction, ToolUiCtx};

pub struct ColorSpaceTool {
    pub conversion: ColorSpaceConversion,
}

impl ColorSpaceTool {
    pub fn new() -> Self {
        Self {
            conversion: ColorSpaceConversion::SrgbToDisplayP3,
        }
    }
}

impl Tool for ColorSpaceTool {
    fn id(&self) -> &'static str {
        "color_space"
    }
    fn display_name(&self) -> &'static str {
        "⬛  Color Space"
    }

    fn render_ui(&mut self, ui: &mut egui::Ui, ctx: &ToolUiCtx<'_>) -> ToolAction {
        egui::ComboBox::from_id_salt("color_space_combo")
            .selected_text(match self.conversion {
                ColorSpaceConversion::SrgbToDisplayP3 => "sRGB → Display P3",
                ColorSpaceConversion::DisplayP3ToSrgb => "Display P3 → sRGB",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut self.conversion,
                    ColorSpaceConversion::SrgbToDisplayP3,
                    "sRGB → Display P3",
                );
                ui.selectable_value(
                    &mut self.conversion,
                    ColorSpaceConversion::DisplayP3ToSrgb,
                    "Display P3 → sRGB",
                );
            });
        if ui
            .add_enabled(ctx.has_image, egui::Button::new("Apply Conversion"))
            .clicked()
        {
            return ToolAction::PushOp(Box::new(ColorSpaceOp::new(self.conversion)));
        }
        ToolAction::None
    }

    fn load_from_op(&mut self, op: &dyn Operation) -> bool {
        if let Some(o) = op.as_any().and_then(|a| a.downcast_ref::<ColorSpaceOp>()) {
            self.conversion = o.conversion;
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
