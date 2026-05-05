use std::any::Any;

use egui::Color32;
use rasterlab_core::ops::FocusStackOp;
use rasterlab_core::traits::operation::Operation;

use super::shared::path_list_ui;
use super::tool_trait::{Tool, ToolAction, ToolUiCtx};
use crate::file_chooser::DialogKind;

pub struct FocusStackTool {
    pub paths: Vec<String>,
    pub preview_active: bool,
}

impl FocusStackTool {
    pub fn new() -> Self {
        Self {
            paths: Vec::new(),
            preview_active: false,
        }
    }
}

impl Tool for FocusStackTool {
    fn id(&self) -> &'static str {
        "focus_stack"
    }
    fn display_name(&self) -> &'static str {
        "🎯  Focus Stack"
    }

    fn render_ui(&mut self, ui: &mut egui::Ui, ctx: &ToolUiCtx<'_>) -> ToolAction {
        ui.label(
            egui::RichText::new("Fuse multiple frames at different focus distances")
                .small()
                .color(Color32::from_gray(140)),
        );
        ui.add_space(2.0);

        let mut action = ToolAction::None;
        if self.paths.is_empty() {
            ui.label(
                egui::RichText::new("No frames added yet.")
                    .small()
                    .italics(),
            );
        } else if let Some(idx) = path_list_ui(ui, &self.paths, "focus_stack_list") {
            self.paths.remove(idx);
            if self.paths.len() < 2 && self.preview_active {
                self.preview_active = false;
                action = ToolAction::RequestRender;
            }
        }

        ui.add_space(4.0);
        if ui
            .add_enabled(ctx.has_image, egui::Button::new("+ Add Frame…"))
            .clicked()
        {
            if self.paths.is_empty()
                && let Some(p) = ctx.last_path
            {
                self.paths.push(p.to_string_lossy().into_owned());
            }
            return ToolAction::RequestFileDialog(DialogKind::FocusStackAddImage);
        }

        ui.add_space(4.0);
        let button_action = super::shared::path_stack_buttons(
            ui,
            ctx.has_image,
            &mut self.paths,
            &mut self.preview_active,
            "Stack",
            |paths| Box::new(FocusStackOp::new(paths)),
        );
        if !matches!(button_action, ToolAction::None) {
            action = button_action;
        }

        if self.paths.len() == 1 {
            ui.label(
                egui::RichText::new("Add at least one more frame to fuse.")
                    .small()
                    .color(egui::Color32::from_rgb(200, 150, 50)),
            );
        }
        action
    }

    super::shared::impl_preview_controls!();
    fn preview_op(&self) -> Option<Box<dyn Operation>> {
        if self.preview_active && self.paths.len() >= 2 {
            Some(Box::new(FocusStackOp::new(self.paths.clone())))
        } else {
            None
        }
    }
    fn load_from_op(&mut self, op: &dyn Operation) -> bool {
        if let Some(o) = op.as_any().and_then(|a| a.downcast_ref::<FocusStackOp>()) {
            self.paths = o.image_paths.clone();
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
