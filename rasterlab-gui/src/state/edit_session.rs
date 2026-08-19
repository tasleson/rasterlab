//! Edit-an-existing-op support.  When the user clicks the pencil button on an
//! edit-stack row, we remember which op is being edited and which tool panel
//! section is bound to it.  While a session is active, Apply buttons in that
//! section call `replace_op` instead of `push_op`, and other tools / stack
//! rows are disabled so the user can only adjust the one op under edit.

use rasterlab_core::{ops::MaskedOp, traits::operation::Operation};

use super::tool_state::ToolState;

/// Which tool panel section is bound to the current edit session.  Also acts
/// as the classifier that decides whether a given op type is editable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditingTool {
    AirplaneWindow,
    ChannelLevels,
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
    ColorSpace,
    Crop,
    HslPanel,
    Blur,
    Denoise,
    FocusStack,
    HdrMerge,
    Heal,
    Lut,
    NoiseReduction,
    LocalLaplacian,
    Panorama,
    Perspective,
    Resize,
    Rotate,
    SprocketFilm,
}

/// Bookkeeping for an active edit session.
#[derive(Debug, Clone, Copy)]
pub struct EditSession {
    pub op_index: usize,
    pub tool: EditingTool,
    pub was_enabled: bool,
}

/// Return the tool responsible for editing `op` without mutating any widget
/// state. This is the single source of truth used by both the stack's pencil
/// button and the loader that starts an edit session.
pub fn editing_tool_for_op(op: &dyn Operation) -> Option<EditingTool> {
    if let Some(masked) = op.as_any().and_then(|any| any.downcast_ref::<MaskedOp>()) {
        return editing_tool_for_op(masked.inner.as_ref());
    }
    op.as_any()?;
    Some(match op.name() {
        "airplane_window" => EditingTool::AirplaneWindow,
        "channel_levels" => EditingTool::ChannelLevels,
        "levels" => EditingTool::Levels,
        "black_and_white" => EditingTool::BlackAndWhite,
        "brightness_contrast" => EditingTool::BrightnessContrast,
        "saturation" => EditingTool::Saturation,
        "sepia" => EditingTool::Sepia,
        "sharpen" => EditingTool::Sharpen,
        "clarity_texture" => EditingTool::ClarityTexture,
        "split_tone" => EditingTool::SplitTone,
        "curves" => EditingTool::Curves,
        "vignette" => EditingTool::Vignette,
        "vibrance" => EditingTool::Vibrance,
        "hue_shift" => EditingTool::HueShift,
        "highlights_shadows" => EditingTool::HighlightsShadows,
        "shadow_exposure" => EditingTool::ShadowExposure,
        "white_balance" => EditingTool::WhiteBalance,
        "faux_hdr" => EditingTool::FauxHdr,
        "grain" => EditingTool::Grain,
        "color_balance" => EditingTool::ColorBalance,
        "color_space" => EditingTool::ColorSpace,
        "hsl_panel" => EditingTool::HslPanel,
        "blur" => EditingTool::Blur,
        "denoise" => EditingTool::Denoise,
        "noise_reduction" => EditingTool::NoiseReduction,
        "local_laplacian" => EditingTool::LocalLaplacian,
        "crop" => EditingTool::Crop,
        "focus_stack" => EditingTool::FocusStack,
        "hdr_merge" => EditingTool::HdrMerge,
        "heal" => EditingTool::Heal,
        "lut" => EditingTool::Lut,
        "panorama" => EditingTool::Panorama,
        "perspective" => EditingTool::Perspective,
        "resize" => EditingTool::Resize,
        "rotate" | "flip" => EditingTool::Rotate,
        "sprocket_film" => EditingTool::SprocketFilm,
        _ => return None,
    })
}

/// Inspect `op` and, if it is one of the editable types, copy its parameters
/// into the matching tool and return the corresponding tool kind.  Returns
/// `None` when the op is not a type we support editing (for example a plugin
/// operation without a matching built-in tool).
pub fn load_op_into_tools(op: &dyn Operation, tools: &mut ToolState) -> Option<EditingTool> {
    let op = op
        .as_any()
        .and_then(|any| any.downcast_ref::<MaskedOp>())
        .map_or(op, |masked| masked.inner.as_ref());
    let expected = editing_tool_for_op(op)?;
    if expected == EditingTool::SprocketFilm {
        let sprocket = op
            .as_any()
            .and_then(|any| any.downcast_ref::<rasterlab_core::ops::SprocketFilmOp>())?;
        tools.sprocket_film_stock = Some(sprocket.stock);
        tools.sprocket_edit_op = Some(sprocket.clone());
        return Some(expected);
    }
    for tool in tools.tools.iter_mut() {
        // Only tools that can host an edit session are consulted: a tool with
        // no `EditingTool` cannot be bound to one, so loading `op` into it
        // would mutate its widgets for nothing.
        if tool.editing_tool() == Some(expected) && tool.load_from_op(op) {
            return Some(expected);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use rasterlab_core::ops::{
        ChannelLevelsOp, ChannelRange, CropOp, FlipOp, FocusStackOp, HdrMergeOp, HealOp,
        LinearMask, LutOp, MaskShape, MaskedOp, PanoramaOp, PerspectiveOp, ResampleMode, ResizeOp,
        RotateOp, SprocketFilmOp,
    };

    use super::*;
    use crate::panels::tools::channel_levels::ChannelLevelsTool;

    #[test]
    fn channel_levels_is_routed_to_its_editor() {
        let op = ChannelLevelsOp::new(
            ChannelRange::new(0.10, 1.50, 0.80),
            ChannelRange::new(0.05, 2.25, 1.10),
            ChannelRange::new(0.02, 0.90, 1.30),
        );
        let mut tools = ToolState::new();

        assert_eq!(
            load_op_into_tools(&op, &mut tools),
            Some(EditingTool::ChannelLevels)
        );

        let editor = tools.find::<ChannelLevelsTool>().unwrap();
        assert_eq!(editor.red, op.red);
        assert_eq!(editor.green, op.green);
        assert_eq!(editor.blue, op.blue);
    }

    #[test]
    fn every_previously_unwired_operation_is_routed_to_an_editor() {
        let cases: Vec<(Box<dyn Operation>, EditingTool)> = vec![
            (Box::new(CropOp::new(1, 2, 30, 40)), EditingTool::Crop),
            (
                Box::new(ResizeOp::new(640, 480, ResampleMode::Bilinear)),
                EditingTool::Resize,
            ),
            (Box::new(RotateOp::cw90()), EditingTool::Rotate),
            (Box::new(FlipOp::horizontal()), EditingTool::Rotate),
            (
                Box::new(PerspectiveOp::new([
                    [0.10, 0.0],
                    [-0.10, 0.0],
                    [0.0, 0.0],
                    [0.0, 0.0],
                ])),
                EditingTool::Perspective,
            ),
            (Box::new(HealOp::default()), EditingTool::Heal),
            (Box::new(LutOp::identity(2)), EditingTool::Lut),
            (
                Box::new(PanoramaOp::new(vec!["a".into(), "b".into()], 42)),
                EditingTool::Panorama,
            ),
            (
                Box::new(FocusStackOp::new(vec!["a".into(), "b".into()])),
                EditingTool::FocusStack,
            ),
            (
                Box::new(HdrMergeOp::new(vec!["a".into(), "b".into()])),
                EditingTool::HdrMerge,
            ),
            (
                Box::new(SprocketFilmOp::default()),
                EditingTool::SprocketFilm,
            ),
        ];

        for (op, expected) in cases {
            assert!(
                op.as_any().is_some(),
                "{} would not show an edit control in the stack",
                op.name()
            );
            assert_eq!(editing_tool_for_op(op.as_ref()), Some(expected));
            let mut tools = ToolState::new();
            assert_eq!(
                load_op_into_tools(op.as_ref(), &mut tools),
                Some(expected),
                "{} is not wired to its tool",
                op.name()
            );
        }
    }

    #[test]
    fn masked_operation_routes_through_its_inner_editor() {
        let op = MaskedOp {
            inner: Box::new(CropOp::new(3, 4, 50, 60)),
            mask: MaskShape::Linear(LinearMask {
                cx: 0.5,
                cy: 0.5,
                angle_deg: 90.0,
                feather: 0.5,
                invert: false,
            }),
        };
        let mut tools = ToolState::new();

        assert_eq!(load_op_into_tools(&op, &mut tools), Some(EditingTool::Crop));
        assert_eq!(editing_tool_for_op(&op), Some(EditingTool::Crop));
    }
}
