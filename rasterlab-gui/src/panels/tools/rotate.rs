use std::any::Any;

use egui::DragValue;
use rasterlab_core::ops::RotateOp;
use rasterlab_core::traits::operation::Operation;

use super::shared::straighten_crop_op;
use super::tool_trait::{Tool, ToolAction, ToolUiCtx};

pub struct RotateTool {
    pub deg: f32,
    pub crop: bool,
    pub preview_active: bool,
    pub flip_h_pending: bool,
    pub flip_v_pending: bool,
    pub flip_preview_active: bool,
}

impl RotateTool {
    pub fn new() -> Self {
        Self {
            deg: 0.0,
            crop: true,
            preview_active: false,
            flip_h_pending: false,
            flip_v_pending: false,
            flip_preview_active: false,
        }
    }
}

impl Tool for RotateTool {
    fn id(&self) -> &'static str {
        "rotate"
    }
    fn display_name(&self) -> &'static str {
        "↻  Rotate"
    }

    fn render_ui(&mut self, ui: &mut egui::Ui, ctx: &ToolUiCtx<'_>) -> ToolAction {
        let mut action = ToolAction::None;

        ui.horizontal(|ui| {
            for deg in [90.0_f32, 180.0, 270.0] {
                if ui
                    .add_enabled(ctx.has_image, egui::Button::new(format!("{deg}°")))
                    .clicked()
                {
                    self.deg = (self.deg + deg) % 360.0;
                    self.preview_active = true;
                    action = ToolAction::RequestRender;
                }
            }
        });
        ui.horizontal(|ui| {
            ui.label("Angle:");
            let changed = ui
                .add(
                    DragValue::new(&mut self.deg)
                        .speed(0.5)
                        .suffix("°")
                        .range(-360.0..=360.0),
                )
                .changed();
            if changed && ctx.has_image {
                self.preview_active = true;
                action = ToolAction::RequestRender;
            }
        });
        ui.horizontal(|ui| {
            let has_rotation = self.preview_active && (self.deg % 360.0).abs() > 0.001;
            if has_rotation
                && ui
                    .add_enabled(ctx.has_image, egui::Button::new("Apply"))
                    .clicked()
            {
                self.preview_active = false;
                let angle = self.deg;
                let is_right_angle = (angle % 90.0).abs() < 0.001;

                let rotate_op: Box<dyn Operation> = match angle as i32 % 360 {
                    90 | -270 => Box::new(RotateOp::cw90()),
                    180 | -180 => Box::new(RotateOp::cw180()),
                    270 | -90 => Box::new(RotateOp::cw270()),
                    _ => Box::new(RotateOp::arbitrary(angle)),
                };

                let crop_op = if self.crop && !is_right_angle {
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
                self.deg = 0.0;
            }
            if self.preview_active
                && ui
                    .add_enabled(ctx.has_image, egui::Button::new("Cancel"))
                    .clicked()
            {
                self.preview_active = false;
                self.deg = 0.0;
                action = ToolAction::RequestRender;
            }
            if ui.button("Reset").clicked() {
                self.deg = 0.0;
                if self.preview_active {
                    self.preview_active = false;
                    action = ToolAction::RequestRender;
                }
            }
        });
        ui.checkbox(&mut self.crop, "Crop to rectangle after apply");

        ui.horizontal(|ui| {
            let h_label = if self.flip_h_pending {
                "Flip H ✓"
            } else {
                "Flip H"
            };
            if ui
                .add_enabled(ctx.has_image, egui::Button::new(h_label))
                .clicked()
            {
                self.flip_h_pending = !self.flip_h_pending;
                self.flip_preview_active = self.flip_h_pending || self.flip_v_pending;
                action = ToolAction::RequestRender;
            }
            let v_label = if self.flip_v_pending {
                "Flip V ✓"
            } else {
                "Flip V"
            };
            if ui
                .add_enabled(ctx.has_image, egui::Button::new(v_label))
                .clicked()
            {
                self.flip_v_pending = !self.flip_v_pending;
                self.flip_preview_active = self.flip_h_pending || self.flip_v_pending;
                action = ToolAction::RequestRender;
            }
        });
        if self.flip_preview_active {
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(ctx.has_image, egui::Button::new("Apply"))
                    .clicked()
                {
                    let mut ops: Vec<Box<dyn Operation>> = Vec::new();
                    if self.flip_h_pending {
                        ops.push(Box::new(rasterlab_core::ops::FlipOp::horizontal()));
                    }
                    if self.flip_v_pending {
                        ops.push(Box::new(rasterlab_core::ops::FlipOp::vertical()));
                    }
                    self.flip_h_pending = false;
                    self.flip_v_pending = false;
                    self.flip_preview_active = false;
                    if !ops.is_empty() {
                        action = ToolAction::PushOps(ops);
                    }
                }
                if ui
                    .add_enabled(ctx.has_image, egui::Button::new("Cancel"))
                    .clicked()
                {
                    self.flip_h_pending = false;
                    self.flip_v_pending = false;
                    self.flip_preview_active = false;
                    action = ToolAction::RequestRender;
                }
            });
        }
        action
    }

    fn is_preview_active(&self) -> bool {
        self.preview_active || self.flip_preview_active
    }
    fn cancel_preview(&mut self) {
        self.preview_active = false;
        self.flip_preview_active = false;
        self.flip_h_pending = false;
        self.flip_v_pending = false;
        self.deg = 0.0;
    }
    fn activate_preview(&mut self) {
        self.preview_active = true;
    }
    fn preview_op(&self) -> Option<Box<dyn Operation>> {
        if self.preview_active && (self.deg % 360.0).abs() > 0.001 {
            Some(Box::new(RotateOp::arbitrary(self.deg)))
        } else if self.flip_preview_active {
            if self.flip_h_pending && self.flip_v_pending {
                None // handled as multi-op
            } else if self.flip_h_pending {
                Some(Box::new(rasterlab_core::ops::FlipOp::horizontal()))
            } else if self.flip_v_pending {
                Some(Box::new(rasterlab_core::ops::FlipOp::vertical()))
            } else {
                None
            }
        } else {
            None
        }
    }
    fn load_from_op(&mut self, op: &dyn Operation) -> bool {
        if let Some(o) = op.as_any().and_then(|a| a.downcast_ref::<RotateOp>()) {
            match o.mode {
                rasterlab_core::ops::RotateMode::Cw90 => self.deg = 90.0,
                rasterlab_core::ops::RotateMode::Cw180 => self.deg = 180.0,
                rasterlab_core::ops::RotateMode::Cw270 => self.deg = 270.0,
                rasterlab_core::ops::RotateMode::Arbitrary(d) => self.deg = d,
            }
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
