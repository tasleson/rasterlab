use egui::Ui;
use rasterlab_core::ops::CropOp;
use rasterlab_core::traits::operation::Operation;

use super::tool_trait::ToolAction;
use crate::state::{EditSession, EditingTool};

macro_rules! impl_preview_tool {
    ($tool:ident => $op:expr) => {
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
                let $tool = self;
                Some(Box::new($op))
            } else {
                None
            }
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
    };
}

pub(crate) use impl_preview_tool;

macro_rules! impl_preview_controls {
    () => {
        fn is_preview_active(&self) -> bool {
            self.preview_active
        }
        fn cancel_preview(&mut self) {
            self.preview_active = false;
        }
        fn activate_preview(&mut self) {
            self.preview_active = true;
        }
    };
}

pub(crate) use impl_preview_controls;

/// Wrap `CollapsingHeader::new` so every header in this panel honours the
/// one-frame force-open flag that drives Expand-All / Collapse-All.
pub(super) fn header(
    force: Option<bool>,
    title: impl Into<egui::WidgetText>,
) -> egui::CollapsingHeader {
    let h = egui::CollapsingHeader::new(title);
    match force {
        Some(open) => h.open(Some(open)),
        None => h,
    }
}

/// Like `header`, but when `editing` matches `this_tool` the title is rendered
/// bold and the section is forced open so the user can immediately find the
/// tool they just started editing from the Edit Stack.
pub(super) fn header_for_tool(
    force: Option<bool>,
    title: &str,
    editing: Option<EditSession>,
    this_tool: EditingTool,
) -> egui::CollapsingHeader {
    let is_active = editing.is_some_and(|s| s.tool == this_tool);
    let widget_text: egui::WidgetText = if is_active {
        egui::RichText::new(title).strong().into()
    } else {
        title.into()
    };
    let h = egui::CollapsingHeader::new(widget_text);
    let effective_force = if is_active { Some(true) } else { force };
    match effective_force {
        Some(open) => h.open(Some(open)),
        None => h,
    }
}

pub(super) fn path_list_ui(ui: &mut Ui, paths: &[String], id_salt: &str) -> Option<usize> {
    let mut remove_idx: Option<usize> = None;
    egui::ScrollArea::vertical()
        .max_height(120.0)
        .id_salt(id_salt)
        .show(ui, |ui| {
            for (i, path) in paths.iter().enumerate() {
                ui.horizontal(|ui| {
                    let name = std::path::Path::new(path)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.clone());
                    ui.label(format!("{}. {}", i + 1, name));
                    if ui.small_button("✕").clicked() {
                        remove_idx = Some(i);
                    }
                });
            }
        });
    remove_idx
}

pub(super) enum PreviewButtonAction {
    Apply,
    Cancel,
    Reset { request_render: bool },
}

pub(super) fn preview_buttons(
    ui: &mut Ui,
    has_image: bool,
    preview_active: &mut bool,
    apply_label: &str,
) -> Option<PreviewButtonAction> {
    let mut action = None;
    ui.horizontal(|ui| {
        if ui
            .add_enabled(has_image, egui::Button::new(apply_label))
            .clicked()
        {
            *preview_active = false;
            action = Some(PreviewButtonAction::Apply);
        }
        if *preview_active
            && ui
                .add_enabled(has_image, egui::Button::new("Cancel"))
                .clicked()
        {
            *preview_active = false;
            action = Some(PreviewButtonAction::Cancel);
        }
        if ui.button("Reset").clicked() {
            let request_render = *preview_active;
            *preview_active = false;
            action = Some(PreviewButtonAction::Reset { request_render });
        }
    });
    action
}

pub(super) fn path_stack_buttons<F>(
    ui: &mut Ui,
    has_image: bool,
    paths: &mut Vec<String>,
    preview_active: &mut bool,
    apply_label: &str,
    build_op: F,
) -> ToolAction
where
    F: FnOnce(Vec<String>) -> Box<dyn Operation>,
{
    let mut action = ToolAction::None;
    ui.horizontal(|ui| {
        let ready = paths.len() >= 2;
        if ui
            .add_enabled(has_image && ready, egui::Button::new(apply_label))
            .clicked()
        {
            *preview_active = false;
            action = ToolAction::PushOp(build_op(paths.clone()));
            paths.clear();
        }
        if *preview_active
            && ui
                .add_enabled(has_image, egui::Button::new("Cancel"))
                .clicked()
        {
            *preview_active = false;
            action = ToolAction::RequestRender;
        }
        if ui.button("Reset").clicked() {
            paths.clear();
            if *preview_active {
                *preview_active = false;
                action = ToolAction::RequestRender;
            }
        }
    });
    action
}

pub(super) fn straighten_crop_op(w: u32, h: u32, angle_deg: f32) -> CropOp {
    let theta = angle_deg.to_radians().abs();
    let cos_t = theta.cos();
    let sin_t = theta.sin();
    let wf = w as f32;
    let hf = h as f32;
    let r = wf / hf;

    let b = f32::min(
        wf / (2.0 * (r * cos_t + sin_t)),
        hf / (2.0 * (r * sin_t + cos_t)),
    );
    let a = r * b;

    let inner_w = (2.0 * a).floor() as u32;
    let inner_h = (2.0 * b).floor() as u32;

    let rot_w = (wf * cos_t + hf * sin_t).ceil() as u32;
    let rot_h = (wf * sin_t + hf * cos_t).ceil() as u32;

    let x = (rot_w.saturating_sub(inner_w)) / 2;
    let y = (rot_h.saturating_sub(inner_h)) / 2;

    CropOp::new(x, y, inner_w.max(1), inner_h.max(1))
}
