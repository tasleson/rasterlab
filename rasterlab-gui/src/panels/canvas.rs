//! Central image viewer with zoom and pan.

use std::sync::Arc;

use egui::{Color32, ColorImage, Pos2, Rect, ScrollArea, TextureHandle, TextureOptions, Ui, Vec2};
use rasterlab_core::Image;

/// Persistent state for the canvas panel (zoom, pan, cached texture).
pub struct CanvasState {
    pub zoom:    f32,
    pub offset:  Vec2,
    texture:     Option<TextureHandle>,
    /// Hash of the last image data used to build the texture.
    last_hash:   u64,
}

impl Default for CanvasState {
    fn default() -> Self {
        Self { zoom: 1.0, offset: Vec2::ZERO, texture: None, last_hash: 0 }
    }
}

impl CanvasState {
    pub fn ui(&mut self, ui: &mut Ui, rendered: Option<&Arc<Image>>) {
        let Some(image) = rendered else {
            ui.centered_and_justified(|ui| {
                ui.label(
                    egui::RichText::new("Open an image to begin")
                        .size(22.0)
                        .color(Color32::from_gray(120)),
                );
            });
            return;
        };

        // Rebuild texture only when the image changes
        let new_hash = compute_hash(image);
        if self.texture.is_none() || new_hash != self.last_hash {
            self.texture = Some(ui.ctx().load_texture(
                "canvas_image",
                image_to_egui(image),
                TextureOptions::LINEAR,
            ));
            self.last_hash = new_hash;
            // Reset zoom/pan when a new image is loaded
            self.zoom   = fit_zoom(image.width, image.height, ui.available_size());
            self.offset = Vec2::ZERO;
        }

        let tex = self.texture.as_ref().unwrap();
        let display_size = Vec2::new(
            image.width  as f32 * self.zoom,
            image.height as f32 * self.zoom,
        );

        // Scrollable canvas area
        ScrollArea::both().show(ui, |ui| {
            let (resp, painter) = ui.allocate_painter(display_size, egui::Sense::drag());

            // Handle pan via drag
            if resp.dragged() {
                self.offset += resp.drag_delta();
            }

            let top_left = resp.rect.min + self.offset;
            painter.image(
                tex.id(),
                Rect::from_min_size(top_left, display_size),
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        });

        // Zoom controls at bottom-right
        ui.with_layout(egui::Layout::bottom_up(egui::Align::RIGHT), |ui| {
            ui.horizontal(|ui| {
                if ui.small_button("−").clicked() {
                    self.zoom = (self.zoom * 0.8).max(0.05);
                }
                ui.label(format!("{:.0}%", self.zoom * 100.0));
                if ui.small_button("+").clicked() {
                    self.zoom = (self.zoom * 1.25).min(32.0);
                }
                if ui.small_button("Fit").clicked() {
                    self.zoom = fit_zoom(image.width, image.height, ui.available_size());
                    self.offset = Vec2::ZERO;
                }
                if ui.small_button("1:1").clicked() {
                    self.zoom   = 1.0;
                    self.offset = Vec2::ZERO;
                }
            });
        });
    }
}

fn image_to_egui(image: &Image) -> ColorImage {
    let pixels: Vec<Color32> = image
        .data
        .chunks_exact(4)
        .map(|p| Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3]))
        .collect();
    ColorImage {
        size:   [image.width as usize, image.height as usize],
        pixels,
    }
}

fn fit_zoom(img_w: u32, img_h: u32, available: Vec2) -> f32 {
    let zoom_w = available.x / img_w as f32;
    let zoom_h = available.y / img_h as f32;
    zoom_w.min(zoom_h).max(0.05)
}

fn compute_hash(image: &Image) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    image.data.as_ptr().hash(&mut h);
    image.data.len().hash(&mut h);
    h.finish()
}
