use rasterlab_core::ops::deconvolve::{DeconvolveOp, take_last_kernel_viz};
use rasterlab_core::traits::operation::Operation;

use super::tool_trait::{Tool, ToolAction, ToolUiCtx};
use crate::state::EditingTool;

#[derive(Clone, Copy, PartialEq, Eq)]
enum DeblurQuality {
    Fast,
    Balanced,
    Best,
}

impl DeblurQuality {
    fn label(self) -> &'static str {
        match self {
            Self::Fast => "Fast",
            Self::Balanced => "Balanced",
            Self::Best => "Best",
        }
    }

    fn from_iterations(iterations: u32) -> Self {
        match iterations {
            0 | 1 => Self::Fast,
            2 | 3 => Self::Balanced,
            _ => Self::Best,
        }
    }
}

pub struct DeconvolveTool {
    pub kernel_size: u32,
    pub regularization: f32,
    pub noise_power: f32,
    pub edge_threshold: f32,
    pub isd_iterations: u32,
    pub preview_active: bool,
    quality: DeblurQuality,
    kernel_texture: Option<egui::TextureHandle>,
}

impl DeconvolveTool {
    pub fn new() -> Self {
        Self {
            kernel_size: 25,
            regularization: 1.0,
            noise_power: 0.01,
            edge_threshold: 2.0,
            isd_iterations: 3,
            preview_active: false,
            quality: DeblurQuality::Balanced,
            kernel_texture: None,
        }
    }

    fn update_kernel_viz(&mut self, ctx: &egui::Context) {
        if let Some(viz) = take_last_kernel_viz() {
            let w = viz.width as usize;
            let h = viz.height as usize;
            let pixels: Vec<egui::Color32> = viz
                .pixels
                .iter()
                .map(|&v| egui::Color32::from_gray(v))
                .collect();
            let image = egui::ColorImage {
                size: [w, h],
                pixels,
                source_size: egui::vec2(w as f32, h as f32),
            };
            let texture = ctx.load_texture("deblur_kernel", image, egui::TextureOptions::NEAREST);
            self.kernel_texture = Some(texture);
        }
    }

    fn apply_quality_preset(&mut self) {
        match self.quality {
            DeblurQuality::Fast => {
                self.edge_threshold = 1.5;
                self.isd_iterations = 1;
            }
            DeblurQuality::Balanced => {
                self.edge_threshold = 2.0;
                self.isd_iterations = 3;
            }
            DeblurQuality::Best => {
                self.edge_threshold = 3.0;
                self.isd_iterations = 5;
            }
        }
    }
}

impl Tool for DeconvolveTool {
    fn id(&self) -> &'static str {
        "deconvolve"
    }
    fn display_name(&self) -> &'static str {
        "⟐  Deblur (Motion)"
    }
    fn editing_tool(&self) -> Option<EditingTool> {
        Some(EditingTool::Deconvolve)
    }

    fn render_ui(&mut self, ui: &mut egui::Ui, ctx: &ToolUiCtx<'_>) -> ToolAction {
        self.update_kernel_viz(ui.ctx());

        let mut changed = false;

        ui.add_enabled_ui(!ctx.deconvolve_in_flight, |ui| {
            egui::Grid::new("deconvolve_grid")
                .num_columns(2)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    ui.label("Kernel size");
                    let mut ks = self.kernel_size as i32;
                    if ui
                        .add(
                            egui::DragValue::new(&mut ks)
                                .range(3..=101)
                                .speed(2)
                                .suffix(" px"),
                        )
                        .changed()
                    {
                        self.kernel_size = (ks as u32) | 1; // force odd
                        changed = true;
                    }
                    ui.end_row();

                    ui.label("Deblur strength");
                    changed |= ui
                        .add(
                            egui::DragValue::new(&mut self.regularization)
                                .range(0.1..=3.0)
                                .speed(0.05)
                                .max_decimals(2),
                        )
                        .changed();
                    ui.end_row();

                    ui.label("Noise robustness");
                    changed |= ui
                        .add(
                            egui::DragValue::new(&mut self.noise_power)
                                .range(0.001..=0.1)
                                .speed(0.001)
                                .max_decimals(4),
                        )
                        .changed();
                    ui.end_row();

                    ui.label("Quality");
                    let old_quality = self.quality;
                    egui::ComboBox::from_id_salt("deconvolve_quality")
                        .selected_text(self.quality.label())
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.quality,
                                DeblurQuality::Fast,
                                DeblurQuality::Fast.label(),
                            );
                            ui.selectable_value(
                                &mut self.quality,
                                DeblurQuality::Balanced,
                                DeblurQuality::Balanced.label(),
                            );
                            ui.selectable_value(
                                &mut self.quality,
                                DeblurQuality::Best,
                                DeblurQuality::Best.label(),
                            );
                        });
                    if self.quality != old_quality {
                        self.apply_quality_preset();
                        changed = true;
                    }
                    ui.end_row();
                });
        });

        let mut action = ToolAction::None;
        if changed && self.preview_active {
            self.preview_active = false;
            action = ToolAction::RequestRender;
        }

        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    ctx.has_image && !ctx.deconvolve_in_flight,
                    egui::Button::new("Preview"),
                )
                .clicked()
            {
                self.preview_active = true;
                action = ToolAction::RequestRender;
            }
            if ui
                .add_enabled(
                    ctx.has_image && !ctx.deconvolve_in_flight,
                    egui::Button::new("Apply Deblur"),
                )
                .clicked()
            {
                self.preview_active = false;
                action = ToolAction::PushOp(Box::new(DeconvolveOp::new(
                    self.kernel_size,
                    self.regularization,
                    self.noise_power,
                    self.edge_threshold,
                    self.isd_iterations,
                )));
                self.reset_defaults();
            }
            if (self.preview_active || ctx.deconvolve_in_flight)
                && ui
                    .add_enabled(ctx.has_image, egui::Button::new("Cancel"))
                    .clicked()
            {
                self.preview_active = false;
                action = if ctx.deconvolve_in_flight {
                    ToolAction::CancelRender
                } else {
                    ToolAction::RequestRender
                };
            }
            if ui
                .add_enabled(!ctx.deconvolve_in_flight, egui::Button::new("Reset"))
                .clicked()
            {
                let request_render = self.preview_active;
                self.reset_defaults();
                if request_render {
                    action = ToolAction::RequestRender;
                }
            }
        });

        // Show estimated kernel visualisation
        if let Some(tex) = &self.kernel_texture {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("Estimated kernel")
                    .small()
                    .color(egui::Color32::from_gray(160)),
            );
            let display_size = 64.0;
            ui.image(egui::load::SizedTexture::new(
                tex.id(),
                egui::vec2(display_size, display_size),
            ));
        }

        action
    }

    super::shared::impl_preview_tool!(tool => DeconvolveOp::new(
        tool.kernel_size,
        tool.regularization,
        tool.noise_power,
        tool.edge_threshold,
        tool.isd_iterations
    ));

    fn load_from_op(&mut self, op: &dyn Operation) -> bool {
        if let Some(o) = op.as_any().and_then(|a| a.downcast_ref::<DeconvolveOp>()) {
            self.kernel_size = o.kernel_size;
            self.regularization = o.regularization;
            self.noise_power = o.noise_power;
            self.edge_threshold = o.edge_threshold;
            self.isd_iterations = o.isd_iterations;
            self.quality = DeblurQuality::from_iterations(o.isd_iterations);
            true
        } else {
            false
        }
    }
}

impl DeconvolveTool {
    fn reset_defaults(&mut self) {
        self.kernel_size = 25;
        self.regularization = 1.0;
        self.noise_power = 0.01;
        self.quality = DeblurQuality::Balanced;
        self.apply_quality_preset();
        self.kernel_texture = None;
    }
}
