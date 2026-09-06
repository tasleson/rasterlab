use rasterlab_core::ops::HslPanelOp;

use super::shared::{ParamTool, param_tool_actions};
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

impl ParamTool for HslTool {
    type Op = HslPanelOp;

    fn op(&self) -> HslPanelOp {
        HslPanelOp::new(self.hue, self.saturation, self.luminance)
    }

    fn reset(&mut self) {
        *self = Self::new();
    }

    fn load(&mut self, op: &HslPanelOp) {
        self.hue = op.hue;
        self.saturation = op.saturation;
        self.luminance = op.luminance;
    }

    fn preview_active(&mut self) -> &mut bool {
        &mut self.preview_active
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

        param_tool_actions(ui, ctx, self, changed)
    }
    super::shared::impl_param_tool!();
}
