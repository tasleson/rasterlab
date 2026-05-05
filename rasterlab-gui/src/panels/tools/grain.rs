use std::any::Any;

use egui::DragValue;
use rasterlab_core::ops::GrainOp;
use rasterlab_core::traits::operation::Operation;

use super::tool_trait::{Tool, ToolAction, ToolUiCtx};
use crate::state::EditingTool;

const GRAIN_PRESETS: &[(&str, f32, f32)] = &[
    ("T-Max 100", 0.03, 1.0),
    ("Gold 200", 0.05, 1.3),
    ("Portra 400", 0.06, 1.5),
    ("Pro 400H", 0.07, 1.2),
    ("HP5 400", 0.09, 1.6),
    ("Tri-X 400", 0.10, 1.8),
    ("Superia 400", 0.08, 1.5),
    ("Portra 800", 0.12, 2.0),
    ("Neopan 1600", 0.18, 2.5),
    ("T-Max 3200", 0.25, 3.0),
    ("Heavy Push", 0.35, 3.5),
];

pub struct GrainTool {
    pub strength: f32,
    pub size: f32,
    pub seed: u64,
    pub preview_active: bool,
}

impl GrainTool {
    pub fn new() -> Self {
        Self {
            strength: 0.1,
            size: 1.8,
            seed: 42,
            preview_active: false,
        }
    }
}

impl Tool for GrainTool {
    fn id(&self) -> &'static str {
        "grain"
    }
    fn display_name(&self) -> &'static str {
        "⣿  Grain"
    }
    fn editing_tool(&self) -> Option<EditingTool> {
        Some(EditingTool::Grain)
    }

    fn render_ui(&mut self, ui: &mut egui::Ui, ctx: &ToolUiCtx<'_>) -> ToolAction {
        ui.label("Film presets:");
        let mut preset_changed = false;
        ui.horizontal_wrapped(|ui| {
            for &(label, strength, size) in GRAIN_PRESETS {
                if ui.small_button(label).clicked() && ctx.has_image {
                    self.strength = strength;
                    self.size = size;
                    preset_changed = true;
                }
            }
        });
        ui.add_space(2.0);

        let mut changed = false;
        egui::Grid::new("grain_grid")
            .num_columns(2)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                ui.label("Strength");
                changed |= ui
                    .add(egui::Slider::new(&mut self.strength, 0.0..=1.0).step_by(0.01))
                    .changed();
                ui.end_row();
                ui.label("Size");
                changed |= ui
                    .add(egui::Slider::new(&mut self.size, 1.0..=32.0).step_by(0.1))
                    .changed();
                ui.end_row();
                ui.label("Seed");
                changed |= ui.add(DragValue::new(&mut self.seed)).changed();
                ui.end_row();
            });
        let mut action = ToolAction::None;
        if (changed || preset_changed) && ctx.has_image {
            self.preview_active = true;
            action = ToolAction::RequestRender;
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(ctx.has_image, egui::Button::new("Apply Grain"))
                .clicked()
            {
                self.preview_active = false;
                action =
                    ToolAction::PushOp(Box::new(GrainOp::new(self.strength, self.size, self.seed)));
                self.strength = 0.1;
                self.size = 1.8;
                self.seed = 42;
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
                self.strength = 0.1;
                self.size = 1.8;
                self.seed = 42;
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
            Some(Box::new(GrainOp::new(self.strength, self.size, self.seed)))
        } else {
            None
        }
    }
    fn load_from_op(&mut self, op: &dyn Operation) -> bool {
        if let Some(o) = op.as_any().and_then(|a| a.downcast_ref::<GrainOp>()) {
            self.strength = o.strength;
            self.size = o.size;
            self.seed = o.seed;
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
