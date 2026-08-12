use std::any::Any;

use egui::{Color32, RichText};
use rasterlab_core::ops::{ChannelLevelsOp, ChannelRange};
use rasterlab_core::traits::operation::Operation;

use super::tool_trait::{Tool, ToolAction, ToolUiCtx};
use crate::state::EditingTool;

pub struct ChannelLevelsTool {
    pub red: ChannelRange,
    pub green: ChannelRange,
    pub blue: ChannelRange,
    pub preview_active: bool,
}

impl ChannelLevelsTool {
    pub fn new() -> Self {
        Self {
            red: ChannelRange::identity(),
            green: ChannelRange::identity(),
            blue: ChannelRange::identity(),
            preview_active: false,
        }
    }

    fn operation(&self) -> ChannelLevelsOp {
        ChannelLevelsOp::new(self.red, self.green, self.blue)
    }

    fn reset(&mut self) {
        self.red = ChannelRange::identity();
        self.green = ChannelRange::identity();
        self.blue = ChannelRange::identity();
    }
}

impl Tool for ChannelLevelsTool {
    fn id(&self) -> &'static str {
        "channel_levels"
    }

    fn display_name(&self) -> &'static str {
        "▥  Channel Levels"
    }

    fn editing_tool(&self) -> Option<EditingTool> {
        Some(EditingTool::ChannelLevels)
    }

    fn render_ui(&mut self, ui: &mut egui::Ui, ctx: &ToolUiCtx<'_>) -> ToolAction {
        ui.label(
            RichText::new("Set independent black, gamma, and white points for each channel.")
                .small()
                .color(Color32::from_gray(170)),
        );

        let mut changed = false;
        channel_controls(
            ui,
            "channel_levels_red",
            "Red",
            Color32::from_rgb(230, 90, 90),
            &mut self.red,
            &mut changed,
        );
        channel_controls(
            ui,
            "channel_levels_green",
            "Green",
            Color32::from_rgb(90, 205, 110),
            &mut self.green,
            &mut changed,
        );
        channel_controls(
            ui,
            "channel_levels_blue",
            "Blue",
            Color32::from_rgb(100, 145, 240),
            &mut self.blue,
            &mut changed,
        );

        ui.label(
            RichText::new("White values above 1.0 attenuate that channel.")
                .small()
                .color(Color32::from_gray(150)),
        );

        let mut action = ToolAction::None;
        if changed && ctx.has_image {
            self.preview_active = true;
            action = ToolAction::RequestRender;
        }

        ui.horizontal(|ui| {
            if ui
                .add_enabled(ctx.has_image, egui::Button::new("Apply Channel Levels"))
                .clicked()
            {
                self.preview_active = false;
                action = ToolAction::PushOp(Box::new(self.operation()));
                self.reset();
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
                self.reset();
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
        self.preview_active
            .then(|| Box::new(self.operation()) as Box<dyn Operation>)
    }

    fn load_from_op(&mut self, op: &dyn Operation) -> bool {
        let Some(op) = op
            .as_any()
            .and_then(|op| op.downcast_ref::<ChannelLevelsOp>())
        else {
            return false;
        };

        self.red = op.red;
        self.green = op.green;
        self.blue = op.blue;
        true
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

fn channel_controls(
    ui: &mut egui::Ui,
    grid_id: &'static str,
    label: &str,
    color: Color32,
    range: &mut ChannelRange,
    changed: &mut bool,
) {
    ui.add_space(4.0);
    ui.label(RichText::new(label).color(color).strong());

    egui::Grid::new(grid_id)
        .num_columns(2)
        .spacing([8.0, 4.0])
        .show(ui, |ui| {
            ui.label("Black");
            if ui
                .add(
                    egui::Slider::new(&mut range.black, 0.0..=1.0)
                        .clamping(egui::SliderClamping::Always)
                        .step_by(0.001),
                )
                .changed()
            {
                if range.black >= range.white {
                    range.black = (range.white - 0.001).max(0.0);
                }
                *changed = true;
            }
            ui.end_row();

            ui.label("Gamma");
            *changed |= ui
                .add(
                    egui::Slider::new(&mut range.gamma, 0.01..=10.0)
                        .clamping(egui::SliderClamping::Always)
                        .step_by(0.01)
                        .logarithmic(true),
                )
                .changed();
            ui.end_row();

            ui.label("White");
            if ui
                .add(
                    egui::Slider::new(&mut range.white, 0.0..=4.0)
                        .clamping(egui::SliderClamping::Always)
                        .step_by(0.001),
                )
                .changed()
            {
                if range.white <= range.black {
                    range.white = (range.black + 0.001).min(4.0);
                }
                *changed = true;
            }
            ui.end_row();
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use rasterlab_core::ops::LevelsOp;

    fn channel_op() -> ChannelLevelsOp {
        ChannelLevelsOp::new(
            ChannelRange::new(0.10, 1.20, 0.80),
            ChannelRange::new(0.05, 2.25, 1.10),
            ChannelRange::new(0.02, 0.90, 1.30),
        )
    }

    #[test]
    fn loads_all_channel_parameters_for_editing() {
        let op = channel_op();
        let mut tool = ChannelLevelsTool::new();

        assert!(tool.load_from_op(&op));
        assert_eq!(tool.red, op.red);
        assert_eq!(tool.green, op.green);
        assert_eq!(tool.blue, op.blue);
    }

    #[test]
    fn preview_preserves_white_values_above_full_scale() {
        let op = channel_op();
        let mut tool = ChannelLevelsTool::new();
        assert!(tool.load_from_op(&op));
        tool.activate_preview();

        let preview = tool.preview_op().unwrap();
        let preview = preview
            .as_any()
            .and_then(|op| op.downcast_ref::<ChannelLevelsOp>())
            .unwrap();
        assert_eq!(preview.green.white, 2.25);
    }

    #[test]
    fn rejects_combined_levels_operations() {
        let mut tool = ChannelLevelsTool::new();
        assert!(!tool.load_from_op(&LevelsOp::new(0.1, 0.9, 1.2)));
    }
}
