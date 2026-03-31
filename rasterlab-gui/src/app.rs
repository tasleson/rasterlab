//! Main application struct that wires together all panels.

use std::path::PathBuf;
use std::sync::mpsc;

use egui::{Context, Key, Modifiers};

use crate::{
    panels::{canvas::CanvasState, edit_stack, histogram_panel, tools},
    state::AppState,
};

pub struct RasterLabApp {
    state: AppState,
    canvas: CanvasState,
    /// Receives the path chosen by an in-progress open dialog (None = cancelled).
    #[cfg(not(target_arch = "wasm32"))]
    open_rx: Option<mpsc::Receiver<Option<PathBuf>>>,
    /// Receives the path chosen by an in-progress Export dialog.
    #[cfg(not(target_arch = "wasm32"))]
    export_rx: Option<mpsc::Receiver<Option<PathBuf>>>,
    /// Receives the path chosen by an in-progress Save As project dialog.
    #[cfg(not(target_arch = "wasm32"))]
    project_save_rx: Option<mpsc::Receiver<Option<PathBuf>>>,
    /// Receives the path chosen by an in-progress Export Edit Stack dialog.
    #[cfg(not(target_arch = "wasm32"))]
    json_export_rx: Option<mpsc::Receiver<Option<PathBuf>>>,
    /// Receives the path chosen by an in-progress Load LUT dialog.
    #[cfg(not(target_arch = "wasm32"))]
    lut_rx: Option<mpsc::Receiver<Option<PathBuf>>>,
}

impl RasterLabApp {
    pub fn new(cc: &eframe::CreationContext, initial_file: Option<PathBuf>) -> Self {
        let mut state = AppState::new(cc.egui_ctx.clone());

        // Apply the stored theme preference.  For ThemePreference::System the fallback
        // is set to Light so the app looks correct on platforms where winit cannot
        // detect the OS theme (returns None).
        cc.egui_ctx.options_mut(|o| {
            o.theme_preference = state.prefs.theme.to_egui();
            o.fallback_theme = egui::Theme::Light;
        });
        if let Some(path) = initial_file {
            state.open_file(path);
        }
        Self {
            state,
            canvas: CanvasState::default(),
            #[cfg(not(target_arch = "wasm32"))]
            open_rx: None,
            #[cfg(not(target_arch = "wasm32"))]
            export_rx: None,
            #[cfg(not(target_arch = "wasm32"))]
            project_save_rx: None,
            #[cfg(not(target_arch = "wasm32"))]
            json_export_rx: None,
            #[cfg(not(target_arch = "wasm32"))]
            lut_rx: None,
        }
    }

    fn handle_keyboard(&mut self, ctx: &Context) {
        ctx.input_mut(|i| {
            if i.consume_key(Modifiers::CTRL, Key::Z) {
                self.state.undo();
            }
            if i.consume_key(Modifiers::CTRL, Key::Y) {
                self.state.redo();
            }
            #[cfg(not(target_arch = "wasm32"))]
            if i.consume_key(Modifiers::CTRL, Key::O) {
                self.open_file_dialog(ctx);
            }
            #[cfg(not(target_arch = "wasm32"))]
            if i.consume_key(Modifiers::CTRL, Key::S) {
                self.save_project_or_prompt(ctx);
            }
            #[cfg(not(target_arch = "wasm32"))]
            if i.consume_key(Modifiers::CTRL | Modifiers::SHIFT, Key::S) {
                self.project_save_dialog(ctx);
            }
            #[cfg(not(target_arch = "wasm32"))]
            if i.consume_key(Modifiers::CTRL, Key::E) {
                self.export_file_dialog(ctx);
            }
        });
    }

    /// Spawn the open dialog on a background thread so the event loop keeps running.
    #[cfg(not(target_arch = "wasm32"))]
    fn open_file_dialog(&mut self, ctx: &Context) {
        if self.open_rx.is_some() {
            return;
        } // dialog already open
        let (tx, rx) = mpsc::channel();
        self.open_rx = Some(rx);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let path = rfd::FileDialog::new()
                .add_filter("All supported", &["rlab", "jpg", "jpeg", "png", "nef"])
                .add_filter("RasterLab Project", &["rlab"])
                .add_filter("Images", &["jpg", "jpeg", "png", "nef"])
                .add_filter("JPEG", &["jpg", "jpeg"])
                .add_filter("PNG", &["png"])
                .add_filter("NEF (Nikon RAW)", &["nef"])
                .pick_file();
            let _ = tx.send(path);
            ctx.request_repaint();
        });
    }

    /// Spawn the Export dialog (save rendered image as JPEG/PNG) on a background thread.
    #[cfg(not(target_arch = "wasm32"))]
    fn export_file_dialog(&mut self, ctx: &Context) {
        if self.export_rx.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.export_rx = Some(rx);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let path = rfd::FileDialog::new()
                .add_filter("JPEG", &["jpg", "jpeg"])
                .add_filter("PNG", &["png"])
                .save_file();
            let _ = tx.send(path);
            ctx.request_repaint();
        });
    }

    /// Save in-place if a project path is already known; otherwise open Save As.
    #[cfg(not(target_arch = "wasm32"))]
    fn save_project_or_prompt(&mut self, ctx: &Context) {
        if let Some(path) = self.state.project_path.clone() {
            self.state.save_project(path);
        } else {
            self.project_save_dialog(ctx);
        }
    }

    /// Open a Save As dialog for writing a `.rlab` project file.
    #[cfg(not(target_arch = "wasm32"))]
    fn project_save_dialog(&mut self, ctx: &Context) {
        if self.project_save_rx.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.project_save_rx = Some(rx);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let path = rfd::FileDialog::new()
                .add_filter("RasterLab Project", &["rlab"])
                .save_file();
            let _ = tx.send(path);
            ctx.request_repaint();
        });
    }

    /// Open a save dialog for exporting the edit stack as a JSON file.
    #[cfg(not(target_arch = "wasm32"))]
    fn export_edit_stack_dialog(&mut self, ctx: &Context) {
        if self.json_export_rx.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.json_export_rx = Some(rx);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let path = rfd::FileDialog::new()
                .add_filter("JSON", &["json"])
                .save_file();
            let _ = tx.send(path);
            ctx.request_repaint();
        });
    }

    /// Spawn the Load LUT dialog on a background thread.
    #[cfg(not(target_arch = "wasm32"))]
    fn lut_file_dialog(&mut self, ctx: &Context) {
        if self.lut_rx.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.lut_rx = Some(rx);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let path = rfd::FileDialog::new()
                .add_filter("CUBE LUT", &["cube"])
                .pick_file();
            let _ = tx.send(path);
            ctx.request_repaint();
        });
    }

    /// Poll dialog result channels and act on completed dialogs.
    #[cfg(not(target_arch = "wasm32"))]
    fn poll_dialogs(&mut self) {
        if let Some(rx) = &self.open_rx
            && let Ok(maybe_path) = rx.try_recv()
        {
            if let Some(path) = maybe_path {
                self.state.open_file(path);
            }
            self.open_rx = None;
        }
        if let Some(rx) = &self.export_rx
            && let Ok(maybe_path) = rx.try_recv()
        {
            if let Some(path) = maybe_path {
                self.state.save_file(path);
            }
            self.export_rx = None;
        }
        if let Some(rx) = &self.project_save_rx
            && let Ok(maybe_path) = rx.try_recv()
        {
            if let Some(path) = maybe_path {
                self.state.save_project(path);
            }
            self.project_save_rx = None;
        }
        if let Some(rx) = &self.json_export_rx
            && let Ok(maybe_path) = rx.try_recv()
        {
            if let Some(path) = maybe_path {
                self.state.export_edit_stack_json(path);
            }
            self.json_export_rx = None;
        }
        if let Some(rx) = &self.lut_rx
            && let Ok(maybe_path) = rx.try_recv()
        {
            if let Some(path) = maybe_path {
                self.state.load_lut(path);
            }
            self.lut_rx = None;
        }
    }
}

impl eframe::App for RasterLabApp {
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.state.prefs.save();
    }

    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        // Save prefs when the window close is requested (more reliable than
        // on_exit alone, which may not fire on all platforms/close paths).
        if ctx.input(|i| i.viewport().close_requested()) {
            self.state.prefs.save();
        }

        self.state.poll_background();
        #[cfg(not(target_arch = "wasm32"))]
        self.poll_dialogs();
        #[cfg(not(target_arch = "wasm32"))]
        if self.state.lut_dialog_requested {
            self.state.lut_dialog_requested = false;
            self.lut_file_dialog(ctx);
        }

        self.handle_keyboard(ctx);

        // ── Window title (reflects project name and dirty state) ──────────
        {
            let dirty_marker = if self.state.is_dirty { " ●" } else { "" };
            let title = match &self.state.project_path {
                Some(p) => format!(
                    "RasterLab — {}{}",
                    p.file_name().unwrap_or_default().to_string_lossy(),
                    dirty_marker
                ),
                None if self.state.pipeline.is_some() => {
                    format!("RasterLab — Unsaved Project{}", dirty_marker)
                }
                None => "RasterLab".to_string(),
            };
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
        }

        // ── Menu bar ─────────────────────────────────────────────────────
        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("File", |ui| {
                    #[cfg(not(target_arch = "wasm32"))]
                    if ui.button("Open…  (Ctrl+O)").clicked() {
                        ui.close_menu();
                        self.open_file_dialog(ctx);
                    }
                    #[cfg(not(target_arch = "wasm32"))]
                    {
                        ui.separator();
                        if ui
                            .add_enabled(
                                self.state.pipeline.is_some(),
                                egui::Button::new("Save  (Ctrl+S)"),
                            )
                            .clicked()
                        {
                            ui.close_menu();
                            self.save_project_or_prompt(ctx);
                        }
                        if self.state.project_path.is_some()
                            && ui
                                .add_enabled(
                                    self.state.pipeline.is_some(),
                                    egui::Button::new("Save As…  (Ctrl+⇧S)"),
                                )
                                .clicked()
                        {
                            ui.close_menu();
                            self.project_save_dialog(ctx);
                        }
                        ui.separator();
                        if ui
                            .add_enabled(
                                self.state.pipeline.is_some(),
                                egui::Button::new("Export…  (Ctrl+E)"),
                            )
                            .clicked()
                        {
                            ui.close_menu();
                            self.export_file_dialog(ctx);
                        }
                        if ui
                            .add_enabled(
                                self.state.pipeline.is_some(),
                                egui::Button::new("Export Edit Stack as JSON…"),
                            )
                            .clicked()
                        {
                            ui.close_menu();
                            self.export_edit_stack_dialog(ctx);
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
                        ui.close_menu();
                        self.state.undo();
                    }
                    if ui
                        .add_enabled(self.state.can_redo(), egui::Button::new("Redo  (Ctrl+Y)"))
                        .clicked()
                    {
                        ui.close_menu();
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
                                ui.close_menu();
                            }
                        }
                    });
                });
                ui.menu_button("Help", |ui| {
                    if ui.button("About RasterLab").clicked() {
                        ui.close_menu();
                    }
                });
            });
        });

        // ── Status bar ───────────────────────────────────────────────────
        egui::TopBottomPanel::bottom("status_bar").show(ctx, |ui| {
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

        // ── Left panel: Tools ─────────────────────────────────────────────
        egui::SidePanel::left("tools_panel")
            .resizable(true)
            .default_width(220.0)
            .min_width(180.0)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    tools::ui(ui, &mut self.state);
                });
            });

        // ── Right panel: Edit stack + Histogram ───────────────────────────
        egui::SidePanel::right("right_panel")
            .resizable(true)
            .default_width(280.0)
            .min_width(220.0)
            .show(ctx, |ui| {
                // Histogram pinned to the bottom; must be declared before the
                // fill content so egui reserves the space correctly.
                egui::TopBottomPanel::bottom("histogram_panel")
                    .resizable(true)
                    .default_height(200.0)
                    .min_height(80.0)
                    .show_inside(ui, |ui| {
                        histogram_panel::ui(ui, self.state.histogram.as_ref());
                    });

                // Edit stack fills whatever space remains above the histogram.
                egui::ScrollArea::vertical().show(ui, |ui| {
                    edit_stack::ui(ui, &mut self.state);
                });
            });

        // ── Central panel: Image canvas ───────────────────────────────────
        egui::CentralPanel::default().show(ctx, |ui| {
            self.canvas.ui(ui, &mut self.state);
        });
    }
}
