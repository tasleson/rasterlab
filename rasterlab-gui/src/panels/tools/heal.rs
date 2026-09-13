use std::any::Any;

use egui::{DragValue, Vec2};
use rasterlab_core::ops::{HealOp, HealSpot};
use rasterlab_core::traits::operation::Operation;

use super::tool_trait::{Tool, ToolAction, ToolUiCtx};
use crate::state::EditingTool;

const HELP: &str = "\
Heal covers a blemish with clean pixels copied from elsewhere in the photo.

\u{2022} Set Radius so the ring on the canvas just covers the spot. The ring \
follows the pointer, and parks on the image while you adjust the radius.
\u{2022} Press Start Painting, then click each blemish. A matching source patch \
nearby is picked for you.
\u{2022} Drag the green circle to pick a different source, or the red one to move \
the repair \u{2014} the source follows it.
\u{2022} Right-click a circle to drop that spot.
\u{2022} Press Apply Heal to commit every spot as one step.

Works best on even backgrounds \u{2014} sky, skin, a plain wall.";

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
    fn editing_tool(&self) -> Option<EditingTool> {
        Some(EditingTool::Heal)
    }
    fn help_text(&self) -> Option<&'static str> {
        Some(HELP)
    }

    fn activate_preview(&mut self) {}

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
