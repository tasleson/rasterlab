//! Histogram panel — renders per-channel bar charts using egui Painter.

use egui::{Color32, Rect, Rounding, Ui, Vec2};

use rasterlab_core::ops::HistogramData;

const CHANNEL_HEIGHT: f32 = 80.0;
const BAR_GAP: f32        = 0.5;

pub fn ui(ui: &mut Ui, hist: Option<&HistogramData>) {
    ui.heading("Histogram");
    ui.separator();

    let Some(hist) = hist else {
        ui.label("No histogram data");
        return;
    };

    let width = ui.available_width().max(256.0);

    draw_channel(ui, &hist.red,   Color32::from_rgba_unmultiplied(220,  60,  60, 200), width, "R");
    draw_channel(ui, &hist.green, Color32::from_rgba_unmultiplied( 60, 180,  60, 200), width, "G");
    draw_channel(ui, &hist.blue,  Color32::from_rgba_unmultiplied( 60,  80, 220, 200), width, "B");
    draw_channel(ui, &hist.luma,  Color32::from_rgba_unmultiplied(200, 200, 200, 200), width, "L");
}

fn draw_channel(ui: &mut Ui, data: &[u64; 256], color: Color32, width: f32, label: &str) {
    let peak = data.iter().copied().max().unwrap_or(1).max(1) as f32;

    ui.label(label);
    let (resp, painter) = ui.allocate_painter(
        Vec2::new(width, CHANNEL_HEIGHT),
        egui::Sense::hover(),
    );

    let rect   = resp.rect;
    let bar_w  = (width / 256.0).max(1.0);

    // Dark background
    painter.rect_filled(rect, Rounding::ZERO, Color32::from_gray(20));

    for (i, &count) in data.iter().enumerate() {
        let bar_height = (count as f32 / peak) * CHANNEL_HEIGHT;
        let x = rect.left() + i as f32 * bar_w;
        let bar_rect = Rect::from_min_size(
            egui::pos2(x + BAR_GAP, rect.bottom() - bar_height),
            Vec2::new((bar_w - BAR_GAP).max(0.5), bar_height),
        );
        painter.rect_filled(bar_rect, Rounding::ZERO, color);
    }

    // Hover tooltip: show bucket value under cursor
    if let Some(hover_pos) = resp.hover_pos() {
        let bucket = ((hover_pos.x - rect.left()) / bar_w) as usize;
        if bucket < 256 {
            let text = format!("Value {}: {} px", bucket, data[bucket]);
            painter.text(
                hover_pos,
                egui::Align2::LEFT_BOTTOM,
                text,
                egui::FontId::monospace(11.0),
                Color32::WHITE,
            );
        }
    }

    ui.add_space(4.0);
}
