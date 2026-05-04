use std::any::Any;

use egui::DragValue;
use rasterlab_core::ops::{CropOp, PerspectiveOp, auto_crop_rect};
use rasterlab_core::traits::operation::Operation;

use super::tool_trait::{Tool, ToolAction, ToolUiCtx};

pub struct PerspectiveTool {
    pub vertical: f32,
    pub horizontal: f32,
    pub scale: f32,
    pub grid_cols: u32,
    pub grid_rows: u32,
    pub crop: bool,
    pub preview_active: bool,
}

impl PerspectiveTool {
    pub fn new() -> Self {
        Self {
            vertical: 0.0,
            horizontal: 0.0,
            scale: 100.0,
            grid_cols: 3,
            grid_rows: 3,
            crop: true,
            preview_active: false,
        }
    }

    pub fn computed_corners(&self) -> [[f32; 2]; 4] {
        let v = self.vertical / 100.0 * 0.4;
        let h = self.horizontal / 100.0 * 0.4;
        let s = (self.scale / 100.0).max(0.01);
        let k = (1.0 - 1.0 / s) / 2.0;
        [[v + k, k], [-v - k, h + k], [-k, -h - k], [k, -k]]
    }
}

impl Tool for PerspectiveTool {
    fn id(&self) -> &'static str {
        "perspective"
    }
    fn display_name(&self) -> &'static str {
        "⬡  Perspective"
    }

    fn render_ui(&mut self, ui: &mut egui::Ui, ctx: &ToolUiCtx<'_>) -> ToolAction {
        let mut changed = false;

        changed |= ui
            .add(
                egui::Slider::new(&mut self.vertical, -100.0..=100.0)
                    .text("Vertical")
                    .step_by(0.5),
            )
            .changed();

        changed |= ui
            .add(
                egui::Slider::new(&mut self.horizontal, -100.0..=100.0)
                    .text("Horizontal")
                    .step_by(0.5),
            )
            .changed();

        changed |= ui
            .add(
                egui::Slider::new(&mut self.scale, 100.0..=150.0)
                    .text("Scale")
                    .step_by(0.1)
                    .suffix("%"),
            )
            .changed();

        if changed && ctx.has_image {
            self.preview_active = true;
            return ToolAction::RequestRender;
        }

        ui.horizontal(|ui| {
            ui.label("Grid");
            ui.add(
                DragValue::new(&mut self.grid_cols)
                    .speed(0.1)
                    .range(1..=24_u32)
                    .prefix("cols: "),
            );
            ui.add(
                DragValue::new(&mut self.grid_rows)
                    .speed(0.1)
                    .range(1..=24_u32)
                    .prefix("rows: "),
            );
        });

        let mut action = ToolAction::None;
        ui.horizontal(|ui| {
            if ui
                .add_enabled(ctx.has_image, egui::Button::new("Apply"))
                .clicked()
            {
                self.preview_active = false;
                let corners = self.computed_corners();

                let crop_op = if self.crop {
                    ctx.rendered_dims.and_then(|(rw, rh)| {
                        let w = (rw as f32 / ctx.rendered_scale).round() as u32;
                        let h = (rh as f32 / ctx.rendered_scale).round() as u32;
                        auto_crop_rect(corners, w, h)
                            .map(|[cx, cy, cw, ch]| CropOp::new(cx, cy, cw, ch))
                    })
                } else {
                    None
                };

                let persp_op: Box<dyn Operation> = Box::new(PerspectiveOp::new(corners));
                if let Some(crop) = crop_op {
                    action = ToolAction::PushOps(vec![persp_op, Box::new(crop)]);
                } else {
                    action = ToolAction::PushOp(persp_op);
                }
                self.vertical = 0.0;
                self.horizontal = 0.0;
                self.scale = 100.0;
            }
            if self.preview_active
                && ui
                    .add_enabled(ctx.has_image, egui::Button::new("Cancel"))
                    .clicked()
            {
                self.preview_active = false;
                self.vertical = 0.0;
                self.horizontal = 0.0;
                self.scale = 100.0;
                action = ToolAction::RequestRender;
            }
            if ui.button("Reset").clicked() {
                self.vertical = 0.0;
                self.horizontal = 0.0;
                self.scale = 100.0;
                if self.preview_active {
                    self.preview_active = false;
                    action = ToolAction::RequestRender;
                }
            }
        });
        ui.checkbox(&mut self.crop, "Crop to rectangle after apply");

        ui.label(
            egui::RichText::new("Use the grid to align straight lines in the image.")
                .small()
                .color(egui::Color32::from_gray(140)),
        );
        action
    }

    fn is_preview_active(&self) -> bool {
        self.preview_active
    }
    fn cancel_preview(&mut self) {
        self.preview_active = false;
        self.vertical = 0.0;
        self.horizontal = 0.0;
        self.scale = 100.0;
    }
    fn activate_preview(&mut self) {
        self.preview_active = true;
    }
    fn preview_op(&self) -> Option<Box<dyn Operation>> {
        if self.preview_active {
            Some(Box::new(PerspectiveOp::new(self.computed_corners())))
        } else {
            None
        }
    }
    fn load_from_op(&mut self, op: &dyn Operation) -> bool {
        op.as_any()
            .and_then(|a| a.downcast_ref::<PerspectiveOp>())
            .is_some()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
