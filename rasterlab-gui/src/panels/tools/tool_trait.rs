use std::any::Any;

use rasterlab_core::ops::HistogramData;
use rasterlab_core::traits::operation::Operation;

use crate::file_chooser::DialogKind;
use crate::state::edit_session::{EditSession, EditingTool};

#[allow(dead_code)]
pub struct ToolUiCtx<'a> {
    pub has_image: bool,
    pub editing: Option<EditSession>,
    pub histogram: Option<&'a HistogramData>,
    pub last_path: Option<&'a std::path::Path>,
    pub nr_in_flight: bool,
    pub deconvolve_in_flight: bool,
    pub source_dims: Option<(u32, u32)>,
    pub rendered_dims: Option<(u32, u32)>,
    pub rendered_scale: f32,
    pub force_open: Option<bool>,
}

pub enum ToolAction {
    None,
    RequestRender,
    CancelRender,
    PushOp(Box<dyn Operation>),
    PushOps(Vec<Box<dyn Operation>>),
    RequestFileDialog(DialogKind),
}

pub trait Tool: Any {
    fn id(&self) -> &'static str;
    fn display_name(&self) -> &'static str;
    fn editing_tool(&self) -> Option<EditingTool> {
        None
    }
    fn render_ui(&mut self, ui: &mut egui::Ui, ctx: &ToolUiCtx<'_>) -> ToolAction;
    fn is_preview_active(&self) -> bool {
        false
    }
    fn cancel_preview(&mut self) {}
    fn activate_preview(&mut self) {}
    fn preview_op(&self) -> Option<Box<dyn Operation>> {
        None
    }
    fn load_from_op(&mut self, op: &dyn Operation) -> bool {
        let _ = op;
        false
    }
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}
