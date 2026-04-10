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
    /// Which kind is currently waiting for a result.
    pending: Option<DialogKind>,

    // ── Native (rfd) backend ───────────────────────────────────────────────
    /// At most one rfd dialog is open at a time.
    rfd_rx: Option<(DialogKind, mpsc::Receiver<Option<PathBuf>>)>,
}

impl FileChooser {
    pub fn new(use_native: bool) -> Self {
        Self {
            use_native,
            open_dlg: FileDialog::new()
                .title("Open Image or Project")
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .add_file_filter_extensions(
                    "All supported",
                    vec!["rlab", "jpg", "jpeg", "png", "nef"],
                )
                .add_file_filter_extensions("RasterLab Project", vec!["rlab"])
                .add_file_filter_extensions("Images", vec!["jpg", "jpeg", "png", "nef"])
                .add_file_filter_extensions("JPEG", vec!["jpg", "jpeg"])
                .add_file_filter_extensions("PNG", vec!["png"])
                .add_file_filter_extensions("NEF (Nikon RAW)", vec!["nef"]),
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
                .add_file_filter_extensions("Images", vec!["jpg", "jpeg", "png", "nef"])
                .add_file_filter_extensions("JPEG", vec!["jpg", "jpeg"])
                .add_file_filter_extensions("PNG", vec!["png"])
                .add_file_filter_extensions("NEF (Nikon RAW)", vec!["nef"]),
            focus_stack_dlg: FileDialog::new()
                .title("Add Frame to Focus Stack")
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .add_file_filter_extensions("Images", vec!["jpg", "jpeg", "png", "nef"])
                .add_file_filter_extensions("JPEG", vec!["jpg", "jpeg"])
                .add_file_filter_extensions("PNG", vec!["png"])
                .add_file_filter_extensions("NEF (Nikon RAW)", vec!["nef"]),
            pending: None,
            rfd_rx: None,
        }
    }

    pub fn set_native(&mut self, native: bool) {
        self.use_native = native;
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

    // ── Per-frame polling ───────────────────────────────────────────────────

    /// Must be called once per frame.  Returns `Some((kind, path))` when the
    /// user has confirmed a selection, `None` otherwise.
    pub fn update(&mut self, ctx: &egui::Context) -> Option<(DialogKind, PathBuf)> {
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
            if is_save {
                dlg.save_file();
            } else {
                dlg.pick_file();
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
        }
    }

    // ── rfd (native) backend ────────────────────────────────────────────────

    fn spawn_rfd(&mut self, ctx: &egui::Context, kind: DialogKind, is_save: bool) {
        let (tx, rx) = mpsc::channel();
        self.rfd_rx = Some((kind, rx));
        let ctx = ctx.clone();

        std::thread::spawn(move || {
            let path = match kind {
                DialogKind::OpenFile => rfd::FileDialog::new()
                    .add_filter("All supported", &["rlab", "jpg", "jpeg", "png", "nef"])
                    .add_filter("RasterLab Project", &["rlab"])
                    .add_filter("Images", &["jpg", "jpeg", "png", "nef"])
                    .add_filter("JPEG", &["jpg", "jpeg"])
                    .add_filter("PNG", &["png"])
                    .add_filter("NEF (Nikon RAW)", &["nef"])
                    .pick_file(),
                DialogKind::ExportImage => rfd::FileDialog::new()
                    .add_filter("JPEG", &["jpg", "jpeg"])
                    .add_filter("PNG", &["png"])
                    .save_file(),
                DialogKind::SaveProject => rfd::FileDialog::new()
                    .add_filter("RasterLab Project", &["rlab"])
                    .save_file(),
                DialogKind::ExportEditStack => rfd::FileDialog::new()
                    .add_filter("JSON", &["json"])
                    .save_file(),
                DialogKind::LoadLut => rfd::FileDialog::new()
                    .add_filter("CUBE LUT", &["cube"])
                    .pick_file(),
                DialogKind::PanoramaAddImage => rfd::FileDialog::new()
                    .add_filter("Images", &["jpg", "jpeg", "png", "nef"])
                    .add_filter("JPEG", &["jpg", "jpeg"])
                    .add_filter("PNG", &["png"])
                    .add_filter("NEF (Nikon RAW)", &["nef"])
                    .pick_file(),
                DialogKind::FocusStackAddImage => rfd::FileDialog::new()
                    .add_filter("Images", &["jpg", "jpeg", "png", "nef"])
                    .add_filter("JPEG", &["jpg", "jpeg"])
                    .add_filter("PNG", &["png"])
                    .add_filter("NEF (Nikon RAW)", &["nef"])
                    .pick_file(),
            };
            let _ = tx.send(path);
            ctx.request_repaint();
        });

        // suppress unused warning for is_save — rfd picks open/save per kind above
        let _ = is_save;
    }

    fn poll_rfd(&mut self) -> Option<(DialogKind, PathBuf)> {
        let (kind, rx) = self.rfd_rx.take()?;
        match rx.try_recv() {
            Ok(Some(path)) => Some((kind, path)),
            Ok(None) => None, // cancelled
            Err(mpsc::TryRecvError::Empty) => {
                // Still waiting — put it back.
                self.rfd_rx = Some((kind, rx));
                None
            }
            Err(mpsc::TryRecvError::Disconnected) => None,
        }
    }

    // ── egui-file-dialog (inline) backend ───────────────────────────────────

    fn poll_egui(&mut self, ctx: &egui::Context) -> Option<(DialogKind, PathBuf)> {
        let kind = self.pending?;
        let dlg = self.dialog_mut(kind);
        dlg.update(ctx);

        if let Some(path) = dlg.take_picked() {
            self.pending = None;
            return Some((kind, path));
        }

        // If the dialog was cancelled or closed without a pick, clear pending
        // so the next open call is not blocked.
        if matches!(dlg.state(), DialogState::Cancelled | DialogState::Closed) {
            self.pending = None;
        }

        None
    }
}
