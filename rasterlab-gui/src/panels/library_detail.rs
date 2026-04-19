use egui::ScrollArea;
use rasterlab_library::PhotoId;

use crate::state::AppState;

/// Right-side detail / metadata-edit panel for the library view.
pub fn ui(ui: &mut egui::Ui, state: &mut AppState) {
    let selected: Vec<PhotoId> = state.library.selected.clone();

    match selected.len() {
        0 => {
            ui.centered_and_justified(|ui| {
                ui.label("Select a photo to view details.");
            });
        }
        1 => single_photo_ui(ui, state, selected[0]),
        n => multi_photo_ui(ui, state, &selected, n),
    }
}

// ── Single-photo detail ───────────────────────────────────────────────────────

fn single_photo_ui(ui: &mut egui::Ui, state: &mut AppState, id: PhotoId) {
    let Some(photo) = state.library.results.iter().find(|p| p.id == id).cloned() else {
        return;
    };

    ScrollArea::vertical().show(ui, |ui| {
        // Thumbnail preview
        if let Some(tex) = state.library.thumb_cache.get(&photo.hash) {
            let avail = ui.available_width();
            let size = egui::Vec2::splat(avail.min(200.0));
            ui.image(egui::load::SizedTexture::new(tex.id(), size));
            ui.add_space(4.0);
        }

        // File info
        if let Some(ref name) = photo.original_filename {
            ui.strong(name);
        }
        ui.label(format!("{}×{}", photo.width, photo.height));
        if let Some(ref date) = photo.capture_date {
            ui.label(format!("Captured: {}", &date[..date.len().min(19)]));
        }

        ui.separator();
        ui.strong("EXIF");

        // Fetch EXIF from the .rlab file if library is open
        if let Some(lib) = state.library.library.clone() {
            let rlab_path = lib.rlab_path(&photo.hash);
            if let Ok(rlab) = rasterlab_core::project::RlabFile::read(&rlab_path)
                && let Some(lmta) = &rlab.lmta
                && let Some(exif) = &lmta.exif
            {
                exif_table(ui, exif);
            }
        }

        ui.separator();
        ui.strong("Metadata");

        // Editable fields — read from .rlab lmta
        if let Some(lib) = state.library.library.clone() {
            let rlab_path = lib.rlab_path(&photo.hash);
            if let Ok(rlab) = rasterlab_core::project::RlabFile::read(&rlab_path)
                && let Some(mut lmta) = rlab.lmta.clone()
            {
                let mut dirty = false;

                // Rating
                ui.horizontal(|ui| {
                    ui.label("Rating:");
                    for star in 0u8..=5 {
                        let filled = star <= lmta.rating;
                        let label = if filled { "★" } else { "☆" };
                        if ui
                            .selectable_label(filled && star == lmta.rating, label)
                            .clicked()
                        {
                            lmta.rating = if lmta.rating == star { 0 } else { star };
                            dirty = true;
                        }
                    }
                });

                // Flag
                ui.horizontal(|ui| {
                    ui.label("Flag:");
                    for flag_opt in [None, Some("pick"), Some("reject")] {
                        let active = lmta.flag.as_deref() == flag_opt;
                        if ui
                            .selectable_label(active, flag_opt.unwrap_or("—"))
                            .clicked()
                        {
                            lmta.flag = flag_opt.map(|s| s.to_owned());
                            dirty = true;
                        }
                    }
                });

                // Color label
                ui.horizontal(|ui| {
                    ui.label("Color:");
                    for color in [
                        None,
                        Some("red"),
                        Some("yellow"),
                        Some("green"),
                        Some("blue"),
                        Some("purple"),
                    ] {
                        let active = lmta.color_label.as_deref() == color;
                        let display = color.unwrap_or("—");
                        if ui.selectable_label(active, display).clicked() {
                            lmta.color_label = color.map(|s| s.to_owned());
                            dirty = true;
                        }
                    }
                });

                // Caption
                ui.label("Caption:");
                let mut caption = lmta.caption.clone().unwrap_or_default();
                if ui.text_edit_multiline(&mut caption).changed() {
                    lmta.caption = if caption.is_empty() {
                        None
                    } else {
                        Some(caption)
                    };
                    dirty = true;
                }

                // Keywords
                ui.label("Keywords:");
                let kw_str: String = lmta.keywords.join(", ");
                let mut kw_edit = kw_str.clone();
                if ui.text_edit_singleline(&mut kw_edit).changed() {
                    lmta.keywords = kw_edit
                        .split(',')
                        .map(|s| s.trim().to_owned())
                        .filter(|s| !s.is_empty())
                        .collect();
                    dirty = true;
                }

                if dirty && let Some(lib) = &state.library.library {
                    lib.update_metadata(id, lmta).ok();
                    state.library.refresh();
                }
            }
        }

        // Library path
        let h = &photo.hash;
        let rel_path = format!("files/{}/{}/{}.rlab", &h[0..2], &h[2..4], h);
        ui.add_space(6.0);
        egui::Grid::new("meta_path_grid")
            .num_columns(2)
            .spacing([8.0, 2.0])
            .show(ui, |ui| {
                ui.label("Library path:");
                ui.add(egui::Label::new(egui::RichText::new(&rel_path).monospace()).truncate());
                ui.end_row();
            });

        ui.separator();

        // Open in editor button
        if ui.button("Open in Editor").clicked()
            && let Some(lib) = &state.library.library
        {
            let rlab_path = lib.rlab_path(&photo.hash);
            state.library_context = Some((lib.root().to_path_buf(), photo.hash.clone()));
            state.open_file(rlab_path);
            state.mode = crate::state::AppMode::Editor;
        }
    });
}

// ── EXIF table ────────────────────────────────────────────────────────────────

fn exif_table(ui: &mut egui::Ui, exif: &rasterlab_library::LibraryExif) {
    egui::Grid::new("exif_grid")
        .num_columns(2)
        .spacing([8.0, 2.0])
        .show(ui, |ui| {
            if let Some(ref v) = exif.camera_make {
                ui.label("Make:");
                ui.label(v);
                ui.end_row();
            }
            if let Some(ref v) = exif.camera_model {
                ui.label("Camera:");
                ui.label(v);
                ui.end_row();
            }
            if let Some(ref v) = exif.lens_model {
                ui.label("Lens:");
                ui.label(v);
                ui.end_row();
            }
            if let Some(ref v) = exif.capture_date {
                let end = v.len().min(19usize);
                ui.label("Date:");
                ui.label(&v[..end]);
                ui.end_row();
            }
            if let Some(v) = exif.iso {
                ui.label("ISO:");
                ui.label(format!("{}", v));
                ui.end_row();
            }
            if let Some(ref v) = exif.shutter_display {
                ui.label("Shutter:");
                ui.label(format!("{} s", v));
                ui.end_row();
            }
            if let Some(v) = exif.aperture {
                ui.label("Aperture:");
                ui.label(format!("f/{:.1}", v));
                ui.end_row();
            }
            if let Some(v) = exif.focal_length {
                ui.label("Focal length:");
                ui.label(format!("{:.0} mm", v));
                ui.end_row();
            }
            if let Some(v) = exif.focal_length_35mm {
                ui.label("35 mm equiv:");
                ui.label(format!("{:.0} mm", v));
                ui.end_row();
            }
        });
}

// ── Multi-photo batch edit ────────────────────────────────────────────────────

fn multi_photo_ui(ui: &mut egui::Ui, state: &mut AppState, ids: &[PhotoId], count: usize) {
    ui.strong(format!("{} photos selected", count));
    ui.separator();

    ui.label("Apply to all selected:");
    ui.add_space(4.0);

    // Rating
    ui.horizontal(|ui| {
        ui.label("Set rating:");
        for star in 1u8..=5 {
            if ui.button(format!("{}", star)).clicked() {
                apply_batch_rating(state, ids, star);
            }
        }
        if ui.button("Clear").clicked() {
            apply_batch_rating(state, ids, 0);
        }
    });

    // Flag
    ui.horizontal(|ui| {
        ui.label("Flag:");
        if ui.button("Pick").clicked() {
            apply_batch_flag(state, ids, Some("pick"));
        }
        if ui.button("Reject").clicked() {
            apply_batch_flag(state, ids, Some("reject"));
        }
        if ui.button("Clear").clicked() {
            apply_batch_flag(state, ids, None);
        }
    });

    // Keywords (add to all)
    let kw_id = egui::Id::new("batch_kw_input");
    let mut kw_text: String = ui.data(|d| d.get_temp::<String>(kw_id).unwrap_or_default());
    ui.horizontal(|ui| {
        ui.label("Add keyword:");
        let resp = ui.text_edit_singleline(&mut kw_text);
        if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !kw_text.is_empty()
        {
            let kw = kw_text.clone();
            apply_batch_keyword(state, ids, &kw);
            kw_text.clear();
        }
    });
    ui.data_mut(|d| d.insert_temp(kw_id, kw_text));
}

fn apply_batch_rating(state: &mut AppState, ids: &[PhotoId], rating: u8) {
    let Some(lib) = state.library.library.clone() else {
        return;
    };
    for &id in ids {
        let rlab_path_opt = state
            .library
            .results
            .iter()
            .find(|p| p.id == id)
            .map(|p| lib.rlab_path(&p.hash));
        if let Some(rlab_path) = rlab_path_opt
            && let Ok(mut rlab) = rasterlab_core::project::RlabFile::read(&rlab_path)
            && let Some(ref mut lmta) = rlab.lmta
        {
            lmta.rating = rating;
            lib.update_metadata(id, lmta.clone()).ok();
        }
    }
    state.library.refresh();
}

fn apply_batch_flag(state: &mut AppState, ids: &[PhotoId], flag: Option<&str>) {
    let Some(lib) = state.library.library.clone() else {
        return;
    };
    for &id in ids {
        let rlab_path_opt = state
            .library
            .results
            .iter()
            .find(|p| p.id == id)
            .map(|p| lib.rlab_path(&p.hash));
        if let Some(rlab_path) = rlab_path_opt
            && let Ok(mut rlab) = rasterlab_core::project::RlabFile::read(&rlab_path)
            && let Some(ref mut lmta) = rlab.lmta
        {
            lmta.flag = flag.map(|s| s.to_owned());
            lib.update_metadata(id, lmta.clone()).ok();
        }
    }
    state.library.refresh();
}

fn apply_batch_keyword(state: &mut AppState, ids: &[PhotoId], kw: &str) {
    let Some(lib) = state.library.library.clone() else {
        return;
    };
    for &id in ids {
        let rlab_path_opt = state
            .library
            .results
            .iter()
            .find(|p| p.id == id)
            .map(|p| lib.rlab_path(&p.hash));
        if let Some(rlab_path) = rlab_path_opt
            && let Ok(mut rlab) = rasterlab_core::project::RlabFile::read(&rlab_path)
            && let Some(ref mut lmta) = rlab.lmta
            && !lmta.keywords.contains(&kw.to_owned())
        {
            lmta.keywords.push(kw.to_owned());
            lib.update_metadata(id, lmta.clone()).ok();
        }
    }
    state.library.refresh();
}
