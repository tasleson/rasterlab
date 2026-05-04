use std::any::Any;

use rasterlab_core::ops::LutOp;
use rasterlab_core::traits::operation::Operation;

use super::tool_trait::{FileDialogKind, Tool, ToolAction, ToolUiCtx};

pub struct LutTool {
    pub lut_op: Option<LutOp>,
    pub strength: f32,
    pub name: String,
    pub preview_active: bool,
}

impl LutTool {
    pub fn new() -> Self {
        Self {
            lut_op: None,
            strength: 1.0,
            name: String::new(),
            preview_active: false,
        }
    }
}

impl Tool for LutTool {
    fn id(&self) -> &'static str {
        "lut"
    }
    fn display_name(&self) -> &'static str {
        "🎞  LUT / Color Grading"
    }
    fn editing_tool(&self) -> Option<crate::state::EditingTool> {
        None
    }

    fn render_ui(&mut self, ui: &mut egui::Ui, ctx: &ToolUiCtx<'_>) -> ToolAction {
        let mut action = ToolAction::None;

        if ui.button("Load .cube LUT…").clicked() {
            return ToolAction::RequestFileDialog(FileDialogKind::Lut);
        }
        if !self.name.is_empty() {
            ui.label(format!("Loaded: {}", self.name));
            let changed = ui
                .add(
                    egui::Slider::new(&mut self.strength, 0.0..=1.0)
                        .step_by(0.01)
                        .text("Strength"),
                )
                .changed();
            if changed && ctx.has_image {
                self.preview_active = true;
                return ToolAction::RequestRender;
            }
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(ctx.has_image, egui::Button::new("Apply LUT"))
                    .clicked()
                    && let Some(lut) = &self.lut_op
                {
                    self.preview_active = false;
                    let mut op = lut.clone();
                    op.strength = self.strength;
                    action = ToolAction::PushOp(Box::new(op));
                    self.lut_op = None;
                    self.name.clear();
                    self.strength = 1.0;
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
                    self.lut_op = None;
                    self.name.clear();
                    self.strength = 1.0;
                    if self.preview_active {
                        self.preview_active = false;
                        action = ToolAction::RequestRender;
                    }
                }
            });
        } else {
            ui.label("No LUT loaded.");
        }
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
            self.lut_op.as_ref().map(|lut| {
                let mut op = lut.clone();
                op.strength = self.strength;
                Box::new(op) as Box<dyn Operation>
            })
        } else {
            None
        }
    }
    fn load_from_op(&mut self, op: &dyn Operation) -> bool {
        if let Some(o) = op.as_any().and_then(|a| a.downcast_ref::<LutOp>()) {
            self.lut_op = Some(o.clone());
            self.strength = o.strength;
            self.name = "Loaded".to_string();
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
