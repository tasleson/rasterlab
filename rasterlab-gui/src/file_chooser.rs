//! Unified file-chooser abstraction.
//!
//! Wraps two backends behind a single API so the rest of the app never needs
//! to know which one is active:
//!
//! * **Native** (`rfd`) — system file-chooser dialog.  Works everywhere except
//!   over network-transparent display protocols like waypipe, where the portal
//!   window appears on the wrong machine (or not at all).
//!
//! * **Inline** (`egui-file-dialog`) — file browser rendered inside the egui
//!   window.  Works over waypipe because it never spawns an OS-level window.
//!
//! Call [`FileChooser::update`] once per frame (always, not only when a dialog
//! is open) and dispatch on the returned `(DialogKind, PathBuf)` pair.

use std::path::PathBuf;
use std::sync::mpsc;

use egui_file_dialog::{DialogState, FileDialog};
use rasterlab_core::formats::raw::RAW_EXTENSIONS;

// ---------------------------------------------------------------------------
// Extension lists (kept as constants so both egui and rfd backends stay in sync)
// ---------------------------------------------------------------------------

/// All image extensions accepted by the open-file dialog (no project files).
fn image_exts() -> Vec<&'static str> {
    let mut v = vec!["jpg", "jpeg", "png"];
    v.extend_from_slice(RAW_EXTENSIONS);
    v
}

/// All extensions accepted by the open-file dialog (images + project files).
fn all_supported_exts() -> Vec<&'static str> {
    let mut v = vec!["rlab", "jpg", "jpeg", "png"];
    v.extend_from_slice(RAW_EXTENSIONS);
    v
}

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Which operation triggered the dialog — returned alongside the chosen path
/// so the caller can dispatch without maintaining extra state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogKind {
    OpenFile,
    ExportImage,
    SaveProject,
    ExportEditStack,
    LoadLut,
    PanoramaAddImage,
    FocusStackAddImage,
    HdrMergeAddImage,
    /// Folder picker for New Library / Open Library.
    OpenLibrary,
    /// Multi-file picker for Import Photos > Select Files.
    ImportFiles,
    /// Folder picker for Import Photos > Select Folder.
    ImportFolder,
}

// ---------------------------------------------------------------------------
// FileChooser
// ---------------------------------------------------------------------------

/// Owns both dialog backends and routes to the active one.
pub struct FileChooser {
    use_native: bool,

    // ── Inline (egui-file-dialog) backend ──────────────────────────────────
    /// One pre-configured dialog per intent, preserving navigation state
    /// (current directory, bookmarks) across multiple opens.
    open_dlg: FileDialog,
    export_dlg: FileDialog,
    save_project_dlg: FileDialog,
    export_stack_dlg: FileDialog,
    lut_dlg: FileDialog,
    panorama_dlg: FileDialog,
    focus_stack_dlg: FileDialog,
    hdr_merge_dlg: FileDialog,
    lib_folder_dlg: FileDialog,
    import_files_dlg: FileDialog,
    import_folder_dlg: FileDialog,
    /// Which kind is currently waiting for a result.
    pending: Option<DialogKind>,

    // ── Native (rfd) backend ───────────────────────────────────────────────
    /// At most one rfd dialog is open at a time.  Vec is empty on cancel.
    rfd_rx: Option<(DialogKind, mpsc::Receiver<Vec<PathBuf>>)>,
}

impl FileChooser {
    /// `open_file_filter` – the filter name selected by default when the Open
    /// dialog appears.  `None` means the library's "All Files" catch-all.
    pub fn new(use_native: bool, open_file_filter: Option<&str>) -> Self {
        let mut open_dlg = FileDialog::new()
            .title("Open Image or Project")
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .add_file_filter_extensions("All supported", all_supported_exts())
            .add_file_filter_extensions("RasterLab Project", vec!["rlab"])
            .add_file_filter_extensions("Images", image_exts())
            .add_file_filter_extensions("JPEG", vec!["jpg", "jpeg"])
            .add_file_filter_extensions("PNG", vec!["png"])
            .add_file_filter_extensions("Camera RAW", RAW_EXTENSIONS.to_vec());
        if let Some(name) = open_file_filter {
            open_dlg = open_dlg.default_file_filter(name);
        }
        Self {
            use_native,
            open_dlg,
            export_dlg: FileDialog::new()
                .title("Export Image")
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .add_file_filter_extensions("JPEG", vec!["jpg", "jpeg"])
                .add_file_filter_extensions("PNG", vec!["png"]),
            save_project_dlg: FileDialog::new()
                .title("Save Project")
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .add_file_filter_extensions("RasterLab Project", vec!["rlab"])
                .default_file_name("project.rlab"),
            export_stack_dlg: FileDialog::new()
                .title("Export Edit Stack")
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .add_file_filter_extensions("JSON", vec!["json"])
                .default_file_name("edit_stack.json"),
            lut_dlg: FileDialog::new()
                .title("Load LUT")
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .add_file_filter_extensions("CUBE LUT", vec!["cube"]),
            panorama_dlg: FileDialog::new()
                .title("Add Image to Panorama")
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .add_file_filter_extensions("Images", image_exts())
                .add_file_filter_extensions("JPEG", vec!["jpg", "jpeg"])
                .add_file_filter_extensions("PNG", vec!["png"])
                .add_file_filter_extensions("Camera RAW", RAW_EXTENSIONS.to_vec()),
            focus_stack_dlg: FileDialog::new()
                .title("Add Frame to Focus Stack")
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .add_file_filter_extensions("Images", image_exts())
                .add_file_filter_extensions("JPEG", vec!["jpg", "jpeg"])
                .add_file_filter_extensions("PNG", vec!["png"])
                .add_file_filter_extensions("Camera RAW", RAW_EXTENSIONS.to_vec()),
            hdr_merge_dlg: FileDialog::new()
                .title("Add Bracketed Exposure")
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .add_file_filter_extensions("Images", image_exts())
                .add_file_filter_extensions("JPEG", vec!["jpg", "jpeg"])
                .add_file_filter_extensions("PNG", vec!["png"])
                .add_file_filter_extensions("Camera RAW", RAW_EXTENSIONS.to_vec()),
            lib_folder_dlg: FileDialog::new()
                .title("Select Library Folder")
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO),
            import_files_dlg: FileDialog::new()
                .title("Select Photos to Import")
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .add_file_filter_extensions("All supported", all_supported_exts())
                .add_file_filter_extensions("Images", image_exts())
                .add_file_filter_extensions("JPEG", vec!["jpg", "jpeg"])
                .add_file_filter_extensions("PNG", vec!["png"])
                .add_file_filter_extensions("Camera RAW", RAW_EXTENSIONS.to_vec()),
            import_folder_dlg: FileDialog::new()
                .title("Select Folder to Import")
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO),
            pending: None,
            rfd_rx: None,
        }
    }

    pub fn set_native(&mut self, native: bool) {
        self.use_native = native;
    }

    /// Update the default filter shown when the Open dialog next appears.
    /// `None` restores the "All Files" catch-all.
    #[allow(dead_code)]
    pub fn set_open_file_filter(&mut self, filter: Option<&str>) {
        self.open_dlg.config_mut().default_file_filter = filter.map(|s| s.to_string());
    }

    // ── Opening ─────────────────────────────────────────────────────────────

    pub fn open_image(&mut self, ctx: &egui::Context) {
        self.open(ctx, DialogKind::OpenFile, false);
    }

    pub fn export_image(&mut self, ctx: &egui::Context) {
        self.open(ctx, DialogKind::ExportImage, true);
    }

    pub fn save_project(&mut self, ctx: &egui::Context) {
        self.open(ctx, DialogKind::SaveProject, true);
    }

    pub fn export_edit_stack(&mut self, ctx: &egui::Context) {
        self.open(ctx, DialogKind::ExportEditStack, true);
    }

    pub fn load_lut(&mut self, ctx: &egui::Context) {
        self.open(ctx, DialogKind::LoadLut, false);
    }

    pub fn panorama_add_image(&mut self, ctx: &egui::Context) {
        self.open(ctx, DialogKind::PanoramaAddImage, false);
    }

    pub fn focus_stack_add_image(&mut self, ctx: &egui::Context) {
        self.open(ctx, DialogKind::FocusStackAddImage, false);
    }

    pub fn hdr_merge_add_image(&mut self, ctx: &egui::Context) {
        self.open(ctx, DialogKind::HdrMergeAddImage, false);
    }

    pub fn open_library(&mut self, ctx: &egui::Context) {
        self.open(ctx, DialogKind::OpenLibrary, false);
    }

    pub fn import_files(&mut self, ctx: &egui::Context) {
        self.open(ctx, DialogKind::ImportFiles, false);
    }

    pub fn import_folder(&mut self, ctx: &egui::Context) {
        self.open(ctx, DialogKind::ImportFolder, false);
    }

    // ── Per-frame polling ───────────────────────────────────────────────────

    /// Must be called once per frame.  Returns `Some((kind, paths))` when the
    /// user has confirmed a selection, `None` otherwise.  Single-file and
    /// folder kinds return a one-element vec; `ImportFiles` may return many.
    pub fn update(&mut self, ctx: &egui::Context) -> Option<(DialogKind, Vec<PathBuf>)> {
        if self.use_native {
            self.poll_rfd()
        } else {
            self.poll_egui(ctx)
        }
    }

    // ── Private helpers ─────────────────────────────────────────────────────

    fn open(&mut self, ctx: &egui::Context, kind: DialogKind, is_save: bool) {
        // Ignore if a dialog of any kind is already open.
        if self.is_busy() {
            return;
        }

        if self.use_native {
            self.spawn_rfd(ctx, kind, is_save);
        } else {
            self.pending = Some(kind);
            let dlg = self.dialog_mut(kind);
            match kind {
                DialogKind::OpenLibrary | DialogKind::ImportFolder => dlg.pick_directory(),
                DialogKind::ImportFiles => dlg.pick_multiple(),
                _ if is_save => dlg.save_file(),
                _ => dlg.pick_file(),
            }
        }
    }

    fn is_busy(&self) -> bool {
        if self.use_native {
            self.rfd_rx.is_some()
        } else {
            self.pending.is_some()
        }
    }

    fn dialog_mut(&mut self, kind: DialogKind) -> &mut FileDialog {
        match kind {
            DialogKind::OpenFile => &mut self.open_dlg,
            DialogKind::ExportImage => &mut self.export_dlg,
            DialogKind::SaveProject => &mut self.save_project_dlg,
            DialogKind::ExportEditStack => &mut self.export_stack_dlg,
            DialogKind::LoadLut => &mut self.lut_dlg,
            DialogKind::PanoramaAddImage => &mut self.panorama_dlg,
            DialogKind::FocusStackAddImage => &mut self.focus_stack_dlg,
            DialogKind::HdrMergeAddImage => &mut self.hdr_merge_dlg,
            DialogKind::OpenLibrary => &mut self.lib_folder_dlg,
            DialogKind::ImportFiles => &mut self.import_files_dlg,
            DialogKind::ImportFolder => &mut self.import_folder_dlg,
        }
    }

    // ── rfd (native) backend ────────────────────────────────────────────────

    fn spawn_rfd(&mut self, ctx: &egui::Context, kind: DialogKind, is_save: bool) {
        let (tx, rx) = mpsc::channel::<Vec<PathBuf>>();
        self.rfd_rx = Some((kind, rx));
        let ctx = ctx.clone();

        std::thread::spawn(move || {
            let paths: Vec<PathBuf> = match kind {
                DialogKind::OpenFile => rfd::FileDialog::new()
                    .add_filter("All supported", &all_supported_exts())
                    .add_filter("RasterLab Project", &["rlab"])
                    .add_filter("Images", &image_exts())
                    .add_filter("JPEG", &["jpg", "jpeg"])
                    .add_filter("PNG", &["png"])
                    .add_filter("Camera RAW", RAW_EXTENSIONS)
                    .pick_file()
                    .map(|p| vec![p])
                    .unwrap_or_default(),
                DialogKind::ExportImage => rfd::FileDialog::new()
                    .add_filter("JPEG", &["jpg", "jpeg"])
                    .add_filter("PNG", &["png"])
                    .save_file()
                    .map(|p| vec![p])
                    .unwrap_or_default(),
                DialogKind::SaveProject => rfd::FileDialog::new()
                    .add_filter("RasterLab Project", &["rlab"])
                    .save_file()
                    .map(|p| vec![p])
                    .unwrap_or_default(),
                DialogKind::ExportEditStack => rfd::FileDialog::new()
                    .add_filter("JSON", &["json"])
                    .save_file()
                    .map(|p| vec![p])
                    .unwrap_or_default(),
                DialogKind::LoadLut => rfd::FileDialog::new()
                    .add_filter("CUBE LUT", &["cube"])
                    .pick_file()
                    .map(|p| vec![p])
                    .unwrap_or_default(),
                DialogKind::PanoramaAddImage => rfd::FileDialog::new()
                    .add_filter("Images", &image_exts())
                    .add_filter("JPEG", &["jpg", "jpeg"])
                    .add_filter("PNG", &["png"])
                    .add_filter("Camera RAW", RAW_EXTENSIONS)
                    .pick_file()
                    .map(|p| vec![p])
                    .unwrap_or_default(),
                DialogKind::FocusStackAddImage => rfd::FileDialog::new()
                    .add_filter("Images", &image_exts())
                    .add_filter("JPEG", &["jpg", "jpeg"])
                    .add_filter("PNG", &["png"])
                    .add_filter("Camera RAW", RAW_EXTENSIONS)
                    .pick_file()
                    .map(|p| vec![p])
                    .unwrap_or_default(),
                DialogKind::HdrMergeAddImage => rfd::FileDialog::new()
                    .add_filter("Images", &image_exts())
                    .add_filter("JPEG", &["jpg", "jpeg"])
                    .add_filter("PNG", &["png"])
                    .add_filter("Camera RAW", RAW_EXTENSIONS)
                    .pick_file()
                    .map(|p| vec![p])
                    .unwrap_or_default(),
                DialogKind::OpenLibrary | DialogKind::ImportFolder => rfd::FileDialog::new()
                    .pick_folder()
                    .map(|p| vec![p])
                    .unwrap_or_default(),
                DialogKind::ImportFiles => rfd::FileDialog::new()
                    .add_filter("All supported", &all_supported_exts())
                    .add_filter("Images", &image_exts())
                    .add_filter("JPEG", &["jpg", "jpeg"])
                    .add_filter("PNG", &["png"])
                    .add_filter("Camera RAW", RAW_EXTENSIONS)
                    .pick_files()
                    .unwrap_or_default(),
            };
            let _ = tx.send(paths);
            ctx.request_repaint();
        });

        // rfd picks open/save per kind above; is_save not needed here
        let _ = is_save;
    }

    fn poll_rfd(&mut self) -> Option<(DialogKind, Vec<PathBuf>)> {
        let (kind, rx) = self.rfd_rx.take()?;
        match rx.try_recv() {
            Ok(paths) if !paths.is_empty() => Some((kind, paths)),
            Ok(_) => None, // cancelled (empty vec)
            Err(mpsc::TryRecvError::Empty) => {
                // Still waiting — put it back.
                self.rfd_rx = Some((kind, rx));
                None
            }
            Err(mpsc::TryRecvError::Disconnected) => None,
        }
    }

    // ── egui-file-dialog (inline) backend ───────────────────────────────────

    fn poll_egui(&mut self, ctx: &egui::Context) -> Option<(DialogKind, Vec<PathBuf>)> {
        let kind = self.pending?;
        let dlg = self.dialog_mut(kind);
        dlg.update(ctx);

        let result = if kind == DialogKind::ImportFiles {
            dlg.take_picked_multiple().map(|paths| (kind, paths))
        } else {
            dlg.take_picked().map(|path| (kind, vec![path]))
        };

        if result.is_some() {
            self.pending = None;
            return result;
        }

        // If the dialog was cancelled or closed without a pick, clear pending
        // so the next open call is not blocked.
        if matches!(dlg.state(), DialogState::Cancelled | DialogState::Closed) {
            self.pending = None;
        }

        None
    }
}
