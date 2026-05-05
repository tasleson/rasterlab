use std::any::Any;

use rasterlab_core::ops::HslPanelOp;
use rasterlab_core::traits::operation::Operation;

use super::tool_trait::{Tool, ToolAction, ToolUiCtx};
use crate::state::EditingTool;

const HSL_BAND_NAMES: [&str; 8] = [
    "Reds", "Oranges", "Yellows", "Greens", "Aquas", "Blues", "Purples", "Magentas",
];

pub struct HslTool {
    pub hue: [f32; 8],
    pub saturation: [f32; 8],
    pub luminance: [f32; 8],
    pub preview_active: bool,
}

impl HslTool {
    pub fn new() -> Self {
        Self {
            hue: [0.0; 8],
            saturation: [0.0; 8],
            luminance: [0.0; 8],
            preview_active: false,
        }
    }
}

impl Tool for HslTool {
    fn id(&self) -> &'static str {
        "hsl_panel"
    }
    fn display_name(&self) -> &'static str {
        "🌈  HSL Panel"
    }
    fn editing_tool(&self) -> Option<EditingTool> {
        Some(EditingTool::HslPanel)
    }

    fn render_ui(&mut self, ui: &mut egui::Ui, ctx: &ToolUiCtx<'_>) -> ToolAction {
        let mut changed = false;

        let hue_header = egui::CollapsingHeader::new("Hue").id_salt("hsl_hue");
        let hue_header = match ctx.force_open {
            Some(open) => hue_header.open(Some(open)),
            None => hue_header,
        };
        hue_header.show(ui, |ui| {
            egui::Grid::new("hsl_hue_grid")
                .num_columns(2)
                .spacing([8.0, 2.0])
                .show(ui, |ui| {
                    for (i, name) in HSL_BAND_NAMES.iter().enumerate() {
                        ui.label(*name);
                        changed |= ui
                            .add(
                                egui::Slider::new(&mut self.hue[i], -180.0..=180.0)
                                    .text("°")
                                    .step_by(1.0),
                            )
                            .changed();
                        ui.end_row();
                    }
                });
        });

        let sat_header = egui::CollapsingHeader::new("Saturation").id_salt("hsl_sat");
        let sat_header = match ctx.force_open {
            Some(open) => sat_header.open(Some(open)),
            None => sat_header,
        };
        sat_header.show(ui, |ui| {
            egui::Grid::new("hsl_sat_grid")
                .num_columns(2)
                .spacing([8.0, 2.0])
                .show(ui, |ui| {
                    for (i, name) in HSL_BAND_NAMES.iter().enumerate() {
                        ui.label(*name);
                        changed |= ui
                            .add(
                                egui::Slider::new(&mut self.saturation[i], -1.0..=1.0)
                                    .step_by(0.01),
                            )
                            .changed();
                        ui.end_row();
                    }
                });
        });

        let lum_header = egui::CollapsingHeader::new("Luminance").id_salt("hsl_lum");
        let lum_header = match ctx.force_open {
            Some(open) => lum_header.open(Some(open)),
            None => lum_header,
        };
        lum_header.show(ui, |ui| {
            egui::Grid::new("hsl_lum_grid")
                .num_columns(2)
                .spacing([8.0, 2.0])
                .show(ui, |ui| {
                    for (i, name) in HSL_BAND_NAMES.iter().enumerate() {
                        ui.label(*name);
                        changed |= ui
                            .add(
                                egui::Slider::new(&mut self.luminance[i], -0.5..=0.5).step_by(0.01),
                            )
                            .changed();
                        ui.end_row();
                    }
                });
        });

        let mut action = ToolAction::None;
        if changed && ctx.has_image {
            self.preview_active = true;
            action = ToolAction::RequestRender;
        }

        ui.horizontal(|ui| {
            if ui
                .add_enabled(ctx.has_image, egui::Button::new("Apply"))
                .clicked()
            {
                self.preview_active = false;
                action = ToolAction::PushOp(Box::new(HslPanelOp::new(
                    self.hue,
                    self.saturation,
                    self.luminance,
                )));
                self.hue = [0.0; 8];
                self.saturation = [0.0; 8];
                self.luminance = [0.0; 8];
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
                self.hue = [0.0; 8];
                self.saturation = [0.0; 8];
                self.luminance = [0.0; 8];
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
            Some(Box::new(HslPanelOp::new(
                self.hue,
                self.saturation,
                self.luminance,
            )))
        } else {
            None
        }
    }
    fn load_from_op(&mut self, op: &dyn Operation) -> bool {
        if let Some(o) = op.as_any().and_then(|a| a.downcast_ref::<HslPanelOp>()) {
            self.hue = o.hue;
            self.saturation = o.saturation;
            self.luminance = o.luminance;
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
