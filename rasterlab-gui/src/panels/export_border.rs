use ab_glyph::{Font, FontRef, Glyph, PxScale, ScaleFont};
use rasterlab_core::image::{Image, ImageMetadata, PixelFormat};
use serde::{Deserialize, Serialize};

/// Presentation border settings shared by single-image and library exports.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExportBorderOptions {
    pub enabled: bool,
    pub custom_text: String,
    pub show_focal_length: bool,
    pub show_iso: bool,
    pub show_aperture: bool,
    pub show_shutter: bool,
}

impl ExportBorderOptions {
    /// Build the line printed in the top border. Missing EXIF fields are omitted.
    pub fn display_text(&self, metadata: &ImageMetadata) -> String {
        let mut sections = Vec::new();
        let custom = self.custom_text.trim();
        if !custom.is_empty() {
            sections.push(custom.to_owned());
        }

        let mut details = Vec::new();
        if self.show_focal_length
            && let Some(value) = metadata.focal_length
        {
            details.push(format!("{} mm", compact_number(value)));
        }
        if self.show_iso
            && let Some(value) = metadata.iso
        {
            details.push(format!("ISO {value}"));
        }
        if self.show_aperture
            && let Some(value) = metadata.aperture
        {
            details.push(format!("f/{}", compact_number(value)));
        }
        if self.show_shutter
            && let Some(value) = metadata.shutter_speed.as_deref()
        {
            let value = value.trim();
            let value = value.strip_suffix(" s").unwrap_or(value);
            details.push(format!("{value} s"));
        }
        if !details.is_empty() {
            sections.push(details.join("  ·  "));
        }
        sections.join("  |  ")
    }
}

/// Common editor used by both export surfaces.
pub fn options_ui(ui: &mut egui::Ui, options: &mut ExportBorderOptions) -> bool {
    let mut changed = ui
        .checkbox(&mut options.enabled, "Add presentation border")
        .changed();
    if options.enabled {
        ui.indent("export_border_options", |ui| {
            egui::Grid::new(ui.id().with("export_border_grid"))
                .num_columns(2)
                .spacing([8.0, 5.0])
                .show(ui, |ui| {
                    ui.label("Custom text:");
                    changed |= ui
                        .add(
                            egui::TextEdit::singleline(&mut options.custom_text)
                                .hint_text("Name, title, or website")
                                .desired_width(230.0),
                        )
                        .changed();
                    ui.end_row();

                    ui.label("Camera details:");
                    ui.horizontal_wrapped(|ui| {
                        changed |= ui
                            .checkbox(&mut options.show_focal_length, "Focal length")
                            .changed();
                        changed |= ui.checkbox(&mut options.show_iso, "ISO").changed();
                        changed |= ui.checkbox(&mut options.show_aperture, "F-stop").changed();
                        changed |= ui.checkbox(&mut options.show_shutter, "Shutter").changed();
                    });
                    ui.end_row();
                });
            ui.label(
                egui::RichText::new(
                    "Text is placed in the top border; unavailable EXIF values are omitted.",
                )
                .small()
                .weak(),
            );
        });
    }
    changed
}

fn compact_number(value: f32) -> String {
    if value.fract().abs() < 0.001 {
        format!("{value:.0}")
    } else {
        let value = format!("{value:.1}");
        value.trim_end_matches('0').trim_end_matches('.').to_owned()
    }
}

/// Add a black presentation frame and a tracked, light-gray top caption.
///
/// The frame is approximately 2.8% of the image's short edge, matching the
/// proportions of the supplied reference. The original image is copied without
/// resampling, so this is safe to apply after the export resize step.
pub fn apply_export_border(
    image: &Image,
    source_metadata: &ImageMetadata,
    options: &ExportBorderOptions,
) -> anyhow::Result<Image> {
    if !options.enabled {
        return Ok(image.deep_clone());
    }

    let border = ((image.width.min(image.height) as f32 * 0.028).round() as u32).max(8);
    let width = image
        .width
        .checked_add(border.saturating_mul(2))
        .ok_or_else(|| anyhow::anyhow!("border makes image width too large"))?;
    let height = image
        .height
        .checked_add(border.saturating_mul(2))
        .ok_or_else(|| anyhow::anyhow!("border makes image height too large"))?;
    let byte_len = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| anyhow::anyhow!("bordered image is too large"))?;
    let mut data = vec![0u8; byte_len];
    // Keep the border opaque even when the source has transparency.
    for alpha in data[3..].iter_mut().step_by(4) {
        *alpha = 255;
    }

    let src_stride = image.width as usize * 4;
    let dst_stride = width as usize * 4;
    let x_offset = border as usize * 4;
    for (y, src_row) in image.data.chunks_exact(src_stride).enumerate() {
        let start = (y + border as usize) * dst_stride + x_offset;
        data[start..start + src_stride].copy_from_slice(src_row);
    }

    let caption = options.display_text(source_metadata);
    if !caption.is_empty() {
        draw_caption(&mut data, width, border, &caption);
    }

    Ok(Image {
        width,
        height,
        data,
        format: PixelFormat::Rgba8,
        metadata: source_metadata.clone(),
    })
}

fn draw_caption(data: &mut [u8], width: u32, border: u32, text: &str) {
    let Ok(font) = FontRef::try_from_slice(epaint_default_fonts::HACK_REGULAR) else {
        return;
    };
    let font_size = (border as f32 * 0.48).clamp(7.0, 64.0);
    let scaled = font.as_scaled(PxScale::from(font_size));
    let tracking = font_size * 0.22;
    let baseline = (border as f32 + scaled.ascent() + scaled.descent()) * 0.5;
    let mut caret_x = border as f32;
    let max_x = width.saturating_sub(border) as f32;
    let mut previous = None;

    for ch in text.chars() {
        let glyph_id = scaled.glyph_id(ch);
        if let Some(prev) = previous {
            caret_x += scaled.kern(prev, glyph_id);
        }
        let advance = scaled.h_advance(glyph_id) + tracking;
        if caret_x + advance >= max_x {
            break;
        }
        let glyph = Glyph {
            id: glyph_id,
            scale: PxScale::from(font_size),
            position: ab_glyph::point(caret_x, baseline),
        };
        if let Some(outlined) = font.outline_glyph(glyph) {
            let bounds = outlined.px_bounds();
            outlined.draw(|x, y, coverage| {
                let px = bounds.min.x as i32 + x as i32;
                let py = bounds.min.y as i32 + y as i32;
                if px < 0 || py < 0 || px >= width as i32 || py >= border as i32 {
                    return;
                }
                let idx = (py as usize * width as usize + px as usize) * 4;
                let gray = (210.0 * coverage).round() as u8;
                data[idx] = gray;
                data[idx + 1] = gray;
                data[idx + 2] = gray;
                data[idx + 3] = 255;
            });
        }
        caret_x += advance;
        previous = Some(glyph_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image_with_metadata(metadata: ImageMetadata) -> Image {
        Image {
            width: 100,
            height: 50,
            data: vec![255; 100 * 50 * 4],
            format: PixelFormat::Rgba8,
            metadata,
        }
    }

    #[test]
    fn caption_combines_custom_text_and_selected_exif() {
        let metadata = ImageMetadata {
            focal_length: Some(50.0),
            iso: Some(400),
            aperture: Some(2.8),
            shutter_speed: Some("1/250 s".into()),
            ..Default::default()
        };
        let options = ExportBorderOptions {
            enabled: true,
            custom_text: "MING THEIN".into(),
            show_focal_length: true,
            show_iso: true,
            show_aperture: true,
            show_shutter: true,
        };

        assert_eq!(
            options.display_text(&metadata),
            "MING THEIN  |  50 mm  ·  ISO 400  ·  f/2.8  ·  1/250 s"
        );
    }

    #[test]
    fn border_expands_canvas_and_preserves_source_pixels() {
        let source = image_with_metadata(ImageMetadata::default());
        let metadata = source.metadata.clone();
        let result = apply_export_border(
            &source,
            &metadata,
            &ExportBorderOptions {
                enabled: true,
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!((result.width, result.height), (116, 66));
        assert_eq!(&result.data[0..4], &[0, 0, 0, 255]);
        let source_origin = ((8 * result.width + 8) * 4) as usize;
        assert_eq!(&result.data[source_origin..source_origin + 4], &[255; 4]);
    }

    #[test]
    fn missing_selected_metadata_is_omitted() {
        let options = ExportBorderOptions {
            show_iso: true,
            show_shutter: true,
            ..Default::default()
        };
        assert!(options.display_text(&ImageMetadata::default()).is_empty());
    }

    #[test]
    fn custom_text_renders_into_top_border() {
        let source = image_with_metadata(ImageMetadata::default());
        let metadata = source.metadata.clone();
        let result = apply_export_border(
            &source,
            &metadata,
            &ExportBorderOptions {
                enabled: true,
                custom_text: "TEST".into(),
                ..Default::default()
            },
        )
        .unwrap();

        let top_border_bytes = result.width as usize * 8 * 4;
        assert!(
            result.data[..top_border_bytes]
                .chunks_exact(4)
                .any(|pixel| pixel[0] > 0 && pixel[0] < 255)
        );
    }

    #[test]
    fn source_metadata_survives_when_rendered_buffer_lost_it() {
        let source = image_with_metadata(ImageMetadata::default());
        let source_metadata = ImageMetadata {
            iso: Some(800),
            ..Default::default()
        };
        let result = apply_export_border(
            &source,
            &source_metadata,
            &ExportBorderOptions {
                enabled: true,
                show_iso: true,
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(result.metadata.iso, Some(800));
        let top_border_bytes = result.width as usize * 8 * 4;
        assert!(
            result.data[..top_border_bytes]
                .chunks_exact(4)
                .any(|pixel| pixel[0] > 0 && pixel[0] < 255)
        );
    }
}
