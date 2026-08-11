//! Spot-heal placement: cursor ring, spot hit-testing, dragging source/dest
//! circles, and the arrow overlay linking them.

use egui::{Color32, Pos2, Ui};

use crate::panels::tools::heal::HealTool;
use crate::state::AppState;

use super::coords::{image_to_screen, screen_to_image};
use super::{CanvasState, CanvasView};

impl CanvasState {
    /// Place, drag, and remove heal spots, then draw them.
    pub(super) fn handle_heal(
        &mut self,
        ui: &mut Ui,
        painter: &egui::Painter,
        state: &mut AppState,
        view: &CanvasView,
    ) {
        let CanvasView {
            image_tl,
            over_canvas,
            ..
        } = *view;

        // Clear crop selection while heal mode is active.
        self.crop_start = None;
        self.crop_end = None;

        let (ptr_pos, primary_pressed, primary_released, primary_down, secondary_clicked) = ui
            .input(|i| {
                (
                    i.pointer.hover_pos(),
                    i.pointer.button_pressed(egui::PointerButton::Primary),
                    i.pointer.button_released(egui::PointerButton::Primary),
                    i.pointer.button_down(egui::PointerButton::Primary),
                    i.pointer.button_clicked(egui::PointerButton::Secondary),
                )
            });

        if over_canvas {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        }

        let heal_radius = state.tools.find::<HealTool>().unwrap().radius;

        // Draw cursor ring at hover position
        if let Some(ptr) = ptr_pos
            && over_canvas
        {
            let r_screen = heal_radius as f32 * self.zoom;
            painter.circle_stroke(
                ptr,
                r_screen,
                egui::Stroke::new(1.5_f32, Color32::from_white_alpha(200)),
            );
            painter.circle_stroke(
                ptr,
                r_screen,
                egui::Stroke::new(0.5_f32, Color32::from_black_alpha(120)),
            );
        }

        // Hit-test existing spots for drag / remove
        let hit_spot = ptr_pos.and_then(|ptr| {
            let img_pos = screen_to_image(ptr, image_tl, self.zoom);
            let handle_r_img = (8.0 / self.zoom).max(heal_radius as f32 * 0.4);
            state
                .tools
                .find::<HealTool>()
                .unwrap()
                .spots
                .iter()
                .enumerate()
                .find_map(|(i, spot)| {
                    let dst = Pos2::new(spot.dest_x as f32, spot.dest_y as f32);
                    let src = Pos2::new(spot.src_x as f32, spot.src_y as f32);
                    if (img_pos - dst).length() < handle_r_img {
                        Some((i, false))
                    } else if (img_pos - src).length() < handle_r_img {
                        Some((i, true))
                    } else {
                        None
                    }
                })
        });

        // Start drag on mouse-down; hitting an existing spot begins a drag
        if primary_pressed && let Some((idx, is_src)) = hit_spot {
            self.heal_dragging = Some((idx, is_src));
        }

        // Continue drag while held
        if primary_down && let (Some((idx, is_src)), Some(ptr)) = (self.heal_dragging, ptr_pos) {
            let img_pos = screen_to_image(ptr, image_tl, self.zoom);
            if let Some(spot) = state
                .tools
                .find_mut::<HealTool>()
                .unwrap()
                .spots
                .get_mut(idx)
            {
                if is_src {
                    spot.src_x = img_pos.x as i32;
                    spot.src_y = img_pos.y as i32;
                } else {
                    let dx = img_pos.x as i32 - spot.dest_x;
                    let dy = img_pos.y as i32 - spot.dest_y;
                    spot.dest_x = img_pos.x as i32;
                    spot.dest_y = img_pos.y as i32;
                    spot.src_x += dx;
                    spot.src_y += dy;
                }
            }
        }

        // On release: place a new spot only if this press was not a drag
        if primary_released {
            if self.heal_dragging.is_none()
                && hit_spot.is_none()
                && let Some(ptr) = ptr_pos
                && over_canvas
            {
                let img_pos = screen_to_image(ptr, image_tl, self.zoom);
                state.heal_place_spot(img_pos.x as i32, img_pos.y as i32);
            }
            self.heal_dragging = None;
        }

        // Right-click removes nearest spot
        if secondary_clicked && let Some((idx, _)) = hit_spot {
            state
                .tools
                .find_mut::<HealTool>()
                .unwrap()
                .spots
                .remove(idx);
        }

        // Draw spot overlays
        for (i, spot) in state
            .tools
            .find::<HealTool>()
            .unwrap()
            .spots
            .iter()
            .enumerate()
        {
            let dst_screen = image_to_screen(
                Pos2::new(spot.dest_x as f32, spot.dest_y as f32),
                image_tl,
                self.zoom,
            );
            let src_screen = image_to_screen(
                Pos2::new(spot.src_x as f32, spot.src_y as f32),
                image_tl,
                self.zoom,
            );
            let r_screen = spot.radius as f32 * self.zoom;

            let hovered_src = matches!(hit_spot, Some((hi, true)) if hi == i);
            let hovered_dst = matches!(hit_spot, Some((hi, false)) if hi == i);

            // Arrow from src to dst
            painter.arrow(
                src_screen,
                dst_screen - src_screen,
                egui::Stroke::new(1.0_f32, Color32::from_white_alpha(180)),
            );

            // Source circle (green)
            painter.circle_stroke(
                src_screen,
                r_screen,
                egui::Stroke::new(
                    if hovered_src { 3.0_f32 } else { 1.5_f32 },
                    Color32::from_rgb(80, 200, 80),
                ),
            );
            painter.circle_filled(src_screen, 4.0, Color32::from_rgb(80, 200, 80));

            // Dest circle (red)
            painter.circle_stroke(
                dst_screen,
                r_screen,
                egui::Stroke::new(
                    if hovered_dst { 3.0_f32 } else { 1.5_f32 },
                    Color32::from_rgb(220, 60, 60),
                ),
            );
            painter.circle_filled(dst_screen, 4.0, Color32::from_rgb(220, 60, 60));
        }
    }
}
