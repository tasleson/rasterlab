use egui::{ScrollArea, Sense, Vec2};
use rasterlab_library::{PhotoId, PhotoRow, SearchFilter, SortOrder};

use crate::state::{AppState, LibraryView};

// ── Public entry point ────────────────────────────────────────────────────────

pub fn ui(ui: &mut egui::Ui, state: &mut AppState) {
    if state.library.library.is_none() {
        no_library_ui(ui, state);
        return;
    }

    // Confirmation dialog (modal window)
    confirm_delete_dialog(ui.ctx(), state);

    // Toolbar (import button, sort, scale slider)
    toolbar_ui(ui, state);
    ui.separator();

    // Main body: sidebar + grid
    egui::Panel::left("lib_sidebar")
        .resizable(true)
        .default_size(200.0)
        .min_size(140.0)
        .show_inside(ui, |ui| sidebar_ui(ui, state));

    egui::CentralPanel::default().show_inside(ui, |ui| grid_ui(ui, state));
}

// ── No-library placeholder ────────────────────────────────────────────────────

fn no_library_ui(ui: &mut egui::Ui, _state: &mut AppState) {
    ui.centered_and_justified(|ui| {
        ui.label("No library open.\nUse File > New Library… or File > Open Library…");
    });
}

// ── Toolbar ───────────────────────────────────────────────────────────────────

fn toolbar_ui(ui: &mut egui::Ui, state: &mut AppState) {
    ui.horizontal(|ui| {
        // Sort order
        ui.label("Sort:");
        let cur = state.library.sort;
        egui::ComboBox::from_id_salt("lib_sort")
            .selected_text(sort_label(cur))
            .show_ui(ui, |ui| {
                for order in [
                    SortOrder::ImportDateDesc,
                    SortOrder::CaptureDateDesc,
                    SortOrder::CaptureDateAsc,
                    SortOrder::RatingDesc,
                    SortOrder::FilenameAsc,
                ] {
                    if ui
                        .selectable_value(&mut state.library.sort, order, sort_label(order))
                        .clicked()
                    {
                        state.library.refresh();
                    }
                }
            });

        ui.separator();

        // Thumbnail scale slider
        ui.label("Size:");
        let scale = &mut state.library.thumb_scale;
        if ui
            .add(egui::Slider::new(scale, 0.25f32..=1.0).show_value(false))
            .changed()
        {
            state.prefs.library_thumb_scale = *scale;
            state.prefs.save();
        }

        ui.separator();

        let count = state.library.results.len();
        let selected = state.library.selected.len();
        if selected > 0 {
            ui.label(format!("{} selected / {} photos", selected, count));
            ui.separator();
            if ui.button("Move to Trash").clicked() {
                state.library.confirm_delete = true;
            }
        } else {
            ui.label(format!("{} photos", count));
        }

        // Import progress
        if let Some(ref p) = state.library.import_progress {
            ui.separator();
            ui.spinner();
            ui.label(format!("Importing… {}/{}", p.done, p.total));
        }

        // Thumbnail load diagnostics — remove once thumbnails confirmed working
        {
            let cached = state.library.thumb_cache.len();
            let pending = state.library.thumb_requested.len();
            if pending > cached {
                ui.separator();
                ui.label(format!("Loading thumbs… {}/{}", cached, pending));
            }
        }
    });
}

// ── Confirmation dialog ───────────────────────────────────────────────────────

fn confirm_delete_dialog(ctx: &egui::Context, state: &mut AppState) {
    if !state.library.confirm_delete {
        return;
    }
    let n = state.library.selected.len();
    let title = if n == 1 {
        "Move to Trash?".to_owned()
    } else {
        format!("Move {} photos to Trash?", n)
    };
    let mut open = true;
    egui::Window::new(&title)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .open(&mut open)
        .show(ctx, |ui| {
            ui.label("The selected photo(s) will be moved to your system trash.");
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Move to Trash").clicked() {
                    state.library.confirm_delete = false;
                    state.library.delete_selected();
                }
                if ui.button("Cancel").clicked() {
                    state.library.confirm_delete = false;
                }
            });
        });
    if !open {
        state.library.confirm_delete = false;
    }
}

fn sort_label(s: SortOrder) -> &'static str {
    match s {
        SortOrder::ImportDateDesc => "Import date (newest)",
        SortOrder::CaptureDateDesc => "Capture date (newest)",
        SortOrder::CaptureDateAsc => "Capture date (oldest)",
        SortOrder::RatingDesc => "Rating (highest)",
        SortOrder::FilenameAsc => "Filename (A–Z)",
    }
}

// ── Sidebar ───────────────────────────────────────────────────────────────────

fn sidebar_ui(ui: &mut egui::Ui, state: &mut AppState) {
    ScrollArea::vertical().show(ui, |ui| {
        // ── Navigation ────────────────────────────────────────────────────
        ui.strong("Library");

        let all_selected = state.library.view == LibraryView::AllPhotos;
        let total = {
            // Show total from sessions sum so we don't need a separate query
            state.library.results.len()
        };
        if ui
            .selectable_label(all_selected, format!("All Photos ({})", total))
            .clicked()
        {
            state.library.view = LibraryView::AllPhotos;
            state.library.filter = SearchFilter::default();
            state.library.refresh();
        }

        // Sessions
        ui.add_space(4.0);
        ui.strong("Import Sessions");
        let sessions = state.library.sessions.clone();
        for sess in &sessions {
            let selected = state.library.view == LibraryView::Session(sess.id.clone());
            let label = format!("{}  ({})", sess.name, sess.photo_count);
            if ui.selectable_label(selected, label).clicked() {
                state.library.view = LibraryView::Session(sess.id.clone());
                state.library.filter = SearchFilter::default();
                state.library.refresh();
            }
        }

        // Collections
        ui.add_space(4.0);
        ui.strong("Collections");
        let collections = state.library.collections.clone();
        for coll in &collections {
            let selected = state.library.view == LibraryView::Collection(coll.id);
            if ui.selectable_label(selected, &coll.name).clicked() {
                state.library.view = LibraryView::Collection(coll.id);
                state.library.filter = SearchFilter::default();
                state.library.refresh();
            }
        }

        ui.add_space(8.0);
        ui.separator();

        // ── Filters ───────────────────────────────────────────────────────
        ui.strong("Filter");
        let mut changed = false;

        // Rating
        ui.horizontal(|ui| {
            ui.label("Min rating:");
            let cur = state.library.filter.rating_min.unwrap_or(0);
            let mut v = cur;
            if ui.add(egui::Slider::new(&mut v, 0u8..=5)).changed() {
                state.library.filter.rating_min = if v > 0 { Some(v) } else { None };
                changed = true;
            }
        });

        // Flag
        ui.horizontal(|ui| {
            ui.label("Flag:");
            let cur = state.library.filter.flag.clone();
            let label = cur.as_deref().unwrap_or("Any");
            egui::ComboBox::from_id_salt("lib_flag_filter")
                .selected_text(label)
                .show_ui(ui, |ui| {
                    for opt in [None, Some("pick"), Some("reject")] {
                        let lbl = opt.unwrap_or("Any");
                        if ui.selectable_label(cur.as_deref() == opt, lbl).clicked() {
                            state.library.filter.flag = opt.map(|s| s.to_owned());
                            changed = true;
                        }
                    }
                });
        });

        // Text search
        ui.horizontal(|ui| {
            ui.label("Search:");
            let mut text = state.library.filter.text.clone().unwrap_or_default();
            if ui.text_edit_singleline(&mut text).changed() {
                state.library.filter.text = if text.is_empty() { None } else { Some(text) };
                changed = true;
            }
        });

        // Camera model
        ui.horizontal(|ui| {
            ui.label("Camera:");
            let mut cam = state
                .library
                .filter
                .camera_model
                .clone()
                .unwrap_or_default();
            if ui.text_edit_singleline(&mut cam).changed() {
                state.library.filter.camera_model = if cam.is_empty() { None } else { Some(cam) };
                changed = true;
            }
        });

        if changed {
            state.library.refresh();
        }

        // Clear filters button
        if !state.library.filter.is_empty() {
            ui.add_space(4.0);
            if ui.button("Clear Filters").clicked() {
                state.library.filter = SearchFilter::default();
                state.library.refresh();
            }
        }
    });
}

// ── Thumbnail grid ────────────────────────────────────────────────────────────

fn grid_ui(ui: &mut egui::Ui, state: &mut AppState) {
    let thumb_px = (512.0 * state.library.thumb_scale).max(64.0);
    let padding = 6.0;
    let cell_sz = thumb_px + padding * 2.0;

    let avail_w = ui.available_width();
    let cols = ((avail_w / cell_sz) as usize).max(1);

    // Deselect when clicking on the grid background (between cells).
    // Use interact() rather than allocate_rect() so the layout cursor is not
    // advanced — allocate_rect() would consume the entire available area and
    // leave the ScrollArea with zero height.
    let bg_id = ui.id().with("lib_grid_bg");
    let bg_rect = ui.available_rect_before_wrap();
    let bg_resp = ui.interact(bg_rect, bg_id, Sense::click());
    if bg_resp.clicked() {
        state.library.select_none();
    }

    ScrollArea::vertical()
        .id_salt("lib_grid_scroll")
        .show(ui, |ui| {
            let photos = state.library.results.clone();
            let rows = photos.chunks(cols);
            for row in rows {
                ui.horizontal(|ui| {
                    for photo in row {
                        thumb_cell(ui, state, photo, thumb_px, padding);
                    }
                });
                ui.add_space(padding);
            }
        });
}

fn thumb_cell(
    ui: &mut egui::Ui,
    state: &mut AppState,
    photo: &PhotoRow,
    thumb_px: f32,
    padding: f32,
) {
    let selected = state.library.is_selected(photo.id);
    let cell_size = Vec2::splat(thumb_px + padding * 2.0);

    let (rect, resp) = ui.allocate_exact_size(cell_size, Sense::click());

    // Selection highlight
    if selected {
        ui.painter()
            .rect_filled(rect, 4.0, ui.visuals().selection.bg_fill);
    } else if resp.hovered() {
        ui.painter()
            .rect_filled(rect, 4.0, ui.visuals().widgets.hovered.bg_fill);
    }

    let img_rect = rect.shrink(padding);

    // Draw thumbnail or placeholder
    if let Some(tex) = state.library.thumb_cache.get(&photo.hash) {
        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        ui.painter()
            .image(tex.id(), img_rect, uv, egui::Color32::WHITE);
    } else {
        // Placeholder grey rect
        ui.painter()
            .rect_filled(img_rect, 2.0, egui::Color32::from_gray(60));
        // Request background load
        state.request_thumb_load(photo.hash.clone());
    }

    // Rating stars overlay (bottom of cell)
    let rating = 0u8; // would need to join ratings table; show zero for now
    if rating > 0 {
        let star_y = img_rect.max.y - 14.0;
        let star_x = img_rect.min.x + 2.0;
        for i in 0..5u8 {
            let col = if i < rating {
                egui::Color32::from_rgb(255, 200, 0)
            } else {
                egui::Color32::from_gray(80)
            };
            let cx = star_x + i as f32 * 13.0 + 6.0;
            ui.painter().circle_filled(egui::pos2(cx, star_y), 4.0, col);
        }
    }

    // Click handling
    if resp.clicked() {
        let id: PhotoId = photo.id;
        if ui.input(|i| i.modifiers.ctrl) {
            state.library.toggle_select(id);
        } else if ui.input(|i| i.modifiers.shift) {
            // Range select: select from last selected to this photo
            if let Some(&last) = state.library.selected.last() {
                let results = &state.library.results;
                let last_pos = results.iter().position(|p| p.id == last);
                let this_pos = results.iter().position(|p| p.id == id);
                if let (Some(a), Some(b)) = (last_pos, this_pos) {
                    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
                    for p in &results[lo..=hi] {
                        if !state.library.is_selected(p.id) {
                            state.library.selected.push(p.id);
                        }
                    }
                }
            } else {
                state.library.select_only(id);
            }
        } else {
            state.library.select_only(id);
        }
    }

    if resp.double_clicked() {
        // Open in editor
        if let Some(lib) = &state.library.library {
            let rlab_path = lib.rlab_path(&photo.hash);
            state.library_context = Some((lib.root().to_path_buf(), photo.hash.clone()));
            state.open_file(rlab_path);
            state.mode = crate::state::AppMode::Editor;
        }
    }

    resp.context_menu(|ui| {
        // Ensure the right-clicked photo is selected
        let id: PhotoId = photo.id;
        if !state.library.is_selected(id) {
            state.library.select_only(id);
        }
        let n = state.library.selected.len();
        let label = if n == 1 {
            "Move to Trash".to_owned()
        } else {
            format!("Move {} to Trash", n)
        };
        if ui.button(label).clicked() {
            state.library.confirm_delete = true;
            ui.close();
        }
    });
}
