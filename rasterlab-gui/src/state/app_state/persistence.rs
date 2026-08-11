//! Project and session persistence: opening images and `.rlab` projects,
//! exporting rendered output, saving projects, dirty tracking against the last
//! clean boundary, and the autosave session.

use std::sync::Arc;

use rasterlab_core::{
    Image,
    formats::FormatRegistry,
    ops::ResizeOp,
    pipeline::EditPipeline,
    project::{RlabFile, RlabMeta},
    traits::operation::Operation,
};

use crate::state::VirtualCopyStore;

use super::{AppMode, AppState, BgMessage};

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
        rlab: Box<RlabFile>,
        image: Image,
    ) {
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

        let tx = self.bg_tx.clone();
        let ctx = self.ctx.clone();

        let is_project = path
            .extension()
            .map(|e| e.eq_ignore_ascii_case("rlab"))
            .unwrap_or(false);

        std::thread::Builder::new()
            .name("rasterlab-load".into())
            .stack_size(32 * 1024 * 1024)
            .spawn(move || {
                // Decoders are third-party code (e.g. rawler) and not guaranteed
                // panic-free on malformed input. A panic here would unwind the
                // thread without ever sending a message, leaving the UI stuck on
                // "Loading…" forever. Contain it so a bad file becomes an error.
                let path_label = path.display().to_string();
                let computed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    if is_project {
                        match RlabFile::read(&path) {
                            Ok(rlab) => {
                                let registry = FormatRegistry::with_builtins();
                                let hint =
                                    rlab.meta.source_path.as_deref().map(std::path::Path::new);
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
                }));
                let msg = computed.unwrap_or_else(|_| {
                    BgMessage::Error(format!(
                        "Failed to load {path_label}: decoder panicked \
                         (corrupt file or unsupported camera variant)"
                    ))
                });
                let _ = tx.send(msg);
                ctx.request_repaint();
            })
            .expect("failed to spawn load thread");
    }

    pub fn save_file(&mut self, path: std::path::PathBuf) {
        let Some(rendered) = &self.rendered else {
            self.status = "Nothing to save — render first".into();
            return;
        };
        // Captions describe the source exposure. Read them from the immutable
        // pipeline source rather than trusting every pixel operation to carry
        // EXIF through its output buffer.
        let source_metadata = self.image_metadata().cloned().unwrap_or_default();

        // Optionally resize before encoding.
        let resized_buf;
        let to_save: &Image = if self.tools.export_resize_enabled
            && self.tools.export_resize_w > 0
            && self.tools.export_resize_h > 0
        {
            let op = ResizeOp::new(
                self.tools.export_resize_w,
                self.tools.export_resize_h,
                self.tools.export_resize_mode,
            );
            match op.apply(rendered.as_ref().deep_clone()) {
                Ok(img) => {
                    resized_buf = img;
                    &resized_buf
                }
                Err(e) => {
                    self.status = format!("Export resize failed: {}", e);
                    return;
                }
            }
        } else {
            rendered.as_ref()
        };

        let bordered_buf;
        let to_encode = if self.tools.export_border.enabled {
            match crate::panels::export_border::apply_export_border(
                to_save,
                &source_metadata,
                &self.tools.export_border,
            ) {
                Ok(image) => {
                    bordered_buf = image;
                    &bordered_buf
                }
                Err(e) => {
                    self.status = format!("Export border failed: {e}");
                    return;
                }
            }
        } else {
            to_save
        };

        match self
            .registry
            .encode_file(to_encode, &path, &self.tools.encode_opts)
        {
            Ok(bytes) => {
                if let Err(e) = std::fs::write(&path, &bytes) {
                    self.status = format!("Write failed: {}", e);
                } else {
                    self.status = format!("Saved {} bytes → {}", bytes.len(), path.display());
                    // Exporting a rendered image counts as preserving the
                    // user's work, so clear the dirty flag — this keeps the
                    // exit confirmation from firing after a successful export.
                    self.capture_clean_edit_state();
                }
            }
            Err(e) => {
                self.status = format!("Encode failed: {}", e);
            }
        }
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
        match std::fs::write(&path, json) {
            Ok(()) => self.status = format!("Edit stack exported → {}", path.display()),
            Err(e) => self.status = format!("Export failed: {}", e),
        }
    }

    /// Save the current project to `path` as a `.rlab` file.
    pub fn save_project(&mut self, path: std::path::PathBuf) {
        let Some(original_bytes) = self.original_bytes.clone() else {
            self.status = "Nothing to save — open an image first".into();
            return;
        };
        let Some(store) = &self.copies else {
            self.status = "Nothing to save — no active pipeline".into();
            return;
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
        // If we're overwriting an existing library `.rlab`, read its current
        // LMTA chunk so we can carry it forward. Otherwise the save drops the
        // library metadata (keywords, rating, source-file timestamps, …) and
        // features that depend on it — like "Export Selection → Original" —
        // lose ground truth.
        let existing_lmta = if path.exists() {
            RlabFile::read(&path).ok().and_then(|r| r.lmta)
        } else {
            None
        };
        let mut rlab = RlabFile::new(meta, original_bytes, copies_saved, active_idx, None);
        rlab.set_lmta(existing_lmta);
        // v4 adds Reed-Solomon parity so the file is repairable by an integrity
        // scrub; this also avoids downgrading a library photo that was imported
        // as v4 when its edits are saved back in place.
        match rlab.write_v5(&path) {
            Ok(()) => {
                self.project_created_at = Some(created_at);
                self.project_path = Some(path.clone());
                self.capture_clean_edit_state();
                // Clean up the autosave file now that the work is safely saved.
                if let Some(session_id) = self.autosave_session_id.take() {
                    crate::autosave::delete(session_id);
                }
                self.status = format!("Saved → {}", path.display());

                // If this file was opened from the library, regenerate its thumbnail.
                let library_hash = self.library_context.as_ref().map(|(_, hash)| hash.clone());
                if let Some(hash) = library_hash {
                    self.spawn_thumbnail_regen(hash);
                }
            }
            Err(e) => {
                self.status = format!("Save failed: {}", e);
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
