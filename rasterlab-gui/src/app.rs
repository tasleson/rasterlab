//! Main application struct that wires together all panels.

use std::path::PathBuf;

use egui::{Context, Key, Modifiers};

use crate::{
    file_chooser::{DialogKind, FileChooser},
    panels::{canvas::CanvasState, edit_stack, histogram_panel, tools},
    state::AppState,
};

pub struct RasterLabApp {
    state: AppState,
    canvas: CanvasState,
    #[cfg(not(target_arch = "wasm32"))]
    chooser: FileChooser,
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
        #[cfg(not(target_arch = "wasm32"))]
        let use_native = state.prefs.use_native_dialogs;
        Self {
            state,
            canvas: CanvasState::default(),
            #[cfg(not(target_arch = "wasm32"))]
            chooser: FileChooser::new(use_native),
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
                self.chooser.open_image(ctx);
            }
            #[cfg(not(target_arch = "wasm32"))]
            if i.consume_key(Modifiers::CTRL, Key::S) {
                self.save_project_or_prompt(ctx);
            }
            #[cfg(not(target_arch = "wasm32"))]
            if i.consume_key(Modifiers::CTRL | Modifiers::SHIFT, Key::S) {
                self.chooser.save_project(ctx);
            }
            #[cfg(not(target_arch = "wasm32"))]
            if i.consume_key(Modifiers::CTRL, Key::E) {
                self.chooser.export_image(ctx);
            }
        });
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
        if let Some((kind, path)) = self.chooser.update(ctx) {
            match kind {
                DialogKind::OpenFile => self.state.open_file(path),
                DialogKind::ExportImage => self.state.save_file(path),
                DialogKind::SaveProject => self.state.save_project(path),
                DialogKind::ExportEditStack => self.state.export_edit_stack_json(path),
                DialogKind::LoadLut => self.state.load_lut(path),
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

        // Save prefs when the window close is requested (more reliable than
        // on_exit alone, which may not fire on all platforms/close paths).
        if ctx.input(|i| i.viewport().close_requested()) {
            self.state.prefs.save();
        }

        self.state.poll_background();
        #[cfg(not(target_arch = "wasm32"))]
        self.poll_dialogs(&ctx);
        #[cfg(not(target_arch = "wasm32"))]
        if self.state.lut_dialog_requested {
            self.state.lut_dialog_requested = false;
            self.chooser.load_lut(&ctx);
        }

        self.handle_keyboard(&ctx);

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
        egui::Panel::top("menu_bar").show_inside(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("File", |ui| {
                    #[cfg(not(target_arch = "wasm32"))]
                    if ui.button("Open…  (Ctrl+O)").clicked() {
                        ui.close_kind(egui::UiKind::Menu);
                        self.chooser.open_image(&ctx);
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
                            ui.close_kind(egui::UiKind::Menu);
                            self.save_project_or_prompt(&ctx);
                        }
                        if self.state.project_path.is_some()
                            && ui
                                .add_enabled(
                                    self.state.pipeline.is_some(),
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
                                self.state.pipeline.is_some(),
                                egui::Button::new("Export…  (Ctrl+E)"),
                            )
                            .clicked()
                        {
                            ui.close_kind(egui::UiKind::Menu);
                            self.chooser.export_image(&ctx);
                        }
                        if ui
                            .add_enabled(
                                self.state.pipeline.is_some(),
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
                    });
                });
                ui.menu_button("Help", |ui| {
                    if ui.button("About RasterLab").clicked() {
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

        // ── Left panel: Tools ─────────────────────────────────────────────
        egui::Panel::left("tools_panel")
            .resizable(true)
            .default_size(220.0)
            .min_size(180.0)
            .show_inside(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    tools::ui(ui, &mut self.state);
                });
            });

        // ── Right panel: Edit stack + Histogram ───────────────────────────
        egui::Panel::right("right_panel")
            .resizable(true)
            .default_size(280.0)
            .min_size(220.0)
            .show_inside(ui, |ui| {
                // Histogram pinned to the bottom; must be declared before the
                // fill content so egui reserves the space correctly.
                egui::Panel::bottom("histogram_panel")
                    .resizable(true)
                    .default_size(200.0)
                    .min_size(80.0)
                    .show_inside(ui, |ui| {
                        histogram_panel::ui(ui, self.state.histogram.as_ref());
                    });

                // Edit stack fills whatever space remains above the histogram.
                egui::ScrollArea::vertical().show(ui, |ui| {
                    edit_stack::ui(ui, &mut self.state);
                });
            });

        // ── Central panel: Image canvas ───────────────────────────────────
        egui::CentralPanel::default().show_inside(ui, |ui| {
            self.canvas.ui(ui, &mut self.state);
        });
    }
}
