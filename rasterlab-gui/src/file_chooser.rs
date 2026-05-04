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

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc;

use egui_file_dialog::{DialogState, FileDialog};
use rasterlab_core::formats::raw::RAW_EXTENSIONS;

// ---------------------------------------------------------------------------
// Extension helpers
// ---------------------------------------------------------------------------

fn image_exts() -> Vec<&'static str> {
    let mut v = vec!["jpg", "jpeg", "png"];
    v.extend_from_slice(RAW_EXTENSIONS);
    v
}

fn all_supported_exts() -> Vec<&'static str> {
    let mut v = vec!["rlab", "jpg", "jpeg", "png"];
    v.extend_from_slice(RAW_EXTENSIONS);
    v
}

// ---------------------------------------------------------------------------
// DialogKind + spec
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DialogKind {
    OpenFile,
    ExportImage,
    SaveProject,
    ExportEditStack,
    LoadLut,
    PanoramaAddImage,
    FocusStackAddImage,
    HdrMergeAddImage,
    NewLibrary,
    OpenLibrary,
    ImportFiles,
    ImportFolder,
    ExportDestination,
}

#[derive(Clone, Copy)]
enum DialogMode {
    PickFile,
    SaveFile,
    PickDirectory,
    PickMultiple,
}

struct DialogSpec {
    title: &'static str,
    mode: DialogMode,
    filters: Vec<(&'static str, Vec<&'static str>)>,
    default_filename: Option<&'static str>,
}

impl DialogKind {
    const ALL: &[Self] = &[
        Self::OpenFile,
        Self::ExportImage,
        Self::SaveProject,
        Self::ExportEditStack,
        Self::LoadLut,
        Self::PanoramaAddImage,
        Self::FocusStackAddImage,
        Self::HdrMergeAddImage,
        Self::NewLibrary,
        Self::OpenLibrary,
        Self::ImportFiles,
        Self::ImportFolder,
        Self::ExportDestination,
    ];

    fn spec(self) -> DialogSpec {
        match self {
            Self::OpenFile => DialogSpec {
                title: "Open Image or Project",
                mode: DialogMode::PickFile,
                filters: vec![
                    ("All supported", all_supported_exts()),
                    ("RasterLab Project", vec!["rlab"]),
                    ("Images", image_exts()),
                    ("JPEG", vec!["jpg", "jpeg"]),
                    ("PNG", vec!["png"]),
                    ("Camera RAW", RAW_EXTENSIONS.to_vec()),
                ],
                default_filename: None,
            },
            Self::ExportImage => DialogSpec {
                title: "Export Image",
                mode: DialogMode::SaveFile,
                filters: vec![("JPEG", vec!["jpg", "jpeg"]), ("PNG", vec!["png"])],
                default_filename: None,
            },
            Self::SaveProject => DialogSpec {
                title: "Save Project",
                mode: DialogMode::SaveFile,
                filters: vec![("RasterLab Project", vec!["rlab"])],
                default_filename: Some("project.rlab"),
            },
            Self::ExportEditStack => DialogSpec {
                title: "Export Edit Stack",
                mode: DialogMode::SaveFile,
                filters: vec![("JSON", vec!["json"])],
                default_filename: Some("edit_stack.json"),
            },
            Self::LoadLut => DialogSpec {
                title: "Load LUT",
                mode: DialogMode::PickFile,
                filters: vec![("CUBE LUT", vec!["cube"])],
                default_filename: None,
            },
            Self::PanoramaAddImage | Self::FocusStackAddImage | Self::HdrMergeAddImage => {
                let title = match self {
                    Self::PanoramaAddImage => "Add Image to Panorama",
                    Self::FocusStackAddImage => "Add Frame to Focus Stack",
                    _ => "Add Bracketed Exposure",
                };
                DialogSpec {
                    title,
                    mode: DialogMode::PickFile,
                    filters: vec![
                        ("Images", image_exts()),
                        ("JPEG", vec!["jpg", "jpeg"]),
                        ("PNG", vec!["png"]),
                        ("Camera RAW", RAW_EXTENSIONS.to_vec()),
                    ],
                    default_filename: None,
                }
            }
            Self::NewLibrary => DialogSpec {
                title: "Create New Library",
                mode: DialogMode::PickDirectory,
                filters: vec![],
                default_filename: None,
            },
            Self::OpenLibrary => DialogSpec {
                title: "Select Library Folder",
                mode: DialogMode::PickDirectory,
                filters: vec![],
                default_filename: None,
            },
            Self::ImportFiles => DialogSpec {
                title: "Select Photos to Import",
                mode: DialogMode::PickMultiple,
                filters: vec![
                    ("All supported", all_supported_exts()),
                    ("Images", image_exts()),
                    ("JPEG", vec!["jpg", "jpeg"]),
                    ("PNG", vec!["png"]),
                    ("Camera RAW", RAW_EXTENSIONS.to_vec()),
                ],
                default_filename: None,
            },
            Self::ImportFolder => DialogSpec {
                title: "Select Folder to Import",
                mode: DialogMode::PickDirectory,
                filters: vec![],
                default_filename: None,
            },
            Self::ExportDestination => DialogSpec {
                title: "Select Export Destination",
                mode: DialogMode::PickDirectory,
                filters: vec![],
                default_filename: None,
            },
        }
    }
}

// ---------------------------------------------------------------------------
// FileChooser
// ---------------------------------------------------------------------------

pub struct FileChooser {
    use_native: bool,

    // ── Inline (egui-file-dialog) backend ─────────────────────────────────
    dialogs: HashMap<DialogKind, FileDialog>,
    pending: Option<DialogKind>,

    // ── Native (rfd) backend ──────────────────────────────────────────────
    rfd_rx: Option<(DialogKind, mpsc::Receiver<Vec<PathBuf>>)>,
    pending_start_dir: Option<PathBuf>,
}

impl FileChooser {
    pub fn new(use_native: bool, open_file_filter: Option<&str>) -> Self {
        let mut dialogs: HashMap<DialogKind, FileDialog> = DialogKind::ALL
            .iter()
            .map(|&kind| {
                let spec = kind.spec();
                let mut dlg = FileDialog::new()
                    .title(spec.title)
                    .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO);
                for (name, exts) in &spec.filters {
                    dlg = dlg.add_file_filter_extensions(name, exts.clone());
                }
                if let Some(filename) = spec.default_filename {
                    dlg = dlg.default_file_name(filename);
                }
                (kind, dlg)
            })
            .collect();

        if let Some(name) = open_file_filter
            && let Some(dlg) = dialogs.get_mut(&DialogKind::OpenFile)
        {
            dlg.config_mut().default_file_filter = Some(name.to_string());
        }

        Self {
            use_native,
            dialogs,
            pending: None,
            rfd_rx: None,
            pending_start_dir: None,
        }
    }

    pub fn set_native(&mut self, native: bool) {
        self.use_native = native;
    }

    #[allow(dead_code)]
    pub fn set_open_file_filter(&mut self, filter: Option<&str>) {
        if let Some(dlg) = self.dialogs.get_mut(&DialogKind::OpenFile) {
            dlg.config_mut().default_file_filter = filter.map(|s| s.to_string());
        }
    }

    // ── Opening ─────────────────────────────────────────────────────────────

    pub fn open_image(&mut self, ctx: &egui::Context) {
        self.open(ctx, DialogKind::OpenFile);
    }

    pub fn export_image(&mut self, ctx: &egui::Context) {
        self.open(ctx, DialogKind::ExportImage);
    }

    pub fn save_project(&mut self, ctx: &egui::Context) {
        self.open(ctx, DialogKind::SaveProject);
    }

    pub fn export_edit_stack(&mut self, ctx: &egui::Context) {
        self.open(ctx, DialogKind::ExportEditStack);
    }

    pub fn choose_export_destination(
        &mut self,
        ctx: &egui::Context,
        start_dir: Option<&std::path::Path>,
    ) {
        if let Some(dir) = start_dir {
            if let Some(dlg) = self.dialogs.get_mut(&DialogKind::ExportDestination) {
                dlg.config_mut().initial_directory = dir.to_path_buf();
            }
            self.pending_start_dir = Some(dir.to_path_buf());
        }
        self.open(ctx, DialogKind::ExportDestination);
    }

    pub fn open_kind(&mut self, ctx: &egui::Context, kind: DialogKind) {
        self.open(ctx, kind);
    }

    // ── Per-frame polling ───────────────────────────────────────────────────

    pub fn update(&mut self, ctx: &egui::Context) -> Option<(DialogKind, Vec<PathBuf>)> {
        if self.use_native {
            self.poll_rfd()
        } else {
            self.poll_egui(ctx)
        }
    }

    // ── Private helpers ─────────────────────────────────────────────────────

    fn open(&mut self, ctx: &egui::Context, kind: DialogKind) {
        if self.is_busy() {
            return;
        }

        if self.use_native {
            self.spawn_rfd(ctx, kind);
        } else {
            self.pending = Some(kind);
            let dlg = self.dialogs.get_mut(&kind).unwrap();
            match kind.spec().mode {
                DialogMode::PickDirectory => dlg.pick_directory(),
                DialogMode::PickMultiple => dlg.pick_multiple(),
                DialogMode::SaveFile => dlg.save_file(),
                DialogMode::PickFile => dlg.pick_file(),
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

    // ── rfd (native) backend ────────────────────────────────────────────────

    fn spawn_rfd(&mut self, ctx: &egui::Context, kind: DialogKind) {
        let (tx, rx) = mpsc::channel::<Vec<PathBuf>>();
        self.rfd_rx = Some((kind, rx));
        let ctx = ctx.clone();
        let spec = kind.spec();
        let start_dir = self.pending_start_dir.take();

        std::thread::spawn(move || {
            let mut dlg = rfd::FileDialog::new();
            for (name, exts) in &spec.filters {
                dlg = dlg.add_filter(*name, exts);
            }
            if let Some(start) = &start_dir {
                dlg = dlg.set_directory(start);
            }
            let paths = match spec.mode {
                DialogMode::PickFile => dlg.pick_file().map(|p| vec![p]).unwrap_or_default(),
                DialogMode::SaveFile => dlg.save_file().map(|p| vec![p]).unwrap_or_default(),
                DialogMode::PickDirectory => dlg.pick_folder().map(|p| vec![p]).unwrap_or_default(),
                DialogMode::PickMultiple => dlg.pick_files().unwrap_or_default(),
            };
            let _ = tx.send(paths);
            ctx.request_repaint();
        });
    }

    fn poll_rfd(&mut self) -> Option<(DialogKind, Vec<PathBuf>)> {
        let (kind, rx) = self.rfd_rx.take()?;
        match rx.try_recv() {
            Ok(paths) if !paths.is_empty() => Some((kind, paths)),
            Ok(_) => None,
            Err(mpsc::TryRecvError::Empty) => {
                self.rfd_rx = Some((kind, rx));
                None
            }
            Err(mpsc::TryRecvError::Disconnected) => None,
        }
    }

    // ── egui-file-dialog (inline) backend ───────────────────────────────────

    fn poll_egui(&mut self, ctx: &egui::Context) -> Option<(DialogKind, Vec<PathBuf>)> {
        let kind = self.pending?;
        let dlg = self.dialogs.get_mut(&kind).unwrap();
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

        if matches!(dlg.state(), DialogState::Cancelled | DialogState::Closed) {
            self.pending = None;
        }

        None
    }
}
