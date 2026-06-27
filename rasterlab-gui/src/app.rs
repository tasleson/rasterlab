//! Main application struct that wires together all panels.

use std::{path::PathBuf, sync::Arc};

use egui::{Context, Key, Modifiers};

use crate::{
    file_chooser::{DialogKind, FileChooser},
    panels::{
        canvas::CanvasState, edit_stack, export_dialog, histogram_panel, library_detail,
        library_panel, tools,
    },
    state::{AppMode, AppState},
};

/// What to do once the user confirms discarding unsaved changes on open.
#[cfg(not(target_arch = "wasm32"))]
enum PendingOpen {
    /// Show the OS / built-in file picker.
    Dialog,
    /// Open a specific path directly (Open Recent, drag-and-drop, etc.).
    Path(PathBuf),
    /// Restore an autosave session.
    Autosave(crate::autosave::AutosaveEntry),
    /// Open a photo from the library grid (also sets library context + mode).
    LibraryPhoto {
        rlab_path: PathBuf,
        lib_root: PathBuf,
        hash: String,
    },
}

pub struct RasterLabApp {
    state: AppState,
    canvas: CanvasState,
    #[cfg(not(target_arch = "wasm32"))]
    chooser: FileChooser,
    about_open: bool,
    /// True while the "unsaved changes on exit" confirmation dialog is shown.
    exit_confirm_open: bool,
    /// Set to true once the user has confirmed discarding unsaved changes,
    /// so the next close request is allowed through without re-prompting.
    allow_close: bool,
    /// True while the "discard changes and open?" confirmation dialog is shown.
    #[cfg(not(target_arch = "wasm32"))]
    open_confirm_open: bool,
    /// The action to execute once the user confirms the open-discard dialog.
    #[cfg(not(target_arch = "wasm32"))]
    pending_open: Option<PendingOpen>,
    /// Index of the photo in `library.results` that the user asked to delete
    /// from the editor. `Some` while the confirmation dialog is open; used to
    /// navigate to the adjacent photo if the deletion is confirmed.
    #[cfg(not(target_arch = "wasm32"))]
    editor_delete_at_idx: Option<usize>,
    /// The last title sent to the window; used to avoid redundant viewport commands.
    last_title: String,
}

impl RasterLabApp {
    pub fn new(cc: &eframe::CreationContext, initial_file: Option<PathBuf>) -> Self {
        let gpu = cc.wgpu_render_state.as_ref().map(|render_state| {
            Arc::new(rasterlab_gpu::GpuContext::new(
                render_state.device.clone(),
                render_state.queue.clone(),
                render_state.device.limits(),
            ))
        });
        if std::env::var("RASTERLAB_GPU_LOG").as_deref() == Ok("1") {
            match gpu.as_ref() {
                Some(gpu) => eprintln!(
                    "[rasterlab-gpu] initialized wgpu context max_texture_dimension_2d={}",
                    gpu.limits().max_texture_dimension_2d
                ),
                None => eprintln!(
                    "[rasterlab-gpu] no eframe wgpu render state; GPU operations unavailable"
                ),
            }
        }
        let mut state = AppState::new(cc.egui_ctx.clone(), gpu);

        // Apply the stored theme preference.  For ThemePreference::System the fallback
        // is set to Light so the app looks correct on platforms where winit cannot
        // detect the OS theme (returns None).
        cc.egui_ctx.options_mut(|o| {
            o.theme_preference = state.prefs.theme.to_egui();
            o.fallback_theme = egui::Theme::Light;
        });
        // Apply the stored UI scale override (if any).
        if let Some(ppp) = state.prefs.ui_scale {
            cc.egui_ctx.set_pixels_per_point(ppp);
        }
        if let Some(path) = initial_file {
            state.open_file(path);
        }
        // Auto-open the last library if one was open when the app last exited.
        if let Some(lib_path) = state.prefs.last_library.clone()
            && lib_path.exists()
        {
            state.open_library(lib_path);
        }
        #[cfg(not(target_arch = "wasm32"))]
        let use_native = state.prefs.use_native_dialogs;
        #[cfg(not(target_arch = "wasm32"))]
        let open_file_filter = state.prefs.open_file_filter.clone();
        Self {
            state,
            canvas: CanvasState::default(),
            #[cfg(not(target_arch = "wasm32"))]
            chooser: FileChooser::new(use_native, open_file_filter.as_deref()),
            about_open: false,
            exit_confirm_open: false,
            allow_close: false,
            #[cfg(not(target_arch = "wasm32"))]
            open_confirm_open: false,
            #[cfg(not(target_arch = "wasm32"))]
            pending_open: None,
            #[cfg(not(target_arch = "wasm32"))]
            editor_delete_at_idx: None,
            last_title: String::new(),
        }
    }

    fn handle_keyboard(&mut self, ctx: &Context) {
        // Compute editor-navigation preconditions outside input_mut to avoid
        // re-borrowing self inside a closure that already captures it mutably.
        #[cfg(not(target_arch = "wasm32"))]
        let in_library_editor = self.state.mode == AppMode::Editor
            && self.state.library_context.is_some()
            && self.state.library.library.is_some();
        // Only steal arrow/delete keys when no widget (text field, slider, …) has focus.
        #[cfg(not(target_arch = "wasm32"))]
        let no_focus = ctx.memory(|m| m.focused().is_none());

        // Consume shortcut keys inside `input_mut`, but defer the handlers
        // until after the closure returns. Handlers like `state.undo()` end
        // up calling `ctx.request_repaint_after`, which takes the egui
        // Context lock; doing that while `input_mut` already holds the write
        // lock on the same Context deadlocks the main thread (parking_lot
        // RwLock is non-reentrant).
        let mut do_undo = false;
        let mut do_redo = false;
        #[cfg(not(target_arch = "wasm32"))]
        let mut do_open = false;
        #[cfg(not(target_arch = "wasm32"))]
        let mut do_save = false;
        #[cfg(not(target_arch = "wasm32"))]
        let mut do_save_as = false;
        #[cfg(not(target_arch = "wasm32"))]
        let mut do_export = false;
        #[cfg(not(target_arch = "wasm32"))]
        let mut nav_delta: i32 = 0;
        #[cfg(not(target_arch = "wasm32"))]
        let mut do_delete = false;

        ctx.input_mut(|i| {
            if i.consume_key(Modifiers::CTRL, Key::Z) {
                do_undo = true;
            }
            if i.consume_key(Modifiers::CTRL, Key::Y) {
                do_redo = true;
            }
            #[cfg(not(target_arch = "wasm32"))]
            if i.consume_key(Modifiers::CTRL, Key::O) {
                do_open = true;
            }
            #[cfg(not(target_arch = "wasm32"))]
            if i.consume_key(Modifiers::CTRL, Key::S) {
                do_save = true;
            }
            #[cfg(not(target_arch = "wasm32"))]
            if i.consume_key(Modifiers::CTRL | Modifiers::SHIFT, Key::S) {
                do_save_as = true;
            }
            #[cfg(not(target_arch = "wasm32"))]
            if i.consume_key(Modifiers::CTRL, Key::E) {
                do_export = true;
            }

            // ── Library-editor navigation ────────────────────────────────────
            #[cfg(not(target_arch = "wasm32"))]
            if in_library_editor && no_focus {
                if i.consume_key(Modifiers::NONE, Key::ArrowLeft) {
                    nav_delta -= 1;
                }
                if i.consume_key(Modifiers::NONE, Key::ArrowRight) {
                    nav_delta += 1;
                }
                if i.consume_key(Modifiers::NONE, Key::Delete) {
                    do_delete = true;
                }
            }
        });

        if do_undo {
            self.state.undo();
        }
        if do_redo {
            self.state.redo();
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            if do_open {
                self.request_open_dialog(ctx);
            }
            if do_save {
                self.save_project_or_prompt(ctx);
            }
            if do_save_as {
                self.chooser.save_project(ctx);
            }
            if do_export {
                self.chooser.export_image(ctx);
            }
            if nav_delta != 0 {
                self.navigate_library(nav_delta);
            }
            if do_delete {
                self.trigger_editor_delete();
            }
        }
    }

    /// Navigate to the adjacent photo in the library results list.
    #[cfg(not(target_arch = "wasm32"))]
    fn navigate_library(&mut self, delta: i32) {
        let Some((lib_root, current_hash)) = self.state.library_context.clone() else {
            return;
        };
        let Some(lib) = self.state.library.library.clone() else {
            return;
        };
        // Borrow results only long enough to find the adjacent photo, then drop.
        let adjacent = {
            let results = &self.state.library.results;
            let Some(idx) = results.iter().position(|p| p.hash == current_hash) else {
                return;
            };
            let new_idx = (idx as i64 + delta as i64).clamp(0, results.len() as i64 - 1) as usize;
            if new_idx == idx {
                return;
            }
            results[new_idx].clone()
        };
        let rlab_path = lib.rlab_path(&adjacent.hash);
        self.request_open_library_photo(rlab_path, lib_root, adjacent.hash);
    }

    /// Select the currently-open library photo and raise the delete confirmation.
    #[cfg(not(target_arch = "wasm32"))]
    fn trigger_editor_delete(&mut self) {
        let Some((_, ref hash)) = self.state.library_context.clone() else {
            return;
        };
        let hash = hash.clone();
        let Some((idx, photo)) = self
            .state
            .library
            .results
            .iter()
            .enumerate()
            .find(|(_, p)| p.hash == hash)
            .map(|(i, p)| (i, p.clone()))
        else {
            return;
        };
        self.state.library.select_only(photo.id);
        self.state.library.confirm_delete = true;
        self.editor_delete_at_idx = Some(idx);
    }

    /// Open the file picker, prompting to discard unsaved changes first if needed.
    #[cfg(not(target_arch = "wasm32"))]
    fn request_open_dialog(&mut self, ctx: &Context) {
        if self.state.is_dirty {
            self.pending_open = Some(PendingOpen::Dialog);
            self.open_confirm_open = true;
        } else {
            self.chooser.open_image(ctx);
        }
    }

    /// Open a specific path, prompting to discard unsaved changes first if needed.
    #[cfg(not(target_arch = "wasm32"))]
    fn request_open_path(&mut self, path: PathBuf) {
        if self.state.is_dirty {
            self.pending_open = Some(PendingOpen::Path(path));
            self.open_confirm_open = true;
        } else {
            self.state.open_file(path);
        }
    }

    /// Restore an autosave session, prompting to discard unsaved changes first if needed.
    #[cfg(not(target_arch = "wasm32"))]
    fn request_restore_autosave(&mut self, entry: crate::autosave::AutosaveEntry) {
        if self.state.is_dirty {
            self.pending_open = Some(PendingOpen::Autosave(entry));
            self.open_confirm_open = true;
        } else {
            self.state.restore_autosave(entry);
        }
    }

    /// Open a photo from the library, prompting to discard unsaved changes first if needed.
    #[cfg(not(target_arch = "wasm32"))]
    fn request_open_library_photo(&mut self, rlab_path: PathBuf, lib_root: PathBuf, hash: String) {
        if self.state.is_dirty {
            self.pending_open = Some(PendingOpen::LibraryPhoto {
                rlab_path,
                lib_root,
                hash,
            });
            self.open_confirm_open = true;
        } else {
            self.open_library_photo(rlab_path, lib_root, hash);
        }
    }

    /// Actually switch the editor to a library photo. Assumes any unsaved-changes
    /// prompt has already been handled.
    #[cfg(not(target_arch = "wasm32"))]
    fn open_library_photo(&mut self, rlab_path: PathBuf, lib_root: PathBuf, hash: String) {
        self.state.library_context = Some((lib_root, hash));
        self.state.open_file(rlab_path);
        self.state.mode = AppMode::Editor;
    }

    /// Save in-place if a project path is already known; otherwise open Save As.
    #[cfg(not(target_arch = "wasm32"))]
    fn save_project_or_prompt(&mut self, ctx: &Context) {
        if let Some(path) = self.state.project_path.clone() {
            self.state.save_project(path);
        } else {
            self.chooser.save_project(ctx);
        }
    }

    /// Poll the active file chooser and dispatch any completed result.
    #[cfg(not(target_arch = "wasm32"))]
    fn poll_dialogs(&mut self, ctx: &Context) {
        let Some((kind, paths)) = self.chooser.update(ctx) else {
            return;
        };
        // Helper: consume the vec and take the first path (single-file/folder kinds).
        let first = || paths.clone().into_iter().next();
        match kind {
            DialogKind::OpenFile => {
                if let Some(p) = first() {
                    self.state.open_file(p)
                }
            }
            DialogKind::ExportImage => {
                if let Some(p) = first() {
                    self.state.save_file(p)
                }
            }
            DialogKind::SaveProject => {
                if let Some(p) = first() {
                    self.state.save_project(p)
                }
            }
            DialogKind::ExportEditStack => {
                if let Some(p) = first() {
                    self.state.export_edit_stack_json(p)
                }
            }
            DialogKind::LoadLut => {
                if let Some(p) = first() {
                    self.state.load_lut(p)
                }
            }
            DialogKind::PanoramaAddImage => {
                if let Some(p) = first() {
                    self.state.panorama_add_image(p)
                }
            }
            DialogKind::FocusStackAddImage => {
                if let Some(p) = first() {
                    self.state.focus_stack_add_image(p)
                }
            }
            DialogKind::HdrMergeAddImage => {
                if let Some(p) = first() {
                    self.state.hdr_merge_add_image(p)
                }
            }
            DialogKind::NewLibrary => {
                if let Some(p) = first() {
                    self.state.new_library(p)
                }
            }
            DialogKind::OpenLibrary => {
                if let Some(p) = first() {
                    self.state.open_library(p)
                }
            }
            DialogKind::ImportFiles => self.state.import_into_library(paths),
            DialogKind::ImportFolder => {
                if let Some(p) = first() {
                    self.state.import_folder_into_library(p)
                }
            }
            DialogKind::ExportDestination => {
                if let Some(p) = first() {
                    self.state.tools.export_dialog.dest_dir = p;
                }
            }
        }
    }
}

impl eframe::App for RasterLabApp {
    fn on_exit(&mut self) {
        self.state.prefs.save();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Clone the context so we can pass it to helper methods while also
        // passing `ui` to panel builders (both need access simultaneously).
        let ctx = ui.ctx().clone();

        // Intercept close requests: if there are unsaved changes and the user
        // hasn't already confirmed, cancel the close and show a confirmation
        // dialog.  Also save prefs on the real close path — this is more
        // reliable than `on_exit` alone, which may not fire on all platforms.
        if ctx.input(|i| i.viewport().close_requested()) {
            if self.state.is_dirty && !self.allow_close {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.exit_confirm_open = true;
            } else {
                self.state.prefs.save();
            }
        }

        self.state.poll_background();
        #[cfg(not(target_arch = "wasm32"))]
        self.poll_dialogs(&ctx);
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(kind) = self.state.tools.pending_dialog.take() {
            use crate::file_chooser::DialogKind;
            if kind == DialogKind::ExportDestination {
                let start = self.state.tools.export_dialog.dest_dir.clone();
                self.chooser
                    .choose_export_destination(&ctx, Some(start.as_path()));
            } else {
                self.chooser.open_kind(&ctx, kind);
            }
        }
        if let Some((rlab_path, lib_root, hash)) = self.state.library.pending_open_photo.take() {
            self.request_open_library_photo(rlab_path, lib_root, hash);
        }

        self.handle_keyboard(&ctx);

        // ── Window title (reflects project name and dirty state) ──────────
        {
            let dirty_marker = if self.state.is_dirty { " ●" } else { "" };
            let title = match &self.state.project_path {
                Some(_) => {
                    let display = self
                        .state
                        .last_path
                        .as_deref()
                        .and_then(|p| p.file_name())
                        .or_else(|| {
                            self.state
                                .project_path
                                .as_deref()
                                .and_then(|p| p.file_name())
                        })
                        .unwrap_or_default()
                        .to_string_lossy();
                    format!("RasterLab — {}{}", display, dirty_marker)
                }
                None if self.state.pipeline().is_some() => {
                    format!("RasterLab — Unsaved Project{}", dirty_marker)
                }
                None => "RasterLab".to_string(),
            };
            if title != self.last_title {
                self.last_title = title.clone();
                ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
            }
        }

        // ── Menu bar ─────────────────────────────────────────────────────
        egui::Panel::top("menu_bar").show_inside(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                // ── Mode toggle ──────────────────────────────────────────────
                let prev_mode = self.state.mode;
                ui.selectable_value(&mut self.state.mode, AppMode::Editor, "Editor");
                ui.selectable_value(&mut self.state.mode, AppMode::Library, "Library");
                if prev_mode == AppMode::Editor && self.state.mode == AppMode::Library {
                    self.state.library.scroll_to_hash =
                        self.state.library_context.as_ref().map(|(_, h)| h.clone());
                }
                ui.separator();

                ui.menu_button("File", |ui| {
                    #[cfg(not(target_arch = "wasm32"))]
                    if ui.button("Open…  (Ctrl+O)").clicked() {
                        ui.close_kind(egui::UiKind::Menu);
                        self.request_open_dialog(&ctx);
                    }
                    #[cfg(not(target_arch = "wasm32"))]
                    {
                        let recent = self.state.prefs.recent_files.clone();
                        ui.add_enabled_ui(!recent.is_empty(), |ui| {
                            ui.menu_button("Open Recent", |ui| {
                                for path in &recent {
                                    let label = self.state.prefs.recent_display_name(path);
                                    if ui
                                        .button(label)
                                        .on_hover_text(path.display().to_string())
                                        .clicked()
                                    {
                                        ui.close_kind(egui::UiKind::Menu);
                                        self.request_open_path(path.clone());
                                    }
                                }
                                ui.separator();
                                if ui.button("Clear Recent").clicked() {
                                    ui.close_kind(egui::UiKind::Menu);
                                    self.state.prefs.recent_files.clear();
                                    self.state.prefs.recent_display_names.clear();
                                    self.state.prefs.save();
                                }
                            });
                        });
                    }
                    #[cfg(not(target_arch = "wasm32"))]
                    {
                        let current_session = self.state.autosave_session_id;
                        let autosave_entries = crate::autosave::list_entries()
                            .into_iter()
                            // Exclude the currently active session — it isn't "previous" work.
                            .filter(|e| Some(e.data.started_at) != current_session)
                            .collect::<Vec<_>>();
                        let has_autosave_entries = !autosave_entries.is_empty();
                        ui.menu_button("Previously Unsaved Work", |ui| {
                            if !has_autosave_entries {
                                ui.add_enabled(false, egui::Button::new("No previous work"));
                            } else {
                                for entry in &autosave_entries {
                                    let name = entry
                                        .data
                                        .project_path
                                        .as_deref()
                                        .map(std::path::Path::new)
                                        .map(|p| self.state.prefs.recent_display_name(p))
                                        .unwrap_or_else(|| {
                                            crate::autosave::display_name(&entry.data)
                                        });
                                    let label = format!(
                                        "{}  —  {}",
                                        name,
                                        crate::autosave::format_age(entry.data.saved_at),
                                    );
                                    let hover = entry
                                        .data
                                        .project_path
                                        .as_deref()
                                        .unwrap_or(&entry.data.source_path);
                                    if ui.button(label).on_hover_text(hover).clicked() {
                                        ui.close_kind(egui::UiKind::Menu);
                                        self.request_restore_autosave(entry.clone());
                                    }
                                }
                            }
                            ui.separator();
                            if ui
                                .add_enabled(
                                    has_autosave_entries,
                                    egui::Button::new("Clear Previously Unsaved Work"),
                                )
                                .clicked()
                            {
                                ui.close_kind(egui::UiKind::Menu);
                                for entry in &autosave_entries {
                                    crate::autosave::delete(entry.data.started_at);
                                }
                            }
                        });
                    }
                    // ── Library ──────────────────────────────────────────────
                    #[cfg(not(target_arch = "wasm32"))]
                    {
                        ui.separator();
                        if ui.button("New Library…").clicked() {
                            ui.close_kind(egui::UiKind::Menu);
                            self.state.tools.pending_dialog =
                                Some(crate::file_chooser::DialogKind::NewLibrary);
                        }
                        if ui.button("Open Library…").clicked() {
                            ui.close_kind(egui::UiKind::Menu);
                            self.state.tools.pending_dialog =
                                Some(crate::file_chooser::DialogKind::OpenLibrary);
                        }
                        {
                            let recent = self.state.prefs.recent_libraries.clone();
                            ui.add_enabled_ui(!recent.is_empty(), |ui| {
                                ui.menu_button("Recent Libraries", |ui| {
                                    for path in &recent {
                                        let label = path
                                            .file_name()
                                            .map(|n| n.to_string_lossy().into_owned())
                                            .unwrap_or_else(|| path.display().to_string());
                                        if ui
                                            .button(label)
                                            .on_hover_text(path.display().to_string())
                                            .clicked()
                                        {
                                            ui.close_kind(egui::UiKind::Menu);
                                            self.state.open_library(path.clone());
                                        }
                                    }
                                });
                            });
                        }
                        ui.add_enabled_ui(self.state.library.library.is_some(), |ui| {
                            ui.menu_button("Import Photos", |ui| {
                                if ui.button("Select Files…").clicked() {
                                    ui.close_kind(egui::UiKind::Menu);
                                    self.state.tools.pending_dialog =
                                        Some(crate::file_chooser::DialogKind::ImportFiles);
                                }
                                if ui.button("Select Folder…").clicked() {
                                    ui.close_kind(egui::UiKind::Menu);
                                    self.state.tools.pending_dialog =
                                        Some(crate::file_chooser::DialogKind::ImportFolder);
                                }
                            });
                        });
                        ui.add_enabled_ui(
                            self.state.library.library.is_some()
                                && !self.state.library.selected.is_empty(),
                            |ui| {
                                if ui.button("Export Selection…").clicked() {
                                    ui.close_kind(egui::UiKind::Menu);
                                    self.state.tools.export_dialog.reset_run_state();
                                    self.state.tools.export_dialog.open = true;
                                    self.state.mode = AppMode::Library;
                                }
                            },
                        );
                        ui.add_enabled_ui(self.state.library.library.is_some(), |ui| {
                            if ui.button("Rebuild Library Index").clicked() {
                                ui.close_kind(egui::UiKind::Menu);
                                self.state.rebuild_library_index();
                            }
                        });
                        ui.add_enabled_ui(self.state.library.library.is_some(), |ui| {
                            let running = self.state.scrub_running();
                            let label = if running {
                                "Stop Integrity Scrub"
                            } else {
                                "Start Integrity Scrub"
                            };
                            if ui
                                .button(label)
                                .on_hover_text(
                                    "Verify every photo's .rlab file and repair correctable \
                                     errors, backing up damaged originals to the library's \
                                     'recovered' folder",
                                )
                                .clicked()
                            {
                                ui.close_kind(egui::UiKind::Menu);
                                if running {
                                    self.state.stop_scrub();
                                } else {
                                    self.state.start_scrub();
                                }
                            }
                        });
                    }

                    #[cfg(not(target_arch = "wasm32"))]
                    {
                        ui.separator();
                        if ui
                            .add_enabled(
                                self.state.pipeline().is_some(),
                                egui::Button::new("Save  (Ctrl+S)"),
                            )
                            .clicked()
                        {
                            ui.close_kind(egui::UiKind::Menu);
                            self.save_project_or_prompt(&ctx);
                        }
                        if self.state.project_path.is_some()
                            && ui
                                .add_enabled(
                                    self.state.pipeline().is_some(),
                                    egui::Button::new("Save As…  (Ctrl+⇧S)"),
                                )
                                .clicked()
                        {
                            ui.close_kind(egui::UiKind::Menu);
                            self.chooser.save_project(&ctx);
                        }
                        ui.separator();
                        if ui
                            .add_enabled(
                                self.state.pipeline().is_some(),
                                egui::Button::new("Export…  (Ctrl+E)"),
                            )
                            .clicked()
                        {
                            ui.close_kind(egui::UiKind::Menu);
                            self.chooser.export_image(&ctx);
                        }
                        if ui
                            .add_enabled(
                                self.state.pipeline().is_some(),
                                egui::Button::new("Export Edit Stack as JSON…"),
                            )
                            .clicked()
                        {
                            ui.close_kind(egui::UiKind::Menu);
                            self.chooser.export_edit_stack(&ctx);
                        }
                    }
                    ui.separator();
                    if ui.button("Quit").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
                ui.menu_button("Edit", |ui| {
                    if ui
                        .add_enabled(self.state.can_undo(), egui::Button::new("Undo  (Ctrl+Z)"))
                        .clicked()
                    {
                        ui.close_kind(egui::UiKind::Menu);
                        self.state.undo();
                    }
                    if ui
                        .add_enabled(self.state.can_redo(), egui::Button::new("Redo  (Ctrl+Y)"))
                        .clicked()
                    {
                        ui.close_kind(egui::UiKind::Menu);
                        self.state.redo();
                    }
                });
                ui.menu_button("Preferences", |ui| {
                    ui.menu_button("Theme", |ui| {
                        use crate::prefs::ThemePref;
                        let current = self.state.prefs.theme;
                        for (label, pref) in [
                            ("System Default", ThemePref::System),
                            ("Light", ThemePref::Light),
                            ("Dark", ThemePref::Dark),
                        ] {
                            if ui.selectable_label(current == pref, label).clicked() {
                                self.state.prefs.theme = pref;
                                ctx.options_mut(|o| o.theme_preference = pref.to_egui());
                                self.state.prefs.save();
                                ui.close_kind(egui::UiKind::Menu);
                            }
                        }
                    });
                    ui.menu_button("UI Scale", |ui| {
                        let current = self.state.prefs.ui_scale;
                        if ui
                            .selectable_label(current.is_none(), "Auto (system DPI)")
                            .clicked()
                        {
                            self.state.prefs.ui_scale = None;
                            ctx.set_zoom_factor(1.0);
                            self.state.prefs.save();
                            ui.close_kind(egui::UiKind::Menu);
                        }
                        for (label, ppp) in [
                            ("75%", 0.75_f32),
                            ("100%", 1.00_f32),
                            ("125%", 1.25_f32),
                            ("150%", 1.50_f32),
                            ("175%", 1.75_f32),
                            ("200%", 2.00_f32),
                            ("250%", 2.50_f32),
                            ("300%", 3.00_f32),
                        ] {
                            let selected = current.is_some_and(|v| (v - ppp).abs() < 0.01);
                            if ui.selectable_label(selected, label).clicked() {
                                self.state.prefs.ui_scale = Some(ppp);
                                ctx.set_pixels_per_point(ppp);
                                self.state.prefs.save();
                                ui.close_kind(egui::UiKind::Menu);
                            }
                        }
                    });
                    #[cfg(not(target_arch = "wasm32"))]
                    ui.menu_button("File Dialogs", |ui| {
                        let native = self.state.prefs.use_native_dialogs;
                        if ui
                            .selectable_label(native, "Native (system dialog)")
                            .clicked()
                        {
                            self.state.prefs.use_native_dialogs = true;
                            self.chooser.set_native(true);
                            self.state.prefs.save();
                            ui.close_kind(egui::UiKind::Menu);
                        }
                        if ui
                            .selectable_label(!native, "Built-in  (works over waypipe)")
                            .clicked()
                        {
                            self.state.prefs.use_native_dialogs = false;
                            self.chooser.set_native(false);
                            self.state.prefs.save();
                            ui.close_kind(egui::UiKind::Menu);
                        }
                        ui.separator();
                        ui.menu_button("Default Open Filter", |ui| {
                            let current = self.state.prefs.open_file_filter.clone();
                            for (label, value) in [
                                ("All Files", None),
                                ("All Supported", Some("All supported")),
                                ("Images", Some("Images")),
                                ("JPEG", Some("JPEG")),
                                ("PNG", Some("PNG")),
                                ("Camera RAW", Some("Camera RAW")),
                                ("RasterLab Project", Some("RasterLab Project")),
                            ] {
                                let selected = current.as_deref() == value;
                                if ui.selectable_label(selected, label).clicked() {
                                    self.state.prefs.open_file_filter =
                                        value.map(|s| s.to_string());
                                    self.chooser.set_open_file_filter(value);
                                    self.state.prefs.save();
                                    ui.close_kind(egui::UiKind::Menu);
                                }
                            }
                        });
                    });
                });
                ui.menu_button("Help", |ui| {
                    if ui.button("About RasterLab").clicked() {
                        self.about_open = true;
                        ui.close_kind(egui::UiKind::Menu);
                    }
                });
            });
        });

        // ── Status bar ───────────────────────────────────────────────────
        egui::Panel::bottom("status_bar").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                if self.state.loading {
                    ui.spinner();
                }
                ui.label(&self.state.status);
                if let Some(img) = &self.state.rendered {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(format!("{}×{}  RGBA8", img.width, img.height));
                    });
                }
            });
        });

        match self.state.mode {
            AppMode::Library => {
                // ── Library mode ─────────────────────────────────────────
                egui::Panel::right("lib_detail_panel")
                    .resizable(true)
                    .default_size(260.0)
                    .min_size(180.0)
                    .show_inside(ui, |ui| {
                        library_detail::ui(ui, &mut self.state);
                    });
                egui::CentralPanel::default().show_inside(ui, |ui| {
                    library_panel::ui(ui, &mut self.state);
                });
                export_dialog::ui(&ctx, &mut self.state);
            }
            AppMode::Editor => {
                // ── Editor mode (original layout) ────────────────────────
                egui::Panel::left("tools_panel")
                    .resizable(true)
                    .default_size(220.0)
                    .min_size(180.0)
                    .show_inside(ui, |ui| {
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            tools::ui(ui, &mut self.state);
                        });
                    });

                egui::Panel::right("right_panel")
                    .resizable(true)
                    .default_size(280.0)
                    .min_size(220.0)
                    .show_inside(ui, |ui| {
                        egui::Panel::bottom("histogram_panel")
                            .resizable(true)
                            .default_size(200.0)
                            .min_size(80.0)
                            .show_inside(ui, |ui| {
                                histogram_panel::ui(ui, self.state.histogram.as_ref());
                            });
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            edit_stack::ui(ui, &mut self.state);
                        });
                    });

                egui::CentralPanel::default().show_inside(ui, |ui| {
                    self.canvas.ui(ui, &mut self.state);
                });
            }
        }

        // ── Delete confirmation (editor mode) ────────────────────────────
        // The confirmation dialog lives in library_panel but must also appear
        // when the user presses Delete while editing a library photo.
        #[cfg(not(target_arch = "wasm32"))]
        if self.state.mode == AppMode::Editor {
            library_panel::confirm_delete_dialog(&ctx, &mut self.state);

            // If a delete was triggered from the editor and the dialog has been
            // dismissed (confirmed or cancelled), decide what to do next.
            if let Some(old_idx) = self.editor_delete_at_idx
                && !self.state.library.confirm_delete
            {
                self.editor_delete_at_idx = None;
                // A hash_gone check distinguishes confirmed vs. cancelled:
                // after cancel the photo is still in results.
                let hash_gone = self
                    .state
                    .library_context
                    .as_ref()
                    .map(|(_, h)| !self.state.library.results.iter().any(|p| p.hash == *h))
                    .unwrap_or(false);
                if hash_gone {
                    // Deletion confirmed — navigate to the photo now at the
                    // same slot (or the last one if we were at the end).
                    let lib = self.state.library.library.clone();
                    let lib_root = self.state.library_context.as_ref().map(|(r, _)| r.clone());
                    let results = &self.state.library.results;
                    if results.is_empty() {
                        self.state.mode = AppMode::Library;
                        self.state.library_context = None;
                    } else if let Some((lib, lib_root)) = lib.zip(lib_root) {
                        let nav_idx = old_idx.min(results.len() - 1);
                        let photo = results[nav_idx].clone();
                        let rlab_path = lib.rlab_path(&photo.hash);
                        // open_library_photo skips the unsaved-changes prompt
                        // (nothing to save — we just deleted the current photo).
                        self.open_library_photo(rlab_path, lib_root, photo.hash);
                    }
                }
            }
        }

        // ── About dialog ─────────────────────────────────────────────────
        self.show_about_window(&ctx);

        // ── Unsaved-changes-on-open confirmation ──────────────────────────
        #[cfg(not(target_arch = "wasm32"))]
        self.show_open_confirm_window(&ctx);

        // ── Unsaved-changes-on-exit confirmation ─────────────────────────
        self.show_exit_confirm_window(&ctx);
    }
}

/// Build-time metadata embedded via `build.rs`.
mod build_info {
    pub const PKG_VERSION: &str = env!("CARGO_PKG_VERSION");
    pub const PKG_NAME: &str = env!("CARGO_PKG_NAME");
    pub const GIT_HASH: &str = env!("GIT_HASH");
    pub const GIT_DIRTY: &str = env!("GIT_DIRTY");
    pub const BUILD_DATE: &str = env!("BUILD_DATE");
    pub const RUSTC_VERSION: &str = env!("RUSTC_VERSION_STR");
    pub const TARGET_TRIPLE: &str = env!("TARGET_TRIPLE");
    pub const PROFILE: &str = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
}

impl RasterLabApp {
    fn show_about_window(&mut self, ctx: &Context) {
        if !self.about_open {
            return;
        }
        let mut open = self.about_open;
        egui::Window::new("About RasterLab")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.heading("RasterLab");
                ui.label("Cross-platform raster image editor");
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(6.0);

                egui::Grid::new("about_grid")
                    .num_columns(2)
                    .spacing([12.0, 4.0])
                    .show(ui, |ui| {
                        ui.label("Version:");
                        ui.monospace(build_info::PKG_VERSION);
                        ui.end_row();

                        ui.label("Package:");
                        ui.monospace(build_info::PKG_NAME);
                        ui.end_row();

                        ui.label("Git commit:");
                        let dirty_suffix = if build_info::GIT_DIRTY == "yes" {
                            " (dirty)"
                        } else {
                            ""
                        };
                        ui.monospace(format!("{}{}", build_info::GIT_HASH, dirty_suffix));
                        ui.end_row();

                        ui.label("Source tree:");
                        ui.monospace(if build_info::GIT_DIRTY == "yes" {
                            "dirty"
                        } else {
                            "clean"
                        });
                        ui.end_row();

                        ui.label("Built:");
                        ui.monospace(build_info::BUILD_DATE);
                        ui.end_row();

                        ui.label("Profile:");
                        ui.monospace(build_info::PROFILE);
                        ui.end_row();

                        ui.label("Target:");
                        ui.monospace(build_info::TARGET_TRIPLE);
                        ui.end_row();

                        ui.label("Compiler:");
                        ui.monospace(build_info::RUSTC_VERSION);
                        ui.end_row();
                    });

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("Copy").clicked() {
                        let text = format!(
                            "RasterLab {}\n\
                             git: {}{}\n\
                             built: {}\n\
                             profile: {}\n\
                             target: {}\n\
                             compiler: {}",
                            build_info::PKG_VERSION,
                            build_info::GIT_HASH,
                            if build_info::GIT_DIRTY == "yes" {
                                " (dirty)"
                            } else {
                                ""
                            },
                            build_info::BUILD_DATE,
                            build_info::PROFILE,
                            build_info::TARGET_TRIPLE,
                            build_info::RUSTC_VERSION,
                        );
                        ui.ctx().copy_text(text);
                    }
                    if ui.button("Close").clicked() {
                        self.about_open = false;
                    }
                });
            });
        if !open {
            self.about_open = false;
        }
    }

    fn show_exit_confirm_window(&mut self, ctx: &Context) {
        if !self.exit_confirm_open {
            return;
        }
        let mut open = true;
        egui::Window::new("Unsaved changes")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(
                    "You have unsaved changes that haven't been saved as a \
                     project or exported.",
                );
                ui.label("Are you sure you want to quit?");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        self.exit_confirm_open = false;
                    }
                    if ui.button("Discard & Quit").clicked() {
                        self.exit_confirm_open = false;
                        self.allow_close = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
            });
        if !open {
            self.exit_confirm_open = false;
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn show_open_confirm_window(&mut self, ctx: &Context) {
        if !self.open_confirm_open {
            return;
        }
        let mut open = true;
        egui::Window::new("Unsaved changes")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(
                    "You have unsaved changes that haven't been saved as a \
                     project or exported.",
                );
                ui.label("Open anyway and discard changes?");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        self.open_confirm_open = false;
                        self.pending_open = None;
                    }
                    if ui.button("Discard & Open").clicked() {
                        self.open_confirm_open = false;
                        match self.pending_open.take() {
                            Some(PendingOpen::Dialog) => self.chooser.open_image(ctx),
                            Some(PendingOpen::Path(p)) => self.state.open_file(p),
                            Some(PendingOpen::Autosave(e)) => self.state.restore_autosave(e),
                            Some(PendingOpen::LibraryPhoto {
                                rlab_path,
                                lib_root,
                                hash,
                            }) => self.open_library_photo(rlab_path, lib_root, hash),
                            None => {}
                        }
                    }
                });
            });
        if !open {
            self.open_confirm_open = false;
            self.pending_open = None;
        }
    }
}
