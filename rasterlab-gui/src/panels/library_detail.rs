use std::time::Duration;

use egui::ScrollArea;
use rasterlab_library::PhotoId;

use crate::state::AppState;

/// Right-side detail / metadata-edit panel for the library view.
pub fn ui(ui: &mut egui::Ui, state: &mut AppState) {
    let selected: Vec<PhotoId> = state.library.selected.clone();
    let detail_selection = (selected.len() == 1)
        .then(|| {
            state
                .library
                .results
                .iter()
                .find(|photo| photo.id == selected[0])
                .map(|photo| (photo.id, photo.hash.clone()))
        })
        .flatten();
    state.sync_library_detail(
        detail_selection
            .as_ref()
            .map(|(id, hash)| (*id, hash.as_str())),
    );

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
        // Thumbnail preview — scale to fit within a square bound while
        // preserving the texture's own aspect (which reflects rotation/crop
        // ops, whereas photo.width/height are the source dimensions).
        if let Some(tex) = state.library.thumbs.get(&photo.hash) {
            let bound = ui.available_width().min(200.0);
            let tex_size = tex.size_vec2();
            let size = if tex_size.x > 0.0 && tex_size.y > 0.0 {
                let aspect = tex_size.x / tex_size.y;
                if aspect >= 1.0 {
                    egui::vec2(bound, bound / aspect)
                } else {
                    egui::vec2(bound * aspect, bound)
                }
            } else {
                egui::Vec2::splat(bound)
            };
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
        // Protection — toggled directly (not via the LMTA rewrite path) so the
        // on-disk filesystem lock is applied/cleared alongside the flag.
        let mut protected = photo.protected;
        if ui
            .checkbox(&mut protected, "🔒 Protected (cannot be deleted)")
            .changed()
            && let Some(lib) = state.library.library.clone()
        {
            if let Err(e) = lib.set_protected(id, protected) {
                state.library.last_error = Some(format!("Protect failed: {e}"));
            }
            state.library.refresh();
        }

        ui.separator();
        ui.strong("EXIF");

        if let Some(exif) = state
            .library
            .selected_detail_metadata()
            .and_then(|lmta| lmta.exif.as_ref())
        {
            exif_table(ui, exif);
        }

        ui.separator();
        ui.strong("Metadata");

        // Editable fields use an in-memory draft. The network rewrite is
        // coalesced and performed by a background worker.
        if let Some(mut lmta) = state.library.selected_detail_metadata().cloned() {
            let mut changed = false;
            let mut commit_now = false;

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
                        changed = true;
                        commit_now = true;
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
                        changed = true;
                        commit_now = true;
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
                        changed = true;
                        commit_now = true;
                    }
                }
            });

            // Caption
            ui.label("Caption:");
            let mut caption = lmta.caption.clone().unwrap_or_default();
            let caption_response = ui.text_edit_multiline(&mut caption);
            if caption_response.changed() {
                lmta.caption = if caption.is_empty() {
                    None
                } else {
                    Some(caption)
                };
                changed = true;
            }
            commit_now |= caption_response.lost_focus();

            // Keywords
            ui.label("Keywords:");
            let mut kw_edit = lmta.keywords.join(", ");
            let keyword_response = ui.text_edit_singleline(&mut kw_edit);
            if keyword_response.changed() {
                lmta.keywords = kw_edit
                    .split(',')
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty())
                    .collect();
                changed = true;
            }
            commit_now |= keyword_response.lost_focus()
                || (keyword_response.has_focus()
                    && ui.input(|input| input.key_pressed(egui::Key::Enter)));

            if changed {
                state.library.edit_selected_detail_metadata(lmta);
                ui.ctx().request_repaint_after(Duration::from_millis(750));
            }
            if commit_now || state.library.selected_detail_commit_due() {
                state.commit_library_detail_metadata(true);
            }
        }

        ui.separator();
        ui.strong("Collections");
        collections_ui(ui, state, id);

        // Virtual copies — only shown when there is more than one copy so the
        // user can control which one gets exported without opening the editor.
        let copy_state = state.library.selected_detail.as_ref().map(|detail| {
            (
                detail.copy_names.clone(),
                detail.active_copy_index,
                detail.active_copy_saving,
                detail.load_error.clone(),
                detail.loading,
            )
        });
        if let Some((copy_names, active, saving, load_error, loading)) = copy_state {
            if loading {
                ui.spinner();
            } else if let Some(error) = load_error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            if copy_names.len() > 1 {
                ui.separator();
                ui.strong("Virtual Copies");
                ui.label("The active copy is used for export.");
                let mut new_active = active;
                ui.add_enabled_ui(!saving, |ui| {
                    for (idx, name) in copy_names.iter().enumerate() {
                        ui.radio_value(&mut new_active, idx, name);
                    }
                });
                if saving {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Saving active copy…");
                    });
                }
                if new_active != active {
                    state.set_active_copy(&photo.hash, new_active);
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
                if let Some(source_path) = state
                    .library
                    .selected_detail
                    .as_ref()
                    .and_then(|detail| detail.source_path.as_ref())
                {
                    ui.label("Original path:");
                    ui.add(
                        egui::Label::new(egui::RichText::new(source_path).monospace()).truncate(),
                    );
                    ui.end_row();
                }
                ui.label("Library path:");
                ui.add(egui::Label::new(egui::RichText::new(&rel_path).monospace()).truncate());
                ui.end_row();
            });

        ui.separator();

        // Open in editor button
        if ui.button("Open in Editor").clicked() {
            state.commit_library_detail_metadata(true);
            if let Some(lib) = &state.library.library {
                let rlab_path = lib.rlab_path(&photo.hash);
                state.library_context = Some((lib.root().to_path_buf(), photo.hash.clone()));
                state.open_file(rlab_path);
                state.mode = crate::state::AppMode::Editor;
            }
        }
    });
}

// ── Collections ───────────────────────────────────────────────────────────────

/// The collections this photo belongs to.
///
/// Read-only: membership is changed from the grid's right-click menu, which
/// can act on a whole selection rather than just the photo shown here.
fn collections_ui(ui: &mut egui::Ui, state: &AppState, id: PhotoId) {
    let names = state.library.collections_for(id);
    if names.is_empty() {
        ui.weak("Not in any collection")
            .on_hover_text("Right-click the photo in the grid to add it to one");
        return;
    }
    for name in names {
        ui.label(name);
    }
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
            if let Some(ref v) = exif.lens_make {
                ui.label("Lens make:");
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

    // Protection (applies the on-disk lock alongside the flag)
    ui.horizontal(|ui| {
        ui.label("Protection:");
        if ui.button("🔒 Protect").clicked() {
            state.library.set_protected_selected(true);
        }
        if ui.button("Unprotect").clicked() {
            state.library.set_protected_selected(false);
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

fn for_each_lmta(
    state: &mut AppState,
    ids: &[PhotoId],
    mut f: impl FnMut(&mut rasterlab_core::library_meta::LibraryMeta) -> bool,
) {
    let Some(lib) = state.library.library.clone() else {
        return;
    };
    for &id in ids {
        let rlab_path_opt = state
            .library
            .results
            .iter()
            .find(|p| p.id == id)
            .map(|p| lib.photo_rlab_path(&p.hash));
        if let Some(rlab_path) = rlab_path_opt
            && let Ok(mut summary) = rasterlab_core::project::read_library_summary(&rlab_path)
            && let Some(ref mut lmta) = summary.lmta
            && f(lmta)
        {
            lib.update_metadata(id, lmta.clone()).ok();
        }
    }
    state.library.refresh();
}

fn apply_batch_rating(state: &mut AppState, ids: &[PhotoId], rating: u8) {
    for_each_lmta(state, ids, |lmta| {
        lmta.rating = rating;
        true
    });
}

fn apply_batch_flag(state: &mut AppState, ids: &[PhotoId], flag: Option<&str>) {
    for_each_lmta(state, ids, |lmta| {
        lmta.flag = flag.map(|s| s.to_owned());
        true
    });
}

fn apply_batch_keyword(state: &mut AppState, ids: &[PhotoId], kw: &str) {
    for_each_lmta(state, ids, |lmta| {
        if lmta.keywords.contains(&kw.to_owned()) {
            return false;
        }
        lmta.keywords.push(kw.to_owned());
        true
    });
}
