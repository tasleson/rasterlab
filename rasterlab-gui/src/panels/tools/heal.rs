use std::any::Any;

use egui::{DragValue, Vec2};
use rasterlab_core::ops::{HealOp, HealSpot};
use rasterlab_core::traits::operation::Operation;

use super::tool_trait::{Tool, ToolAction, ToolUiCtx};

pub struct HealTool {
    pub active: bool,
    pub radius: u32,
    pub spots: Vec<HealSpot>,
}

impl HealTool {
    pub fn new() -> Self {
        Self {
            active: false,
            radius: 30,
            spots: Vec::new(),
        }
    }
}

impl Tool for HealTool {
    fn id(&self) -> &'static str {
        "heal"
    }
    fn display_name(&self) -> &'static str {
        "✦  Heal"
    }

    fn render_ui(&mut self, ui: &mut egui::Ui, ctx: &ToolUiCtx<'_>) -> ToolAction {
        egui::Grid::new("heal_grid")
            .num_columns(2)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                ui.label("Radius:");
                ui.add(
                    DragValue::new(&mut self.radius)
                        .speed(1)
                        .range(5_u32..=300_u32),
                );
                ui.end_row();
            });

        let mode_btn_text = if self.active {
            "Stop Painting"
        } else {
            "Start Painting"
        };
        if ui
            .add_enabled(
                ctx.has_image,
                egui::Button::new(mode_btn_text).min_size(Vec2::new(ui.available_width(), 0.0)),
            )
            .clicked()
        {
            self.active = !self.active;
        }

        let mut action = ToolAction::None;
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    ctx.has_image && !self.spots.is_empty(),
                    egui::Button::new("Apply Heal"),
                )
                .clicked()
            {
                let spots = std::mem::take(&mut self.spots);
                self.active = false;
                action = ToolAction::PushOp(Box::new(HealOp::new(spots)));
            }
            if ui
                .add_enabled(!self.spots.is_empty(), egui::Button::new("Clear"))
                .clicked()
            {
                self.spots.clear();
            }
        });

        if self.active {
            ui.label(
                egui::RichText::new(
                    "Click on blemishes to heal them.\nRight-click a spot to remove it.",
                )
                .small()
                .color(egui::Color32::from_gray(140)),
            );
        }
        action
    }

    fn is_preview_active(&self) -> bool {
        !self.spots.is_empty()
    }
    fn cancel_preview(&mut self) {
        self.spots.clear();
        self.active = false;
    }
    fn preview_op(&self) -> Option<Box<dyn Operation>> {
        if !self.spots.is_empty() {
            Some(Box::new(HealOp::new(self.spots.clone())))
        } else {
            None
        }
    }
    fn load_from_op(&mut self, op: &dyn Operation) -> bool {
        if let Some(o) = op.as_any().and_then(|a| a.downcast_ref::<HealOp>()) {
            self.spots = o.spots.clone();
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
