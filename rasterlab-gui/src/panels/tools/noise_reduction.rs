use std::any::Any;

use rasterlab_core::ops::{NoiseReductionOp, NrMethod};
use rasterlab_core::traits::operation::Operation;

use super::tool_trait::{Tool, ToolAction, ToolUiCtx};
use crate::state::EditingTool;

pub struct NoiseReductionTool {
    pub method: NrMethod,
    pub luma: f32,
    pub color: f32,
    pub detail: f32,
    pub preview_active: bool,
}

impl NoiseReductionTool {
    pub fn new() -> Self {
        Self {
            method: NrMethod::Wavelet,
            luma: 0.3,
            color: 0.5,
            detail: 0.5,
            preview_active: false,
        }
    }
}

impl Tool for NoiseReductionTool {
    fn id(&self) -> &'static str {
        "noise_reduction"
    }
    fn display_name(&self) -> &'static str {
        "◉  Noise Reduction"
    }
    fn editing_tool(&self) -> Option<EditingTool> {
        Some(EditingTool::NoiseReduction)
    }

    fn render_ui(&mut self, ui: &mut egui::Ui, ctx: &ToolUiCtx<'_>) -> ToolAction {
        let mut changed = false;
        let old_method = self.method.clone();
        egui::Grid::new("nr_grid")
            .num_columns(2)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                ui.label("Method:");
                egui::ComboBox::from_id_salt("nr_method")
                    .selected_text(match self.method {
                        NrMethod::Wavelet => "Wavelet (fast)",
                        NrMethod::NonLocalMeans => "Non-Local Means",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.method, NrMethod::Wavelet, "Wavelet (fast)");
                        ui.selectable_value(
                            &mut self.method,
                            NrMethod::NonLocalMeans,
                            "Non-Local Means",
                        );
                    });
                ui.end_row();
                if self.method != old_method {
                    changed = true;
                }

                ui.label("Luminance:");
                changed |= ui
                    .add(egui::Slider::new(&mut self.luma, 0.0..=1.0_f32).show_value(true))
                    .changed();
                ui.end_row();

                ui.label("Color:");
                changed |= ui
                    .add(egui::Slider::new(&mut self.color, 0.0..=1.0_f32).show_value(true))
                    .changed();
                ui.end_row();

                ui.label("Detail:");
                changed |= ui
                    .add(egui::Slider::new(&mut self.detail, 0.0..=1.0_f32).show_value(true))
                    .changed();
                ui.end_row();
            });

        if self.method == NrMethod::NonLocalMeans {
            ui.label(
                egui::RichText::new("⚠ NLM is slow on large images (30s+)")
                    .small()
                    .color(egui::Color32::from_rgb(200, 150, 50)),
            );
        }

        if changed && ctx.has_image {
            self.preview_active = true;
            return ToolAction::RequestRender;
        }
        let mut action = ToolAction::None;
        ui.horizontal(|ui| {
            if ui
                .add_enabled(ctx.has_image, egui::Button::new("Apply Noise Reduction"))
                .clicked()
            {
                self.preview_active = false;
                action = ToolAction::PushOp(Box::new(NoiseReductionOp {
                    method: self.method.clone(),
                    luma_strength: self.luma,
                    color_strength: self.color,
                    detail_preservation: self.detail,
                }));
                self.method = NrMethod::Wavelet;
                self.luma = 0.3;
                self.color = 0.5;
                self.detail = 0.5;
            }
            if (self.preview_active || ctx.nr_in_flight)
                && ui
                    .add_enabled(ctx.has_image, egui::Button::new("Cancel"))
                    .clicked()
            {
                self.preview_active = false;
                action = ToolAction::RequestRender;
            }
            if ui.button("Reset").clicked() {
                self.method = NrMethod::Wavelet;
                self.luma = 0.3;
                self.color = 0.5;
                self.detail = 0.5;
                if self.preview_active {
                    self.preview_active = false;
                    action = ToolAction::RequestRender;
                }
            }
        });
        action
    }

    fn is_preview_active(&self) -> bool {
        self.preview_active
    }
    fn cancel_preview(&mut self) {
        self.preview_active = false;
    }
    fn activate_preview(&mut self) {
        self.preview_active = true;
    }
    fn preview_op(&self) -> Option<Box<dyn Operation>> {
        if self.preview_active {
            Some(Box::new(NoiseReductionOp {
                method: self.method.clone(),
                luma_strength: self.luma,
                color_strength: self.color,
                detail_preservation: self.detail,
            }))
        } else {
            None
        }
    }
    fn load_from_op(&mut self, op: &dyn Operation) -> bool {
        if let Some(o) = op
            .as_any()
            .and_then(|a| a.downcast_ref::<NoiseReductionOp>())
        {
            self.method = o.method.clone();
            self.luma = o.luma_strength;
            self.color = o.color_strength;
            self.detail = o.detail_preservation;
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
