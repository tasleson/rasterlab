use std::any::Any;

use egui::{ComboBox, DragValue};
use rasterlab_core::ops::{BlackAndWhiteOp, BwMode};
use rasterlab_core::traits::operation::Operation;

use super::tool_trait::{Tool, ToolAction, ToolUiCtx};
use crate::state::EditingTool;

const BW_MODES: &[&str] = &[
    "Luminance (BT.709)",
    "Average",
    "Perceptual (BT.601)",
    "Channel Mixer",
];

const BW_PRESETS: &[(&str, f32, f32, f32)] = &[
    ("Neutral", 0.2126, 0.7152, 0.0722),
    ("Dramatic Contrast", 0.60, 0.40, 0.00),
    ("Red Filter", 1.00, 0.00, 0.00),
    ("Green Filter", 0.00, 1.00, 0.00),
    ("Blue Filter", 0.00, 0.00, 1.00),
    ("Soften / Skin", 0.25, 0.55, 0.20),
    ("Urban / Cool", 0.00, 0.30, 0.70),
    ("High Key", 0.40, 0.50, 0.30),
    ("Low Key", 0.10, 0.20, 0.05),
    ("Infrared", 0.90, 0.10, -0.10),
];

pub struct BwTool {
    pub mode_idx: usize,
    pub mixer_r: f32,
    pub mixer_g: f32,
    pub mixer_b: f32,
    pub preview_active: bool,
}

impl BwTool {
    pub fn new() -> Self {
        Self {
            mode_idx: 0,
            mixer_r: 0.2126,
            mixer_g: 0.7152,
            mixer_b: 0.0722,
            preview_active: false,
        }
    }

    fn make_op(&self) -> Box<dyn Operation> {
        match self.mode_idx {
            1 => Box::new(BlackAndWhiteOp::average()),
            2 => Box::new(BlackAndWhiteOp::perceptual()),
            3 => Box::new(BlackAndWhiteOp::channel_mixer(
                self.mixer_r,
                self.mixer_g,
                self.mixer_b,
            )),
            _ => Box::new(BlackAndWhiteOp::luminance()),
        }
    }
}

impl Tool for BwTool {
    fn id(&self) -> &'static str {
        "bw"
    }
    fn display_name(&self) -> &'static str {
        "◑  Black & White"
    }
    fn editing_tool(&self) -> Option<EditingTool> {
        Some(EditingTool::BlackAndWhite)
    }

    fn render_ui(&mut self, ui: &mut egui::Ui, ctx: &ToolUiCtx<'_>) -> ToolAction {
        let old_idx = self.mode_idx;
        let combo_resp = ComboBox::from_label("Mode")
            .selected_text(BW_MODES[self.mode_idx])
            .show_ui(ui, |ui| {
                for (i, &label) in BW_MODES.iter().enumerate() {
                    ui.selectable_value(&mut self.mode_idx, i, label);
                }
            });
        let mut action = ToolAction::None;
        if (combo_resp.response.changed() || self.mode_idx != old_idx) && ctx.has_image {
            self.preview_active = true;
            action = ToolAction::RequestRender;
        }

        if self.mode_idx == 3 {
            let mut changed = false;

            ui.label("Presets:");
            let mut preset_clicked = false;
            ui.horizontal_wrapped(|ui| {
                for &(label, r, g, b) in BW_PRESETS {
                    if ui.small_button(label).clicked() && ctx.has_image {
                        self.mixer_r = r;
                        self.mixer_g = g;
                        self.mixer_b = b;
                        preset_clicked = true;
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
                            DragValue::new(&mut self.mixer_r)
                                .speed(0.01)
                                .range(-2.0..=2.0),
                        )
                        .changed();
                    ui.end_row();
                    ui.label("G");
                    changed |= ui
                        .add(
                            DragValue::new(&mut self.mixer_g)
                                .speed(0.01)
                                .range(-2.0..=2.0),
                        )
                        .changed();
                    ui.end_row();
                    ui.label("B");
                    changed |= ui
                        .add(
                            DragValue::new(&mut self.mixer_b)
                                .speed(0.01)
                                .range(-2.0..=2.0),
                        )
                        .changed();
                    ui.end_row();
                });
            if (changed || preset_clicked) && ctx.has_image {
                self.preview_active = true;
                action = ToolAction::RequestRender;
            }
        }

        ui.horizontal(|ui| {
            if ui
                .add_enabled(ctx.has_image, egui::Button::new("Apply B&W"))
                .clicked()
            {
                self.preview_active = false;
                action = ToolAction::PushOp(self.make_op());
                self.mode_idx = 0;
                self.mixer_r = 0.2126;
                self.mixer_g = 0.7152;
                self.mixer_b = 0.0722;
            }
            if self.preview_active
                && ui
                    .add_enabled(ctx.has_image, egui::Button::new("Cancel"))
                    .clicked()
            {
                self.preview_active = false;
                action = ToolAction::RequestRender;
            }
            if ui.button("Reset").clicked() {
                self.mode_idx = 0;
                self.mixer_r = 0.2126;
                self.mixer_g = 0.7152;
                self.mixer_b = 0.0722;
                if self.preview_active {
                    self.preview_active = false;
                    action = ToolAction::RequestRender;
                }
            }
        });
        action
    }

    super::shared::impl_preview_controls!();
    fn preview_op(&self) -> Option<Box<dyn Operation>> {
        if self.preview_active {
            Some(self.make_op())
        } else {
            None
        }
    }
    fn load_from_op(&mut self, op: &dyn Operation) -> bool {
        if let Some(o) = op
            .as_any()
            .and_then(|a| a.downcast_ref::<BlackAndWhiteOp>())
        {
            match o.mode {
                BwMode::Luminance => self.mode_idx = 0,
                BwMode::Average => self.mode_idx = 1,
                BwMode::Perceptual => self.mode_idx = 2,
                BwMode::ChannelMixer { r, g, b } => {
                    self.mode_idx = 3;
                    self.mixer_r = r;
                    self.mixer_g = g;
                    self.mixer_b = b;
                }
            }
            true
        } else {
            false
        }
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
