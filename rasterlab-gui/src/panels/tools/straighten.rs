use std::any::Any;

use egui::Vec2;
use rasterlab_core::ops::RotateOp;
use rasterlab_core::traits::operation::Operation;

use super::shared::straighten_crop_op;
use super::tool_trait::{Tool, ToolAction, ToolUiCtx};

pub struct StraightenTool {
    pub angle: f32,
    pub active: bool,
    pub crop: bool,
    pub preview_active: bool,
}

impl StraightenTool {
    pub fn new() -> Self {
        Self {
            angle: 0.0,
            active: false,
            crop: true,
            preview_active: false,
        }
    }
}

impl Tool for StraightenTool {
    fn id(&self) -> &'static str {
        "straighten"
    }
    fn display_name(&self) -> &'static str {
        "⟳  Straighten"
    }

    fn render_ui(&mut self, ui: &mut egui::Ui, ctx: &ToolUiCtx<'_>) -> ToolAction {
        let changed = ui
            .add(
                egui::Slider::new(&mut self.angle, -45.0..=45.0)
                    .step_by(0.1)
                    .text("Angle")
                    .suffix("°"),
            )
            .changed();
        if changed && ctx.has_image {
            self.preview_active = true;
        }

        ui.checkbox(&mut self.crop, "Crop to rectangle after apply");

        let toggle_text = if self.active {
            "Hide Horizon Line"
        } else {
            "Show Horizon Line"
        };
        if ui
            .add_enabled(
                ctx.has_image,
                egui::Button::new(toggle_text).min_size(Vec2::new(ui.available_width(), 0.0)),
            )
            .clicked()
        {
            self.active = !self.active;
        }

        let mut action = if changed && ctx.has_image {
            ToolAction::RequestRender
        } else {
            ToolAction::None
        };
        ui.horizontal(|ui| {
            if ui
                .add_enabled(ctx.has_image, egui::Button::new("Apply Straighten"))
                .clicked()
            {
                self.preview_active = false;
                let angle = self.angle;
                let rotate_op: Box<dyn Operation> = Box::new(RotateOp::arbitrary(angle));

                let crop_op = if self.crop {
                    ctx.rendered_dims.map(|(rw, rh)| {
                        let w = (rw as f32 / ctx.rendered_scale).round() as u32;
                        let h = (rh as f32 / ctx.rendered_scale).round() as u32;
                        straighten_crop_op(w, h, angle)
                    })
                } else {
                    None
                };

                if let Some(crop) = crop_op {
                    action = ToolAction::PushOps(vec![rotate_op, Box::new(crop)]);
                } else {
                    action = ToolAction::PushOp(rotate_op);
                }
                self.angle = 0.0;
                self.active = false;
            }
            if self.preview_active
                && ui
                    .add_enabled(ctx.has_image, egui::Button::new("Cancel"))
                    .clicked()
            {
                self.preview_active = false;
                self.angle = 0.0;
                action = ToolAction::RequestRender;
            }
            if ui.button("Reset").clicked() {
                self.angle = 0.0;
                self.active = false;
                if self.preview_active {
                    self.preview_active = false;
                    action = ToolAction::RequestRender;
                }
            }
        });

        if self.active {
            ui.label(
                egui::RichText::new(
                    "Drag the horizon line to match a level reference in the image.",
                )
                .small()
                .color(egui::Color32::from_gray(140)),
            );
        }
        action
    }

    fn is_preview_active(&self) -> bool {
        self.preview_active
    }
    fn cancel_preview(&mut self) {
        self.preview_active = false;
        self.angle = 0.0;
    }
    fn activate_preview(&mut self) {
        self.preview_active = true;
    }
    fn preview_op(&self) -> Option<Box<dyn Operation>> {
        if self.preview_active && self.angle.abs() > 0.001 {
            Some(Box::new(RotateOp::arbitrary(self.angle)))
        } else {
            None
        }
    }
    fn load_from_op(&mut self, op: &dyn Operation) -> bool {
        if let Some(o) = op.as_any().and_then(|a| a.downcast_ref::<RotateOp>())
            && let rasterlab_core::ops::RotateMode::Arbitrary(d) = o.mode
        {
            self.angle = d;
            return true;
        }
        false
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
