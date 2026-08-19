use std::any::Any;

use egui::Color32;
use rasterlab_core::ops::HdrMergeOp;
use rasterlab_core::traits::operation::Operation;

use super::shared::{MIN_STACK_FRAMES, StackFrame, frame_list_ui, frame_paths};
use super::tool_trait::{Tool, ToolAction, ToolUiCtx};
use crate::file_chooser::DialogKind;
use crate::state::EditingTool;

pub struct HdrMergeTool {
    pub frames: Vec<StackFrame>,
    pub preview_active: bool,
}

impl HdrMergeTool {
    pub fn new() -> Self {
        Self {
            frames: Vec::new(),
            preview_active: false,
        }
    }
}

impl Tool for HdrMergeTool {
    fn id(&self) -> &'static str {
        "hdr_merge"
    }
    fn display_name(&self) -> &'static str {
        "✺  HDR Merge"
    }
    fn editing_tool(&self) -> Option<EditingTool> {
        Some(EditingTool::HdrMerge)
    }

    fn render_ui(&mut self, ui: &mut egui::Ui, ctx: &ToolUiCtx<'_>) -> ToolAction {
        ui.label(
            egui::RichText::new("Fuse bracketed exposures into a single extended-range image")
                .small()
                .color(Color32::from_gray(140)),
        );
        ui.add_space(2.0);

        let mut action = ToolAction::None;
        if self.frames.is_empty() {
            ui.label(
                egui::RichText::new("No exposures added yet.")
                    .small()
                    .italics(),
            );
        } else {
            action = frame_list_ui(
                ui,
                &mut self.frames,
                &mut self.preview_active,
                "hdr_merge_list",
            );
        }

        ui.add_space(4.0);
        if ui
            .add_enabled(ctx.has_image, egui::Button::new("+ Add Exposure…"))
            .clicked()
        {
            if self.frames.is_empty()
                && let Some(p) = ctx.last_path
            {
                self.frames.push(StackFrame::new(p.to_string_lossy()));
            }
            return ToolAction::RequestFileDialog(DialogKind::HdrMergeAddImage);
        }

        ui.add_space(4.0);
        let button_action = super::shared::path_stack_buttons(
            ui,
            ctx.has_image,
            &mut self.frames,
            &mut self.preview_active,
            "Merge",
            |paths| Box::new(HdrMergeOp::new(paths)),
        );
        if !matches!(button_action, ToolAction::None) {
            action = button_action;
        }

        if self.frames.len() == 1 {
            ui.label(
                egui::RichText::new("Add at least one more bracket to merge.")
                    .small()
                    .color(egui::Color32::from_rgb(200, 150, 50)),
            );
        }
        action
    }

    super::shared::impl_preview_controls!();
    fn preview_op(&self) -> Option<Box<dyn Operation>> {
        if self.preview_active && self.frames.len() >= MIN_STACK_FRAMES {
            Some(Box::new(HdrMergeOp::new(frame_paths(&self.frames))))
        } else {
            None
        }
    }
    fn load_from_op(&mut self, op: &dyn Operation) -> bool {
        if let Some(o) = op.as_any().and_then(|a| a.downcast_ref::<HdrMergeOp>()) {
            self.frames = o
                .image_paths
                .iter()
                .map(|p| StackFrame::new(p.as_str()))
                .collect();
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
