use egui::{Color32, Ui};

use super::shared::header;
use crate::state::AppState;

pub(super) fn ui(ui: &mut Ui, state: &mut AppState, _has_image: bool) {
    // ── Metadata viewer ───────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("metadata");
    let resp = header(state.tools_force_open, "🏷  Metadata")
        .id_salt("metadata")
        .default_open(default_open)
        .show(ui, |ui| match state.image_metadata() {
            None => {
                ui.label(egui::RichText::new("No image loaded.").small().italics());
            }
            Some(meta) => {
                metadata_ui(ui, meta);
            }
        });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("metadata".to_string(), !default_open);
    }
}

fn metadata_ui(ui: &mut egui::Ui, meta: &rasterlab_core::image::ImageMetadata) {
    let row = |ui: &mut egui::Ui, label: &str, value: &str| {
        ui.label(
            egui::RichText::new(label)
                .small()
                .color(Color32::from_gray(160)),
        );
        ui.label(egui::RichText::new(value).small());
        ui.end_row();
    };

    // ── File ──────────────────────────────────────────────────────────────
    if let Some(ref path) = meta.original_path {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        egui::Grid::new("meta_file")
            .num_columns(2)
            .spacing([8.0, 2.0])
            .show(ui, |ui| {
                row(ui, "File", &name);
            });
        ui.add_space(2.0);
    }

    // ── Camera ────────────────────────────────────────────────────────────
    let has_camera = meta.camera_make.is_some()
        || meta.camera_model.is_some()
        || meta.lens_make.is_some()
        || meta.lens_model.is_some()
        || meta.date_time.is_some();

    if has_camera {
        ui.label(egui::RichText::new("Camera").small().strong());
        egui::Grid::new("meta_camera")
            .num_columns(2)
            .spacing([8.0, 2.0])
            .show(ui, |ui| {
                if let (Some(make), Some(model)) = (&meta.camera_make, &meta.camera_model) {
                    row(ui, "Camera", &format!("{make} {model}"));
                } else if let Some(model) = &meta.camera_model {
                    row(ui, "Camera", model);
                }
                if let (Some(make), Some(model)) = (&meta.lens_make, &meta.lens_model) {
                    row(ui, "Lens", &format!("{make} {model}"));
                } else if let Some(lens) = &meta.lens_model {
                    row(ui, "Lens", lens);
                }
                if let Some(dt) = &meta.date_time {
                    row(ui, "Date", dt);
                }
            });
        ui.add_space(2.0);
    }

    // ── Exposure ──────────────────────────────────────────────────────────
    let has_exposure = meta.iso.is_some()
        || meta.shutter_speed.is_some()
        || meta.aperture.is_some()
        || meta.focal_length.is_some()
        || meta.subject_distance.is_some();

    if has_exposure {
        ui.label(egui::RichText::new("Exposure").small().strong());
        egui::Grid::new("meta_exposure")
            .num_columns(2)
            .spacing([8.0, 2.0])
            .show(ui, |ui| {
                if let Some(iso) = meta.iso {
                    row(ui, "ISO", &iso.to_string());
                }
                if let Some(ref ss) = meta.shutter_speed {
                    row(ui, "Shutter", ss);
                }
                if let Some(f) = meta.aperture {
                    row(ui, "Aperture", &format!("f/{:.1}", f));
                }
                if let Some(fl) = meta.focal_length {
                    let s = if let Some(fl35) = meta.focal_length_35mm {
                        format!("{:.0} mm  ({} mm equiv.)", fl, fl35)
                    } else {
                        format!("{:.0} mm", fl)
                    };
                    row(ui, "Focal length", &s);
                }
                if let Some(ev) = meta.exposure_bias
                    && ev.abs() > 0.01
                {
                    row(ui, "Exp. bias", &format!("{:+.2} EV", ev));
                }
                if let Some(distance) = meta.subject_distance {
                    row(ui, "Subject distance", &format!("{distance:.2} m"));
                }
                if let Some(ref prog) = meta.exposure_program {
                    row(ui, "Program", prog);
                }
                if let Some(ref mode) = meta.metering_mode {
                    row(ui, "Metering", mode);
                }
                if let Some(ref flash) = meta.flash {
                    row(ui, "Flash", flash);
                }
            });
        ui.add_space(2.0);
    }

    // ── GPS ───────────────────────────────────────────────────────────────
    if meta.gps_lat.is_some() || meta.gps_lon.is_some() {
        ui.label(egui::RichText::new("Location").small().strong());
        egui::Grid::new("meta_gps")
            .num_columns(2)
            .spacing([8.0, 2.0])
            .show(ui, |ui| {
                if let (Some(lat), Some(lon)) = (meta.gps_lat, meta.gps_lon) {
                    let lat_s = if lat >= 0.0 {
                        format!("{:.5}° N", lat)
                    } else {
                        format!("{:.5}° S", -lat)
                    };
                    let lon_s = if lon >= 0.0 {
                        format!("{:.5}° E", lon)
                    } else {
                        format!("{:.5}° W", -lon)
                    };
                    row(ui, "Lat", &lat_s);
                    row(ui, "Lon", &lon_s);
                }
                if let Some(alt) = meta.gps_alt {
                    row(ui, "Altitude", &format!("{:.0} m", alt));
                }
            });
        ui.add_space(2.0);
    }

    if !has_camera && !has_exposure && meta.gps_lat.is_none() && meta.original_path.is_none() {
        ui.label(
            egui::RichText::new("No EXIF metadata found.")
                .small()
                .italics(),
        );
    }
}
