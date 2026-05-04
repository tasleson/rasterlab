//! Edit-an-existing-op support.  When the user clicks the pencil button on an
//! edit-stack row, we remember which op is being edited and which tool panel
//! section is bound to it.  While a session is active, Apply buttons in that
//! section call `replace_op` instead of `push_op`, and other tools / stack
//! rows are disabled so the user can only adjust the one op under edit.

use rasterlab_core::traits::operation::Operation;

use super::tool_state::ToolState;

/// Which tool panel section is bound to the current edit session.  Also acts
/// as the classifier that decides whether a given op type is editable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditingTool {
    Levels,
    BlackAndWhite,
    BrightnessContrast,
    Saturation,
    Sepia,
    Sharpen,
    ClarityTexture,
    SplitTone,
    Curves,
    Vignette,
    Vibrance,
    HueShift,
    HighlightsShadows,
    ShadowExposure,
    WhiteBalance,
    FauxHdr,
    Grain,
    ColorBalance,
    HslPanel,
    Blur,
    Denoise,
    NoiseReduction,
}

/// Bookkeeping for an active edit session.
#[derive(Debug, Clone, Copy)]
pub struct EditSession {
    pub op_index: usize,
    pub tool: EditingTool,
}

/// Inspect `op` and, if it is one of the editable types, copy its parameters
/// into the matching tool and return the corresponding tool kind.  Returns
/// `None` when the op is not a type we support editing for (geometric ops,
/// file-based ops, etc.).
pub fn load_op_into_tools(op: &dyn Operation, tools: &mut ToolState) -> Option<EditingTool> {
    for tool in tools.tools.iter_mut() {
        if tool.load_from_op(op) {
            return tool.editing_tool();
        }
    }
    None
}
