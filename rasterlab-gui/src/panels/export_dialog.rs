use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use egui::ScrollArea;
use rasterlab_core::{
    formats::FormatRegistry,
    library_meta::{FileTimeStamp, LibraryMeta},
    project::RlabFile,
};

use crate::state::AppState;

// ── Export dialog state ───────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ExportDialogState {
    pub open: bool,
    pub format: ExportFormat,
    pub jpeg_quality: u8,
    pub max_side: Option<u32>,
    pub dest_dir: PathBuf,
    /// `Some((done, total))` while an export is running, `None` when idle.
    pub progress: Option<(usize, usize)>,
    /// True when the last export finished (success or with errors).
    pub done: bool,
    pub errors: Vec<String>,
    /// Shared handle that the background worker updates as it progresses.
    /// Drained into `progress` / `done` / `errors` at the top of each
    /// dialog frame so the UI can react without spinning its own channel.
    pub shared: Arc<Mutex<ExportShared>>,
}

#[derive(Debug, Default)]
pub struct ExportShared {
    pub done: usize,
    pub total: usize,
    pub finished: bool,
    pub errors: Vec<String>,
}

impl ExportDialogState {
    /// Clear any stale progress/result state — called when the user re-opens
    /// the dialog so a completed prior export doesn't leave the Export button
    /// greyed out as "busy".
    pub fn reset_run_state(&mut self) {
        self.progress = None;
        self.done = false;
        self.errors.clear();
        if let Ok(mut s) = self.shared.lock() {
            *s = ExportShared::default();
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Jpeg,
    Png,
    /// Verbatim copy of the original imported bytes, with filesystem
    /// timestamps restored to their values at import time.
    Original,
}

impl ExportFormat {
    fn ext(self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            Self::Png => "png",
            // Unused for Original — the original filename is preserved instead.
            Self::Original => "",
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
            shared: Arc::new(Mutex::new(ExportShared::default())),
        }
    }
}

// ── UI ────────────────────────────────────────────────────────────────────────

pub fn ui(ctx: &egui::Context, state: &mut AppState) {
    if !state.tools.export_dialog.open {
        return;
    }

    // Pull the latest values from the worker into the dialog's view-state so
    // the progress bar, completion label, and Export-button enable flag all
    // reflect the current run.
    {
        let export = &mut state.tools.export_dialog;
        let snapshot = export
            .shared
            .lock()
            .map(|s| (s.done, s.total, s.finished, s.errors.clone()))
            .ok();
        if let Some((done, total, finished, errors)) = snapshot {
            if finished {
                export.progress = None;
                export.done = true;
                export.errors = errors;
            } else if total > 0 {
                export.progress = Some((done, total));
                export.done = false;
            }
        }
        // Also repaint while a run is in flight so progress ticks visibly.
        if export.progress.is_some() {
            ctx.request_repaint();
        }
    }

    let selected_count = state.library.selected.len();
    let mut open = state.tools.export_dialog.open;
    let mut do_export = false;
    let mut do_close = false;
    let mut browse_requested = false;

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
                        ui.selectable_value(&mut export.format, ExportFormat::Original, "Original");
                    });
                    ui.end_row();

                    if export.format == ExportFormat::Original {
                        ui.label("");
                        ui.label(
                            "Writes the unmodified imported bytes with the \
                             original filename and timestamps.",
                        );
                        ui.end_row();
                    }

                    if export.format == ExportFormat::Jpeg {
                        ui.label("Quality:");
                        ui.add(egui::Slider::new(&mut export.jpeg_quality, 1u8..=100));
                        ui.end_row();
                    }

                    if export.format != ExportFormat::Original {
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
                    }

                    ui.label("Destination:");
                    let mut browse_clicked = false;
                    ui.horizontal(|ui| {
                        let mut dir_str = export.dest_dir.display().to_string();
                        let resp =
                            ui.add(egui::TextEdit::singleline(&mut dir_str).desired_width(200.0));
                        if resp.changed() {
                            export.dest_dir = PathBuf::from(&dir_str);
                        }
                        #[cfg(not(target_arch = "wasm32"))]
                        if ui.button("Browse…").clicked() {
                            browse_clicked = true;
                        }
                    });
                    ui.end_row();
                    // Defer the actual folder pick to the outer app, which owns
                    // the unified FileChooser — this works across Wayland/
                    // waypipe where a raw rfd call from the UI thread does not.
                    if browse_clicked {
                        browse_requested = true;
                    }
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
    if browse_requested {
        state.tools.export_dest_dialog_requested = true;
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
    let shared = Arc::clone(&export.shared);

    let total = photos.len();
    state.tools.export_dialog.progress = Some((0, total));
    state.tools.export_dialog.done = false;
    state.tools.export_dialog.errors.clear();
    if let Ok(mut s) = shared.lock() {
        *s = ExportShared {
            done: 0,
            total,
            finished: false,
            errors: Vec::new(),
        };
    }

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
                    if let Ok(mut s) = shared.lock() {
                        s.errors.push(format!("{}: {e}", photo.hash));
                    }
                }
                if let Ok(mut s) = shared.lock() {
                    s.done += 1;
                }
            }
            if let Ok(mut s) = shared.lock() {
                s.finished = true;
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

    // ── Original export: write the verbatim ORIG bytes and restore mtime/atime. ──
    if format == ExportFormat::Original {
        return export_original(rlab_path, dest_dir, &rlab, hash);
    }

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
    if let Some(max) = max_side
        && (image.width > max || image.height > max)
    {
        let scale = max as f32 / image.width.max(image.height) as f32;
        let nw = ((image.width as f32 * scale).round() as u32).max(1);
        let nh = ((image.height as f32 * scale).round() as u32).max(1);
        use rasterlab_core::{ops::ResizeOp, traits::operation::Operation};
        image = ResizeOp::new(nw, nh, rasterlab_core::ops::ResampleMode::Bicubic).apply(image)?;
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

// ── Original-bytes export ────────────────────────────────────────────────────

fn export_original(
    rlab_path: &Path,
    dest_dir: &Path,
    rlab: &RlabFile,
    hash: &str,
) -> anyhow::Result<()> {
    let filename = original_filename_for(rlab, rlab_path, hash);
    let dest = unique_dest_path(dest_dir, &filename);
    std::fs::write(&dest, &rlab.original_bytes)?;
    apply_source_timestamps(&dest, rlab.lmta.as_ref());
    Ok(())
}

/// Pick the best filename to use for the original-bytes export.
///
/// Priority:
/// 1. `LibraryMeta::original_filename` (recorded at import).
/// 2. `RlabMeta::source_path` basename (older imports / non-library files).
/// 3. `{rlab-stem}` or `{hash}` as a last-ditch fallback.
fn original_filename_for(rlab: &RlabFile, rlab_path: &Path, hash: &str) -> String {
    if let Some(lmta) = &rlab.lmta
        && let Some(name) = &lmta.original_filename
        && !name.is_empty()
    {
        return name.clone();
    }
    if let Some(src) = rlab.meta.source_path.as_deref()
        && let Some(name) = Path::new(src).file_name().and_then(|n| n.to_str())
        && !name.is_empty()
    {
        return name.to_owned();
    }
    rlab_path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_owned())
        .unwrap_or_else(|| hash.to_owned())
}

/// Avoid clobbering an existing file in the destination directory by appending
/// ` (2)`, ` (3)`, … before the extension.
fn unique_dest_path(dest_dir: &Path, filename: &str) -> PathBuf {
    let initial = dest_dir.join(filename);
    if !initial.exists() {
        return initial;
    }
    let path = Path::new(filename);
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(filename);
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
    for n in 2u32..10_000 {
        let candidate = if ext.is_empty() {
            format!("{stem} ({n})")
        } else {
            format!("{stem} ({n}).{ext}")
        };
        let p = dest_dir.join(candidate);
        if !p.exists() {
            return p;
        }
    }
    initial
}

/// Best-effort restore of the source file's mtime / atime. Failures are logged
/// but not propagated — the bytes were written successfully, and on some
/// filesystems (e.g. FAT, read-only mounts) timestamp updates may be denied.
fn apply_source_timestamps(dest: &Path, lmta: Option<&LibraryMeta>) {
    let Some(lmta) = lmta else {
        return;
    };
    // Fall back to atime=mtime when the stored atime is missing so the file
    // does not get "touched" to "now" by the write itself.
    let mtime = lmta.source_mtime;
    let atime = lmta.source_atime.or(mtime);
    if let (Some(m), Some(a)) = (mtime, atime) {
        let mft = filetime_from_stamp(m);
        let aft = filetime_from_stamp(a);
        if let Err(e) = filetime::set_file_times(dest, aft, mft) {
            eprintln!(
                "export: could not restore timestamps on {}: {e}",
                dest.display()
            );
        }
    }
}

fn filetime_from_stamp(ts: FileTimeStamp) -> filetime::FileTime {
    filetime::FileTime::from_system_time(ts.to_system_time())
}
