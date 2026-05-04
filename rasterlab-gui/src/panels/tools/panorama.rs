use std::any::Any;

use rasterlab_core::ops::PanoramaOp;
use rasterlab_core::traits::operation::Operation;

use super::shared::path_list_ui;
use super::tool_trait::{Tool, ToolAction, ToolUiCtx};
use crate::file_chooser::DialogKind;

pub struct PanoramaTool {
    pub paths: Vec<String>,
    pub feather_px: u32,
    pub preview_active: bool,
}

impl PanoramaTool {
    pub fn new() -> Self {
        Self {
            paths: Vec::new(),
            feather_px: 80,
            preview_active: false,
        }
    }
}

impl Tool for PanoramaTool {
    fn id(&self) -> &'static str {
        "panorama"
    }
    fn display_name(&self) -> &'static str {
        "🌅  Panorama"
    }

    fn render_ui(&mut self, ui: &mut egui::Ui, ctx: &ToolUiCtx<'_>) -> ToolAction {
        if self.paths.is_empty() {
            ui.label(
                egui::RichText::new("No images added yet.")
                    .small()
                    .italics(),
            );
        } else if let Some(idx) = path_list_ui(ui, &self.paths, "panorama_list") {
            self.paths.remove(idx);
            if self.paths.len() < 2 && self.preview_active {
                self.preview_active = false;
                return ToolAction::RequestRender;
            }
        }

        ui.add_space(4.0);
        if ui
            .add_enabled(ctx.has_image, egui::Button::new("+ Add Image…"))
            .clicked()
        {
            if self.paths.is_empty()
                && let Some(p) = ctx.last_path
            {
                self.paths.push(p.to_string_lossy().into_owned());
            }
            return ToolAction::RequestFileDialog(DialogKind::PanoramaAddImage);
        }

        ui.add_space(4.0);
        egui::Grid::new("panorama_grid")
            .num_columns(2)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                ui.label("Feather:");
                let changed = ui
                    .add(egui::Slider::new(&mut self.feather_px, 1u32..=300).suffix(" px"))
                    .changed();
                ui.end_row();
                if changed && self.paths.len() >= 2 {
                    self.preview_active = true;
                }
            });

        let mut action = ToolAction::None;
        ui.horizontal(|ui| {
            let ready = self.paths.len() >= 2;
            if ui
                .add_enabled(ctx.has_image && ready, egui::Button::new("Stitch"))
                .clicked()
            {
                self.preview_active = false;
                action = ToolAction::PushOp(Box::new(PanoramaOp::new(
                    self.paths.clone(),
                    self.feather_px,
                )));
                self.paths.clear();
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
                self.paths.clear();
                self.feather_px = 80;
                if self.preview_active {
                    self.preview_active = false;
                    action = ToolAction::RequestRender;
                }
            }
        });

        if self.paths.len() == 1 {
            ui.label(
                egui::RichText::new("Add at least one more image to stitch.")
                    .small()
                    .color(egui::Color32::from_rgb(200, 150, 50)),
            );
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
        if self.preview_active && self.paths.len() >= 2 {
            Some(Box::new(PanoramaOp::new(
                self.paths.clone(),
                self.feather_px,
            )))
        } else {
            None
        }
    }
    fn load_from_op(&mut self, op: &dyn Operation) -> bool {
        if let Some(o) = op.as_any().and_then(|a| a.downcast_ref::<PanoramaOp>()) {
            self.paths = o.image_paths.clone();
            self.feather_px = o.feather_px;
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
