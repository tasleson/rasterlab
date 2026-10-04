//! Interactive gradient-mask placement, handle drawing, and the translucent
//! mask preview overlay.

use egui::{Color32, ColorImage, Pos2, Rect, Stroke, TextureOptions, Ui, Vec2};

use crate::state::{AppState, EditingTool, ToolState};

use super::coords::{norm_to_screen, screen_to_norm};
use super::{CanvasState, CanvasView};

/// Mask selector values used by the tools panel.
const MASK_LINEAR: usize = 1;
const MASK_RADIAL: usize = 2;

/// Screen-space distance within which the pointer grabs a mask handle.
const HANDLE_GRAB_RADIUS: f32 = 10.0;
/// Smallest radius a dragged radial edge may produce, matching the slider.
const MIN_RADIAL_RADIUS: f32 = 0.01;
/// Fill for the handle under the pointer or being dragged.
const HANDLE_ACTIVE_FILL: Color32 = Color32::from_rgb(90, 160, 255);

/// A draggable point on the active mask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MaskHandle {
    LinearStart,
    LinearEnd,
    LinearCenter,
    RadialEdge,
    RadialCenter,
}

/// What the current primary drag on the canvas is doing to the mask.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum MaskDrag {
    /// Dragging out a new mask from this normalised start point.
    Place(Pos2),
    /// Moving one handle of the existing mask. `grab_offset` is the handle's
    /// position relative to the pointer at press time, so the handle does not
    /// jump to the pointer when the grab is slightly off-centre.
    Handle {
        handle: MaskHandle,
        grab_offset: Vec2,
    },
}

impl CanvasState {
    /// Drag on empty canvas to place the active gradient mask, or drag one of
    /// its handles to adjust it, and draw the handles.
    pub(super) fn handle_mask(
        &mut self,
        ui: &mut Ui,
        painter: &egui::Painter,
        state: &mut AppState,
        view: &CanvasView,
    ) {
        let CanvasView {
            canvas_rect,
            image_tl,
            display_size,
            over_canvas,
            ..
        } = *view;

        // Clear any stale crop selection while mask mode is active.
        self.crop_start = None;
        self.crop_end = None;

        let (ptr_pos, primary_pressed, primary_down) = ui.input(|i| {
            (
                i.pointer.hover_pos(),
                i.pointer.button_pressed(egui::PointerButton::Primary),
                i.pointer.button_down(egui::PointerButton::Primary),
            )
        });

        let hit = |tools: &ToolState| {
            ptr_pos
                .filter(|_| over_canvas)
                .and_then(|p| hit_test_mask_handle(tools, p, image_tl, display_size))
        };

        if primary_pressed
            && over_canvas
            && let Some(p) = ptr_pos
        {
            let ptr = screen_to_norm(p, image_tl, display_size);
            self.mask_drag = Some(match hit(&state.tools) {
                Some((handle, at)) => MaskDrag::Handle {
                    handle,
                    grab_offset: at - ptr,
                },
                None => MaskDrag::Place(ptr),
            });
        }
        if primary_down {
            if let (Some(drag), Some(p)) = (self.mask_drag, ptr_pos) {
                let ptr = screen_to_norm(p, image_tl, display_size);
                let before = state.tools.current_mask_shape();
                match drag {
                    MaskDrag::Place(start) => match state.tools.mask_sel {
                        MASK_LINEAR => update_linear_mask(&mut state.tools, start, ptr),
                        MASK_RADIAL => update_radial_mask(&mut state.tools, start, ptr),
                        _ => {}
                    },
                    MaskDrag::Handle {
                        handle,
                        grab_offset,
                    } => move_mask_handle(&mut state.tools, handle, ptr + grab_offset),
                }
                if before != state.tools.current_mask_shape()
                    && state
                        .editing
                        .is_some_and(|session| session.tool == EditingTool::Masking)
                {
                    state.request_render();
                }
            }
        } else {
            self.mask_drag = None;
        }

        // Grabbing a handle overrides the canvas-wide placement crosshair.
        let active = match self.mask_drag {
            Some(MaskDrag::Handle { handle, .. }) => {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                Some(handle)
            }
            Some(MaskDrag::Place(_)) => None,
            None => hit(&state.tools).map(|(handle, _)| {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
                handle
            }),
        };

        match state.tools.mask_sel {
            MASK_LINEAR => draw_linear_mask_handles(
                painter,
                &state.tools,
                active,
                image_tl,
                display_size,
                canvas_rect,
            ),
            MASK_RADIAL => draw_radial_mask_handles(
                painter,
                &state.tools,
                active,
                image_tl,
                display_size,
                canvas_rect,
            ),
            _ => {}
        }
    }

    /// Draw the translucent mask preview over the image area.
    ///
    /// Rendered at 256×256 and scaled to the image area so the user can see
    /// where the next masked Apply will take effect. Releases the texture when
    /// no mask is selected.
    pub(super) fn draw_mask_overlay(&mut self, ui: &Ui, state: &AppState, view: &CanvasView) {
        if state.tools.mask_sel == 0 {
            self.mask_overlay_texture = None;
            self.mask_overlay_hash = 0;
            return;
        }

        let hash = mask_params_hash(state);
        if self.mask_overlay_texture.is_none() || hash != self.mask_overlay_hash {
            self.mask_overlay_texture = Some(ui.ctx().load_texture(
                "mask_overlay",
                build_mask_preview(state, 256, 256),
                TextureOptions::LINEAR,
            ));
            self.mask_overlay_hash = hash;
        }
        let Some(texture) = &self.mask_overlay_texture else {
            return;
        };

        // The overlay covers the image area in screen space.
        let scale = state.rendered_scale;
        let full_w = view.img_w as f32 / scale;
        let full_h = view.img_h as f32 / scale;
        let overlay_rect = Rect::from_min_size(
            view.image_tl,
            Vec2::new(full_w * self.zoom, full_h * self.zoom),
        );
        // Use a clipped painter so it stays inside the canvas area.
        ui.painter().with_clip_rect(view.canvas_rect).image(
            texture.id(),
            overlay_rect,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
    }
}

/// Update linear mask from a drag: start is the "0% effect" end,
/// end is the "100% effect" end.  Center, angle, and feather are derived.
fn update_linear_mask(tools: &mut ToolState, start: Pos2, end: Pos2) {
    let dx = end.x - start.x;
    let dy = end.y - start.y;
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1e-4 {
        return; // Too short — skip to avoid a degenerate angle.
    }
    tools.mask_lin_cx = (start.x + end.x) * 0.5;
    tools.mask_lin_cy = (start.y + end.y) * 0.5;
    tools.mask_lin_angle = dy.atan2(dx).to_degrees();
    tools.mask_lin_feather = len;
}

/// Update radial mask from a drag: start is the centre, end defines the radius.
fn update_radial_mask(tools: &mut ToolState, start: Pos2, end: Pos2) {
    tools.mask_rad_cx = start.x;
    tools.mask_rad_cy = start.y;
    tools.mask_rad_radius = (end - start).length();
}

/// The linear mask's 0% end, centre, and 100% end in normalised coordinates.
fn linear_points(tools: &ToolState) -> (Pos2, Pos2, Pos2) {
    let rad = tools.mask_lin_angle.to_radians();
    let half = Vec2::new(rad.cos(), rad.sin()) * (tools.mask_lin_feather * 0.5);
    let center = Pos2::new(tools.mask_lin_cx, tools.mask_lin_cy);
    (center - half, center, center + half)
}

/// The radial mask's centre and the point on its edge used to drag the
/// radius, in normalised coordinates.
fn radial_points(tools: &ToolState) -> (Pos2, Pos2) {
    let center = Pos2::new(tools.mask_rad_cx, tools.mask_rad_cy);
    (center, center + Vec2::new(tools.mask_rad_radius, 0.0))
}

/// Every handle of the selected mask, in normalised coordinates. Ends come
/// before the centre so a collapsed linear mask can still be stretched out.
fn mask_handles(tools: &ToolState) -> Vec<(MaskHandle, Pos2)> {
    match tools.mask_sel {
        MASK_LINEAR => {
            let (start, center, end) = linear_points(tools);
            vec![
                (MaskHandle::LinearStart, start),
                (MaskHandle::LinearEnd, end),
                (MaskHandle::LinearCenter, center),
            ]
        }
        MASK_RADIAL => {
            let (center, edge) = radial_points(tools);
            vec![
                (MaskHandle::RadialEdge, edge),
                (MaskHandle::RadialCenter, center),
            ]
        }
        _ => Vec::new(),
    }
}

/// The handle nearest `ptr` within grabbing distance, with its normalised
/// position. On a tie the earlier handle in [`mask_handles`] wins.
fn hit_test_mask_handle(
    tools: &ToolState,
    ptr: Pos2,
    image_tl: Pos2,
    display_size: Vec2,
) -> Option<(MaskHandle, Pos2)> {
    mask_handles(tools)
        .into_iter()
        .map(|(handle, at)| {
            let distance = norm_to_screen(at, image_tl, display_size).distance(ptr);
            (handle, at, distance)
        })
        .filter(|&(_, _, distance)| distance <= HANDLE_GRAB_RADIUS)
        .min_by(|a, b| a.2.total_cmp(&b.2))
        .map(|(handle, at, _)| (handle, at))
}

/// Move `handle` to the normalised position `to`. Dragging a linear end rotates
/// and stretches the gradient about its fixed centre, mirroring the other end;
/// only dragging a centre moves the mask, kept on the image so its handle
/// stays reachable.
fn move_mask_handle(tools: &mut ToolState, handle: MaskHandle, to: Pos2) {
    let on_image = Pos2::new(to.x.clamp(0.0, 1.0), to.y.clamp(0.0, 1.0));
    match handle {
        MaskHandle::LinearStart | MaskHandle::LinearEnd => {
            let (_, center, _) = linear_points(tools);
            let half = if handle == MaskHandle::LinearEnd {
                to - center
            } else {
                center - to
            };
            update_linear_mask(tools, center - half, center + half);
        }
        MaskHandle::LinearCenter => {
            tools.mask_lin_cx = on_image.x;
            tools.mask_lin_cy = on_image.y;
        }
        MaskHandle::RadialCenter => {
            tools.mask_rad_cx = on_image.x;
            tools.mask_rad_cy = on_image.y;
        }
        MaskHandle::RadialEdge => {
            let (center, _) = radial_points(tools);
            tools.mask_rad_radius = (to - center).length().max(MIN_RADIAL_RADIUS);
        }
    }
}

/// Draw one round handle, highlighted while hovered or dragged.
fn draw_handle(painter: &egui::Painter, at: Pos2, radius: f32, active: bool) {
    let fill = if active {
        HANDLE_ACTIVE_FILL
    } else {
        Color32::from_black_alpha(160)
    };
    painter.circle_filled(at, radius, fill);
    painter.circle_stroke(at, radius, Stroke::new(1.5_f32, Color32::WHITE));
}

/// Draw handles showing the current linear gradient mask extent.
fn draw_linear_mask_handles(
    painter: &egui::Painter,
    tools: &ToolState,
    active: Option<MaskHandle>,
    image_tl: Pos2,
    display_size: Vec2,
    canvas_rect: Rect,
) {
    let painter = painter.with_clip_rect(canvas_rect);
    let (a_norm, center, b_norm) = linear_points(tools);

    let center_s = norm_to_screen(center, image_tl, display_size);
    let a_s = norm_to_screen(a_norm, image_tl, display_size);
    let b_s = norm_to_screen(b_norm, image_tl, display_size);

    let shadow = Stroke::new(3.0_f32, Color32::from_black_alpha(160));
    let white = Stroke::new(1.5_f32, Color32::WHITE);

    painter.line_segment([a_s, b_s], shadow);
    painter.line_segment([a_s, b_s], white);

    for (handle, pt) in [
        (MaskHandle::LinearStart, a_s),
        (MaskHandle::LinearCenter, center_s),
        (MaskHandle::LinearEnd, b_s),
    ] {
        draw_handle(&painter, pt, 6.0, active == Some(handle));
    }
}

/// Draw handles showing the current radial gradient mask extent.
fn draw_radial_mask_handles(
    painter: &egui::Painter,
    tools: &ToolState,
    active: Option<MaskHandle>,
    image_tl: Pos2,
    display_size: Vec2,
    canvas_rect: Rect,
) {
    let painter = painter.with_clip_rect(canvas_rect);
    let (center_norm, edge_norm) = radial_points(tools);
    let center_s = norm_to_screen(center_norm, image_tl, display_size);

    // Convert radius from normalised space to screen pixels per axis.
    let rx = tools.mask_rad_radius * display_size.x;
    let ry = tools.mask_rad_radius * display_size.y;

    draw_ellipse_stroke(
        &painter,
        center_s,
        rx,
        ry,
        Stroke::new(3.0_f32, Color32::from_black_alpha(160)),
    );
    draw_ellipse_stroke(
        &painter,
        center_s,
        rx,
        ry,
        Stroke::new(1.5_f32, Color32::WHITE),
    );

    // Crosshair at centre.
    let arm = 8.0_f32;
    painter.line_segment(
        [
            center_s - Vec2::new(arm, 0.0),
            center_s + Vec2::new(arm, 0.0),
        ],
        Stroke::new(1.5_f32, Color32::WHITE),
    );
    painter.line_segment(
        [
            center_s - Vec2::new(0.0, arm),
            center_s + Vec2::new(0.0, arm),
        ],
        Stroke::new(1.5_f32, Color32::WHITE),
    );
    draw_handle(
        &painter,
        center_s,
        4.0,
        active == Some(MaskHandle::RadialCenter),
    );
    draw_handle(
        &painter,
        norm_to_screen(edge_norm, image_tl, display_size),
        6.0,
        active == Some(MaskHandle::RadialEdge),
    );
}

/// Approximate an ellipse with line segments.
fn draw_ellipse_stroke(painter: &egui::Painter, center: Pos2, rx: f32, ry: f32, stroke: Stroke) {
    const N: usize = 48;
    let pts: Vec<Pos2> = (0..=N)
        .map(|i| {
            let a = i as f32 * 2.0 * std::f32::consts::PI / N as f32;
            Pos2::new(center.x + rx * a.cos(), center.y + ry * a.sin())
        })
        .collect();
    for w in pts.windows(2) {
        painter.line_segment([w[0], w[1]], stroke);
    }
}

/// Hash the current mask parameters so the overlay texture is only rebuilt
/// when something actually changes.
fn mask_params_hash(state: &AppState) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    state.tools.mask_sel.hash(&mut h);
    // Hash float bits — NaN-safe for UI values.
    state.tools.mask_lin_cx.to_bits().hash(&mut h);
    state.tools.mask_lin_cy.to_bits().hash(&mut h);
    state.tools.mask_lin_angle.to_bits().hash(&mut h);
    state.tools.mask_lin_feather.to_bits().hash(&mut h);
    state.tools.mask_lin_invert.hash(&mut h);
    state.tools.mask_rad_cx.to_bits().hash(&mut h);
    state.tools.mask_rad_cy.to_bits().hash(&mut h);
    state.tools.mask_rad_radius.to_bits().hash(&mut h);
    state.tools.mask_rad_feather.to_bits().hash(&mut h);
    state.tools.mask_rad_invert.hash(&mut h);
    h.finish()
}

/// Build a small `ColorImage` that visualises the current mask as a
/// semi-transparent blue overlay.  Opacity of each pixel = mask opacity.
fn build_mask_preview(state: &AppState, w: usize, h: usize) -> ColorImage {
    let shape = match state.tools.current_mask_shape() {
        Some(s) => s,
        None => return ColorImage::new([w, h], vec![Color32::TRANSPARENT; w * h]),
    };
    let mut pixels = Vec::with_capacity(w * h);
    for y in 0..h {
        let ny = (y as f32 + 0.5) / h as f32;
        for x in 0..w {
            let nx = (x as f32 + 0.5) / w as f32;
            let opacity = shape.eval(nx, ny);
            let alpha = (opacity * 140.0) as u8;
            pixels.push(Color32::from_rgba_unmultiplied(30, 90, 255, alpha));
        }
    }
    ColorImage {
        size: [w, h],
        pixels,
        source_size: egui::Vec2::new(w as f32, h as f32),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IMAGE_TL: Pos2 = Pos2::new(20.0, 10.0);
    const DISPLAY: Vec2 = Vec2::new(1000.0, 500.0);

    fn linear_tools() -> ToolState {
        let mut tools = ToolState::new();
        tools.mask_sel = MASK_LINEAR;
        update_linear_mask(&mut tools, Pos2::new(0.2, 0.4), Pos2::new(0.6, 0.4));
        tools
    }

    fn radial_tools() -> ToolState {
        let mut tools = ToolState::new();
        tools.mask_sel = MASK_RADIAL;
        update_radial_mask(&mut tools, Pos2::new(0.5, 0.5), Pos2::new(0.75, 0.5));
        tools
    }

    fn assert_near(actual: Pos2, expected: Pos2) {
        assert!(
            (actual - expected).length() < 1e-4,
            "{actual:?} != {expected:?}"
        );
    }

    #[test]
    fn hovering_a_point_grabs_the_nearest_handle() {
        let cases = [
            (
                linear_tools(),
                Pos2::new(0.2, 0.4),
                Some(MaskHandle::LinearStart),
            ),
            (
                linear_tools(),
                Pos2::new(0.4, 0.4),
                Some(MaskHandle::LinearCenter),
            ),
            (
                linear_tools(),
                Pos2::new(0.6, 0.4),
                Some(MaskHandle::LinearEnd),
            ),
            (linear_tools(), Pos2::new(0.3, 0.4), None),
            (
                radial_tools(),
                Pos2::new(0.5, 0.5),
                Some(MaskHandle::RadialCenter),
            ),
            (
                radial_tools(),
                Pos2::new(0.75, 0.5),
                Some(MaskHandle::RadialEdge),
            ),
            (radial_tools(), Pos2::new(0.5, 0.75), None),
        ];
        for (tools, at, expected) in cases {
            let screen = norm_to_screen(at, IMAGE_TL, DISPLAY);
            for nudge in [Vec2::ZERO, Vec2::splat(HANDLE_GRAB_RADIUS * 0.6)] {
                let hit = hit_test_mask_handle(&tools, screen + nudge, IMAGE_TL, DISPLAY);
                assert_eq!(hit.map(|(handle, _)| handle), expected, "at {at:?}");
            }
            let far = screen + Vec2::splat(HANDLE_GRAB_RADIUS);
            assert_eq!(hit_test_mask_handle(&tools, far, IMAGE_TL, DISPLAY), None);
        }
    }

    #[test]
    fn collapsed_linear_mask_grabs_an_end_so_it_can_be_stretched() {
        let mut tools = linear_tools();
        tools.mask_lin_feather = 0.0;
        let center = norm_to_screen(Pos2::new(0.4, 0.4), IMAGE_TL, DISPLAY);
        let hit = hit_test_mask_handle(&tools, center, IMAGE_TL, DISPLAY);
        assert_eq!(hit.map(|(handle, _)| handle), Some(MaskHandle::LinearStart));
    }

    #[test]
    fn grabbing_without_moving_leaves_the_mask_unchanged() {
        for make in [linear_tools, radial_tools] {
            let before = mask_handles(&make());
            for &(handle, at) in &before {
                let mut tools = make();
                move_mask_handle(&mut tools, handle, at);
                for (&(_, expected), (_, actual)) in before.iter().zip(mask_handles(&tools)) {
                    assert_near(actual, expected);
                }
            }
        }
    }

    #[test]
    fn dragging_a_linear_end_pivots_around_the_fixed_centre() {
        // linear_tools() is centred on (0.4, 0.4), so each grabbed end lands
        // under the pointer and the other end mirrors it through the centre.
        for (handle, to, mirrored) in [
            (
                MaskHandle::LinearStart,
                Pos2::new(0.6, 0.1),
                Pos2::new(0.2, 0.7),
            ),
            (
                MaskHandle::LinearEnd,
                Pos2::new(0.2, 0.9),
                Pos2::new(0.6, -0.1),
            ),
        ] {
            let mut tools = linear_tools();
            move_mask_handle(&mut tools, handle, to);
            let (start, center, end) = linear_points(&tools);
            assert_near(center, Pos2::new(0.4, 0.4));
            let (grabbed, other) = if handle == MaskHandle::LinearStart {
                (start, end)
            } else {
                (end, start)
            };
            assert_near(grabbed, to);
            assert_near(other, mirrored);
        }
    }

    #[test]
    fn dragging_a_centre_moves_the_mask_and_keeps_it_on_the_image() {
        let mut tools = linear_tools();
        let (angle, feather) = (tools.mask_lin_angle, tools.mask_lin_feather);
        move_mask_handle(&mut tools, MaskHandle::LinearCenter, Pos2::new(0.7, 1.3));
        assert_near(linear_points(&tools).1, Pos2::new(0.7, 1.0));
        assert_eq!(
            (tools.mask_lin_angle, tools.mask_lin_feather),
            (angle, feather)
        );

        let mut tools = radial_tools();
        let radius = tools.mask_rad_radius;
        move_mask_handle(&mut tools, MaskHandle::RadialCenter, Pos2::new(-0.2, 0.3));
        assert_near(radial_points(&tools).0, Pos2::new(0.0, 0.3));
        assert_eq!(tools.mask_rad_radius, radius);
    }

    #[test]
    fn dragging_the_radial_edge_sets_the_radius() {
        for (to, radius) in [
            (Pos2::new(0.9, 0.5), 0.4),
            (Pos2::new(0.5, 0.2), 0.3),
            (Pos2::new(0.5, 0.5), MIN_RADIAL_RADIUS),
        ] {
            let mut tools = radial_tools();
            move_mask_handle(&mut tools, MaskHandle::RadialEdge, to);
            assert!((tools.mask_rad_radius - radius).abs() < 1e-5, "{to:?}");
            assert_near(radial_points(&tools).0, Pos2::new(0.5, 0.5));
        }
    }
}
