use rasterlab_core::{
    ops::{FilmStock, LinearMask, MaskShape, RadialMask, SprocketFilmOp},
    traits::operation::Operation,
};

use super::edit_session::EditingTool;
use crate::file_chooser::DialogKind;
use crate::panels::tools::{
    airplane_window::AirplaneWindowTool, blur::BlurTool,
    brightness_contrast::BrightnessContrastTool, bw::BwTool, channel_levels::ChannelLevelsTool,
    clarity_texture::ClarityTextureTool, color_balance::ColorBalanceTool,
    color_space::ColorSpaceTool, crop::CropTool, curves::CurvesTool, denoise::DenoiseTool,
    faux_hdr::FauxHdrTool, focus_stack::FocusStackTool, grain::GrainTool, hdr_merge::HdrMergeTool,
    heal::HealTool, highlights_shadows::HighlightsShadowsTool, hsl::HslTool,
    hue_shift::HueShiftTool, intensify_hdr::IntensifyHdrTool, levels::LevelsTool,
    local_laplacian::LocalLaplacianTool, lut::LutTool, noise_reduction::NoiseReductionTool,
    panorama::PanoramaTool, perspective::PerspectiveTool, resize::ResizeTool, rotate::RotateTool,
    saturation::SaturationTool, sepia::SepiaTool, shadow_exposure::ShadowExposureTool,
    sharpen::SharpenTool, split_tone::SplitToneTool, straighten::StraightenTool, tool_trait::Tool,
    vibrance::VibranceTool, vignette::VignetteTool, white_balance::WhiteBalanceTool,
};

/// All tool state: trait-based tools in a Vec, plus masking, export, and dialog fields.
pub struct ToolState {
    pub tools: Vec<Box<dyn Tool>>,

    // ── Looks ─────────────────────────────────────────────────────────────
    /// `None` keeps the original random-stock behaviour.
    pub sprocket_film_stock: Option<FilmStock>,
    /// Original operation and preview state while a Sprocket Film stack row
    /// is being edited through the non-trait Looks panel.
    pub sprocket_edit_op: Option<SprocketFilmOp>,
    pub sprocket_edit_preview_active: bool,
    /// Enables the fixed 2:1 crop overlay used by the sprocket look.
    pub sprocket_crop_active: bool,

    // ── Masking ───────────────────────────────────────────────────────────
    /// 0 = None, 1 = Linear Gradient, 2 = Radial Gradient.
    pub mask_sel: usize,
    pub mask_lin_cx: f32,
    pub mask_lin_cy: f32,
    pub mask_lin_angle: f32,
    pub mask_lin_feather: f32,
    pub mask_lin_invert: bool,
    pub mask_rad_cx: f32,
    pub mask_rad_cy: f32,
    pub mask_rad_radius: f32,
    pub mask_rad_feather: f32,
    pub mask_rad_invert: bool,

    // ── Export settings ───────────────────────────────────────────────────
    pub export_border: crate::panels::export_border::ExportBorderOptions,

    // ── Library batch export dialog ───────────────────────────────────────
    pub export_dialog: crate::panels::export_dialog::ExportDialogState,

    // ── Dialog request flags ─────────────────────────────────────────────
    pub pending_dialog: Option<DialogKind>,

    /// `id()` of a tool the panel should open and scroll into view on the next
    /// frame, then clear. Set when something outside the panel hands a tool its
    /// input (e.g. the library's Focus Stack action) and the user has to be
    /// shown where that landed.
    pub reveal_tool: Option<&'static str>,
}

/// The part of a display name the tools panel is ordered by.
///
/// Display names follow the convention `"<icon>  Name"` — one icon codepoint
/// followed by two ASCII spaces. Leading non-ASCII characters and the
/// whitespace after them are dropped so only the human-readable label is
/// compared.
fn label(display_name: &str) -> &str {
    let s = display_name.trim_start_matches(|c: char| !c.is_ascii());
    s.trim_start()
}

/// Case-insensitive ordering key for a tool's display name.
fn sort_key(display_name: &str) -> String {
    label(display_name).to_lowercase()
}

impl ToolState {
    pub fn new() -> Self {
        Self {
            tools: Self::build_tools(),
            sprocket_film_stock: None,
            sprocket_edit_op: None,
            sprocket_edit_preview_active: false,
            sprocket_crop_active: false,
            mask_sel: 0,
            mask_lin_cx: 0.5,
            mask_lin_cy: 0.5,
            mask_lin_angle: 90.0,
            mask_lin_feather: 0.5,
            mask_lin_invert: false,
            mask_rad_cx: 0.5,
            mask_rad_cy: 0.5,
            mask_rad_radius: 0.3,
            mask_rad_feather: 0.5,
            mask_rad_invert: false,
            export_border: crate::panels::export_border::ExportBorderOptions::default(),
            export_dialog: crate::panels::export_dialog::ExportDialogState::default(),
            pending_dialog: None,
            reveal_tool: None,
        }
    }

    fn build_tools() -> Vec<Box<dyn Tool>> {
        vec![
            Box::new(AirplaneWindowTool::new()),
            Box::new(BwTool::new()),
            Box::new(BlurTool::new()),
            Box::new(BrightnessContrastTool::new()),
            Box::new(ChannelLevelsTool::new()),
            Box::new(ClarityTextureTool::new()),
            Box::new(ColorBalanceTool::new()),
            Box::new(ColorSpaceTool::new()),
            Box::new(CropTool::new()),
            Box::new(CurvesTool::new()),
            Box::new(DenoiseTool::new()),
            Box::new(FauxHdrTool::new()),
            Box::new(FocusStackTool::new()),
            Box::new(GrainTool::new()),
            Box::new(HdrMergeTool::new()),
            Box::new(HealTool::new()),
            Box::new(HighlightsShadowsTool::new()),
            Box::new(HslTool::new()),
            Box::new(HueShiftTool::new()),
            Box::new(IntensifyHdrTool::new()),
            Box::new(LevelsTool::new()),
            Box::new(LocalLaplacianTool::new()),
            Box::new(LutTool::new()),
            Box::new(NoiseReductionTool::new()),
            Box::new(PanoramaTool::new()),
            Box::new(PerspectiveTool::new()),
            Box::new(ResizeTool::new()),
            Box::new(RotateTool::new()),
            Box::new(SaturationTool::new()),
            Box::new(SepiaTool::new()),
            Box::new(ShadowExposureTool::new()),
            Box::new(SharpenTool::new()),
            Box::new(SplitToneTool::new()),
            Box::new(StraightenTool::new()),
            Box::new(VibranceTool::new()),
            Box::new(VignetteTool::new()),
            Box::new(WhiteBalanceTool::new()),
        ]
    }

    /// Add tools supplied from outside this crate, each at its alphabetical
    /// position by display name.
    ///
    /// The tools already present are never moved: a new tool goes in front of
    /// the first one that sorts after it, and after any whose label is equal.
    ///
    /// # Panics
    ///
    /// If a tool's id is already in use. Ids key the remembered open/closed
    /// state and identify the tool in an edit session, so two tools sharing
    /// one would silently answer for each other.
    pub fn add_extra_tools(&mut self, extra: Vec<Box<dyn Tool>>) {
        for tool in extra {
            let id = tool.id();
            assert!(
                self.tools.iter().all(|t| t.id() != id),
                "duplicate tool id {id:?}: an extra tool must not reuse the id of \
                 a built-in tool or of another extra tool",
            );
            let key = sort_key(tool.display_name());
            let at = self
                .tools
                .iter()
                .position(|t| sort_key(t.display_name()) > key)
                .unwrap_or(self.tools.len());
            self.tools.insert(at, tool);
        }
    }

    /// The edit-session identity of the tool that claims the operation called
    /// `op_name` through [`Tool::edits_op`], if any does.
    pub fn editing_tool_claiming(&self, op_name: &str) -> Option<EditingTool> {
        self.tools
            .iter()
            .filter(|t| t.edits_op() == Some(op_name))
            .find_map(|t| t.editing_tool())
    }

    pub fn find<T: 'static>(&self) -> Option<&T> {
        self.tools
            .iter()
            .find_map(|t| t.as_any().downcast_ref::<T>())
    }

    pub fn find_mut<T: 'static>(&mut self) -> Option<&mut T> {
        self.tools
            .iter_mut()
            .find_map(|t| t.as_any_mut().downcast_mut::<T>())
    }

    pub fn any_preview_active(&self) -> bool {
        self.sprocket_edit_preview_active || self.tools.iter().any(|t| t.is_preview_active())
    }

    pub fn preview_op(&self) -> Option<Box<dyn Operation>> {
        if self.sprocket_edit_preview_active {
            return self.sprocket_edit_op.as_ref().map(|original| {
                let mut op = original.clone();
                if let Some(stock) = self.sprocket_film_stock {
                    op.stock = stock;
                }
                Box::new(op) as Box<dyn Operation>
            });
        }
        self.tools.iter().find_map(|t| t.preview_op())
    }

    pub fn cancel_all_previews(&mut self) {
        for tool in &mut self.tools {
            tool.cancel_preview();
        }
        self.sprocket_crop_active = false;
        self.sprocket_edit_op = None;
        self.sprocket_edit_preview_active = false;
    }

    /// Keep the preview owned by `keep_idx` and dismiss every other tool's
    /// preview. The render path accepts a single preview operation, so allowing
    /// multiple tools to remain active would make the first tool in the list
    /// silently hide changes made in a later tool.
    pub fn cancel_previews_except(&mut self, keep_idx: usize) {
        for (idx, tool) in self.tools.iter_mut().enumerate() {
            if idx != keep_idx {
                tool.cancel_preview();
            }
        }
        self.sprocket_crop_active = false;
        self.sprocket_edit_op = None;
        self.sprocket_edit_preview_active = false;
    }

    /// Reset the per-image tool state for a freshly loaded `w`×`h` image.
    ///
    /// Previews are dismissed as well: one left active from the previous image
    /// is picked up by `preview_op` as soon as the new image renders, so the
    /// user would see an edit they never asked for on top of a file they just
    /// opened.
    pub fn reset_for_new_image(&mut self, w: u32, h: u32) {
        self.cancel_all_previews();
        if let Some(crop) = self.find_mut::<CropTool>() {
            crop.x = 0;
            crop.y = 0;
            crop.w = w;
            crop.h = h;
        }
        if let Some(resize) = self.find_mut::<ResizeTool>() {
            resize.w = w;
            resize.h = h;
        }
        if let Some(rotate) = self.find_mut::<RotateTool>() {
            rotate.deg = 0.0;
        }
    }

    pub fn load_mask_shape(&mut self, mask: Option<&MaskShape>) {
        match mask {
            None => self.mask_sel = 0,
            Some(MaskShape::Linear(mask)) => {
                self.mask_sel = 1;
                self.mask_lin_cx = mask.cx;
                self.mask_lin_cy = mask.cy;
                self.mask_lin_angle = mask.angle_deg;
                self.mask_lin_feather = mask.feather;
                self.mask_lin_invert = mask.invert;
            }
            Some(MaskShape::Radial(mask)) => {
                self.mask_sel = 2;
                self.mask_rad_cx = mask.cx;
                self.mask_rad_cy = mask.cy;
                self.mask_rad_radius = mask.radius;
                self.mask_rad_feather = mask.feather;
                self.mask_rad_invert = mask.invert;
            }
        }
    }

    pub fn current_mask_shape(&self) -> Option<MaskShape> {
        match self.mask_sel {
            1 => Some(MaskShape::Linear(LinearMask {
                cx: self.mask_lin_cx,
                cy: self.mask_lin_cy,
                angle_deg: self.mask_lin_angle,
                feather: self.mask_lin_feather,
                invert: self.mask_lin_invert,
            })),
            2 => Some(MaskShape::Radial(RadialMask {
                cx: self.mask_rad_cx,
                cy: self.mask_rad_cy,
                radius: self.mask_rad_radius,
                feather: self.mask_rad_feather,
                invert: self.mask_rad_invert,
            })),
            _ => None,
        }
    }

    pub fn crop_aspect_ratio(&self) -> Option<(f32, f32)> {
        if self.sprocket_crop_active {
            return Some((2.0, 1.0));
        }
        let crop = self.find::<CropTool>()?;
        let flip = crop.portrait;
        match crop.aspect_idx {
            1 => {
                let (w, h) = (3.0, 2.0);
                Some(if flip { (h, w) } else { (w, h) })
            }
            2 => {
                let (w, h) = (4.0, 3.0);
                Some(if flip { (h, w) } else { (w, h) })
            }
            3 => Some((1.0, 1.0)),
            4 => {
                let (w, h) = (16.0, 9.0);
                Some(if flip { (h, w) } else { (w, h) })
            }
            5 => Some((9.0, 16.0)),
            6 => Some((crop.custom_ratio, 1.0)),
            _ => None,
        }
    }
}

/// A stand-in for a tool and operation defined outside this crate, written
/// against the exported API only (`crate::…` here is `rasterlab_gui::…` there).
#[cfg(test)]
pub(super) mod extension_fixture {
    use rasterlab_core::{Image, error::RasterResult};
    use serde::{Deserialize, Serialize};

    use crate::{Operation, ParamTool, Tool, ToolAction, ToolUiCtx, param_tool_actions};

    pub const EXTENSION_TOOL_ID: &str = "test_extension";
    pub const EXTENSION_OP_NAME: &str = "test_extension_op";

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct ExtensionOp {
        pub amount: f32,
    }

    impl ExtensionOp {
        pub fn new(amount: f32) -> Self {
            Self { amount }
        }
    }

    #[typetag::serde]
    impl Operation for ExtensionOp {
        fn name(&self) -> &'static str {
            EXTENSION_OP_NAME
        }
        fn clone_box(&self) -> Box<dyn Operation> {
            Box::new(self.clone())
        }
        fn as_any(&self) -> Option<&dyn std::any::Any> {
            Some(self)
        }
        fn apply(&self, image: Image) -> RasterResult<Image> {
            Ok(image)
        }
        fn describe(&self) -> String {
            format!("Test extension {}", self.amount)
        }
    }

    pub struct ExtensionTool {
        pub id: &'static str,
        pub display_name: &'static str,
        /// The operation name reported by `edits_op`.
        pub edits: &'static str,
        pub amount: f32,
        pub preview_active: bool,
    }

    impl ExtensionTool {
        pub fn new() -> Self {
            Self {
                id: EXTENSION_TOOL_ID,
                display_name: "🧪  Test Extension",
                edits: EXTENSION_OP_NAME,
                amount: 0.0,
                preview_active: false,
            }
        }
    }

    impl ParamTool for ExtensionTool {
        type Op = ExtensionOp;

        fn op(&self) -> ExtensionOp {
            ExtensionOp::new(self.amount)
        }
        fn reset(&mut self) {
            self.amount = 0.0;
        }
        fn load(&mut self, op: &ExtensionOp) {
            self.amount = op.amount;
        }
        fn preview_active(&mut self) -> &mut bool {
            &mut self.preview_active
        }
    }

    impl Tool for ExtensionTool {
        fn id(&self) -> &'static str {
            self.id
        }
        fn display_name(&self) -> &'static str {
            self.display_name
        }
        fn edits_op(&self) -> Option<&'static str> {
            Some(self.edits)
        }
        fn render_ui(&mut self, ui: &mut egui::Ui, ctx: &ToolUiCtx<'_>) -> ToolAction {
            let changed = ui
                .add(egui::Slider::new(&mut self.amount, 0.0..=1.0))
                .changed();
            param_tool_actions(ui, ctx, self, changed)
        }

        crate::impl_param_tool!();
    }
}

#[cfg(test)]
mod tests {
    use super::extension_fixture::ExtensionTool;
    use super::*;

    #[test]
    fn newest_tool_preview_can_exclusively_own_the_render() {
        let mut state = ToolState::new();
        let first = state
            .tools
            .iter()
            .position(|tool| tool.id() == "brightness_contrast")
            .unwrap();
        let hsl = state
            .tools
            .iter()
            .position(|tool| tool.id() == "hsl_panel")
            .unwrap();

        state.tools[first].activate_preview();
        state.tools[hsl].activate_preview();

        // Without exclusive ownership, the earlier tool shadows HSL because
        // preview_op returns the first active preview in display order.
        assert_eq!(state.preview_op().unwrap().name(), "brightness_contrast");

        state.cancel_previews_except(hsl);

        assert!(!state.tools[first].is_preview_active());
        assert!(state.tools[hsl].is_preview_active());
        assert_eq!(state.preview_op().unwrap().name(), "hsl_panel");
    }

    /// CLAUDE.md mandates:
    ///   "All other tools are placed in strict alphabetical order by display
    ///    name after [Auto Enhance and Looks]."
    ///
    /// Verify that the trait-tool registration order in `build_tools` matches
    /// the case-insensitive alphabetical sort of the display name labels.
    /// Adding a new tool out of position fails this test immediately.
    #[test]
    fn tools_are_in_alphabetical_order_by_display_name() {
        let tools = ToolState::build_tools();
        let labels: Vec<String> = tools
            .iter()
            .map(|t| label(t.display_name()).to_string())
            .collect();

        let mut sorted = labels.clone();
        sorted.sort_by_key(|s| s.to_lowercase());

        assert_eq!(
            labels, sorted,
            "Tool order is not strictly alphabetical by display name.\n\
             Current:  {:?}\n\
             Expected: {:?}",
            labels, sorted,
        );
    }

    /// Every tool must have a unique `id()` so prefs/state lookups are
    /// unambiguous; a duplicate id would silently mask one tool's open/closed
    /// state.
    #[test]
    fn tool_ids_are_unique() {
        let tools = ToolState::build_tools();
        let mut ids: Vec<&'static str> = tools.iter().map(|t| t.id()).collect();
        ids.sort();
        let mut deduped = ids.clone();
        deduped.dedup();
        assert_eq!(
            ids, deduped,
            "duplicate tool id found in build_tools(): {:?}",
            ids,
        );
    }

    /// Display labels must not be empty after trimming the icon prefix —
    /// otherwise the tools panel renders an icon with no name.
    #[test]
    fn tool_display_labels_are_non_empty() {
        let tools = ToolState::build_tools();
        for t in &tools {
            let l = label(t.display_name());
            assert!(
                !l.is_empty(),
                "tool {:?} has an empty display label after icon stripping",
                t.id(),
            );
        }
    }

    /// Opening a file must not carry a preview over from the previous image:
    /// `preview_op` feeds the render directly, so a leftover preview shows the
    /// user an edit they never applied to the file they just opened.
    #[test]
    fn loading_a_new_image_leaves_no_preview_active() {
        let mut state = ToolState::new();
        for tool in &mut state.tools {
            tool.activate_preview();
        }
        state.sprocket_crop_active = true;

        state.reset_for_new_image(800, 600);

        assert!(!state.any_preview_active());
        assert!(state.preview_op().is_none());
        assert!(!state.sprocket_crop_active);
    }

    #[test]
    fn loading_a_new_image_resizes_the_geometry_tools() {
        let mut state = ToolState::new();
        let crop = state.find_mut::<CropTool>().unwrap();
        crop.x = 100;
        crop.y = 50;
        crop.w = 10;
        crop.h = 10;
        state.find_mut::<RotateTool>().unwrap().deg = 90.0;

        state.reset_for_new_image(800, 600);

        let crop = state.find::<CropTool>().unwrap();
        assert_eq!((crop.x, crop.y, crop.w, crop.h), (0, 0, 800, 600));
        let resize = state.find::<ResizeTool>().unwrap();
        assert_eq!((resize.w, resize.h), (800, 600));
        assert_eq!(state.find::<RotateTool>().unwrap().deg, 0.0);
    }

    fn ids(tools: &[Box<dyn Tool>]) -> Vec<&'static str> {
        tools.iter().map(|t| t.id()).collect()
    }

    fn extra(id: &'static str, display_name: &'static str) -> Box<dyn Tool> {
        Box::new(ExtensionTool {
            id,
            display_name,
            ..ExtensionTool::new()
        })
    }

    /// An extra tool is found where its name says it should be, wherever that
    /// is, and the built-in tools around it stay exactly as they were.
    #[test]
    fn extra_tools_land_at_their_alphabetical_position() {
        let builtin = ids(&ToolState::build_tools());
        // (display names registered together, expected neighbours of each:
        //  the id before it and the id after it)
        type Neighbours = (Option<&'static str>, Option<&'static str>);
        let cases: &[(&[&'static str], &[Neighbours])] = &[
            (&[], &[]),
            (&["🧪  Aardvark"], &[(None, Some("airplane_window"))]),
            (
                &["🧪  B&W Extension"],
                &[(Some("airplane_window"), Some("bw"))],
            ),
            // Case and a missing icon change nothing about where a name sorts.
            (&["zebra"], &[(Some("white_balance"), None)]),
            // A label equal to a built-in one goes after it.
            (&["🧪  SEPIA"], &[(Some("sepia"), Some("shadow_exposure"))]),
            // Registration order is irrelevant, also between two extras.
            (
                &["🧪  Vignette Plus", "🧪  Vignette Extra"],
                &[
                    (Some("extra_1"), Some("white_balance")),
                    (Some("vignette"), Some("extra_0")),
                ],
            ),
        ];
        const EXTRA_IDS: [&str; 2] = ["extra_0", "extra_1"];

        for (names, expected) in cases {
            let mut state = ToolState::new();
            state.add_extra_tools(
                names
                    .iter()
                    .zip(EXTRA_IDS)
                    .map(|(name, id)| extra(id, name))
                    .collect(),
            );
            let all = ids(&state.tools);

            let without_extras: Vec<_> = all
                .iter()
                .copied()
                .filter(|id| !EXTRA_IDS.contains(id))
                .collect();
            assert_eq!(without_extras, builtin, "{names:?} moved a built-in tool");
            assert_eq!(all.len(), builtin.len() + names.len(), "{names:?}");

            for (id, (before, after)) in EXTRA_IDS.iter().zip(expected.iter()) {
                let at = all.iter().position(|t| t == id).unwrap();
                let neighbours = (at.checked_sub(1).map(|i| all[i]), all.get(at + 1).copied());
                assert_eq!(neighbours, (*before, *after), "{names:?}: {id}");
            }
        }
    }

    #[test]
    #[should_panic(expected = "duplicate tool id \"sepia\"")]
    fn an_extra_tool_reusing_a_built_in_id_is_refused() {
        ToolState::new().add_extra_tools(vec![extra("sepia", "🧪  Anything")]);
    }

    #[test]
    fn sprocket_crop_overrides_regular_crop_aspect() {
        let mut tools = ToolState::new();
        tools.find_mut::<CropTool>().unwrap().aspect_idx = 0;
        assert_eq!(tools.crop_aspect_ratio(), None);

        tools.sprocket_crop_active = true;
        assert_eq!(tools.crop_aspect_ratio(), Some((2.0, 1.0)));
    }
}
