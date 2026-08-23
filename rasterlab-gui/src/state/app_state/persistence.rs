//! Project and session persistence: opening images and `.rlab` projects,
//! exporting rendered output, saving projects, dirty tracking against the last
//! clean boundary, and the autosave session.

use std::sync::Arc;

use rasterlab_core::{
    Image,
    formats::FormatRegistry,
    ops::{MaskedOp, ResizeOp},
    pipeline::EditPipeline,
    project::{RlabFile, RlabMeta},
    traits::format_handler::EncodeOptions,
    traits::operation::Operation,
    verified_write::write_atomic,
};

use crate::panels::{
    export_border::ExportBorderOptions,
    export_dialog::{SizeConstraint, constrained_dimensions},
};
use crate::state::VirtualCopyStore;

use super::{AppMode, AppState, BgMessage, workers};

#[derive(Debug, Clone, Copy)]
enum RenderedExportResize {
    None,
    Constraint(SizeConstraint),
}

impl AppState {
    // -----------------------------------------------------------------------
    // Load results
    // -----------------------------------------------------------------------

    /// Adopt a freshly decoded source image as the open document.
    pub(super) fn on_image_loaded(
        &mut self,
        path: std::path::PathBuf,
        image: Image,
        original_bytes: Vec<u8>,
    ) {
        let w = image.width;
        let h = image.height;
        self.reset_tools_for_new_image(w, h);
        self.last_path = Some(path.clone());
        self.original_bytes = Some(original_bytes);
        self.project_path = None;
        self.is_dirty = false;
        self.clean_edit_state = None;
        self.project_created_at = None;
        self.project_lmta = None;
        self.status = format!("Opened {}  ({}×{})", path.display(), w, h);
        self.rename_pending = None;

        self.begin_autosave_session();

        if let Some((saved_copies, saved_active)) = self.autosave_restore.take() {
            let image_arc = Arc::new(image);
            let clean_store = VirtualCopyStore::new(
                "Copy 1".into(),
                EditPipeline::new_virtual_copy(Arc::clone(&image_arc)),
            );
            self.clean_edit_state = clean_store.edit_state_snapshot().ok();
            match VirtualCopyStore::load_from_saved(
                Arc::clone(&image_arc),
                saved_copies,
                saved_active,
            ) {
                Ok(store) => {
                    self.copies = Some(store);
                    self.mark_dirty();
                }
                Err(e) => {
                    self.status = format!("Warning: could not restore edit stack: {}", e);
                    self.copies = Some(clean_store);
                }
            }
        } else {
            self.copies = Some(VirtualCopyStore::new(
                "Copy 1".into(),
                EditPipeline::new(image),
            ));
            self.capture_clean_edit_state();
        }

        self.prefs.push_recent(path, None);
        self.prefs.save();
        self.loading = false;
        self.image_generation += 1;
        self.request_render();
    }

    /// Adopt a decoded `.rlab` project as the open document.
    pub(super) fn on_project_loaded(
        &mut self,
        path: std::path::PathBuf,
        mut rlab: Box<RlabFile>,
        image: Image,
    ) {
        rlab.resolve_relative_paths(path.parent().unwrap_or_else(|| std::path::Path::new(".")));
        let w = image.width;
        let h = image.height;
        self.reset_tools_for_new_image(w, h);
        self.last_path = rlab
            .meta
            .source_path
            .as_deref()
            .map(std::path::PathBuf::from)
            .or_else(|| Some(path.clone()));
        self.project_created_at = Some(rlab.meta.created_at);
        self.project_lmta = rlab.lmta.clone();
        self.original_bytes = Some(rlab.original_bytes.clone());
        self.project_path = Some(path.clone());
        self.is_dirty = false;
        self.clean_edit_state = None;
        self.copies = None;
        self.status = format!("Opened {}  ({}×{})", path.display(), w, h);
        self.rename_pending = None;

        self.begin_autosave_session();

        let display_name = rlab.lmta.as_ref().and_then(|l| l.original_filename.clone());
        let source = Arc::new(image);
        if let Some((saved_copies, saved_active)) = self.autosave_restore.take() {
            // Establish the clean boundary from the deserialised project stack.
            // Some operation values (notably f32s) normalise during load, so the
            // raw JSON is not always byte-for-byte equivalent to its in-memory
            // form.
            self.clean_edit_state = VirtualCopyStore::load_from_saved(
                Arc::clone(&source),
                rlab.copies,
                rlab.active_copy_index,
            )
            .ok()
            .and_then(|store| store.edit_state_snapshot().ok());
            match VirtualCopyStore::load_from_saved(source, saved_copies, saved_active) {
                Ok(store) => {
                    self.copies = Some(store);
                    self.mark_dirty();
                }
                Err(e) => {
                    self.status = format!("Warning: could not restore edit stack: {}", e);
                }
            }
        } else {
            match VirtualCopyStore::load_from_saved(source, rlab.copies, rlab.active_copy_index) {
                Ok(store) => {
                    self.copies = Some(store);
                    self.capture_clean_edit_state();
                }
                Err(e) => {
                    self.status = format!("Warning: could not restore edit stack: {}", e);
                }
            }
        }
        self.prefs.push_recent(path, display_name);
        self.prefs.save();
        self.loading = false;
        self.image_generation += 1;
        self.request_render();
    }

    // -----------------------------------------------------------------------
    // File I/O
    // -----------------------------------------------------------------------

    /// Begin loading `path` in a background thread.
    ///
    /// Dispatches on the file extension: `.rlab` files are loaded as projects
    /// (restoring the full edit stack); all other files are loaded as source images.
    pub fn open_file(&mut self, path: std::path::PathBuf) {
        self.loading = true;
        self.status = format!("Loading {}…", path.display());
        self.mode = AppMode::Editor;

        // Clear the canvas so the previous image doesn't flash while the new
        // one is still decoding/rendering in the background.
        self.rendered = None;
        self.preview_overlay = None;
        self.preview_overlay_rect = None;
        self.histogram = None;

        let is_project = path
            .extension()
            .map(|e| e.eq_ignore_ascii_case("rlab"))
            .unwrap_or(false);

        // Decoders are third-party code (e.g. rawler) and not guaranteed
        // panic-free on malformed input, so the worker is spawned through the
        // helper that converts a panic — or a thread that never starts — into a
        // `BgMessage::Error`. That is what clears `loading`; without it the UI
        // would sit on "Loading…" for the rest of the session.
        workers::spawn(
            "rasterlab-load",
            workers::IMAGE_WORKER_STACK,
            self.bg_tx.clone(),
            self.ctx.clone(),
            |message| {
                BgMessage::Error(format!(
                    "{message} (the file may be corrupt or an unsupported camera variant)"
                ))
            },
            move || {
                if is_project {
                    match RlabFile::read(&path) {
                        Ok(rlab) => {
                            let registry = FormatRegistry::with_builtins();
                            let hint = rlab.meta.source_path.as_deref().map(std::path::Path::new);
                            match registry.decode_bytes(&rlab.original_bytes, hint) {
                                Ok(image) => BgMessage::ProjectLoaded {
                                    path,
                                    rlab: Box::new(rlab),
                                    image,
                                },
                                Err(e) => BgMessage::Error(e.to_string()),
                            }
                        }
                        Err(e) => BgMessage::Error(e.to_string()),
                    }
                } else {
                    // Read the raw bytes for storage in .rlab saves, then decode.
                    match std::fs::read(&path) {
                        Ok(original_bytes) => {
                            let registry = FormatRegistry::with_builtins();
                            match registry.decode_file(&path) {
                                Ok(image) => BgMessage::ImageLoaded {
                                    path,
                                    image,
                                    original_bytes,
                                },
                                Err(e) => BgMessage::Error(e.to_string()),
                            }
                        }
                        Err(e) => BgMessage::Error(e.to_string()),
                    }
                }
            },
        );
    }

    /// Export the current editor document using the options from the shared
    /// export dialog. This is the standalone-file counterpart to library batch
    /// export and deliberately uses the same resize, quality, and border rules.
    pub(crate) fn save_file_with_dialog_options(
        &mut self,
        path: std::path::PathBuf,
        size_constraint: Option<SizeConstraint>,
        encode_opts: &EncodeOptions,
        border: &ExportBorderOptions,
    ) -> Result<usize, String> {
        let resize = size_constraint.map_or(RenderedExportResize::None, |constraint| {
            RenderedExportResize::Constraint(constraint)
        });
        self.finish_rendered_export(path, resize, border, encode_opts)
    }

    fn finish_rendered_export(
        &mut self,
        path: std::path::PathBuf,
        resize: RenderedExportResize,
        border: &ExportBorderOptions,
        encode_opts: &EncodeOptions,
    ) -> Result<usize, String> {
        let result = self.write_rendered_export(&path, resize, border, encode_opts);
        match &result {
            Ok(bytes) => {
                self.status = format!("Saved {} bytes → {}", bytes, path.display());
                // Exporting a rendered image counts as preserving the user's
                // work, matching the pre-dialog standalone export behaviour.
                self.capture_clean_edit_state();
            }
            Err(error) => self.status = error.clone(),
        }
        result
    }

    fn write_rendered_export(
        &mut self,
        path: &std::path::Path,
        resize: RenderedExportResize,
        border: &ExportBorderOptions,
        encode_opts: &EncodeOptions,
    ) -> Result<usize, String> {
        // The canvas is a presentation cache, not an export source. During a
        // live tool preview it deliberately contains a quarter-resolution
        // image, and exporting that buffer permanently bakes in the reduced
        // resolution and its downsampled tonal range. Render the committed
        // pipeline at source resolution and then apply the current preview op
        // at full resolution so export matches what the user sees.
        let preview_op = self.tools.preview_op().map(|preview| {
            let edit_mask = self.editing.and_then(|session| {
                self.pipeline()
                    .and_then(|pipeline| pipeline.ops().get(session.op_index))
                    .and_then(|entry| entry.operation.as_any())
                    .and_then(|any| any.downcast_ref::<MaskedOp>())
                    .map(|masked| masked.mask.clone())
            });
            if let Some(mask) = edit_mask {
                Box::new(MaskedOp {
                    inner: preview,
                    mask,
                }) as Box<dyn Operation>
            } else {
                preview
            }
        });
        let Some(pipeline) = self.pipeline_mut() else {
            return Err("Nothing to save — open an image first".into());
        };
        let mut rendered = match pipeline.render() {
            Ok(image) => image,
            Err(e) => return Err(format!("Export render failed: {e}")),
        };
        if let Some(preview) = preview_op {
            let image =
                Arc::try_unwrap(rendered).unwrap_or_else(|shared| shared.as_ref().deep_clone());
            rendered = match preview.apply(image) {
                Ok(image) => Arc::new(image),
                Err(e) => return Err(format!("Export preview render failed: {e}")),
            };
        }
        // Captions describe the source exposure. Read them from the immutable
        // pipeline source rather than trusting every pixel operation to carry
        // EXIF through its output buffer.
        let source_metadata = self.image_metadata().cloned().unwrap_or_default();

        // Optionally resize before encoding using the shared dialog's
        // long-side / megapixel constraints.
        let resize_target = match resize {
            RenderedExportResize::None => None,
            RenderedExportResize::Constraint(constraint) => {
                let (width, height) =
                    constrained_dimensions(rendered.width, rendered.height, constraint);
                (width != rendered.width || height != rendered.height).then_some((width, height))
            }
        };
        let resized_buf;
        let to_save: &Image = if let Some((width, height)) = resize_target {
            let op = ResizeOp::new(width, height, rasterlab_core::ops::ResampleMode::Bicubic);
            match op.apply(rendered.as_ref().deep_clone()) {
                Ok(img) => {
                    resized_buf = img;
                    &resized_buf
                }
                Err(e) => return Err(format!("Export resize failed: {e}")),
            }
        } else {
            rendered.as_ref()
        };

        let bordered_buf;
        let to_encode = if border.enabled {
            match crate::panels::export_border::apply_export_border(
                to_save,
                &source_metadata,
                border,
            ) {
                Ok(image) => {
                    bordered_buf = image;
                    &bordered_buf
                }
                Err(e) => return Err(format!("Export border failed: {e}")),
            }
        } else {
            to_save
        };

        let bytes = self
            .registry
            .encode_file(to_encode, path, encode_opts)
            .map_err(|e| format!("Encode failed: {e}"))?;
        write_atomic(path, &bytes).map_err(|e| format!("Write failed: {e}"))?;
        Ok(bytes.len())
    }

    /// Export the current edit stack to a JSON file consumable by the CLI.
    ///
    /// The resulting file can be passed to `rasterlab process --load-pipeline <path>`
    /// or `rasterlab batch --load-pipeline <path>` to replay the same edits on
    /// any image without opening the GUI.
    pub fn export_edit_stack_json(&mut self, path: std::path::PathBuf) {
        let Some(pipeline) = self.pipeline() else {
            self.status = "No edit stack to export".into();
            return;
        };
        let state = match pipeline.save_state() {
            Ok(s) => s,
            Err(e) => {
                self.status = format!("Export failed: {}", e);
                return;
            }
        };
        let json = match serde_json::to_string_pretty(&state) {
            Ok(j) => j,
            Err(e) => {
                self.status = format!("JSON serialisation failed: {}", e);
                return;
            }
        };
        match write_atomic(&path, json.as_bytes()) {
            Ok(()) => self.status = format!("Edit stack exported → {}", path.display()),
            Err(e) => self.status = format!("Export failed: {}", e),
        }
    }

    /// Save the current project to `path` as a `.rlab` file.
    pub fn save_project(&mut self, path: std::path::PathBuf) {
        // An edit session temporarily disables the committed operation while
        // its replacement is shown as a live preview. Neither state is a valid
        // save boundary: serialising now would persist the disabled operation
        // but omit the preview. Keep this guard here as well as in the UI so a
        // chooser that was already open (or any other direct caller) cannot
        // write transient state.
        if self.editing.is_some() {
            self.status = "Finish or cancel the active edit before saving".into();
            return;
        }
        let Some(original_bytes) = self.original_bytes.clone() else {
            self.status = "Nothing to save — open an image first".into();
            return;
        };
        let Some(store) = &mut self.copies else {
            self.status = "Nothing to save — no active pipeline".into();
            return;
        };

        // Render the committed active copy once and carry its thumbnail in the
        // same authoritative write. Previously the save omitted PREV and then
        // thumbnail regeneration read and rewrote the whole remote container.
        let rendered = match store.active_pipeline_mut().render() {
            Ok(image) => image,
            Err(e) => {
                self.status = format!("Save failed (thumbnail render): {e}");
                return;
            }
        };
        let thumbnail = match rasterlab_library::thumbnail::generate_thumbnail(&rendered, 512) {
            Ok(bytes) => bytes,
            Err(e) => {
                self.status = format!("Save failed (thumbnail): {e}");
                return;
            }
        };

        let (copies_saved, active_idx) = match store.save_states() {
            Ok(s) => s,
            Err(e) => {
                self.status = format!("Save failed (pipeline): {}", e);
                return;
            }
        };

        let source = store.source();
        let (w, h) = (source.width, source.height);
        let source_path = self
            .last_path
            .as_deref()
            .and_then(|p| p.to_str())
            .map(String::from);
        let app_version = env!("CARGO_PKG_VERSION").to_string();

        let mut meta = RlabMeta::new(app_version, source_path, w, h);
        // Preserve the original creation timestamp on in-place re-saves.
        if let Some(created_at) = self.project_created_at {
            meta.created_at = created_at;
        }
        meta = meta.touch();

        let created_at = meta.created_at;
        let mut rlab = RlabFile::new(
            meta,
            original_bytes,
            copies_saved,
            active_idx,
            Some(thumbnail.clone()),
        );
        rlab.set_lmta(self.project_lmta.clone());
        let has_edits = rlab.has_edits();
        // v4 adds Reed-Solomon parity so the file is repairable by an integrity
        // scrub; this also avoids downgrading a library photo that was imported
        // as v4 when its edits are saved back in place.
        let library_target = self
            .library_context
            .as_ref()
            .and_then(|(_, hash)| self.library.library.clone().map(|lib| (lib, hash.clone())))
            .filter(|(lib, hash)| lib.rlab_path(hash) == path);
        let write_result = match &library_target {
            Some((lib, hash)) => lib
                .save_edited_project(hash, rlab)
                .map_err(|error| error.to_string()),
            None => rlab
                .write_v5(&path)
                .map(|()| self.project_lmta.clone())
                .map_err(|error| error.to_string()),
        };
        match write_result {
            Ok(saved_lmta) => {
                self.project_lmta = saved_lmta;
                self.project_created_at = Some(created_at);
                self.project_path = Some(path.clone());
                self.capture_clean_edit_state();
                // Clean up the autosave file now that the work is safely saved.
                if let Some(session_id) = self.autosave_session_id.take() {
                    crate::autosave::delete(session_id);
                }
                self.status = format!("Saved → {}", path.display());

                // The PREV bytes were produced from this exact saved pipeline;
                // publish the same bytes to the library's derived thumbnail
                // cache without reopening or rewriting the container.
                //
                // Only when the write landed on the library's own file for
                // this hash. `library_context` outlives the document it was
                // set for — a Save As, or opening an unrelated file, leaves it
                // pointing at a photo this render has nothing to do with, and
                // publishing then puts one photo's thumbnail on another.
                if let Some((lib, hash)) = &library_target {
                    if let Err(e) = lib.update_thumbnail_cache(hash, &thumbnail, has_edits) {
                        self.status = format!(
                            "Saved project, but could not update its library thumbnail: {e}"
                        );
                    } else {
                        self.library.thumbs.remove(hash);
                    }
                }
            }
            Err(e) => {
                self.status = format!("Save failed: {e}");
            }
        }
    }

    // -----------------------------------------------------------------------
    // Dirty tracking
    // -----------------------------------------------------------------------

    /// Recompute whether the effective edit state differs from the last open or
    /// save boundary, and schedule an autosave only while changes remain.
    pub(crate) fn mark_dirty(&mut self) {
        let was_dirty = self.is_dirty;
        self.is_dirty = match (&self.clean_edit_state, &self.copies) {
            (Some(clean), Some(store)) => store
                .edit_state_snapshot()
                .map_or(true, |current| current != *clean),
            _ => true,
        };
        self.autosave_pending = self.is_dirty;

        // An autosave from an earlier edit must not survive after the user has
        // returned all copies to their clean state.
        if was_dirty
            && !self.is_dirty
            && let Some(session_id) = self.autosave_session_id
        {
            crate::autosave::delete(session_id);
        }
    }

    /// Make the current effective edit state the clean comparison boundary.
    fn capture_clean_edit_state(&mut self) {
        self.clean_edit_state = self
            .copies
            .as_ref()
            .and_then(|store| store.edit_state_snapshot().ok());
        self.is_dirty = false;
        self.autosave_pending = false;
    }

    // -----------------------------------------------------------------------
    // Autosave
    // -----------------------------------------------------------------------

    /// Start (or adopt) the autosave session for a newly opened document.
    ///
    /// Reuses the session ID from an autosave restore so the original autosave
    /// file is correctly cleaned up on project save; otherwise mints a fresh one.
    fn begin_autosave_session(&mut self) {
        self.autosave_session_id = Some(
            self.autosave_restore_session_id
                .take()
                .unwrap_or_else(crate::autosave::unix_now),
        );
        self.autosave_pending = false;
    }

    /// Write the autosave file if a change is pending.  Called every frame from
    /// `poll_background`; is a no-op when nothing has changed.
    pub(super) fn maybe_write_autosave(&mut self) {
        if !self.autosave_pending {
            return;
        }
        let Some(session_id) = self.autosave_session_id else {
            return;
        };
        let Some(source_path) = self.last_path.clone() else {
            return;
        };
        let Some(store) = &self.copies else { return };
        let Ok((copies, active)) = store.save_states() else {
            return;
        };
        let display_name = self
            .project_path
            .as_deref()
            .map(|path| self.prefs.recent_display_name(path))
            .or_else(|| {
                source_path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            });
        crate::autosave::write(
            session_id,
            &source_path,
            self.project_path.as_deref(),
            display_name.as_deref(),
            &copies,
            active,
        );
        self.autosave_pending = false;
    }

    /// Begin restoring an autosave session.
    ///
    /// Stores the pipeline data from `entry` and opens the project file when
    /// available, falling back to the original source image. When loading
    /// finishes, the autosaved pipeline state is applied automatically.
    pub fn restore_autosave(&mut self, entry: crate::autosave::AutosaveEntry) {
        let restore_path = entry
            .data
            .project_path
            .as_deref()
            .map(std::path::PathBuf::from)
            .filter(|p| p.exists())
            .unwrap_or_else(|| std::path::PathBuf::from(&entry.data.source_path));
        self.autosave_restore = Some((entry.data.copies, entry.data.active_copy));
        self.autosave_restore_session_id = Some(entry.data.started_at);
        self.open_file(restore_path);
    }
}

#[cfg(test)]
mod tests {
    use rasterlab_core::{
        Image,
        formats::FormatRegistry,
        ops::{BrightnessContrastOp, SaturationOp},
        pipeline::EditPipeline,
        project::RlabFile,
    };

    use super::*;

    fn state_with_saturation(strength: f32) -> AppState {
        let mut pipeline = EditPipeline::new(Image::new(8, 8));
        pipeline.push_op(Box::new(SaturationOp::new(strength)));
        let mut state = AppState::new(egui::Context::default(), None);
        state.copies = Some(VirtualCopyStore::new("Copy 1".into(), pipeline));
        state.original_bytes = Some(vec![1, 2, 3, 4]);
        state
    }

    fn saved_entry(path: &std::path::Path) -> serde_json::Value {
        RlabFile::read(path).unwrap().copies[0]
            .pipeline_state
            .entries[0]
            .clone()
    }

    #[test]
    fn export_uses_full_resolution_pipeline_and_live_preview_not_canvas_cache() {
        use crate::panels::tools::brightness_contrast::BrightnessContrastTool;

        let mut source = Image::new(8, 6);
        for y in 0..source.height {
            for x in 0..source.width {
                source.set_pixel(
                    x,
                    y,
                    [(x * 23) as u8, (y * 31) as u8, ((x + y) * 17) as u8, 255],
                );
            }
        }
        let expected = BrightnessContrastOp::new(0.2, 0.1)
            .apply(source.deep_clone())
            .unwrap();
        let pipeline = EditPipeline::new(source);
        let mut state = AppState::new(egui::Context::default(), None);
        state.copies = Some(VirtualCopyStore::new("Copy 1".into(), pipeline));

        // Reproduce the bad state: the canvas currently holds the fast 25%
        // preview while a tool's unapplied values are visible.
        state.rendered = Some(Arc::new(Image::new(2, 2)));
        state.rendered_is_preview = true;
        state.rendered_scale = rasterlab_render::PREVIEW_SCALE;
        let tool = state.tools.find_mut::<BrightnessContrastTool>().unwrap();
        tool.brightness = 0.2;
        tool.contrast = 0.1;
        tool.preview_active = true;

        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("full-resolution.png");
        state
            .save_file_with_dialog_options(
                output.clone(),
                None,
                &EncodeOptions::default(),
                &ExportBorderOptions::default(),
            )
            .unwrap();

        let exported = FormatRegistry::with_builtins()
            .decode_file(&output)
            .unwrap();
        assert_eq!((exported.width, exported.height), (8, 6));
        assert_eq!(exported.data, expected.data);
    }

    #[test]
    fn dialog_export_applies_the_library_resize_constraint_to_standalone_files() {
        let source = Image::new(400, 200);
        let pipeline = EditPipeline::new(source);
        let mut state = AppState::new(egui::Context::default(), None);
        state.copies = Some(VirtualCopyStore::new("Copy 1".into(), pipeline));

        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("constrained.png");
        state
            .save_file_with_dialog_options(
                output.clone(),
                Some(SizeConstraint::LongSide(100)),
                &EncodeOptions::default(),
                &ExportBorderOptions::default(),
            )
            .unwrap();

        let exported = FormatRegistry::with_builtins()
            .decode_file(&output)
            .unwrap();
        assert_eq!((exported.width, exported.height), (100, 50));
    }

    #[test]
    fn dialog_export_keeps_exif_when_the_preference_says_to() {
        // The unified dialog replaced a path that honoured the user's
        // preserve-metadata preference, so the option has to survive the trip
        // from the dialog through to the encoder.
        let mut source = Image::new(8, 8);
        source.metadata.raw_exif = Some(b"Exif\0\0II*\0\x08\0\0\0\0\0".to_vec());
        let mut state = AppState::new(egui::Context::default(), None);
        state.copies = Some(VirtualCopyStore::new(
            "Copy 1".into(),
            EditPipeline::new(source),
        ));

        let dir = tempfile::tempdir().unwrap();
        let kept = dir.path().join("kept.jpg");
        let stripped = dir.path().join("stripped.jpg");

        state
            .save_file_with_dialog_options(
                kept.clone(),
                None,
                &EncodeOptions {
                    preserve_metadata: true,
                    ..EncodeOptions::default()
                },
                &ExportBorderOptions::default(),
            )
            .unwrap();
        state
            .save_file_with_dialog_options(
                stripped.clone(),
                None,
                &EncodeOptions {
                    preserve_metadata: false,
                    ..EncodeOptions::default()
                },
                &ExportBorderOptions::default(),
            )
            .unwrap();

        let contains_exif = |path: &std::path::Path| {
            std::fs::read(path)
                .unwrap()
                .windows(4)
                .any(|w| w == b"Exif")
        };
        assert!(contains_exif(&kept));
        assert!(!contains_exif(&stripped));
    }

    #[test]
    fn save_during_edit_then_cancel_never_persists_the_transient_disabled_op() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cancel.rlab");
        let mut state = state_with_saturation(0.5);
        state.begin_edit(0);

        state.save_project(path.clone());

        assert!(!path.exists());
        assert!(state.editing.is_some());
        assert!(!state.pipeline().unwrap().ops()[0].enabled);
        assert_eq!(
            state.status,
            "Finish or cancel the active edit before saving"
        );

        state.end_edit();
        state.save_project(path.clone());

        assert!(saved_entry(&path)["enabled"].as_bool().unwrap());
        assert!(state.project_path.is_some());
        assert!(!state.is_dirty);
    }

    #[test]
    fn save_during_edit_then_apply_never_marks_the_unsaved_edit_clean() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("apply.rlab");
        let mut state = state_with_saturation(0.5);
        state.begin_edit(0);

        state.save_project(path.clone());
        state.commit_edit(Box::new(SaturationOp::new(1.25)));

        assert!(!path.exists());
        assert!(state.project_path.is_none());
        assert!(state.is_dirty);

        state.save_project(path.clone());

        let entry = saved_entry(&path);
        assert!(entry["enabled"].as_bool().unwrap());
        assert_eq!(entry["operation"]["saturation"].as_f64().unwrap(), 1.25);
        assert!(!state.is_dirty);
    }

    #[test]
    fn save_as_leaves_the_source_photos_library_thumbnail_alone() {
        // `library_context` survives a Save As, so the only thing separating
        // "update this photo's thumbnail" from "overwrite an unrelated
        // photo's" is whether the write actually landed on the library file.
        let dir = tempfile::tempdir().unwrap();
        let library = std::sync::Arc::new(
            rasterlab_library::Library::open_or_create(&dir.path().join("library")).unwrap(),
        );
        let hash = "0".repeat(64);
        let thumb_path = library.thumb_path(&hash);
        std::fs::create_dir_all(thumb_path.parent().unwrap()).unwrap();
        std::fs::write(&thumb_path, b"the thumbnail this photo already had").unwrap();

        let mut state = state_with_saturation(0.5);
        state.library.library = Some(std::sync::Arc::clone(&library));
        state.library_context = Some((library.root().to_path_buf(), hash.clone()));

        state.save_project(dir.path().join("somewhere-else.rlab"));

        assert_eq!(
            std::fs::read(&thumb_path).unwrap(),
            b"the thumbnail this photo already had"
        );
    }

    #[test]
    fn save_uses_cached_lmta_and_embeds_the_current_thumbnail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cached-metadata.rlab");
        let mut state = state_with_saturation(0.5);
        state.project_lmta = Some(rasterlab_core::library_meta::LibraryMeta {
            caption: Some("kept without rereading the destination".into()),
            ..Default::default()
        });

        state.save_project(path.clone());

        let saved = RlabFile::read(&path).unwrap();
        assert_eq!(
            saved.lmta.and_then(|lmta| lmta.caption),
            Some("kept without rereading the destination".into())
        );
        assert!(saved.thumbnail.is_some());
    }
}
