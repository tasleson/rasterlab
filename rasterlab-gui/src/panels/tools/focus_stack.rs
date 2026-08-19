use std::any::Any;

use egui::Color32;
use rasterlab_core::ops::{FocusStackOp, FrameAlignment};
use rasterlab_core::traits::operation::Operation;

use super::shared::{MIN_STACK_FRAMES, StackFrame, frame_list_ui, frame_paths};
use super::tool_trait::{Tool, ToolAction, ToolUiCtx};
use crate::file_chooser::DialogKind;
use crate::state::EditingTool;

pub struct FocusStackTool {
    pub frames: Vec<StackFrame>,
    /// Whether to fit out the magnification difference between frames.
    pub correct_breathing: bool,
    pub preview_active: bool,
}

impl FocusStackTool {
    pub fn new() -> Self {
        Self {
            frames: Vec::new(),
            correct_breathing: true,
            preview_active: false,
        }
    }

    fn alignment(&self) -> FrameAlignment {
        if self.correct_breathing {
            FrameAlignment::Similarity
        } else {
            FrameAlignment::None
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
    fn editing_tool(&self) -> Option<EditingTool> {
        Some(EditingTool::FocusStack)
    }

    fn render_ui(&mut self, ui: &mut egui::Ui, ctx: &ToolUiCtx<'_>) -> ToolAction {
        ui.label(
            egui::RichText::new("Fuse multiple frames at different focus distances")
                .small()
                .color(Color32::from_gray(140)),
        );
        ui.add_space(2.0);

        let mut action = ToolAction::None;
        if self.frames.is_empty() {
            ui.label(
                egui::RichText::new("No frames added yet.")
                    .small()
                    .italics(),
            );
        } else {
            action = frame_list_ui(
                ui,
                &mut self.frames,
                &mut self.preview_active,
                "focus_stack_list",
            );
        }

        ui.add_space(4.0);
        if ui
            .add_enabled(ctx.has_image, egui::Button::new("+ Add Frame…"))
            .clicked()
        {
            if self.frames.is_empty()
                && let Some(p) = ctx.last_path
            {
                self.frames.push(StackFrame::new(p.to_string_lossy()));
            }
            return ToolAction::RequestFileDialog(DialogKind::FocusStackAddImage);
        }

        ui.add_space(4.0);
        if ui
            .checkbox(&mut self.correct_breathing, "Correct focus breathing")
            .on_hover_text(
                "Racking focus also changes the lens's magnification, so the frames do not \
                 overlap pixel for pixel.  Fits a scale, rotation and shift for each frame \
                 against the first one before fusing.",
            )
            .changed()
            && self.preview_active
        {
            action = ToolAction::RequestRender;
        }

        ui.add_space(4.0);
        let alignment = self.alignment();
        let button_action = super::shared::path_stack_buttons(
            ui,
            ctx.has_image,
            &mut self.frames,
            &mut self.preview_active,
            "Stack",
            |paths| Box::new(FocusStackOp::with_alignment(paths, alignment)),
        );
        if !matches!(button_action, ToolAction::None) {
            action = button_action;
        }

        if self.frames.len() == 1 {
            ui.label(
                egui::RichText::new("Add at least one more frame to fuse.")
                    .small()
                    .color(egui::Color32::from_rgb(200, 150, 50)),
            );
        } else if self.frames.len() > 1 && !self.preview_active {
            // Reached by way of the library's Focus Stack action, which fills
            // the frame list without starting a preview.
            ui.label(
                egui::RichText::new(format!("{} frames ready — press Stack.", self.frames.len()))
                    .small()
                    .color(Color32::from_gray(140)),
            );
        }
        action
    }

    super::shared::impl_preview_controls!();
    fn preview_op(&self) -> Option<Box<dyn Operation>> {
        if self.preview_active && self.frames.len() >= MIN_STACK_FRAMES {
            Some(Box::new(FocusStackOp::with_alignment(
                frame_paths(&self.frames),
                self.alignment(),
            )))
        } else {
            None
        }
    }
    fn load_from_op(&mut self, op: &dyn Operation) -> bool {
        if let Some(o) = op.as_any().and_then(|a| a.downcast_ref::<FocusStackOp>()) {
            self.frames = o
                .image_paths
                .iter()
                .map(|p| StackFrame::new(p.as_str()))
                .collect();
            self.correct_breathing = o.alignment != FrameAlignment::None;
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
