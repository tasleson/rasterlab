//! Tools panel — inputs for adding operations to the pipeline.

use egui::{Color32, ComboBox, CornerRadius, DragValue, Pos2, Rect, Stroke, Ui, Vec2};
use rasterlab_core::ops::{CurvesOp, NrMethod};

use crate::state::AppState;

const BW_MODES: &[&str] = &[
    "Luminance (BT.709)",
    "Average",
    "Perceptual (BT.601)",
    "Channel Mixer",
];

/// Film-grain presets: (label, strength, size).
/// Inspired by popular 35 mm film stocks.
const GRAIN_PRESETS: &[(&str, f32, f32)] = &[
    ("T-Max 100", 0.03, 1.0),   // finest grain, technical pan
    ("Gold 200", 0.05, 1.3),    // Kodak Gold — fine, warm
    ("Portra 400", 0.06, 1.5),  // Kodak Portra — portrait favourite
    ("Pro 400H", 0.07, 1.2),    // Fuji Pro 400H — very fine for 400
    ("HP5 400", 0.09, 1.6),     // Ilford HP5 — classic reportage
    ("Tri-X 400", 0.10, 1.8),   // Kodak Tri-X — definitive B&W stock
    ("Superia 400", 0.08, 1.5), // Fuji Superia — consumer colour
    ("Portra 800", 0.12, 2.0),  // Kodak Portra 800 — low-light portrait
    ("Neopan 1600", 0.18, 2.5), // Fuji Neopan 1600 — gritty documentary
    ("T-Max 3200", 0.25, 3.0),  // Kodak T-Max P3200 — extreme push
    ("Heavy Push", 0.35, 3.5),  // fictional heavy-push look
];

/// Named channel-mixer presets: (label, R, G, B).
/// Weights need not sum to 1 — clamped to [0,255] per-pixel in the op.
const BW_PRESETS: &[(&str, f32, f32, f32)] = &[
    ("Neutral", 0.2126, 0.7152, 0.0722),     // BT.709
    ("Dramatic Contrast", 0.60, 0.40, 0.00), // red/yellow boost, dark skies
    ("Red Filter", 1.00, 0.00, 0.00),        // mimics red lens filter
    ("Green Filter", 0.00, 1.00, 0.00),      // maximum fine detail
    ("Blue Filter", 0.00, 0.00, 1.00),       // hazy, atmospheric
    ("Soften / Skin", 0.25, 0.55, 0.20),     // flatters skin tones
    ("Urban / Cool", 0.00, 0.30, 0.70),      // gritty blue-channel look
    ("High Key", 0.40, 0.50, 0.30),          // weights > sum→lifted midtones
    ("Low Key", 0.10, 0.20, 0.05),           // weights < sum→crushed midtones
    ("Infrared", 0.90, 0.10, -0.10),         // blown-out foliage simulation
];

pub fn ui(ui: &mut Ui, state: &mut AppState) {
    ui.heading("Tools");
    ui.separator();

    let has_image = state.pipeline().is_some();

    // ── Auto Enhance ──────────────────────────────────────────────────────
    let btn = egui::Button::new("✨  Auto Enhance").min_size(Vec2::new(ui.available_width(), 0.0));
    if ui.add_enabled(has_image, btn).clicked() {
        state.push_auto_enhance();
    }

    ui.separator();

    // ── Looks ─────────────────────────────────────────────────────────────
    egui::CollapsingHeader::new("🎞  Looks")
        .id_salt("looks")
        .default_open(false)
        .show(ui, |ui| {
            let btn =
                egui::Button::new("Classic B&W").min_size(Vec2::new(ui.available_width(), 0.0));
            if ui.add_enabled(has_image, btn).clicked() {
                state.push_classic_bw();
            }
        });

    ui.add_space(4.0);
    ui.separator();
    ui.add_space(4.0);

    // ── Black & White ─────────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("bw");
    let resp = egui::CollapsingHeader::new("◑  Black & White")
        .id_salt("bw")
        .default_open(default_open)
        .show(ui, |ui| {
            let old_idx = state.tools.bw_mode_idx;
            let combo_resp = ComboBox::from_label("Mode")
                .selected_text(BW_MODES[state.tools.bw_mode_idx])
                .show_ui(ui, |ui| {
                    for (i, &label) in BW_MODES.iter().enumerate() {
                        ui.selectable_value(&mut state.tools.bw_mode_idx, i, label);
                    }
                });
            if (combo_resp.response.changed() || state.tools.bw_mode_idx != old_idx) && has_image {
                state.update_bw_preview();
            }

            // Channel mixer sliders — only shown when that mode is selected.
            if state.tools.bw_mode_idx == 3 {
                let mut changed = false;

                // Preset buttons — clicking one loads the weights and previews.
                ui.label("Presets:");
                ui.horizontal_wrapped(|ui| {
                    for &(label, r, g, b) in BW_PRESETS {
                        if ui.small_button(label).clicked() && has_image {
                            state.tools.bw_mixer_r = r;
                            state.tools.bw_mixer_g = g;
                            state.tools.bw_mixer_b = b;
                            state.update_bw_preview();
                        }
                    }
                });
                ui.add_space(2.0);

                egui::Grid::new("bw_mixer_grid")
                    .num_columns(2)
                    .spacing([8.0, 4.0])
                    .show(ui, |ui| {
                        ui.label("R");
                        changed |= ui
                            .add(
                                DragValue::new(&mut state.tools.bw_mixer_r)
                                    .speed(0.01)
                                    .range(-2.0..=2.0),
                            )
                            .changed();
                        ui.end_row();
                        ui.label("G");
                        changed |= ui
                            .add(
                                DragValue::new(&mut state.tools.bw_mixer_g)
                                    .speed(0.01)
                                    .range(-2.0..=2.0),
                            )
                            .changed();
                        ui.end_row();
                        ui.label("B");
                        changed |= ui
                            .add(
                                DragValue::new(&mut state.tools.bw_mixer_b)
                                    .speed(0.01)
                                    .range(-2.0..=2.0),
                            )
                            .changed();
                        ui.end_row();
                    });
                if changed && has_image {
                    state.update_bw_preview();
                }
            }

            ui.horizontal(|ui| {
                if ui
                    .add_enabled(has_image, egui::Button::new("Apply B&W"))
                    .clicked()
                {
                    state.push_bw();
                }
                if state.tools.bw_preview_active
                    && ui
                        .add_enabled(has_image, egui::Button::new("Cancel"))
                        .clicked()
                {
                    state.cancel_bw_preview();
                }
            });
        });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("bw".to_string(), !default_open);
    }

    ui.separator();

    // ── Blur ──────────────────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("blur");
    let resp = egui::CollapsingHeader::new("≋  Blur")
        .id_salt("blur")
        .default_open(default_open)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("Radius (σ):");
                ui.add(
                    DragValue::new(&mut state.tools.blur_radius)
                        .speed(0.1)
                        .range(0.1..=100.0_f32)
                        .suffix(" px"),
                );
            });
            if ui
                .add_enabled(has_image, egui::Button::new("Apply Blur"))
                .clicked()
            {
                state.push_blur();
            }
        });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("blur".to_string(), !default_open);
    }

    ui.separator();

    // ── Brightness / Contrast ─────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("brightness_contrast");
    let resp = egui::CollapsingHeader::new("☀  Brightness / Contrast")
        .id_salt("brightness_contrast")
        .default_open(default_open)
        .show(ui, |ui| {
            let mut changed = false;
            egui::Grid::new("bc_grid")
                .num_columns(2)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    ui.label("Brightness");
                    changed |= ui
                        .add(
                            egui::Slider::new(&mut state.tools.bc_brightness, -1.0..=1.0)
                                .step_by(0.01),
                        )
                        .changed();
                    ui.end_row();
                    ui.label("Contrast");
                    changed |= ui
                        .add(
                            egui::Slider::new(&mut state.tools.bc_contrast, -1.0..=1.0)
                                .step_by(0.01),
                        )
                        .changed();
                    ui.end_row();
                });
            if changed && has_image {
                state.update_bc_preview();
            }
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(has_image, egui::Button::new("Apply"))
                    .clicked()
                {
                    state.push_bc();
                }
                if state.tools.bc_preview_active
                    && ui
                        .add_enabled(has_image, egui::Button::new("Cancel"))
                        .clicked()
                {
                    state.cancel_bc_preview();
                }
                if ui.button("Reset").clicked() {
                    state.reset_bc();
                }
            });
        });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("brightness_contrast".to_string(), !default_open);
    }

    ui.separator();

    // ── Clarity / Texture ─────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("clarity_texture");
    let resp = egui::CollapsingHeader::new("◈  Clarity / Texture")
        .id_salt("clarity_texture")
        .default_open(default_open)
        .show(ui, |ui| {
            let c_changed = ui
                .add(
                    egui::Slider::new(&mut state.tools.clarity, -1.0..=1.0)
                        .step_by(0.01)
                        .text("Clarity"),
                )
                .changed();
            let t_changed = ui
                .add(
                    egui::Slider::new(&mut state.tools.texture, -1.0..=1.0)
                        .step_by(0.01)
                        .text("Texture"),
                )
                .changed();
            if (c_changed || t_changed) && has_image {
                state.update_clarity_texture_preview();
            }
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(has_image, egui::Button::new("Apply"))
                    .clicked()
                {
                    state.push_clarity_texture();
                }
                if state.tools.clarity_preview_active
                    && ui
                        .add_enabled(has_image, egui::Button::new("Cancel"))
                        .clicked()
                {
                    state.cancel_clarity_texture_preview();
                }
            });
        });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("clarity_texture".to_string(), !default_open);
    }

    ui.separator();

    // ── Color Balance ─────────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("color_balance");
    let resp = egui::CollapsingHeader::new("⚖  Color Balance")
        .id_salt("color_balance")
        .default_open(default_open)
        .show(ui, |ui| {
            let mut changed = false;
            let zone_labels = ["Shadows", "Midtones", "Highlights"];
            {
                ui.label("Cyan ↔ Red");
                egui::Grid::new("cb_cr_grid")
                    .num_columns(2)
                    .spacing([8.0, 2.0])
                    .show(ui, |ui| {
                        for (i, zone) in zone_labels.iter().enumerate() {
                            ui.label(*zone);
                            changed |= ui
                                .add(
                                    egui::Slider::new(&mut state.tools.cb_cyan_red[i], -1.0..=1.0)
                                        .step_by(0.01),
                                )
                                .changed();
                            ui.end_row();
                        }
                    });
                ui.add_space(4.0);
                ui.label("Magenta ↔ Green");
                egui::Grid::new("cb_mg_grid")
                    .num_columns(2)
                    .spacing([8.0, 2.0])
                    .show(ui, |ui| {
                        for (i, zone) in zone_labels.iter().enumerate() {
                            ui.label(*zone);
                            changed |= ui
                                .add(
                                    egui::Slider::new(
                                        &mut state.tools.cb_magenta_green[i],
                                        -1.0..=1.0,
                                    )
                                    .step_by(0.01),
                                )
                                .changed();
                            ui.end_row();
                        }
                    });
                ui.add_space(4.0);
                ui.label("Yellow ↔ Blue");
                egui::Grid::new("cb_yb_grid")
                    .num_columns(2)
                    .spacing([8.0, 2.0])
                    .show(ui, |ui| {
                        for (i, zone) in zone_labels.iter().enumerate() {
                            ui.label(*zone);
                            changed |= ui
                                .add(
                                    egui::Slider::new(
                                        &mut state.tools.cb_yellow_blue[i],
                                        -1.0..=1.0,
                                    )
                                    .step_by(0.01),
                                )
                                .changed();
                            ui.end_row();
                        }
                    });
                ui.add_space(4.0);
            }
            if changed && has_image {
                state.update_cb_preview();
            }
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(has_image, egui::Button::new("Apply"))
                    .clicked()
                {
                    state.push_cb();
                }
                if state.tools.cb_preview_active
                    && ui
                        .add_enabled(has_image, egui::Button::new("Cancel"))
                        .clicked()
                {
                    state.cancel_cb_preview();
                }
                if ui.button("Reset").clicked() {
                    state.reset_cb();
                }
            });
        });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("color_balance".to_string(), !default_open);
    }

    ui.separator();

    // ── Color Space Conversion ────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("color_space");
    let resp = egui::CollapsingHeader::new("⬛  Color Space")
        .id_salt("color_space")
        .default_open(default_open)
        .show(ui, |ui| {
            use rasterlab_core::ops::ColorSpaceConversion;
            egui::ComboBox::from_id_salt("color_space_combo")
                .selected_text(match state.tools.color_space_conversion {
                    ColorSpaceConversion::SrgbToDisplayP3 => "sRGB → Display P3",
                    ColorSpaceConversion::DisplayP3ToSrgb => "Display P3 → sRGB",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut state.tools.color_space_conversion,
                        ColorSpaceConversion::SrgbToDisplayP3,
                        "sRGB → Display P3",
                    );
                    ui.selectable_value(
                        &mut state.tools.color_space_conversion,
                        ColorSpaceConversion::DisplayP3ToSrgb,
                        "Display P3 → sRGB",
                    );
                });
            if ui
                .add_enabled(has_image, egui::Button::new("Apply Conversion"))
                .clicked()
            {
                state.push_color_space();
            }
        });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("color_space".to_string(), !default_open);
    }

    ui.separator();

    // ── Crop ─────────────────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("crop");
    let resp = egui::CollapsingHeader::new("✂  Crop")
        .id_salt("crop")
        .default_open(default_open)
        .show(ui, |ui| {
            egui::Grid::new("crop_grid")
                .num_columns(2)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    ui.label("Aspect:");
                    const ASPECT_LABELS: &[&str] =
                        &["Free", "3:2", "4:3", "1:1", "16:9", "9:16", "Custom"];
                    egui::ComboBox::from_id_salt("crop_aspect")
                        .selected_text(ASPECT_LABELS[state.tools.crop_aspect_idx])
                        .show_ui(ui, |ui| {
                            for (i, &label) in ASPECT_LABELS.iter().enumerate() {
                                ui.selectable_value(&mut state.tools.crop_aspect_idx, i, label);
                            }
                        });
                    ui.end_row();

                    if state.tools.crop_aspect_idx == 6 {
                        ui.label("Ratio W:H");
                        ui.add(
                            DragValue::new(&mut state.tools.crop_custom_ratio)
                                .speed(0.01)
                                .range(0.1..=20.0_f32),
                        );
                        ui.end_row();
                    }

                    ui.label("X");
                    ui.add(DragValue::new(&mut state.tools.crop_x).speed(1));
                    ui.end_row();
                    ui.label("Y");
                    ui.add(DragValue::new(&mut state.tools.crop_y).speed(1));
                    ui.end_row();
                    ui.label("W");
                    ui.add(
                        DragValue::new(&mut state.tools.crop_w)
                            .speed(1)
                            .range(1..=u32::MAX),
                    );
                    ui.end_row();
                    ui.label("H");
                    ui.add(
                        DragValue::new(&mut state.tools.crop_h)
                            .speed(1)
                            .range(1..=u32::MAX),
                    );
                    ui.end_row();
                });
            if ui
                .add_enabled(has_image, egui::Button::new("Apply Crop"))
                .clicked()
            {
                state.push_crop();
            }
        });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("crop".to_string(), !default_open);
    }

    ui.separator();

    // ── Curves ────────────────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("curves");
    let resp = egui::CollapsingHeader::new("〜  Curves")
        .id_salt("curves")
        .default_open(default_open)
        .show(ui, |ui| {
            curves_ui(ui, state);
        });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("curves".to_string(), !default_open);
    }

    ui.separator();

    // ── Denoise ───────────────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("denoise");
    let resp = egui::CollapsingHeader::new("◌  Denoise")
        .id_salt("denoise")
        .default_open(default_open)
        .show(ui, |ui| {
            egui::Grid::new("denoise_grid")
                .num_columns(2)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    ui.label("Strength:");
                    ui.add(
                        DragValue::new(&mut state.tools.denoise_strength)
                            .speed(0.01)
                            .range(0.01..=1.0_f32),
                    );
                    ui.end_row();
                    ui.label("Radius:");
                    ui.add(
                        DragValue::new(&mut state.tools.denoise_radius)
                            .speed(1)
                            .range(1..=10_u32)
                            .suffix(" px"),
                    );
                    ui.end_row();
                });
            if ui
                .add_enabled(has_image, egui::Button::new("Apply Denoise"))
                .clicked()
            {
                state.push_denoise();
            }
        });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("denoise".to_string(), !default_open);
    }

    ui.separator();

    // ── Export settings ──────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("export_settings");
    let resp = egui::CollapsingHeader::new("⚙  Export Settings")
        .id_salt("export_settings")
        .default_open(default_open)
        .show(ui, |ui| {
            egui::Grid::new("export_grid")
                .num_columns(2)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    ui.label("JPEG quality:");
                    ui.add(
                        DragValue::new(&mut state.tools.encode_opts.jpeg_quality).range(1..=100u8),
                    );
                    ui.end_row();
                    ui.label("PNG compression:");
                    ui.add(
                        DragValue::new(&mut state.tools.encode_opts.png_compression).range(0..=9u8),
                    );
                    ui.end_row();
                });

            ui.separator();
            ui.label("Resize on export:");
            ui.checkbox(&mut state.tools.export_resize_enabled, "Enable");
            if state.tools.export_resize_enabled {
                egui::Grid::new("export_resize_grid")
                    .num_columns(2)
                    .spacing([8.0, 4.0])
                    .show(ui, |ui| {
                        ui.label("Width:");
                        ui.add(
                            DragValue::new(&mut state.tools.export_resize_w)
                                .range(1..=65535u32)
                                .suffix(" px"),
                        );
                        ui.end_row();
                        ui.label("Height:");
                        ui.add(
                            DragValue::new(&mut state.tools.export_resize_h)
                                .range(1..=65535u32)
                                .suffix(" px"),
                        );
                        ui.end_row();
                        ui.label("Mode:");
                        egui::ComboBox::from_id_salt("export_resize_mode")
                            .selected_text(format!("{:?}", state.tools.export_resize_mode))
                            .show_ui(ui, |ui| {
                                use rasterlab_core::ops::ResampleMode;
                                ui.selectable_value(
                                    &mut state.tools.export_resize_mode,
                                    ResampleMode::NearestNeighbour,
                                    "Nearest",
                                );
                                ui.selectable_value(
                                    &mut state.tools.export_resize_mode,
                                    ResampleMode::Bilinear,
                                    "Bilinear",
                                );
                                ui.selectable_value(
                                    &mut state.tools.export_resize_mode,
                                    ResampleMode::Bicubic,
                                    "Bicubic",
                                );
                            });
                        ui.end_row();
                    });
            }
        });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("export_settings".to_string(), !default_open);
    }
    ui.separator();

    // ── Faux HDR ──────────────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("faux_hdr");
    let resp = egui::CollapsingHeader::new("◈  Faux HDR")
        .id_salt("faux_hdr")
        .default_open(default_open)
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new("Exposure fusion from ±1 stop virtual brackets")
                    .small()
                    .color(Color32::from_gray(140)),
            );
            ui.add_space(2.0);
            let changed = ui
                .add(
                    egui::Slider::new(&mut state.tools.hdr_strength, 0.0..=1.0)
                        .text("Strength")
                        .step_by(0.01),
                )
                .changed();
            if changed && has_image {
                state.update_hdr_preview();
            }
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(has_image, egui::Button::new("Apply"))
                    .clicked()
                {
                    state.push_hdr();
                }
                if state.tools.hdr_preview_active
                    && ui
                        .add_enabled(has_image, egui::Button::new("Cancel"))
                        .clicked()
                {
                    state.cancel_hdr_preview();
                }
                if ui.button("Reset").clicked() {
                    state.reset_hdr();
                }
            });
        });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("faux_hdr".to_string(), !default_open);
    }

    ui.separator();

    // ── Grain ─────────────────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("grain");
    let resp = egui::CollapsingHeader::new("⣿  Grain")
        .id_salt("grain")
        .default_open(default_open)
        .show(ui, |ui| {
            grain_ui(ui, state);
        });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("grain".to_string(), !default_open);
    }

    ui.separator();

    // ── Spot Heal ─────────────────────────────────────────────────────────
    {
        let default_open = state.prefs.is_tool_open("heal");
        let heal_label = if state.tools.heal_active {
            format!(
                "✦  Spot Heal  [ACTIVE \u{2014} {} spot{}]",
                state.tools.heal_spots.len(),
                if state.tools.heal_spots.len() == 1 {
                    ""
                } else {
                    "s"
                }
            )
        } else {
            "✦  Spot Heal".to_string()
        };
        let resp = egui::CollapsingHeader::new(heal_label)
            .id_salt("heal")
            .default_open(default_open)
            .show(ui, |ui| {
                egui::Grid::new("heal_grid")
                    .num_columns(2)
                    .spacing([8.0, 4.0])
                    .show(ui, |ui| {
                        ui.label("Radius:");
                        ui.add(
                            DragValue::new(&mut state.tools.heal_radius)
                                .speed(1)
                                .range(5_u32..=300_u32),
                        );
                        ui.end_row();
                    });

                let mode_btn_text = if state.tools.heal_active {
                    "Stop Painting"
                } else {
                    "Start Painting"
                };
                if ui
                    .add_enabled(
                        has_image,
                        egui::Button::new(mode_btn_text)
                            .min_size(Vec2::new(ui.available_width(), 0.0)),
                    )
                    .clicked()
                {
                    state.tools.heal_active = !state.tools.heal_active;
                }

                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            has_image && !state.tools.heal_spots.is_empty(),
                            egui::Button::new("Apply Heal"),
                        )
                        .clicked()
                    {
                        state.push_heal();
                    }
                    if ui
                        .add_enabled(
                            !state.tools.heal_spots.is_empty(),
                            egui::Button::new("Clear"),
                        )
                        .clicked()
                    {
                        state.tools.heal_spots.clear();
                    }
                });

                if state.tools.heal_active {
                    ui.label(
                        egui::RichText::new(
                            "Click on blemishes to heal them.\nRight-click a spot to remove it.",
                        )
                        .small()
                        .color(egui::Color32::from_gray(140)),
                    );
                }
            });
        if resp.header_response.clicked() {
            state
                .prefs
                .tools_open
                .insert("heal".to_string(), !default_open);
        }
    }

    ui.separator();

    // ── Highlights & Shadows ──────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("highlights_shadows");
    let resp = egui::CollapsingHeader::new("◑  Highlights / Shadows")
        .id_salt("highlights_shadows")
        .default_open(default_open)
        .show(ui, |ui| {
            let mut changed = false;
            egui::Grid::new("hl_grid")
                .num_columns(2)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    ui.label("Highlights");
                    changed |= ui
                        .add(
                            egui::Slider::new(&mut state.tools.hl_highlights, -1.0..=1.0)
                                .step_by(0.01),
                        )
                        .changed();
                    ui.end_row();
                    ui.label("Shadows");
                    changed |= ui
                        .add(
                            egui::Slider::new(&mut state.tools.hl_shadows, -1.0..=1.0)
                                .step_by(0.01),
                        )
                        .changed();
                    ui.end_row();
                });
            if changed && has_image {
                state.update_hl_preview();
            }
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(has_image, egui::Button::new("Apply"))
                    .clicked()
                {
                    state.push_hl();
                }
                if state.tools.hl_preview_active
                    && ui
                        .add_enabled(has_image, egui::Button::new("Cancel"))
                        .clicked()
                {
                    state.cancel_hl_preview();
                }
                if ui.button("Reset").clicked() {
                    state.reset_hl();
                }
            });
        });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("highlights_shadows".to_string(), !default_open);
    }

    ui.separator();

    // ── HSL Panel ─────────────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("hsl_panel");
    let resp = egui::CollapsingHeader::new("🌈  HSL Panel")
        .id_salt("hsl_panel")
        .default_open(default_open)
        .show(ui, |ui| {
            hsl_panel_ui(ui, state, has_image);
        });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("hsl_panel".to_string(), !default_open);
    }

    ui.separator();

    // ── Hue Shift ─────────────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("hue_shift");
    let resp = egui::CollapsingHeader::new("🎡  Hue Shift")
        .id_salt("hue_shift")
        .default_open(default_open)
        .show(ui, |ui| {
            let changed = ui
                .add(
                    egui::Slider::new(&mut state.tools.hue_degrees, -180.0..=180.0)
                        .text("Degrees")
                        .step_by(1.0),
                )
                .changed();
            if changed && has_image {
                state.update_hue_preview();
            }
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(has_image, egui::Button::new("Apply"))
                    .clicked()
                {
                    state.push_hue();
                }
                if state.tools.hue_preview_active
                    && ui
                        .add_enabled(has_image, egui::Button::new("Cancel"))
                        .clicked()
                {
                    state.cancel_hue_preview();
                }
                if ui.button("Reset").clicked() {
                    state.reset_hue();
                }
            });
        });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("hue_shift".to_string(), !default_open);
    }

    ui.separator();

    // ── Levels ────────────────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("levels");
    let resp = egui::CollapsingHeader::new("▨  Levels")
        .id_salt("levels")
        .default_open(default_open)
        .show(ui, |ui| {
            levels_ui(ui, state);
        });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("levels".to_string(), !default_open);
    }

    ui.separator();

    // ── Masking ───────────────────────────────────────────────────────────
    // Global modifier: when active, the next "Apply" button wraps its
    // operation in a MaskedOp that restricts the effect to the masked region.
    const MASK_LABELS: &[&str] = &["None", "Linear Gradient", "Radial Gradient"];
    let mask_default_open = state.prefs.is_tool_open("masking");
    let mask_active = state.tools.mask_sel > 0;
    let mask_header = if mask_active {
        format!("◈  Masking  [{}]", MASK_LABELS[state.tools.mask_sel])
    } else {
        "◈  Masking".to_string()
    };
    let mask_resp = egui::CollapsingHeader::new(mask_header)
        .id_salt("masking")
        .default_open(mask_default_open)
        .show(ui, |ui| {
            egui::Grid::new("mask_type_grid")
                .num_columns(2)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    ui.label("Type:");
                    egui::ComboBox::from_id_salt("mask_type_combo")
                        .selected_text(MASK_LABELS[state.tools.mask_sel])
                        .show_ui(ui, |ui| {
                            for (i, &label) in MASK_LABELS.iter().enumerate() {
                                ui.selectable_value(&mut state.tools.mask_sel, i, label);
                            }
                        });
                    ui.end_row();
                });

            if state.tools.mask_sel == 1 {
                // Linear gradient controls
                ui.separator();
                egui::Grid::new("mask_lin_grid")
                    .num_columns(2)
                    .spacing([8.0, 4.0])
                    .show(ui, |ui| {
                        ui.label("Angle:");
                        ui.add(
                            egui::Slider::new(&mut state.tools.mask_lin_angle, 0.0..=360.0)
                                .suffix("°")
                                .step_by(1.0),
                        );
                        ui.end_row();
                        ui.label("Center X:");
                        ui.add(
                            egui::Slider::new(&mut state.tools.mask_lin_cx, 0.0..=1.0)
                                .step_by(0.01),
                        );
                        ui.end_row();
                        ui.label("Center Y:");
                        ui.add(
                            egui::Slider::new(&mut state.tools.mask_lin_cy, 0.0..=1.0)
                                .step_by(0.01),
                        );
                        ui.end_row();
                        ui.label("Feather:");
                        ui.add(
                            egui::Slider::new(&mut state.tools.mask_lin_feather, 0.01..=1.0)
                                .step_by(0.01),
                        );
                        ui.end_row();
                        ui.label("Invert:");
                        ui.checkbox(&mut state.tools.mask_lin_invert, "");
                        ui.end_row();
                    });
            }

            if state.tools.mask_sel == 2 {
                // Radial gradient controls
                ui.separator();
                egui::Grid::new("mask_rad_grid")
                    .num_columns(2)
                    .spacing([8.0, 4.0])
                    .show(ui, |ui| {
                        ui.label("Center X:");
                        ui.add(
                            egui::Slider::new(&mut state.tools.mask_rad_cx, 0.0..=1.0)
                                .step_by(0.01),
                        );
                        ui.end_row();
                        ui.label("Center Y:");
                        ui.add(
                            egui::Slider::new(&mut state.tools.mask_rad_cy, 0.0..=1.0)
                                .step_by(0.01),
                        );
                        ui.end_row();
                        ui.label("Radius:");
                        ui.add(
                            egui::Slider::new(&mut state.tools.mask_rad_radius, 0.01..=1.5)
                                .step_by(0.01),
                        );
                        ui.end_row();
                        ui.label("Feather:");
                        ui.add(
                            egui::Slider::new(&mut state.tools.mask_rad_feather, 0.01..=2.0)
                                .step_by(0.01),
                        );
                        ui.end_row();
                        ui.label("Invert:");
                        ui.checkbox(&mut state.tools.mask_rad_invert, "");
                        ui.end_row();
                    });
            }

            if mask_active {
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new("⚠ Next Apply will be masked")
                        .small()
                        .color(egui::Color32::from_rgb(220, 160, 40)),
                );
                if ui.small_button("Clear mask").clicked() {
                    state.tools.mask_sel = 0;
                }
            }
        });
    if mask_resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("masking".to_string(), !mask_default_open);
    }

    ui.separator();

    // ── LUT (Color Grading) ───────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("lut");
    let resp = egui::CollapsingHeader::new("🎞  LUT / Color Grading")
        .id_salt("lut")
        .default_open(default_open)
        .show(ui, |ui| {
            if ui.button("Load .cube LUT…").clicked() {
                state.tools.lut_dialog_requested = true;
            }
            if !state.tools.lut_name.is_empty() {
                ui.label(format!("Loaded: {}", state.tools.lut_name));
                let changed = ui
                    .add(
                        egui::Slider::new(&mut state.tools.lut_strength, 0.0..=1.0)
                            .step_by(0.01)
                            .text("Strength"),
                    )
                    .changed();
                if changed && has_image {
                    state.update_lut_preview();
                }
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(has_image, egui::Button::new("Apply LUT"))
                        .clicked()
                    {
                        state.push_lut();
                    }
                    if state.tools.lut_preview_active
                        && ui
                            .add_enabled(has_image, egui::Button::new("Cancel"))
                            .clicked()
                    {
                        state.cancel_lut_preview();
                    }
                });
            } else {
                ui.label("No LUT loaded.");
            }
        });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("lut".to_string(), !default_open);
    }

    ui.separator();

    // ── Noise Reduction (Advanced) ───────────────────────────────────────
    let default_open = state.prefs.is_tool_open("noise_reduction");
    let resp = egui::CollapsingHeader::new("◉  Noise Reduction")
        .id_salt("noise_reduction")
        .default_open(default_open)
        .show(ui, |ui| {
            egui::Grid::new("nr_grid")
                .num_columns(2)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    ui.label("Method:");
                    egui::ComboBox::from_id_salt("nr_method")
                        .selected_text(match state.tools.nr_method {
                            NrMethod::Wavelet => "Wavelet (fast)",
                            NrMethod::NonLocalMeans => "Non-Local Means",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut state.tools.nr_method,
                                NrMethod::Wavelet,
                                "Wavelet (fast)",
                            );
                            ui.selectable_value(
                                &mut state.tools.nr_method,
                                NrMethod::NonLocalMeans,
                                "Non-Local Means",
                            );
                        });
                    ui.end_row();

                    ui.label("Luminance:");
                    ui.add(
                        egui::Slider::new(&mut state.tools.nr_luma, 0.0..=1.0_f32).show_value(true),
                    );
                    ui.end_row();

                    ui.label("Color:");
                    ui.add(
                        egui::Slider::new(&mut state.tools.nr_color, 0.0..=1.0_f32)
                            .show_value(true),
                    );
                    ui.end_row();

                    ui.label("Detail:");
                    ui.add(
                        egui::Slider::new(&mut state.tools.nr_detail, 0.0..=1.0_f32)
                            .show_value(true),
                    );
                    ui.end_row();
                });

            if state.tools.nr_method == NrMethod::NonLocalMeans {
                ui.label(
                    egui::RichText::new("⚠ NLM is slow on large images (30s+)")
                        .small()
                        .color(egui::Color32::from_rgb(200, 150, 50)),
                );
            }

            if ui
                .add_enabled(has_image, egui::Button::new("Apply Noise Reduction"))
                .clicked()
            {
                state.push_noise_reduction();
            }
        });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("noise_reduction".to_string(), !default_open);
    }

    ui.separator();

    // ── Perspective ───────────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("perspective");
    let resp = egui::CollapsingHeader::new("⬡  Perspective")
        .id_salt("perspective")
        .default_open(default_open)
        .show(ui, |ui| {
            let corner_labels = [
                ("Top-left", 0usize),
                ("Top-right", 1),
                ("Bottom-right", 2),
                ("Bottom-left", 3),
            ];
            egui::Grid::new("perspective_grid")
                .num_columns(3)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    ui.label("");
                    ui.label("X");
                    ui.label("Y");
                    ui.end_row();
                    for (label, i) in corner_labels {
                        ui.label(label);
                        ui.add(
                            DragValue::new(&mut state.tools.perspective_corners[i][0])
                                .speed(0.005)
                                .range(-1.0..=1.0_f32),
                        );
                        ui.add(
                            DragValue::new(&mut state.tools.perspective_corners[i][1])
                                .speed(0.005)
                                .range(-1.0..=1.0_f32),
                        );
                        ui.end_row();
                    }
                });
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(has_image, egui::Button::new("Apply"))
                    .clicked()
                {
                    state.push_perspective();
                }
                if ui.button("Reset").clicked() {
                    state.reset_perspective();
                }
            });
        });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("perspective".to_string(), !default_open);
    }

    ui.separator();

    // ── Resize ────────────────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("resize");
    let resp = egui::CollapsingHeader::new("⤢  Resize")
        .id_salt("resize")
        .default_open(default_open)
        .show(ui, |ui| {
            use rasterlab_core::ops::ResampleMode;

            let orig_w = state.tools.resize_w;
            let orig_h = state.tools.resize_h;

            // Source image dims for preset filtering and aspect-ratio math.
            let src_dims = state
                .pipeline()
                .map(|p: &rasterlab_core::pipeline::EditPipeline| {
                    (p.source().width, p.source().height)
                });

            // ── MP preset dropdown ────────────────────────────────────
            // (label, total target pixels)
            const MP_PRESETS: &[(&str, u32)] = &[
                ("24 MP", 24_000_000),
                ("20 MP", 20_000_000),
                ("16 MP", 16_000_000),
                ("12 MP", 12_000_000),
                ("10 MP", 10_000_000),
                ("8 MP", 8_000_000),
                ("6 MP", 6_000_000),
                ("5 MP", 5_000_000),
                ("4 MP", 4_000_000),
                ("3 MP", 3_000_000),
                ("2 MP", 2_000_000),
                ("1 MP", 1_000_000),
            ];

            if let Some((src_w, src_h)) = src_dims {
                let src_px = src_w as u64 * src_h as u64;
                // h/w aspect ratio of the source image.
                let aspect = src_h as f64 / src_w as f64;

                let available: Vec<(&str, u32)> = MP_PRESETS
                    .iter()
                    .filter(|(_, px)| (*px as u64) < src_px)
                    .map(|(lbl, px)| (*lbl, *px))
                    .collect();

                if !available.is_empty() {
                    // Check whether the current w×h matches a preset (±2 px rounding).
                    let cur_px = orig_w as u64 * orig_h as u64;
                    let selected_label = available
                        .iter()
                        .find(|(_, target_px)| {
                            let w = ((*target_px as f64 / aspect).sqrt().round() as u32).max(1);
                            let h = (w as f64 * aspect).round() as u32;
                            (w as u64 * h as u64).abs_diff(cur_px) <= 2
                        })
                        .map(|(lbl, _)| *lbl)
                        .unwrap_or("— Preset —");

                    ui.horizontal(|ui| {
                        ui.label("Preset");
                        egui::ComboBox::from_id_salt("resize_mp_preset")
                            .selected_text(selected_label)
                            .show_ui(ui, |ui| {
                                for (lbl, target_px) in &available {
                                    let w =
                                        ((*target_px as f64 / aspect).sqrt().round() as u32).max(1);
                                    let h = (w as f64 * aspect).round() as u32;
                                    let hint = format!("{lbl}  ({w}×{h})");
                                    if ui.selectable_label(selected_label == *lbl, hint).clicked() {
                                        state.tools.resize_w = w;
                                        state.tools.resize_h = h;
                                    }
                                }
                            });
                    });
                    ui.add_space(4.0);
                }
            }

            egui::Grid::new("resize_grid")
                .num_columns(2)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    ui.label("Width");
                    let w_resp = ui.add(
                        DragValue::new(&mut state.tools.resize_w)
                            .speed(1)
                            .range(1..=32000_u32)
                            .suffix(" px"),
                    );
                    ui.end_row();
                    ui.label("Height");
                    let h_resp = ui.add(
                        DragValue::new(&mut state.tools.resize_h)
                            .speed(1)
                            .range(1..=32000_u32)
                            .suffix(" px"),
                    );
                    ui.end_row();
                    ui.label("Method");
                    egui::ComboBox::from_id_salt("resize_mode")
                        .selected_text(match state.tools.resize_mode {
                            ResampleMode::NearestNeighbour => "Nearest",
                            ResampleMode::Bilinear => "Bilinear",
                            ResampleMode::Bicubic => "Bicubic",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut state.tools.resize_mode,
                                ResampleMode::NearestNeighbour,
                                "Nearest",
                            );
                            ui.selectable_value(
                                &mut state.tools.resize_mode,
                                ResampleMode::Bilinear,
                                "Bilinear",
                            );
                            ui.selectable_value(
                                &mut state.tools.resize_mode,
                                ResampleMode::Bicubic,
                                "Bicubic",
                            );
                        });
                    ui.end_row();
                    ui.label("Lock aspect");
                    ui.checkbox(&mut state.tools.resize_lock_aspect, "");
                    ui.end_row();

                    // Propagate aspect-ratio constraint after editing.
                    if state.tools.resize_lock_aspect && orig_w > 0 && orig_h > 0 {
                        if w_resp.changed() && orig_w != state.tools.resize_w {
                            let ratio = orig_h as f64 / orig_w as f64;
                            state.tools.resize_h =
                                ((state.tools.resize_w as f64 * ratio).round() as u32).max(1);
                        } else if h_resp.changed() && orig_h != state.tools.resize_h {
                            let ratio = orig_w as f64 / orig_h as f64;
                            state.tools.resize_w =
                                ((state.tools.resize_h as f64 * ratio).round() as u32).max(1);
                        }
                    }
                });

            if ui
                .add_enabled(has_image, egui::Button::new("Apply Resize"))
                .clicked()
            {
                state.push_resize();
            }
        });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("resize".to_string(), !default_open);
    }

    ui.separator();

    // ── Rotate ───────────────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("rotate");
    let resp = egui::CollapsingHeader::new("↻  Rotate")
        .id_salt("rotate")
        .default_open(default_open)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(has_image, egui::Button::new("90°"))
                    .clicked()
                {
                    state.push_rotate_90();
                }
                if ui
                    .add_enabled(has_image, egui::Button::new("180°"))
                    .clicked()
                {
                    state.push_rotate_180();
                }
                if ui
                    .add_enabled(has_image, egui::Button::new("270°"))
                    .clicked()
                {
                    state.push_rotate_270();
                }
            });
            ui.horizontal(|ui| {
                ui.label("Angle:");
                ui.add(
                    DragValue::new(&mut state.tools.rotate_deg)
                        .speed(0.5)
                        .suffix("°")
                        .range(-360.0..=360.0),
                );
                if ui
                    .add_enabled(has_image, egui::Button::new("Apply"))
                    .clicked()
                {
                    state.push_rotate_arbitrary();
                }
            });
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(has_image, egui::Button::new("Flip H"))
                    .clicked()
                {
                    state.push_flip_horizontal();
                }
                if ui
                    .add_enabled(has_image, egui::Button::new("Flip V"))
                    .clicked()
                {
                    state.push_flip_vertical();
                }
            });
        });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("rotate".to_string(), !default_open);
    }

    ui.separator();

    // ── Straighten ───────────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("straighten");
    let straight_label = if state.tools.straighten_active {
        format!("⟳  Straighten  [{:.2}°]", state.tools.straighten_angle)
    } else {
        "⟳  Straighten".to_string()
    };
    let resp = egui::CollapsingHeader::new(straight_label)
        .id_salt("straighten")
        .default_open(default_open)
        .show(ui, |ui| {
            egui::Grid::new("straighten_grid")
                .num_columns(2)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    ui.label("Angle:");
                    ui.label(format!("{:.2}°", state.tools.straighten_angle));
                    ui.end_row();
                });

            ui.checkbox(
                &mut state.tools.straighten_crop,
                "Crop to rectangle after apply",
            );

            let toggle_text = if state.tools.straighten_active {
                "Hide Horizon Line"
            } else {
                "Show Horizon Line"
            };
            if ui
                .add_enabled(
                    has_image,
                    egui::Button::new(toggle_text).min_size(Vec2::new(ui.available_width(), 0.0)),
                )
                .clicked()
            {
                state.tools.straighten_active = !state.tools.straighten_active;
            }

            if ui
                .add_enabled(
                    has_image,
                    egui::Button::new("Apply Straighten")
                        .min_size(Vec2::new(ui.available_width(), 0.0)),
                )
                .clicked()
            {
                state.push_straighten();
            }

            if state.tools.straighten_active {
                ui.label(
                    egui::RichText::new(
                        "Drag the horizon line to match a level reference in the image.",
                    )
                    .small()
                    .color(egui::Color32::from_gray(140)),
                );
            }
        });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("straighten".to_string(), !default_open);
    }

    ui.separator();

    // ── Saturation ────────────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("saturation");
    let resp = egui::CollapsingHeader::new("🎨  Saturation")
        .id_salt("saturation")
        .default_open(default_open)
        .show(ui, |ui| {
            let changed = ui
                .add(egui::Slider::new(&mut state.tools.saturation, 0.0..=4.0).step_by(0.01))
                .changed();
            if changed && has_image {
                state.update_sat_preview();
            }
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(has_image, egui::Button::new("Apply"))
                    .clicked()
                {
                    state.push_saturation();
                }
                if state.tools.sat_preview_active
                    && ui
                        .add_enabled(has_image, egui::Button::new("Cancel"))
                        .clicked()
                {
                    state.cancel_sat_preview();
                }
                if ui.button("Reset").clicked() {
                    state.reset_saturation();
                }
            });
        });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("saturation".to_string(), !default_open);
    }

    ui.separator();

    // ── Sepia ─────────────────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("sepia");
    let resp = egui::CollapsingHeader::new("🟫  Sepia")
        .id_salt("sepia")
        .default_open(default_open)
        .show(ui, |ui| {
            let changed = ui
                .add(egui::Slider::new(&mut state.tools.sepia_strength, 0.0..=1.0).step_by(0.01))
                .changed();
            if changed && has_image {
                state.update_sepia_preview();
            }
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(has_image, egui::Button::new("Apply Sepia"))
                    .clicked()
                {
                    state.push_sepia();
                }
                if state.tools.sepia_preview_active
                    && ui
                        .add_enabled(has_image, egui::Button::new("Cancel"))
                        .clicked()
                {
                    state.cancel_sepia_preview();
                }
                if ui.button("Reset").clicked() {
                    state.reset_sepia();
                }
            });
        });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("sepia".to_string(), !default_open);
    }

    ui.separator();

    // ── Sharpen ──────────────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("sharpen");
    let resp = egui::CollapsingHeader::new("◈  Sharpen")
        .id_salt("sharpen")
        .default_open(default_open)
        .show(ui, |ui| {
            let changed = ui
                .add(
                    egui::Slider::new(&mut state.tools.sharpen_strength, 0.0..=10.0)
                        .step_by(0.05)
                        .text("Strength"),
                )
                .changed();
            if changed && has_image {
                state.update_sharpen_preview();
            }
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(has_image, egui::Button::new("Apply Sharpen"))
                    .clicked()
                {
                    state.push_sharpen();
                }
                if state.tools.sharpen_preview_active
                    && ui
                        .add_enabled(has_image, egui::Button::new("Cancel"))
                        .clicked()
                {
                    state.cancel_sharpen_preview();
                }
            });
        });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("sharpen".to_string(), !default_open);
    }

    ui.separator();

    // ── Split Tone ────────────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("split_tone");
    let resp = egui::CollapsingHeader::new("🎨  Split Tone")
        .id_salt("split_tone")
        .default_open(default_open)
        .show(ui, |ui| {
            let mut changed = false;

            egui::Grid::new("split_tone_grid")
                .num_columns(2)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    ui.label("Shadow hue");
                    changed |= ui
                        .add(
                            DragValue::new(&mut state.tools.split_shadow_hue)
                                .speed(1.0)
                                .range(0.0..=359.9_f32)
                                .suffix("°"),
                        )
                        .changed();
                    ui.end_row();

                    ui.label("Shadow sat");
                    changed |= ui
                        .add(
                            egui::Slider::new(&mut state.tools.split_shadow_sat, 0.0..=1.0)
                                .step_by(0.01),
                        )
                        .changed();
                    ui.end_row();

                    ui.label("Highlight hue");
                    changed |= ui
                        .add(
                            DragValue::new(&mut state.tools.split_highlight_hue)
                                .speed(1.0)
                                .range(0.0..=359.9_f32)
                                .suffix("°"),
                        )
                        .changed();
                    ui.end_row();

                    ui.label("Highlight sat");
                    changed |= ui
                        .add(
                            egui::Slider::new(&mut state.tools.split_highlight_sat, 0.0..=1.0)
                                .step_by(0.01),
                        )
                        .changed();
                    ui.end_row();

                    ui.label("Balance");
                    changed |= ui
                        .add(
                            egui::Slider::new(&mut state.tools.split_balance, -1.0..=1.0)
                                .step_by(0.01),
                        )
                        .changed();
                    ui.end_row();
                });

            if changed && has_image {
                state.update_split_preview();
            }

            ui.horizontal(|ui| {
                if ui
                    .add_enabled(has_image, egui::Button::new("Apply"))
                    .clicked()
                {
                    state.push_split_tone();
                }
                if ui.button("Reset").clicked() {
                    state.reset_split_tone();
                }
                if state.tools.split_preview_active
                    && ui
                        .add_enabled(has_image, egui::Button::new("Cancel"))
                        .clicked()
                {
                    state.cancel_split_preview();
                }
            });
        });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("split_tone".to_string(), !default_open);
    }

    ui.separator();

    // ── Vibrance ──────────────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("vibrance");
    let resp = egui::CollapsingHeader::new("✦  Vibrance")
        .id_salt("vibrance")
        .default_open(default_open)
        .show(ui, |ui| {
            let changed = ui
                .add(egui::Slider::new(&mut state.tools.vibrance, -1.0..=1.0).step_by(0.01))
                .changed();
            if changed && has_image {
                state.update_vibrance_preview();
            }
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(has_image, egui::Button::new("Apply"))
                    .clicked()
                {
                    state.push_vibrance();
                }
                if state.tools.vibrance_preview_active
                    && ui
                        .add_enabled(has_image, egui::Button::new("Cancel"))
                        .clicked()
                {
                    state.cancel_vibrance_preview();
                }
                if ui.button("Reset").clicked() {
                    state.reset_vibrance();
                }
            });
        });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("vibrance".to_string(), !default_open);
    }

    ui.separator();

    // ── Vignette ──────────────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("vignette");
    let resp = egui::CollapsingHeader::new("◎  Vignette")
        .id_salt("vignette")
        .default_open(default_open)
        .show(ui, |ui| {
            let mut changed = false;
            egui::Grid::new("vignette_grid")
                .num_columns(2)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    ui.label("Strength");
                    changed |= ui
                        .add(
                            DragValue::new(&mut state.tools.vignette_strength)
                                .speed(0.01)
                                .range(0.0..=1.0),
                        )
                        .changed();
                    ui.end_row();
                    ui.label("Radius");
                    changed |= ui
                        .add(
                            DragValue::new(&mut state.tools.vignette_radius)
                                .speed(0.01)
                                .range(0.0..=1.0),
                        )
                        .changed();
                    ui.end_row();
                    ui.label("Feather");
                    changed |= ui
                        .add(
                            DragValue::new(&mut state.tools.vignette_feather)
                                .speed(0.01)
                                .range(0.0..=1.0),
                        )
                        .changed();
                    ui.end_row();
                });
            if changed && has_image {
                state.update_vignette_preview();
            }
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(has_image, egui::Button::new("Apply Vignette"))
                    .clicked()
                {
                    state.push_vignette();
                }
                if state.tools.vignette_preview_active
                    && ui
                        .add_enabled(has_image, egui::Button::new("Cancel"))
                        .clicked()
                {
                    state.cancel_vignette_preview();
                }
            });
        });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("vignette".to_string(), !default_open);
    }

    ui.separator();

    // ── White Balance ─────────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("white_balance");
    let resp = egui::CollapsingHeader::new("🌡  White Balance")
        .id_salt("white_balance")
        .default_open(default_open)
        .show(ui, |ui| {
            let mut changed = false;
            egui::Grid::new("wb_grid")
                .num_columns(2)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    ui.label("Temperature");
                    changed |= ui
                        .add(
                            egui::Slider::new(&mut state.tools.wb_temperature, -1.0..=1.0)
                                .step_by(0.01),
                        )
                        .changed();
                    ui.end_row();
                    ui.label("Tint");
                    changed |= ui
                        .add(egui::Slider::new(&mut state.tools.wb_tint, -1.0..=1.0).step_by(0.01))
                        .changed();
                    ui.end_row();
                });
            if changed && has_image {
                state.update_wb_preview();
            }
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(has_image, egui::Button::new("Apply"))
                    .clicked()
                {
                    state.push_wb();
                }
                if state.tools.wb_preview_active
                    && ui
                        .add_enabled(has_image, egui::Button::new("Cancel"))
                        .clicked()
                {
                    state.cancel_wb_preview();
                }
                if ui.button("Reset").clicked() {
                    state.reset_wb();
                }
            });
        });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("white_balance".to_string(), !default_open);
    }
}

// ---------------------------------------------------------------------------
// Curves tool
// ---------------------------------------------------------------------------

fn curves_ui(ui: &mut Ui, state: &mut AppState) {
    let has_image = state.pipeline().is_some();

    // Square canvas — fill available width up to 200 px.
    let size = ui.available_width().min(200.0);
    let (resp, painter) = ui.allocate_painter(Vec2::splat(size), egui::Sense::click_and_drag());
    let rect = resp.rect;
    let w = rect.width();
    let h = rect.height();

    // Background and grid.
    painter.rect_filled(rect, CornerRadius::ZERO, Color32::from_gray(25));
    for i in 1..4 {
        let t = i as f32 / 4.0;
        let gx = rect.min.x + t * w;
        let gy = rect.min.y + t * h;
        let grid_col = Color32::from_gray(50);
        painter.line_segment(
            [Pos2::new(gx, rect.min.y), Pos2::new(gx, rect.max.y)],
            Stroke::new(1.0, grid_col),
        );
        painter.line_segment(
            [Pos2::new(rect.min.x, gy), Pos2::new(rect.max.x, gy)],
            Stroke::new(1.0, grid_col),
        );
    }
    // Identity diagonal (subtle reference).
    painter.line_segment(
        [
            Pos2::new(rect.min.x, rect.max.y),
            Pos2::new(rect.max.x, rect.min.y),
        ],
        Stroke::new(1.0, Color32::from_gray(60)),
    );

    // Build and draw the curve.
    let lut = CurvesOp::build_lut(&state.tools.curve_points);
    {
        let mut prev: Option<Pos2> = None;
        for (i, &y_val) in lut.iter().enumerate() {
            let cx = rect.min.x + (i as f32 / 255.0) * w;
            let cy = rect.max.y - (y_val as f32 / 255.0) * h;
            let pos = Pos2::new(cx, cy);
            if let Some(p) = prev {
                painter.line_segment([p, pos], Stroke::new(1.5, Color32::WHITE));
            }
            prev = Some(pos);
        }
    }

    // Draw control point handles.
    const PT_R: f32 = 5.0;
    for (i, &[px, py]) in state.tools.curve_points.iter().enumerate() {
        let sx = rect.min.x + px * w;
        let sy = rect.max.y - py * h;
        let col = if state.tools.curve_dragging_idx == Some(i) {
            Color32::from_rgb(255, 200, 0)
        } else {
            Color32::WHITE
        };
        painter.circle_filled(Pos2::new(sx, sy), PT_R, col);
        painter.circle_stroke(Pos2::new(sx, sy), PT_R, Stroke::new(1.0, Color32::BLACK));
    }

    // ── Interaction ───────────────────────────────────────────────────────
    let (mouse_pos, primary_down, primary_pressed, secondary_pressed) = ui.input(|i| {
        (
            i.pointer.interact_pos(),
            i.pointer.button_down(egui::PointerButton::Primary),
            i.pointer.button_pressed(egui::PointerButton::Primary),
            i.pointer.button_pressed(egui::PointerButton::Secondary),
        )
    });

    // Release drag.
    if !primary_down {
        state.tools.curve_dragging_idx = None;
    }

    if let Some(pos) = mouse_pos {
        // Convert screen position to curve coordinates.
        let cx = ((pos.x - rect.min.x) / w).clamp(0.0, 1.0);
        let cy = (1.0 - (pos.y - rect.min.y) / h).clamp(0.0, 1.0);

        // Continue existing drag.
        if primary_down && let Some(drag_idx) = state.tools.curve_dragging_idx {
            let npts = state.tools.curve_points.len();
            let new_x = if drag_idx == 0 {
                0.0
            } else if drag_idx == npts - 1 {
                1.0
            } else {
                // Constrain x between neighbours so sort order is preserved.
                let lo = state.tools.curve_points[drag_idx - 1][0] + 0.005;
                let hi = state.tools.curve_points[drag_idx + 1][0] - 0.005;
                cx.clamp(lo, hi)
            };
            let old = state.tools.curve_points[drag_idx];
            state.tools.curve_points[drag_idx] = [new_x, cy];
            if state.tools.curve_points[drag_idx] != old && has_image {
                state.update_curve_preview();
            }
        }

        if primary_pressed && rect.contains(pos) {
            // Find a control point close enough to start a drag.
            let hit = state.tools.curve_points.iter().position(|&[px, py]| {
                let sx = rect.min.x + px * w;
                let sy = rect.max.y - py * h;
                ((pos.x - sx).powi(2) + (pos.y - sy).powi(2)).sqrt() < PT_R + 3.0
            });
            if let Some(idx) = hit {
                state.tools.curve_dragging_idx = Some(idx);
            } else {
                // Click on empty area → add a new point.
                state.tools.curve_points.push([cx, cy]);
                state
                    .tools
                    .curve_points
                    .sort_by(|a, b| a[0].partial_cmp(&b[0]).unwrap());
                if has_image {
                    state.update_curve_preview();
                }
            }
        }

        if secondary_pressed && rect.contains(pos) {
            // Right-click → remove the nearest non-endpoint control point.
            let hit = state.tools.curve_points[1..state.tools.curve_points.len() - 1]
                .iter()
                .enumerate()
                .find(|(_, pt)| {
                    let sx = rect.min.x + pt[0] * w;
                    let sy = rect.max.y - pt[1] * h;
                    ((pos.x - sx).powi(2) + (pos.y - sy).powi(2)).sqrt() < PT_R + 4.0
                })
                .map(|(i, _)| i + 1); // offset by 1 for the slice starting at index 1
            if let Some(idx) = hit {
                state.tools.curve_points.remove(idx);
                if has_image {
                    state.update_curve_preview();
                }
            }
        }
    }

    ui.add_space(2.0);
    ui.horizontal(|ui| {
        if ui
            .add_enabled(has_image, egui::Button::new("Apply Curve"))
            .clicked()
        {
            state.push_curves();
        }
        if state.tools.curve_preview_active
            && ui
                .add_enabled(has_image, egui::Button::new("Cancel"))
                .clicked()
        {
            state.cancel_curve_preview();
        }
        if ui.button("Reset").clicked() {
            state.reset_curves();
        }
    });
}

// ---------------------------------------------------------------------------
// Levels tool
// ---------------------------------------------------------------------------

fn levels_ui(ui: &mut Ui, state: &mut AppState) {
    let has_image = state.pipeline().is_some();

    // Combined histogram
    draw_combined_histogram(ui, state);

    ui.add_space(4.0);

    // Black / midtone / white sliders
    let mut changed = false;

    egui::Grid::new("levels_grid")
        .num_columns(2)
        .spacing([8.0, 4.0])
        .show(ui, |ui| {
            ui.label("Black:");
            let r = ui.add(
                egui::Slider::new(&mut state.tools.levels_black, 0.0..=1.0)
                    .clamping(egui::SliderClamping::Always)
                    .step_by(0.001),
            );
            if r.changed() {
                // Black point must not exceed white point
                if state.tools.levels_black >= state.tools.levels_white {
                    state.tools.levels_black = (state.tools.levels_white - 0.001).max(0.0);
                }
                changed = true;
            }
            ui.end_row();

            ui.label("Mid:");
            let r = ui.add(
                egui::Slider::new(&mut state.tools.levels_mid, 0.10..=10.0)
                    .clamping(egui::SliderClamping::Always)
                    .step_by(0.01)
                    .logarithmic(true),
            );
            if r.changed() {
                changed = true;
            }
            ui.end_row();

            ui.label("White:");
            let r = ui.add(
                egui::Slider::new(&mut state.tools.levels_white, 0.0..=1.0)
                    .clamping(egui::SliderClamping::Always)
                    .step_by(0.001),
            );
            if r.changed() {
                // White point must not go below black point
                if state.tools.levels_white <= state.tools.levels_black {
                    state.tools.levels_white = (state.tools.levels_black + 0.001).min(1.0);
                }
                changed = true;
            }
            ui.end_row();
        });

    if changed && has_image {
        state.update_levels_preview();
    }

    ui.add_space(4.0);
    ui.horizontal(|ui| {
        if ui
            .add_enabled(has_image, egui::Button::new("Apply Levels"))
            .clicked()
        {
            state.apply_levels();
        }
        if ui.button("Reset").clicked() {
            state.reset_levels();
        }
    });
}

// ---------------------------------------------------------------------------
// Grain tool
// ---------------------------------------------------------------------------

fn grain_ui(ui: &mut Ui, state: &mut AppState) {
    let has_image = state.pipeline().is_some();

    // Film preset buttons.
    ui.label("Film presets:");
    ui.horizontal_wrapped(|ui| {
        for &(label, strength, size) in GRAIN_PRESETS {
            if ui.small_button(label).clicked() && has_image {
                state.tools.grain_strength = strength;
                state.tools.grain_size = size;
                state.update_grain_preview();
            }
        }
    });
    ui.add_space(2.0);

    // Strength and size sliders.
    let mut changed = false;
    egui::Grid::new("grain_grid")
        .num_columns(2)
        .spacing([8.0, 4.0])
        .show(ui, |ui| {
            ui.label("Strength");
            changed |= ui
                .add(egui::Slider::new(&mut state.tools.grain_strength, 0.0..=1.0).step_by(0.01))
                .changed();
            ui.end_row();
            ui.label("Size");
            changed |= ui
                .add(egui::Slider::new(&mut state.tools.grain_size, 1.0..=32.0).step_by(0.1))
                .changed();
            ui.end_row();
            ui.label("Seed");
            changed |= ui
                .add(DragValue::new(&mut state.tools.grain_seed))
                .changed();
            ui.end_row();
        });
    if changed && has_image {
        state.update_grain_preview();
    }

    ui.horizontal(|ui| {
        if ui
            .add_enabled(has_image, egui::Button::new("Apply Grain"))
            .clicked()
        {
            state.push_grain();
        }
        if state.tools.grain_preview_active
            && ui
                .add_enabled(has_image, egui::Button::new("Cancel"))
                .clicked()
        {
            state.cancel_grain_preview();
        }
        if ui.button("Reset").clicked() {
            state.reset_grain();
        }
    });
}

// ---------------------------------------------------------------------------
// HSL Panel tool
// ---------------------------------------------------------------------------

const HSL_BAND_NAMES: [&str; 8] = [
    "Reds", "Oranges", "Yellows", "Greens", "Aquas", "Blues", "Purples", "Magentas",
];

fn hsl_panel_ui(ui: &mut Ui, state: &mut AppState, has_image: bool) {
    let mut changed = false;

    let default_open = state.prefs.is_tool_open("hsl_hue");
    let resp = egui::CollapsingHeader::new("Hue")
        .id_salt("hsl_hue")
        .default_open(default_open)
        .show(ui, |ui| {
            egui::Grid::new("hsl_hue_grid")
                .num_columns(2)
                .spacing([8.0, 2.0])
                .show(ui, |ui| {
                    for (i, name) in HSL_BAND_NAMES.iter().enumerate() {
                        ui.label(*name);
                        changed |= ui
                            .add(
                                egui::Slider::new(&mut state.tools.hsl_hue[i], -180.0..=180.0)
                                    .text("°")
                                    .step_by(1.0),
                            )
                            .changed();
                        ui.end_row();
                    }
                });
        });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("hsl_hue".to_string(), !default_open);
    }

    let default_open = state.prefs.is_tool_open("hsl_sat");
    let resp = egui::CollapsingHeader::new("Saturation")
        .id_salt("hsl_sat")
        .default_open(default_open)
        .show(ui, |ui| {
            egui::Grid::new("hsl_sat_grid")
                .num_columns(2)
                .spacing([8.0, 2.0])
                .show(ui, |ui| {
                    for (i, name) in HSL_BAND_NAMES.iter().enumerate() {
                        ui.label(*name);
                        changed |= ui
                            .add(
                                egui::Slider::new(&mut state.tools.hsl_sat[i], -1.0..=1.0)
                                    .step_by(0.01),
                            )
                            .changed();
                        ui.end_row();
                    }
                });
        });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("hsl_sat".to_string(), !default_open);
    }

    let default_open = state.prefs.is_tool_open("hsl_lum");
    let resp = egui::CollapsingHeader::new("Luminance")
        .id_salt("hsl_lum")
        .default_open(default_open)
        .show(ui, |ui| {
            egui::Grid::new("hsl_lum_grid")
                .num_columns(2)
                .spacing([8.0, 2.0])
                .show(ui, |ui| {
                    for (i, name) in HSL_BAND_NAMES.iter().enumerate() {
                        ui.label(*name);
                        changed |= ui
                            .add(
                                egui::Slider::new(&mut state.tools.hsl_lum[i], -0.5..=0.5)
                                    .step_by(0.01),
                            )
                            .changed();
                        ui.end_row();
                    }
                });
        });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("hsl_lum".to_string(), !default_open);
    }

    if changed && has_image {
        state.update_hsl_preview();
    }

    ui.horizontal(|ui| {
        if ui
            .add_enabled(has_image, egui::Button::new("Apply"))
            .clicked()
        {
            state.push_hsl();
        }
        if state.tools.hsl_preview_active
            && ui
                .add_enabled(has_image, egui::Button::new("Cancel"))
                .clicked()
        {
            state.cancel_hsl_preview();
        }
        if ui.button("Reset").clicked() {
            state.reset_hsl();
        }
    });
}

/// Draw all four histogram channels (R, G, B, L) overlaid on a single canvas,
/// with vertical markers for the current black and white point positions.
fn draw_combined_histogram(ui: &mut Ui, state: &AppState) {
    const HEIGHT: f32 = 96.0;

    let width = ui.available_width().max(256.0);
    let (resp, painter) = ui.allocate_painter(Vec2::new(width, HEIGHT), egui::Sense::hover());
    let rect = resp.rect;

    // Dark background
    painter.rect_filled(rect, CornerRadius::ZERO, Color32::from_gray(20));

    let Some(hist) = &state.histogram else {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "No image",
            egui::FontId::monospace(11.0),
            Color32::from_gray(100),
        );
        return;
    };

    // Normalise all channels together so relative brightnesses are preserved.
    let peak = hist
        .red
        .iter()
        .chain(hist.green.iter())
        .chain(hist.blue.iter())
        .chain(hist.luma.iter())
        .copied()
        .max()
        .unwrap_or(1)
        .max(1) as f32;

    let bar_w = (width / 256.0).max(1.0);

    let channels: [(&[u64; 256], Color32); 4] = [
        (
            &hist.luma,
            Color32::from_rgba_unmultiplied(200, 200, 200, 80),
        ),
        (&hist.red, Color32::from_rgba_unmultiplied(220, 60, 60, 120)),
        (
            &hist.green,
            Color32::from_rgba_unmultiplied(60, 180, 60, 120),
        ),
        (
            &hist.blue,
            Color32::from_rgba_unmultiplied(60, 80, 220, 120),
        ),
    ];

    for (data, color) in &channels {
        for (i, &count) in data.iter().enumerate() {
            if count == 0 {
                continue;
            }
            let bar_h = (count as f32 / peak) * HEIGHT;
            let x = rect.left() + i as f32 * bar_w;
            painter.rect_filled(
                Rect::from_min_size(
                    egui::pos2(x, rect.bottom() - bar_h),
                    Vec2::new(bar_w.max(0.5), bar_h),
                ),
                CornerRadius::ZERO,
                *color,
            );
        }
    }

    // Black-point marker (left, dark handle)
    let bx = rect.left() + state.tools.levels_black * width;
    painter.line_segment(
        [egui::pos2(bx, rect.top()), egui::pos2(bx, rect.bottom())],
        egui::Stroke::new(1.5, Color32::from_gray(60)),
    );
    // Small triangle handle at bottom
    let tp = egui::pos2(bx, rect.bottom());
    painter.add(egui::Shape::convex_polygon(
        vec![
            tp,
            egui::pos2(tp.x - 5.0, tp.y + 7.0),
            egui::pos2(tp.x + 5.0, tp.y + 7.0),
        ],
        Color32::from_gray(60),
        egui::Stroke::NONE,
    ));

    // White-point marker (right, bright handle)
    let wx = rect.left() + state.tools.levels_white * width;
    painter.line_segment(
        [egui::pos2(wx, rect.top()), egui::pos2(wx, rect.bottom())],
        egui::Stroke::new(1.5, Color32::from_gray(220)),
    );
    let tp = egui::pos2(wx, rect.bottom());
    painter.add(egui::Shape::convex_polygon(
        vec![
            tp,
            egui::pos2(tp.x - 5.0, tp.y + 7.0),
            egui::pos2(tp.x + 5.0, tp.y + 7.0),
        ],
        Color32::from_gray(220),
        egui::Stroke::NONE,
    ));

    // Midtone marker — positioned at the geometric midpoint between black/white
    let mid_frac =
        state.tools.levels_black + (state.tools.levels_white - state.tools.levels_black) * 0.5;
    let mx = rect.left() + mid_frac * width;
    painter.line_segment(
        [egui::pos2(mx, rect.top()), egui::pos2(mx, rect.bottom())],
        egui::Stroke::new(1.5, Color32::from_rgba_unmultiplied(180, 140, 60, 200)),
    );
    let tp = egui::pos2(mx, rect.bottom());
    painter.add(egui::Shape::convex_polygon(
        vec![
            tp,
            egui::pos2(tp.x - 5.0, tp.y + 7.0),
            egui::pos2(tp.x + 5.0, tp.y + 7.0),
        ],
        Color32::from_rgba_unmultiplied(180, 140, 60, 200),
        egui::Stroke::NONE,
    ));

    // Hover tooltip
    if let Some(pos) = resp.hover_pos() {
        let bucket = ((pos.x - rect.left()) / bar_w).clamp(0.0, 255.0) as usize;
        let text = format!(
            "{}  R:{} G:{} B:{} L:{}",
            bucket, hist.red[bucket], hist.green[bucket], hist.blue[bucket], hist.luma[bucket],
        );
        painter.text(
            egui::pos2(pos.x.min(rect.right() - 10.0), rect.top() + 12.0),
            egui::Align2::LEFT_CENTER,
            text,
            egui::FontId::monospace(10.0),
            Color32::WHITE,
        );
    }
}
