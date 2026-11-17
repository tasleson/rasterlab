use std::any::Any;

use rasterlab_core::ops::HistogramData;
use rasterlab_core::traits::operation::Operation;

use crate::file_chooser::DialogKind;
use crate::state::edit_session::{EditSession, EditingTool};

/// What the tools panel tells a tool about the application while drawing it.
///
/// Built by the host each frame; a tool only reads it.
#[non_exhaustive]
pub struct ToolUiCtx<'a> {
    /// Whether an image is open. Apply buttons are disabled without one.
    pub has_image: bool,
    /// The edit session in progress, if the user is re-editing a stack entry.
    pub editing: Option<EditSession>,
    /// Histogram of the image as currently rendered.
    pub histogram: Option<&'a HistogramData>,
    /// Path of the open file, if it has one.
    pub last_path: Option<&'a std::path::Path>,
    /// Whether a noise-reduction render is running in the background.
    pub nr_in_flight: bool,
    /// Dimensions of the source image, before any operation.
    pub source_dims: Option<(u32, u32)>,
    /// Full-resolution dimensions of the committed pipeline output, before
    /// any live preview operation is applied.
    pub committed_dims: Option<(u32, u32)>,
    /// One-frame Expand All (`Some(true)`) / Collapse All (`Some(false)`)
    /// request, for tools that draw collapsing sections of their own.
    pub force_open: Option<bool>,
}

/// What a tool asks the host to do after drawing itself for one frame.
pub enum ToolAction {
    /// Nothing changed.
    None,
    /// Re-render, picking up this tool's preview if it has one active.
    RequestRender,
    /// Abandon the render in flight.
    CancelRender,
    /// Commit one operation to the edit stack — or, during an edit session,
    /// replace the entry being edited.
    PushOp(Box<dyn Operation>),
    /// Commit several operations in order.
    PushOps(Vec<Box<dyn Operation>>),
    /// Open one of the host's file dialogs.
    RequestFileDialog(DialogKind),
}

/// One collapsible section of the tools panel.
///
/// The host owns the header, the collapsing state and the edit-session
/// locking; a tool draws its body in [`render_ui`](Tool::render_ui) and
/// reports what it wants done as a [`ToolAction`]. Tools whose panel is a set
/// of widgets over a single operation get most of this from
/// [`ParamTool`](super::shared::ParamTool).
pub trait Tool: Any {
    /// Stable identifier, unique among all tools. Keys the remembered
    /// open/closed state, so changing it forgets the user's choice.
    fn id(&self) -> &'static str;
    /// Header text. By convention an icon, two spaces, then the name; the
    /// name is what the panel is ordered by.
    fn display_name(&self) -> &'static str;
    /// The edit-session identity of this tool, for tools that can re-edit an
    /// operation already on the stack. `None` means the tool only adds.
    ///
    /// Built-in tools return their own [`EditingTool`] variant. A tool
    /// registered through [`AppOptions::extra_tools`](crate::AppOptions) should
    /// leave this alone and implement [`edits_op`](Tool::edits_op) instead:
    /// the default then identifies it as [`EditingTool::Extension`] by its id.
    /// Registration refuses an extra tool that claims any other identity.
    fn editing_tool(&self) -> Option<EditingTool> {
        self.edits_op().map(|_| EditingTool::Extension(self.id()))
    }
    /// The [`Operation::name`] of the operation this tool re-edits, for a tool
    /// the host has no built-in mapping for.
    ///
    /// Naming an operation here is what puts the pencil button on its
    /// edit-stack rows; clicking it hands the operation to
    /// [`load_from_op`](Tool::load_from_op). The operation must also return
    /// `Some` from [`Operation::as_any`], or it cannot be loaded back. Built-in
    /// operation names are resolved first and cannot be claimed.
    fn edits_op(&self) -> Option<&'static str> {
        None
    }
    /// Short "how to use this" text shown when hovering the tool's header.
    ///
    /// Worth writing for tools whose panel alone does not explain them —
    /// anything with a canvas interaction, for instance.
    fn help_text(&self) -> Option<&'static str> {
        None
    }
    /// Draw the body of the section and say what, if anything, should happen.
    fn render_ui(&mut self, ui: &mut egui::Ui, ctx: &ToolUiCtx<'_>) -> ToolAction;
    /// Whether this tool currently has an uncommitted preview on screen.
    fn is_preview_active(&self) -> bool {
        false
    }
    /// Drop the preview without committing it.
    fn cancel_preview(&mut self) {}
    /// Turn the preview on, as happens when an edit session starts.
    fn activate_preview(&mut self) {}
    /// The operation to render on top of the stack while previewing.
    fn preview_op(&self) -> Option<Box<dyn Operation>> {
        None
    }
    /// Copy `op`'s parameters into the widgets so it can be re-edited.
    /// Returns `false` when `op` is not this tool's operation.
    fn load_from_op(&mut self, op: &dyn Operation) -> bool {
        let _ = op;
        false
    }
    /// Downcast hook; return `self`.
    fn as_any(&self) -> &dyn Any;
    /// Downcast hook; return `self`.
    fn as_any_mut(&mut self) -> &mut dyn Any;
}
