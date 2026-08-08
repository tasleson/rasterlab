use egui::Ui;
use rasterlab_core::ops::CropOp;
use rasterlab_core::traits::operation::Operation;

use super::tool_trait::{ToolAction, ToolUiCtx};
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

/// One source frame in a multi-image tool's list.
///
/// The op stores a plain path, but a managed-library photo is stored under the
/// Blake3 of its content, so a list built from path file names shows the user
/// 64 hex characters where they expect their own file names — and no way to
/// tell whether the frames are the ones they picked, in the order they picked
/// them. The name is resolved once, when the frame is added, and travels with
/// the path from there.
#[derive(Clone)]
pub struct StackFrame {
    pub path: String,
    label: String,
}

impl StackFrame {
    pub fn new(path: impl Into<String>) -> Self {
        let path = path.into();
        Self {
            label: frame_label(&path),
            path,
        }
    }
}

/// Display name for a frame: its file name, or — for a `.rlab` container —
/// the name of the image it was made from. Falls back to the file name when
/// the container records no source or cannot be read.
fn frame_label(path: &str) -> String {
    let path = std::path::Path::new(path);
    if rasterlab_core::project::is_rlab_path(path)
        && let Ok(Some(name)) = rasterlab_core::project::read_original_filename(path)
    {
        return name;
    }
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

/// The paths to hand an op, in list order.
pub(super) fn frame_paths(frames: &[StackFrame]) -> Vec<String> {
    frames.iter().map(|f| f.path.clone()).collect()
}

pub(super) fn path_list_ui(ui: &mut Ui, frames: &[StackFrame], id_salt: &str) -> Option<usize> {
    let mut remove_idx: Option<usize> = None;
    egui::ScrollArea::vertical()
        .max_height(120.0)
        .id_salt(id_salt)
        .show(ui, |ui| {
            for (i, frame) in frames.iter().enumerate() {
                ui.horizontal(|ui| {
                    // Two library photos can share an imported name, so the
                    // full path stays reachable on hover.
                    ui.label(format!("{}. {}", i + 1, frame.label))
                        .on_hover_text(&frame.path);
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
    frames: &mut Vec<StackFrame>,
    preview_active: &mut bool,
    apply_label: &str,
    build_op: F,
) -> ToolAction
where
    F: FnOnce(Vec<String>) -> Box<dyn Operation>,
{
    let mut action = ToolAction::None;
    ui.horizontal(|ui| {
        let ready = frames.len() >= 2;
        if ui
            .add_enabled(has_image && ready, egui::Button::new(apply_label))
            .clicked()
        {
            *preview_active = false;
            action = ToolAction::PushOp(build_op(frame_paths(frames)));
            frames.clear();
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
            frames.clear();
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

/// Apply button for geometric tools that pair their operation with an
/// auto-crop.
///
/// The crop is derived from [`ToolUiCtx::committed_dims`], which is `None`
/// while the committed pipeline output is not cached (a render is in flight).
/// Committing then would push the geometric op *without* its crop, silently
/// ignoring the checkbox — so the button is disabled until the dimensions are
/// known.  Pass `needs_crop = false` when the pending apply does not use the
/// crop; the button then only depends on there being an image.
pub(super) fn apply_button(
    ui: &mut Ui,
    ctx: &ToolUiCtx<'_>,
    label: &str,
    needs_crop: bool,
) -> bool {
    let awaiting_dims = needs_crop && ctx.committed_dims.is_none();
    let response = ui.add_enabled(ctx.has_image && !awaiting_dims, egui::Button::new(label));
    if awaiting_dims {
        response
            .on_disabled_hover_text("Waiting for the current render to finish…")
            .clicked()
    } else {
        response.clicked()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rasterlab_core::{
        library_meta::LibraryMeta,
        pipeline::PipelineState,
        project::{RlabFile, RlabMeta, SavedCopy},
    };

    /// A library photo is stored under the Blake3 of its content, so labelling
    /// frames with their path's file name shows the user 64 hex characters and
    /// no way to check that the stack holds the photos they picked.
    #[test]
    fn library_frames_are_labelled_with_the_imported_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(format!("{}.rlab", "9f".repeat(32)));

        let mut rlab = RlabFile::new(
            RlabMeta::new("test", Some("/cards/DCIM/DSC_0042.NEF"), 4, 4),
            vec![0u8; 8],
            vec![SavedCopy {
                name: "Copy 1".into(),
                pipeline_state: PipelineState {
                    entries: Vec::new(),
                    cursor: 0,
                },
            }],
            0,
            None,
        );
        rlab.set_lmta(Some(LibraryMeta {
            original_filename: Some("DSC_0042.NEF".to_owned()),
            ..Default::default()
        }));
        rlab.write_v5(&path).unwrap();

        assert_eq!(
            StackFrame::new(path.to_string_lossy()).label,
            "DSC_0042.NEF",
        );
    }

    #[test]
    fn other_frames_keep_their_file_name() {
        assert_eq!(
            StackFrame::new("/photos/DSC_0001.NEF").label,
            "DSC_0001.NEF",
        );
        // A container that cannot be read still gets a stable label rather
        // than an empty row in the list.
        assert_eq!(StackFrame::new("/gone/abc123.rlab").label, "abc123.rlab");
    }
}
