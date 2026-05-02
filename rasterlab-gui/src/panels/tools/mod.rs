//! Tools panel — inputs for adding operations to the pipeline.

mod shared;

mod auto_enhance;
mod blur;
mod brightness_contrast;
mod bw;
mod clarity_texture;
mod color_balance;
mod color_space;
mod crop;
mod curves;
mod denoise;
mod export;
mod faux_hdr;
mod focus_stack;
mod grain;
mod hdr_merge;
mod heal;
mod highlights_shadows;
mod hsl;
mod hue_shift;
mod levels;
mod looks;
mod lut;
mod masking;
mod metadata;
mod noise_reduction;
mod panorama;
mod perspective;
mod resize;
mod rotate;
mod saturation;
mod sepia;
mod shadow_exposure;
mod sharpen;
mod split_tone;
mod straighten;
mod vibrance;
mod vignette;
mod white_balance;

use egui::{Color32, Stroke, Ui};

use crate::state::AppState;

pub fn ui(ui: &mut Ui, state: &mut AppState) {
    ui.horizontal(|ui| {
        ui.heading("Tools");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .small_button("Collapse all")
                .on_hover_text("Collapse every tool section")
                .clicked()
            {
                state.tools_force_open = Some(false);
            }
            if ui
                .small_button("Expand all")
                .on_hover_text("Expand every tool section")
                .clicked()
            {
                state.tools_force_open = Some(true);
            }
        });
    });
    ui.separator();

    // Edit-session banner: prominent indicator that one existing op is under
    // edit and every other tool is temporarily locked out.
    if let Some(session) = state.editing {
        let frame = egui::Frame::group(ui.style())
            .stroke(Stroke::new(2.0, Color32::from_rgb(90, 160, 255)))
            .fill(Color32::from_rgb(20, 35, 60));
        frame.show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!("✎ Editing op #{}", session.op_index + 1))
                        .color(Color32::from_rgb(140, 190, 255))
                        .strong(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Cancel Edit").clicked() {
                        state.end_edit();
                    }
                });
            });
            ui.label(
                egui::RichText::new(
                    "Other tools are locked. Adjust this tool, then press its Apply button \
                     to save changes, or Cancel Edit to discard.",
                )
                .color(Color32::from_gray(180))
                .small(),
            );
        });
        ui.separator();
    }

    let has_image = state.pipeline().is_some();

    auto_enhance::ui(ui, state, has_image);
    ui.separator();

    looks::ui(ui, state, has_image);

    ui.add_space(4.0);
    ui.separator();
    ui.add_space(4.0);

    bw::ui(ui, state, has_image);
    ui.separator();

    blur::ui(ui, state, has_image);
    ui.separator();

    brightness_contrast::ui(ui, state, has_image);
    ui.separator();

    clarity_texture::ui(ui, state, has_image);
    ui.separator();

    color_balance::ui(ui, state, has_image);
    ui.separator();

    color_space::ui(ui, state, has_image);
    ui.separator();

    crop::ui(ui, state, has_image);
    ui.separator();

    curves::ui(ui, state, has_image);
    ui.separator();

    denoise::ui(ui, state, has_image);
    ui.separator();

    export::ui(ui, state, has_image);
    ui.separator();

    faux_hdr::ui(ui, state, has_image);
    ui.separator();

    focus_stack::ui(ui, state, has_image);
    ui.separator();

    grain::ui(ui, state, has_image);
    ui.separator();

    hdr_merge::ui(ui, state, has_image);
    ui.separator();

    heal::ui(ui, state, has_image);
    ui.separator();

    highlights_shadows::ui(ui, state, has_image);
    ui.separator();

    hsl::ui(ui, state, has_image);
    ui.separator();

    hue_shift::ui(ui, state, has_image);
    ui.separator();

    levels::ui(ui, state, has_image);
    ui.separator();

    masking::ui(ui, state, has_image);
    ui.separator();

    lut::ui(ui, state, has_image);
    ui.separator();

    metadata::ui(ui, state, has_image);
    ui.separator();

    noise_reduction::ui(ui, state, has_image);
    ui.separator();

    panorama::ui(ui, state, has_image);
    ui.separator();

    perspective::ui(ui, state, has_image);
    ui.separator();

    resize::ui(ui, state, has_image);
    ui.separator();

    rotate::ui(ui, state, has_image);
    ui.separator();

    straighten::ui(ui, state, has_image);
    ui.separator();

    saturation::ui(ui, state, has_image);
    ui.separator();

    sepia::ui(ui, state, has_image);
    ui.separator();

    shadow_exposure::ui(ui, state, has_image);
    ui.separator();

    sharpen::ui(ui, state, has_image);
    ui.separator();

    split_tone::ui(ui, state, has_image);
    ui.separator();

    vibrance::ui(ui, state, has_image);
    ui.separator();

    vignette::ui(ui, state, has_image);
    ui.separator();

    white_balance::ui(ui, state, has_image);

    // Clear one-frame force-open flag so the user can subsequently toggle
    // individual headers without them being re-forced every frame.
    state.tools_force_open = None;
}
