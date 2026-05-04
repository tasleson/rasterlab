use std::any::Any;

use egui::DragValue;
use rasterlab_core::ops::CropOp;
use rasterlab_core::traits::operation::Operation;

use super::tool_trait::{Tool, ToolAction, ToolUiCtx};

const ASPECT_LABELS: &[&str] = &["Free", "3:2", "4:3", "1:1", "16:9", "9:16", "Custom"];

pub struct CropTool {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub aspect_idx: usize,
    pub portrait: bool,
    pub custom_ratio: f32,
}

impl CropTool {
    pub fn new() -> Self {
        Self {
            x: 0,
            y: 0,
            w: 100,
            h: 100,
            aspect_idx: 0,
            portrait: false,
            custom_ratio: 1.5,
        }
    }
}

impl Tool for CropTool {
    fn id(&self) -> &'static str {
        "crop"
    }
    fn display_name(&self) -> &'static str {
        "✂  Crop"
    }

    fn render_ui(&mut self, ui: &mut egui::Ui, ctx: &ToolUiCtx<'_>) -> ToolAction {
        egui::Grid::new("crop_grid")
            .num_columns(2)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                ui.label("Aspect:");
                egui::ComboBox::from_id_salt("crop_aspect")
                    .selected_text(ASPECT_LABELS[self.aspect_idx])
                    .show_ui(ui, |ui| {
                        for (i, &label) in ASPECT_LABELS.iter().enumerate() {
                            ui.selectable_value(&mut self.aspect_idx, i, label);
                        }
                    });
                ui.end_row();

                ui.label("Orientation:");
                let orientation_matters = matches!(self.aspect_idx, 1 | 2 | 4);
                let btn = egui::Button::new(if self.portrait {
                    "◫ Portrait"
                } else {
                    "◫ Landscape"
                })
                .small();
                if ui.add_enabled(orientation_matters, btn).clicked() {
                    self.portrait = !self.portrait;
                }
                ui.end_row();

                if self.aspect_idx == 6 {
                    ui.label("Ratio W:H");
                    ui.add(
                        DragValue::new(&mut self.custom_ratio)
                            .speed(0.01)
                            .range(0.1..=20.0_f32),
                    );
                    ui.end_row();
                }

                ui.label("X");
                ui.add(DragValue::new(&mut self.x).speed(1));
                ui.end_row();
                ui.label("Y");
                ui.add(DragValue::new(&mut self.y).speed(1));
                ui.end_row();
                ui.label("W");
                ui.add(DragValue::new(&mut self.w).speed(1).range(1..=u32::MAX));
                ui.end_row();
                ui.label("H");
                ui.add(DragValue::new(&mut self.h).speed(1).range(1..=u32::MAX));
                ui.end_row();
            });
        if ui
            .add_enabled(ctx.has_image, egui::Button::new("Apply Crop"))
            .clicked()
        {
            return ToolAction::PushOp(Box::new(CropOp::new(self.x, self.y, self.w, self.h)));
        }
        ToolAction::None
    }

    fn load_from_op(&mut self, op: &dyn Operation) -> bool {
        if let Some(o) = op.as_any().and_then(|a| a.downcast_ref::<CropOp>()) {
            self.x = o.x;
            self.y = o.y;
            self.w = o.width;
            self.h = o.height;
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
