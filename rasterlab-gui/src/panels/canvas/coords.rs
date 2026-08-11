//! Conversions between screen space, image-pixel space, and the normalised
//! [0, 1] image space the mask shapes are expressed in.

use egui::{Pos2, Vec2};

/// Convert a screen position to image-space coordinates.
pub(super) fn screen_to_image(pos: Pos2, image_tl: Pos2, zoom: f32) -> Pos2 {
    Pos2::new((pos.x - image_tl.x) / zoom, (pos.y - image_tl.y) / zoom)
}

/// Convert an image-space position to screen coordinates.
pub(super) fn image_to_screen(pos: Pos2, image_tl: Pos2, zoom: f32) -> Pos2 {
    Pos2::new(pos.x * zoom + image_tl.x, pos.y * zoom + image_tl.y)
}

/// Convert a screen position to normalised [0, 1] image coordinates.
pub(super) fn screen_to_norm(screen: Pos2, image_tl: Pos2, display_size: Vec2) -> Pos2 {
    Pos2::new(
        (screen.x - image_tl.x) / display_size.x,
        (screen.y - image_tl.y) / display_size.y,
    )
}

/// Convert a normalised [0, 1] image position to screen coordinates.
pub(super) fn norm_to_screen(norm: Pos2, image_tl: Pos2, display_size: Vec2) -> Pos2 {
    Pos2::new(
        image_tl.x + norm.x * display_size.x,
        image_tl.y + norm.y * display_size.y,
    )
}
