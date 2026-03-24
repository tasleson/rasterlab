//! Main application struct that wires together all panels.

use egui::{Context, Key, Modifiers};

use crate::{
    panels::{canvas::CanvasState, edit_stack, histogram_panel, tools},
    state::AppState,
};

pub struct RasterLabApp {
    state:  AppState,
    canvas: CanvasState,
}

impl RasterLabApp {
    pub fn new(_cc: &eframe::CreationContext) -> Self {
        Self {
            state:  AppState::new(),
            canvas: CanvasState::default(),
        }
    }

    fn handle_keyboard(&mut self, ctx: &Context) {
        ctx.input_mut(|i| {
            // Ctrl+Z / Ctrl+Y for undo/redo
            if i.consume_key(Modifiers::CTRL, Key::Z) {
                self.state.undo();
            }
            if i.consume_key(Modifiers::CTRL, Key::Y) {
                self.state.redo();
            }
            // Ctrl+O — open file
            #[cfg(not(target_arch = "wasm32"))]
            if i.consume_key(Modifiers::CTRL, Key::O) {
                self.open_file_dialog();
            }
            // Ctrl+S — save file
            #[cfg(not(target_arch = "wasm32"))]
            if i.consume_key(Modifiers::CTRL, Key::S) {
                self.save_file_dialog();
            }
        });
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn open_file_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Images", &["jpg", "jpeg", "png", "nef"])
            .add_filter("JPEG", &["jpg", "jpeg"])
            .add_filter("PNG",  &["png"])
            .add_filter("NEF (Nikon RAW)", &["nef"])
            .pick_file()
        {
            self.state.open_file(path);
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn save_file_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("JPEG", &["jpg", "jpeg"])
            .add_filter("PNG",  &["png"])
            .save_file()
        {
            self.state.save_file(path);
        }
    }
}

impl eframe::App for RasterLabApp {
    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        self.handle_keyboard(ctx);

        // ── Menu bar ─────────────────────────────────────────────────────
        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("File", |ui| {
                    #[cfg(not(target_arch = "wasm32"))]
                    if ui.button("Open…  (Ctrl+O)").clicked() {
                        ui.close_menu();
                        self.open_file_dialog();
                    }
                    #[cfg(not(target_arch = "wasm32"))]
                    if ui.button("Save…  (Ctrl+S)").clicked() {
                        ui.close_menu();
                        self.save_file_dialog();
                    }
                    ui.separator();
                    if ui.button("Quit").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
                ui.menu_button("Edit", |ui| {
                    if ui.add_enabled(self.state.can_undo(), egui::Button::new("Undo  (Ctrl+Z)")).clicked() {
                        ui.close_menu();
                        self.state.undo();
                    }
                    if ui.add_enabled(self.state.can_redo(), egui::Button::new("Redo  (Ctrl+Y)")).clicked() {
                        ui.close_menu();
                        self.state.redo();
                    }
                });
                ui.menu_button("Help", |ui| {
                    if ui.button("About RasterLab").clicked() {
                        ui.close_menu();
                        // TODO: show about dialog
                    }
                });
            });
        });

        // ── Status bar ───────────────────────────────────────────────────
        egui::TopBottomPanel::bottom("status_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
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
                // Upper half: edit stack
                egui::TopBottomPanel::top("edit_stack_panel")
                    .resizable(true)
                    .default_height(350.0)
                    .show_inside(ui, |ui| {
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            edit_stack::ui(ui, &mut self.state);
                        });
                    });

                // Lower half: histogram
                egui::ScrollArea::vertical().show(ui, |ui| {
                    histogram_panel::ui(ui, self.state.histogram.as_ref());
                });
            });

        // ── Central panel: Image canvas ───────────────────────────────────
        egui::CentralPanel::default().show(ctx, |ui| {
            self.canvas.ui(ui, self.state.rendered.as_ref());
        });
    }
}
