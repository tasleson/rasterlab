use std::any::Any;

use egui::{Color32, CornerRadius, Rect, Vec2};
use rasterlab_core::ops::{HistogramData, LevelsOp};
use rasterlab_core::traits::operation::Operation;

use super::tool_trait::{Tool, ToolAction, ToolUiCtx};
use crate::state::EditingTool;

pub struct LevelsTool {
    pub black: f32,
    pub mid: f32,
    pub white: f32,
    pub preview_active: bool,
}

impl LevelsTool {
    pub fn new() -> Self {
        Self {
            black: 0.0,
            mid: 1.0,
            white: 1.0,
            preview_active: false,
        }
    }
}

impl Tool for LevelsTool {
    fn id(&self) -> &'static str {
        "levels"
    }
    fn display_name(&self) -> &'static str {
        "▨  Levels"
    }
    fn editing_tool(&self) -> Option<EditingTool> {
        Some(EditingTool::Levels)
    }

    fn render_ui(&mut self, ui: &mut egui::Ui, ctx: &ToolUiCtx<'_>) -> ToolAction {
        draw_combined_histogram(ui, ctx.histogram, self.black, self.white, self.mid);

        ui.add_space(4.0);

        let mut changed = false;
        egui::Grid::new("levels_grid")
            .num_columns(2)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                ui.label("Black:");
                let r = ui.add(
                    egui::Slider::new(&mut self.black, 0.0..=1.0)
                        .clamping(egui::SliderClamping::Always)
                        .step_by(0.001),
                );
                if r.changed() {
                    if self.black >= self.white {
                        self.black = (self.white - 0.001).max(0.0);
                    }
                    changed = true;
                }
                ui.end_row();

                ui.label("Mid:");
                let r = ui.add(
                    egui::Slider::new(&mut self.mid, 0.10..=10.0)
                        .clamping(egui::SliderClamping::Always)
                        .step_by(0.01)
                        .logarithmic(true),
                );
                if r.changed() {
                    changed = true;
                }
                ui.end_row();

                ui.label("White:");
                let r = ui.add(
                    egui::Slider::new(&mut self.white, 0.0..=1.0)
                        .clamping(egui::SliderClamping::Always)
                        .step_by(0.001),
                );
                if r.changed() {
                    if self.white <= self.black {
                        self.white = (self.black + 0.001).min(1.0);
                    }
                    changed = true;
                }
                ui.end_row();
            });

        if changed && ctx.has_image {
            self.preview_active = true;
            return ToolAction::RequestRender;
        }

        ui.add_space(4.0);
        let mut action = ToolAction::None;
        ui.horizontal(|ui| {
            if ui
                .add_enabled(ctx.has_image, egui::Button::new("Apply Levels"))
                .clicked()
            {
                self.preview_active = false;
                action =
                    ToolAction::PushOp(Box::new(LevelsOp::new(self.black, self.white, self.mid)));
                self.black = 0.0;
                self.mid = 1.0;
                self.white = 1.0;
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
                self.black = 0.0;
                self.mid = 1.0;
                self.white = 1.0;
                if self.preview_active {
                    self.preview_active = false;
                    action = ToolAction::RequestRender;
                }
            }
        });
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
            Some(Box::new(LevelsOp::new(self.black, self.white, self.mid)))
        } else {
            None
        }
    }
    fn load_from_op(&mut self, op: &dyn Operation) -> bool {
        if let Some(o) = op.as_any().and_then(|a| a.downcast_ref::<LevelsOp>()) {
            self.black = o.black_point;
            self.mid = o.midtone;
            self.white = o.white_point;
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

fn draw_combined_histogram(
    ui: &mut egui::Ui,
    histogram: Option<&HistogramData>,
    black: f32,
    white: f32,
    _mid: f32,
) {
    const HEIGHT: f32 = 96.0;

    let width = ui.available_width().max(256.0);
    let (resp, painter) = ui.allocate_painter(Vec2::new(width, HEIGHT), egui::Sense::hover());
    let rect = resp.rect;

    painter.rect_filled(rect, CornerRadius::ZERO, Color32::from_gray(20));

    let Some(hist) = histogram else {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "No image",
            egui::FontId::monospace(11.0),
            Color32::from_gray(100),
        );
        return;
    };

    let peak = hist
        .red
        .iter()
        .chain(hist.green.iter())
        .chain(hist.blue.iter())
        .chain(hist.luma.iter())
        .copied()
        .max()
        .unwrap_or(1)
        .max(1) as f32;

    let bar_w = (width / 256.0).max(1.0);

    let channels: [(&[u64; 256], Color32); 4] = [
        (
            &hist.luma,
            Color32::from_rgba_unmultiplied(200, 200, 200, 80),
        ),
        (&hist.red, Color32::from_rgba_unmultiplied(220, 60, 60, 120)),
        (
            &hist.green,
            Color32::from_rgba_unmultiplied(60, 180, 60, 120),
        ),
        (
            &hist.blue,
            Color32::from_rgba_unmultiplied(60, 80, 220, 120),
        ),
    ];

    for (data, color) in &channels {
        for (i, &count) in data.iter().enumerate() {
            if count == 0 {
                continue;
            }
            let bar_h = (count as f32 / peak) * HEIGHT;
            let x = rect.left() + i as f32 * bar_w;
            painter.rect_filled(
                Rect::from_min_size(
                    egui::pos2(x, rect.bottom() - bar_h),
                    Vec2::new(bar_w.max(0.5), bar_h),
                ),
                CornerRadius::ZERO,
                *color,
            );
        }
    }

    // Black-point marker
    let bx = rect.left() + black * width;
    painter.line_segment(
        [egui::pos2(bx, rect.top()), egui::pos2(bx, rect.bottom())],
        egui::Stroke::new(1.5, Color32::from_gray(60)),
    );
    let tp = egui::pos2(bx, rect.bottom());
    painter.add(egui::Shape::convex_polygon(
        vec![
            tp,
            egui::pos2(tp.x - 5.0, tp.y + 7.0),
            egui::pos2(tp.x + 5.0, tp.y + 7.0),
        ],
        Color32::from_gray(60),
        egui::Stroke::NONE,
    ));

    // White-point marker
    let wx = rect.left() + white * width;
    painter.line_segment(
        [egui::pos2(wx, rect.top()), egui::pos2(wx, rect.bottom())],
        egui::Stroke::new(1.5, Color32::from_gray(220)),
    );
    let tp = egui::pos2(wx, rect.bottom());
    painter.add(egui::Shape::convex_polygon(
        vec![
            tp,
            egui::pos2(tp.x - 5.0, tp.y + 7.0),
            egui::pos2(tp.x + 5.0, tp.y + 7.0),
        ],
        Color32::from_gray(220),
        egui::Stroke::NONE,
    ));

    // Midtone marker
    let mid_frac = black + (white - black) * 0.5;
    let mx = rect.left() + mid_frac * width;
    painter.line_segment(
        [egui::pos2(mx, rect.top()), egui::pos2(mx, rect.bottom())],
        egui::Stroke::new(1.5, Color32::from_rgba_unmultiplied(180, 140, 60, 200)),
    );
    let tp = egui::pos2(mx, rect.bottom());
    painter.add(egui::Shape::convex_polygon(
        vec![
            tp,
            egui::pos2(tp.x - 5.0, tp.y + 7.0),
            egui::pos2(tp.x + 5.0, tp.y + 7.0),
        ],
        Color32::from_rgba_unmultiplied(180, 140, 60, 200),
        egui::Stroke::NONE,
    ));

    // Hover tooltip
    if let Some(pos) = resp.hover_pos() {
        let bucket = ((pos.x - rect.left()) / bar_w).clamp(0.0, 255.0) as usize;
        let text = format!(
            "{}  R:{} G:{} B:{} L:{}",
            bucket, hist.red[bucket], hist.green[bucket], hist.blue[bucket], hist.luma[bucket],
        );
        painter.text(
            egui::pos2(pos.x.min(rect.right() - 10.0), rect.top() + 12.0),
            egui::Align2::LEFT_CENTER,
            text,
            egui::FontId::monospace(10.0),
            Color32::WHITE,
        );
    }
}
