//! Presentation textures: uploading pipeline output to the GPU at roughly the
//! size it is actually drawn at.

use std::sync::Arc;

use egui::{ColorImage, TextureHandle, TextureId, TextureOptions};
use rasterlab_core::Image;
use rasterlab_core::ops::resize::reduce_pow2;

/// wgpu's hard limit on texture dimensions (D3D12/Metal/Vulkan minimum guarantee).
const MAX_TEXTURE_DIM: u32 = 8192;

/// Identity of the pixels held by a [`PresentationTexture`].
#[derive(Default)]
pub(super) enum ContentId {
    #[default]
    Empty,
    /// A render result shared with `AppState`.  Pointer identity is exact, and
    /// holding the `Arc` stops the allocator recycling the address underneath
    /// us — so unlike a sampled content hash it can never miss a small edit.
    Shared(Arc<Image>),
    /// A canvas-local render, identified by the pipeline generation counters
    /// that produced it.
    Generation(u64),
}

impl PartialEq for ContentId {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (ContentId::Shared(a), ContentId::Shared(b)) => Arc::ptr_eq(a, b),
            (ContentId::Generation(a), ContentId::Generation(b)) => a == b,
            _ => false,
        }
    }
}

/// A canvas texture held at roughly the image's physical on-screen size.
///
/// egui allocates a single mip level per managed texture, so drawing a 24 MP
/// photo at "Fit" leaves the GPU minifying it with one 2×2 bilinear tap: it
/// samples a few hundredths of a percent of the source pixels, and fine detail
/// and local contrast simply vanish. Reducing on the CPU first restores them.
///
/// The reduction is quantised to powers of two, which makes it an exact area
/// average over each destination pixel's footprint — the ideal minification
/// filter, and far cheaper than a general resample (~4 ms versus ~200 ms for
/// Lanczos3 on 24 MP). Quantising also means ordinary zooming only rebuilds
/// when it crosses a power of two, and the ≤2× residual left over is precisely
/// what the GPU's bilinear tap handles well.
#[derive(Default)]
pub(super) struct PresentationTexture {
    handle: Option<TextureHandle>,
    /// Pixels currently uploaded.
    content: ContentId,
    /// Power-of-two reduction applied on upload.
    level: u32,
    /// Dimensions of the source image, before reduction.  Split view needs
    /// these to place the image, which rotate/crop can resize.
    pub(super) source_size: (u32, u32),
}

impl PresentationTexture {
    pub(super) fn id(&self) -> Option<TextureId> {
        self.handle.as_ref().map(|t| t.id())
    }

    pub(super) fn clear(&mut self) {
        *self = Self::default();
    }

    /// True when the pixels, or the reduction the current zoom calls for, have
    /// changed.  Callers that must run an expensive render to produce the
    /// source image check this first.
    pub(super) fn is_stale(&self, content: &ContentId, screen_scale: f32) -> bool {
        self.handle.is_none()
            || self.content != *content
            || presentation_level(self.source_size, screen_scale) != self.level
    }

    pub(super) fn upload(
        &mut self,
        ctx: &egui::Context,
        name: &'static str,
        image: &Image,
        content: ContentId,
        screen_scale: f32,
    ) {
        let source_size = (image.width, image.height);
        let level = presentation_level(source_size, screen_scale);
        self.handle =
            Some(ctx.load_texture(name, image_to_egui(image, level), TextureOptions::LINEAR));
        self.content = content;
        self.level = level;
        self.source_size = source_size;
    }

    /// Check-and-upload for sources that are already in hand.
    pub(super) fn sync(
        &mut self,
        ctx: &egui::Context,
        name: &'static str,
        image: &Image,
        content: ContentId,
        screen_scale: f32,
    ) {
        if self.is_stale(&content, screen_scale) {
            self.upload(ctx, name, image, content, screen_scale);
        }
    }
}

/// Number of halvings to apply before upload.
///
/// `screen_scale` is source pixels per framebuffer pixel: 1.0 means the image
/// is drawn at its native resolution.  The result keeps the texture between 1×
/// and 2× the on-screen size, subject to the GPU's dimension limit.
fn presentation_level(source_size: (u32, u32), screen_scale: f32) -> u32 {
    let longest = source_size.0.max(source_size.1);
    if longest == 0 {
        return 0;
    }
    // Reduce at least enough that the upload fits the GPU's texture limit.
    let for_gpu_limit = longest
        .div_ceil(MAX_TEXTURE_DIM)
        .next_power_of_two()
        .trailing_zeros();
    // …and enough that the GPU is left with at most a 2× minification.
    let for_display = if screen_scale.is_finite() && screen_scale > 0.0 && screen_scale < 1.0 {
        (-screen_scale.log2()) as u32
    } else {
        0
    };
    // Never reduce past a single pixel.
    for_gpu_limit.max(for_display).min(longest.ilog2())
}

fn image_to_egui(image: &Image, level: u32) -> ColorImage {
    let mut color_image = if level == 0 {
        ColorImage::from_rgba_unmultiplied(
            [image.width as usize, image.height as usize],
            &image.data,
        )
    } else {
        let reduced = reduce_pow2(image, level);
        ColorImage::from_rgba_unmultiplied(
            [reduced.width as usize, reduced.height as usize],
            &reduced.data,
        )
    };

    // The painter supplies its own rectangle, but retaining the logical source
    // size also keeps TextureHandle/ColorImage introspection meaningful.
    color_image.source_size = egui::Vec2::new(image.width as f32, image.height as f32);
    color_image
}

#[cfg(test)]
mod tests {
    use super::*;
    use rasterlab_core::ops::resize::level_size;

    /// A 24 MP photo, fit into a window, must not be uploaded at full size —
    /// and the level must track the *physical* framebuffer, so the same
    /// logical view needs one level less on a 2× display.
    #[test]
    fn presentation_level_tracks_physical_display_scale() {
        let photo = (4899, 3266);
        assert_eq!(presentation_level(photo, 0.20), 2);
        assert_eq!(presentation_level(photo, 0.40), 1);
        assert_eq!(presentation_level(photo, 1.0), 0);
        // Exact powers of two land on the level that maps 1:1 to the screen.
        assert_eq!(presentation_level(photo, 0.5), 1);
        assert_eq!(presentation_level(photo, 0.25), 2);
    }

    /// The reduction must never leave the GPU with more than a 2× minification,
    /// which is all a single bilinear tap can resolve.
    #[test]
    fn presentation_level_leaves_at_most_a_2x_residual() {
        let photo = (4899, 3266);
        for step in 1..400 {
            let scale = step as f32 / 400.0;
            let (w, _) = level_size(photo.0, photo.1, presentation_level(photo, scale));
            let residual = photo.0 as f32 * scale / w as f32;
            assert!(
                (0.5..=1.0).contains(&residual),
                "scale={scale} texture_width={w} residual={residual}"
            );
        }
    }

    #[test]
    fn presentation_level_respects_gpu_dimension_limit() {
        // Over the limit even at 1:1, so it must reduce regardless of zoom.
        assert_eq!(presentation_level((10_000, 5_000), 1.0), 1);
        assert_eq!(level_size(10_000, 5_000, 1), (5_000, 2_500));
        assert_eq!(presentation_level((8_192, 4_000), 1.0), 0);
        // A very large panorama needs two levels.
        assert_eq!(presentation_level((20_000, 4_000), 1.0), 2);
    }

    /// The whole point of reducing on the CPU: every source pixel contributes,
    /// so a checkerboard averages to mid-grey instead of collapsing onto
    /// whichever few texels a bilinear tap happened to land on.
    #[test]
    fn presentation_downsample_filters_the_source_footprint() {
        let mut image = Image::new(8, 8);
        for y in 0..8 {
            for x in 0..8 {
                let value = if (x + y) % 2 == 0 { 0 } else { 255 };
                image.set_pixel(x, y, [value, value, value, 255]);
            }
        }

        let downsampled = image_to_egui(&image, 3);
        assert_eq!(downsampled.size, [1, 1]);
        assert_eq!(downsampled.pixels[0].r(), 128);
    }

    /// Odd dimensions must still reduce, and the recorded size must match the
    /// pixels actually produced.
    #[test]
    fn presentation_downsample_handles_odd_dimensions() {
        let image = Image::new(7, 3);
        for level in 0..4 {
            let reduced = image_to_egui(&image, level);
            let (w, h) = level_size(7, 3, level);
            assert_eq!(reduced.size, [w as usize, h as usize], "level={level}");
            assert_eq!(reduced.pixels.len(), (w * h) as usize, "level={level}");
        }
    }
}
