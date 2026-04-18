use std::path::PathBuf;

use egui::ScrollArea;
use rasterlab_core::{formats::FormatRegistry, project::RlabFile};
use rasterlab_library::Library;

use crate::state::AppState;

// ── Export dialog state ───────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ExportDialogState {
    pub open: bool,
    pub format: ExportFormat,
    pub jpeg_quality: u8,
    pub max_side: Option<u32>,
    pub dest_dir: PathBuf,
    pub progress: Option<(usize, usize)>,
    pub done: bool,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Jpeg,
    Png,
}

impl ExportFormat {
    fn label(self) -> &'static str {
        match self {
            Self::Jpeg => "JPEG",
            Self::Png => "PNG",
        }
    }
    fn ext(self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            Self::Png => "png",
        }
    }
}

impl Default for ExportDialogState {
    fn default() -> Self {
        Self {
            open: false,
            format: ExportFormat::Jpeg,
            jpeg_quality: 90,
            max_side: Some(2048),
            dest_dir: dirs::picture_dir().unwrap_or_else(|| PathBuf::from(".")),
            progress: None,
            done: false,
            errors: Vec::new(),
        }
    }
}

// ── UI ────────────────────────────────────────────────────────────────────────

pub fn ui(ctx: &egui::Context, state: &mut AppState) {
    if !state.tools.export_dialog.open {
        return;
    }

    let selected_count = state.library.selected.len();
    let mut open = state.tools.export_dialog.open;
    let mut do_export = false;
    let mut do_close = false;

    egui::Window::new(format!("Export {} Photos", selected_count))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .open(&mut open)
        .show(ctx, |ui| {
            let export = &mut state.tools.export_dialog;

            egui::Grid::new("export_grid")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    ui.label("Format:");
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut export.format, ExportFormat::Jpeg, "JPEG");
                        ui.selectable_value(&mut export.format, ExportFormat::Png, "PNG");
                    });
                    ui.end_row();

                    if export.format == ExportFormat::Jpeg {
                        ui.label("Quality:");
                        ui.add(egui::Slider::new(&mut export.jpeg_quality, 1u8..=100));
                        ui.end_row();
                    }

                    ui.label("Long side:");
                    ui.horizontal(|ui| {
                        let mut resize = export.max_side.is_some();
                        if ui.checkbox(&mut resize, "").changed() {
                            export.max_side = if resize { Some(2048) } else { None };
                        }
                        if let Some(ref mut side) = export.max_side {
                            ui.add(
                                egui::DragValue::new(side)
                                    .range(64u32..=65536)
                                    .suffix(" px"),
                            );
                        }
                    });
                    ui.end_row();

                    ui.label("Destination:");
                    ui.horizontal(|ui| {
                        let dir_str = export.dest_dir.display().to_string();
                        ui.add(
                            egui::TextEdit::singleline(&mut dir_str.clone()).desired_width(200.0),
                        );
                        #[cfg(not(target_arch = "wasm32"))]
                        if ui.button("Browse…").clicked() {
                            if let Some(path) = rfd::FileDialog::new().pick_folder() {
                                export.dest_dir = path;
                            }
                        }
                    });
                    ui.end_row();
                });

            ui.add_space(8.0);

            if let Some((done, total)) = export.progress {
                ui.label(format!("Exporting… {}/{}", done, total));
                let frac = if total > 0 {
                    done as f32 / total as f32
                } else {
                    0.0
                };
                ui.add(egui::ProgressBar::new(frac));
                ui.add_space(4.0);
            } else if export.done {
                if export.errors.is_empty() {
                    ui.label("Export complete.");
                } else {
                    ScrollArea::vertical().max_height(80.0).show(ui, |ui| {
                        for e in &export.errors {
                            ui.colored_label(egui::Color32::RED, e);
                        }
                    });
                }
            }

            let busy = export.progress.is_some();
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        !busy && selected_count > 0,
                        egui::Button::new(format!("Export {} photos", selected_count)),
                    )
                    .clicked()
                {
                    do_export = true;
                }
                if ui.button("Close").clicked() {
                    do_close = true;
                }
            });
        });

    if do_export {
        start_export(state);
    }
    if do_close || !open {
        state.tools.export_dialog.open = false;
    }
}

// ── Export worker ─────────────────────────────────────────────────────────────

fn start_export(state: &mut AppState) {
    let Some(lib) = state.library.library.clone() else {
        return;
    };
    let selected: Vec<_> = state.library.selected.clone();
    let photos: Vec<_> = state
        .library
        .results
        .iter()
        .filter(|p| selected.contains(&p.id))
        .cloned()
        .collect();

    let export = &state.tools.export_dialog;
    let format = export.format;
    let quality = export.jpeg_quality;
    let max_side = export.max_side;
    let dest_dir = export.dest_dir.clone();

    state.tools.export_dialog.progress = Some((0, photos.len()));
    state.tools.export_dialog.done = false;
    state.tools.export_dialog.errors.clear();

    std::thread::Builder::new()
        .name("rasterlab-export".into())
        .stack_size(32 * 1024 * 1024)
        .spawn(move || {
            let registry = FormatRegistry::with_builtins();
            for photo in &photos {
                let rlab_path = lib.rlab_path(&photo.hash);
                if let Err(e) = export_one(
                    &rlab_path,
                    &dest_dir,
                    &registry,
                    format,
                    quality,
                    max_side,
                    &photo.hash,
                ) {
                    eprintln!("export error {}: {e}", photo.hash);
                }
            }
        })
        .ok();
}

fn export_one(
    rlab_path: &std::path::Path,
    dest_dir: &std::path::Path,
    registry: &FormatRegistry,
    format: ExportFormat,
    quality: u8,
    max_side: Option<u32>,
    hash: &str,
) -> anyhow::Result<()> {
    let rlab = RlabFile::read(rlab_path)?;
    let hint = rlab.meta.source_path.as_deref().map(std::path::Path::new);
    let source = registry.decode_bytes(&rlab.original_bytes, hint)?;

    // Apply active virtual copy pipeline
    use rasterlab_core::pipeline::EditPipeline;
    use std::sync::Arc;
    let copy_idx = rlab.active_copy_index;
    let image_arc = if let Some(copy) = rlab.copies.get(copy_idx) {
        let source_arc = Arc::new(source);
        let mut pipeline = EditPipeline::new_virtual_copy(Arc::clone(&source_arc));
        pipeline.load_state(copy.pipeline_state.clone())?;
        pipeline.render()?
    } else {
        Arc::new(source)
    };
    let mut image = Arc::try_unwrap(image_arc).unwrap_or_else(|a| {
        // Arc has multiple owners (shouldn't happen here); make a shallow copy via raw
        use rasterlab_core::image::{Image, PixelFormat};
        Image {
            width: a.width,
            height: a.height,
            data: a.data.clone(),
            format: PixelFormat::Rgba8,
            metadata: a.metadata.clone(),
        }
    });

    // Resize if requested
    if let Some(max) = max_side {
        if image.width > max || image.height > max {
            let scale = max as f32 / image.width.max(image.height) as f32;
            let nw = ((image.width as f32 * scale).round() as u32).max(1);
            let nh = ((image.height as f32 * scale).round() as u32).max(1);
            use rasterlab_core::{ops::ResizeOp, traits::operation::Operation};
            image =
                ResizeOp::new(nw, nh, rasterlab_core::ops::ResampleMode::Bicubic).apply(image)?;
        }
    }

    // Encode
    let stem = rlab_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(hash);
    let filename = format!("{}.{}", stem, format.ext());
    let dest = dest_dir.join(&filename);
    let opts = rasterlab_core::traits::format_handler::EncodeOptions {
        jpeg_quality: quality,
        png_compression: 6,
        preserve_metadata: false,
    };
    let bytes = registry.encode_file(&image, &dest, &opts)?;
    std::fs::write(&dest, &bytes)?;
    Ok(())
}
