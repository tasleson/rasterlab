use std::any::Any;

use egui::{Color32, CornerRadius, Pos2, Stroke, Vec2};
use rasterlab_core::ops::CurvesOp;
use rasterlab_core::traits::operation::Operation;

use super::tool_trait::{Tool, ToolAction, ToolUiCtx};
use crate::state::EditingTool;

pub struct CurvesTool {
    pub points: Vec<[f32; 2]>,
    pub preview_active: bool,
    pub dragging_idx: Option<usize>,
}

impl CurvesTool {
    pub fn new() -> Self {
        Self {
            points: vec![[0.0, 0.0], [1.0, 1.0]],
            preview_active: false,
            dragging_idx: None,
        }
    }
}

impl Tool for CurvesTool {
    fn id(&self) -> &'static str {
        "curves"
    }
    fn display_name(&self) -> &'static str {
        "〜  Curves"
    }
    fn editing_tool(&self) -> Option<EditingTool> {
        Some(EditingTool::Curves)
    }

    fn render_ui(&mut self, ui: &mut egui::Ui, ctx: &ToolUiCtx<'_>) -> ToolAction {
        let mut action = ToolAction::None;

        let size = ui.available_width().min(200.0);
        let (resp, painter) = ui.allocate_painter(Vec2::splat(size), egui::Sense::click_and_drag());
        let rect = resp.rect;
        let w = rect.width();
        let h = rect.height();

        // Background and grid.
        painter.rect_filled(rect, CornerRadius::ZERO, Color32::from_gray(25));
        for i in 1..4 {
            let t = i as f32 / 4.0;
            let gx = rect.min.x + t * w;
            let gy = rect.min.y + t * h;
            let grid_col = Color32::from_gray(50);
            painter.line_segment(
                [Pos2::new(gx, rect.min.y), Pos2::new(gx, rect.max.y)],
                Stroke::new(1.0, grid_col),
            );
            painter.line_segment(
                [Pos2::new(rect.min.x, gy), Pos2::new(rect.max.x, gy)],
                Stroke::new(1.0, grid_col),
            );
        }
        // Identity diagonal.
        painter.line_segment(
            [
                Pos2::new(rect.min.x, rect.max.y),
                Pos2::new(rect.max.x, rect.min.y),
            ],
            Stroke::new(1.0, Color32::from_gray(60)),
        );

        // Build and draw the curve.
        let lut = CurvesOp::build_lut(&self.points);
        {
            let mut prev: Option<Pos2> = None;
            for (i, &y_val) in lut.iter().enumerate() {
                let cx = rect.min.x + (i as f32 / 255.0) * w;
                let cy = rect.max.y - (y_val as f32 / 255.0) * h;
                let pos = Pos2::new(cx, cy);
                if let Some(p) = prev {
                    painter.line_segment([p, pos], Stroke::new(1.5, Color32::WHITE));
                }
                prev = Some(pos);
            }
        }

        // Draw control point handles.
        const PT_R: f32 = 5.0;
        for (i, &[px, py]) in self.points.iter().enumerate() {
            let sx = rect.min.x + px * w;
            let sy = rect.max.y - py * h;
            let col = if self.dragging_idx == Some(i) {
                Color32::from_rgb(255, 200, 0)
            } else {
                Color32::WHITE
            };
            painter.circle_filled(Pos2::new(sx, sy), PT_R, col);
            painter.circle_stroke(Pos2::new(sx, sy), PT_R, Stroke::new(1.0, Color32::BLACK));
        }

        // Interaction.
        let (mouse_pos, primary_down, primary_pressed, secondary_pressed) = ui.input(|i| {
            (
                i.pointer.interact_pos(),
                i.pointer.button_down(egui::PointerButton::Primary),
                i.pointer.button_pressed(egui::PointerButton::Primary),
                i.pointer.button_pressed(egui::PointerButton::Secondary),
            )
        });

        if !primary_down {
            self.dragging_idx = None;
        }

        let mut curve_changed = false;

        if let Some(pos) = mouse_pos {
            let cx = ((pos.x - rect.min.x) / w).clamp(0.0, 1.0);
            let cy = (1.0 - (pos.y - rect.min.y) / h).clamp(0.0, 1.0);

            if primary_down && let Some(drag_idx) = self.dragging_idx {
                let npts = self.points.len();
                let new_x = if drag_idx == 0 {
                    0.0
                } else if drag_idx == npts - 1 {
                    1.0
                } else {
                    let lo = self.points[drag_idx - 1][0] + 0.005;
                    let hi = self.points[drag_idx + 1][0] - 0.005;
                    cx.clamp(lo, hi)
                };
                let old = self.points[drag_idx];
                self.points[drag_idx] = [new_x, cy];
                if self.points[drag_idx] != old {
                    curve_changed = true;
                }
            }

            if primary_pressed && rect.contains(pos) {
                let hit = self.points.iter().position(|&[px, py]| {
                    let sx = rect.min.x + px * w;
                    let sy = rect.max.y - py * h;
                    ((pos.x - sx).powi(2) + (pos.y - sy).powi(2)).sqrt() < PT_R + 3.0
                });
                if let Some(idx) = hit {
                    self.dragging_idx = Some(idx);
                } else {
                    self.points.push([cx, cy]);
                    self.points.sort_by(|a, b| a[0].partial_cmp(&b[0]).unwrap());
                    curve_changed = true;
                }
            }

            if secondary_pressed && rect.contains(pos) {
                let hit = self.points[1..self.points.len() - 1]
                    .iter()
                    .enumerate()
                    .find(|(_, pt)| {
                        let sx = rect.min.x + pt[0] * w;
                        let sy = rect.max.y - pt[1] * h;
                        ((pos.x - sx).powi(2) + (pos.y - sy).powi(2)).sqrt() < PT_R + 4.0
                    })
                    .map(|(i, _)| i + 1);
                if let Some(idx) = hit {
                    self.points.remove(idx);
                    curve_changed = true;
                }
            }
        }

        if curve_changed && ctx.has_image {
            self.preview_active = true;
            action = ToolAction::RequestRender;
        }

        ui.add_space(2.0);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(ctx.has_image, egui::Button::new("Apply Curve"))
                .clicked()
            {
                self.preview_active = false;
                action = ToolAction::PushOp(Box::new(CurvesOp {
                    points: self.points.clone(),
                }));
                self.points = vec![[0.0, 0.0], [1.0, 1.0]];
                self.dragging_idx = None;
            }
            if self.preview_active
                && ui
                    .add_enabled(ctx.has_image, egui::Button::new("Cancel"))
                    .clicked()
            {
                self.preview_active = false;
                self.points = vec![[0.0, 0.0], [1.0, 1.0]];
                self.dragging_idx = None;
                action = ToolAction::RequestRender;
            }
            if ui.button("Reset").clicked() {
                self.points = vec![[0.0, 0.0], [1.0, 1.0]];
                self.dragging_idx = None;
                if self.preview_active {
                    self.preview_active = false;
                    action = ToolAction::RequestRender;
                }
            }
        });
        action
    }

    super::shared::impl_preview_controls!();
    fn preview_op(&self) -> Option<Box<dyn Operation>> {
        if self.preview_active {
            Some(Box::new(CurvesOp {
                points: self.points.clone(),
            }))
        } else {
            None
        }
    }
    fn load_from_op(&mut self, op: &dyn Operation) -> bool {
        if let Some(o) = op.as_any().and_then(|a| a.downcast_ref::<CurvesOp>()) {
            self.points = o.points.clone();
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
