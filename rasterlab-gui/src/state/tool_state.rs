use rasterlab_core::{
    ops::{
        BlackAndWhiteOp, BlurOp, BrightnessContrastOp, ClarityTextureOp, ColorBalanceOp,
        ColorSpaceConversion, CurvesOp, DenoiseOp, FauxHdrOp, FlipOp, GrainOp, HealSpot,
        HighlightsShadowsOp, HslPanelOp, HueShiftOp, LevelsOp, LinearMask, LutOp, MaskShape,
        NoiseReductionOp, NrMethod, PerspectiveOp, RadialMask, ResampleMode, RotateOp,
        SaturationOp, SepiaOp, SharpenOp, SplitToneOp, VibranceOp, VignetteOp, WhiteBalanceOp,
    },
    traits::format_handler::EncodeOptions,
    traits::operation::Operation,
};

/// All per-tool input fields, preview flags, and export settings.
///
/// Extracted from `AppState` so that adding a new tool only requires changes
/// here (fields + `any_preview_active` + `preview_op` + `cancel_all_previews`)
/// and in the tools panel UI.
pub struct ToolState {
    // ── Crop ──────────────────────────────────────────────────────────────
    pub crop_x: u32,
    pub crop_y: u32,
    pub crop_w: u32,
    pub crop_h: u32,
    /// 0=Free, 1=3:2, 2=4:3, 3=1:1, 4=16:9, 5=9:16, 6=Custom
    pub crop_aspect_idx: usize,
    /// Custom ratio (width / height), only used when crop_aspect_idx == 6.
    pub crop_custom_ratio: f32,

    // ── Rotate / Flip ─────────────────────────────────────────────────────
    pub rotate_deg: f32,
    pub rotate_preview_active: bool,
    /// Pending horizontal flip waiting for Apply.
    pub flip_h_pending: bool,
    /// Pending vertical flip waiting for Apply.
    pub flip_v_pending: bool,
    pub flip_preview_active: bool,

    // ── Straighten ────────────────────────────────────────────────────────
    /// Angle in degrees for the straighten tool, range [-45, 45].
    pub straighten_angle: f32,
    /// When true, show the straighten line overlay on the canvas.
    pub straighten_active: bool,
    /// When true, automatically crop after straighten to remove exposed corners.
    pub straighten_crop: bool,
    pub straighten_preview_active: bool,

    // ── Sharpen ───────────────────────────────────────────────────────────
    pub sharpen_strength: f32,
    pub sharpen_preview_active: bool,

    // ── Clarity / Texture ─────────────────────────────────────────────────
    pub clarity: f32,
    pub texture: f32,
    pub clarity_preview_active: bool,

    // ── Black & White ─────────────────────────────────────────────────────
    pub bw_mode_idx: usize,
    /// Channel mixer weights for the ChannelMixer B&W mode.
    pub bw_mixer_r: f32,
    pub bw_mixer_g: f32,
    pub bw_mixer_b: f32,
    /// When true, a BlackAndWhiteOp preview is appended to each render.
    pub bw_preview_active: bool,

    // ── Brightness / Contrast ─────────────────────────────────────────────
    pub bc_brightness: f32,
    pub bc_contrast: f32,
    pub bc_preview_active: bool,

    // ── Saturation ────────────────────────────────────────────────────────
    pub saturation: f32,
    pub sat_preview_active: bool,

    // ── Curves ────────────────────────────────────────────────────────────
    /// Control points `[input, output]` in `[0,1]`, sorted by input.
    pub curve_points: Vec<[f32; 2]>,
    pub curve_preview_active: bool,
    /// Index of the control point currently being dragged in the curve editor.
    pub curve_dragging_idx: Option<usize>,

    // ── Vignette ──────────────────────────────────────────────────────────
    pub vignette_strength: f32,
    pub vignette_radius: f32,
    pub vignette_feather: f32,
    /// When true, a VignetteOp preview is appended to each render.
    pub vignette_preview_active: bool,

    // ── Vibrance ──────────────────────────────────────────────────────────
    pub vibrance: f32,
    pub vibrance_preview_active: bool,

    // ── Sepia ─────────────────────────────────────────────────────────────
    pub sepia_strength: f32,
    pub sepia_preview_active: bool,

    // ── Split Tone ────────────────────────────────────────────────────────
    pub split_shadow_hue: f32,
    pub split_shadow_sat: f32,
    pub split_highlight_hue: f32,
    pub split_highlight_sat: f32,
    pub split_balance: f32,
    pub split_preview_active: bool,

    // ── Resize ────────────────────────────────────────────────────────────
    pub resize_w: u32,
    pub resize_h: u32,
    pub resize_mode: ResampleMode,
    pub resize_lock_aspect: bool,

    // ── Blur ──────────────────────────────────────────────────────────────
    pub blur_radius: f32,
    pub blur_preview_active: bool,

    // ── Denoise ───────────────────────────────────────────────────────────
    pub denoise_strength: f32,
    pub denoise_radius: u32,
    pub denoise_preview_active: bool,

    // ── Noise Reduction (advanced) ────────────────────────────────────────
    pub nr_method: NrMethod,
    pub nr_luma: f32,
    pub nr_color: f32,
    pub nr_detail: f32,
    pub nr_preview_active: bool,

    // ── Heal / Clone stamp ────────────────────────────────────────────────
    /// Whether the heal tool is active (canvas interaction mode).
    pub heal_active: bool,
    /// Brush radius in pixels for the heal tool.
    pub heal_radius: u32,
    /// Spots placed by the user, pending commit to the pipeline.
    pub heal_spots: Vec<HealSpot>,

    // ── Perspective ───────────────────────────────────────────────────────
    /// Corner offsets `[[tl_x, tl_y], [tr_x, tr_y], [br_x, br_y], [bl_x, bl_y]]`
    /// as fractions of image width/height in `[-1, 1]`.
    pub perspective_corners: [[f32; 2]; 4],
    pub perspective_preview_active: bool,

    // ── Color Space Conversion ────────────────────────────────────────────
    pub color_space_conversion: ColorSpaceConversion,

    // ── LUT ───────────────────────────────────────────────────────────────
    /// Loaded LUT op, or `None` if no LUT has been loaded.
    pub lut_op: Option<LutOp>,
    /// Blend strength for the loaded LUT.
    pub lut_strength: f32,
    /// Display name of the loaded LUT file.
    pub lut_name: String,
    pub lut_preview_active: bool,
    /// Set to true by the tools panel to ask app.rs to open the LUT file dialog.
    pub lut_dialog_requested: bool,

    // ── Hue Shift ─────────────────────────────────────────────────────────
    pub hue_degrees: f32,
    pub hue_preview_active: bool,

    // ── Highlights & Shadows ──────────────────────────────────────────────
    pub hl_highlights: f32,
    pub hl_shadows: f32,
    pub hl_preview_active: bool,

    // ── White Balance ─────────────────────────────────────────────────────
    pub wb_temperature: f32,
    pub wb_tint: f32,
    pub wb_preview_active: bool,

    // ── Faux HDR ──────────────────────────────────────────────────────────
    pub hdr_strength: f32,
    pub hdr_preview_active: bool,

    // ── Grain ─────────────────────────────────────────────────────────────
    pub grain_strength: f32,
    pub grain_size: f32,
    pub grain_seed: u64,
    /// When true, a GrainOp preview is appended to each render (always full-res).
    pub grain_preview_active: bool,

    // ── Color Balance ─────────────────────────────────────────────────────
    /// `[shadows, midtones, highlights]` on each axis.
    pub cb_cyan_red: [f32; 3],
    pub cb_magenta_green: [f32; 3],
    pub cb_yellow_blue: [f32; 3],
    pub cb_preview_active: bool,

    // ── HSL Panel ─────────────────────────────────────────────────────────
    /// Per-band hue shifts in degrees (8 bands: Reds … Magentas).
    pub hsl_hue: [f32; 8],
    pub hsl_sat: [f32; 8],
    pub hsl_lum: [f32; 8],
    pub hsl_preview_active: bool,

    // ── Levels ─────────────────────────────────────────────────────────────
    /// Live slider values for the levels tool (not yet committed to pipeline).
    pub levels_black: f32,
    pub levels_mid: f32,
    pub levels_white: f32,
    /// When true, a LevelsOp preview is appended to each render.
    pub levels_preview_active: bool,

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
    pub encode_opts: EncodeOptions,
    /// When `true`, apply a resize step before encoding.
    pub export_resize_enabled: bool,
    pub export_resize_w: u32,
    pub export_resize_h: u32,
    pub export_resize_mode: ResampleMode,
}

impl ToolState {
    pub fn new() -> Self {
        Self {
            crop_x: 0,
            crop_y: 0,
            crop_w: 0,
            crop_h: 0,
            crop_aspect_idx: 0,
            crop_custom_ratio: 1.5,
            rotate_deg: 0.0,
            rotate_preview_active: false,
            flip_h_pending: false,
            flip_v_pending: false,
            flip_preview_active: false,
            straighten_angle: 0.0,
            straighten_active: false,
            straighten_crop: true,
            straighten_preview_active: false,
            sharpen_strength: 1.0,
            sharpen_preview_active: false,
            clarity: 0.0,
            texture: 0.0,
            clarity_preview_active: false,
            bw_mode_idx: 0,
            bw_mixer_r: 0.2126,
            bw_mixer_g: 0.7152,
            bw_mixer_b: 0.0722,
            bw_preview_active: false,
            bc_brightness: 0.0,
            bc_contrast: 0.0,
            bc_preview_active: false,
            saturation: 1.0,
            sat_preview_active: false,
            curve_points: vec![[0.0, 0.0], [1.0, 1.0]],
            curve_preview_active: false,
            curve_dragging_idx: None,
            vibrance: 0.0,
            vibrance_preview_active: false,
            sepia_strength: 1.0,
            sepia_preview_active: false,
            split_shadow_hue: 220.0,
            split_shadow_sat: 0.20,
            split_highlight_hue: 40.0,
            split_highlight_sat: 0.15,
            split_balance: 0.0,
            split_preview_active: false,
            resize_w: 0,
            resize_h: 0,
            resize_mode: ResampleMode::Bicubic,
            resize_lock_aspect: true,
            blur_radius: 2.0,
            blur_preview_active: false,
            denoise_strength: 0.1,
            denoise_radius: 3,
            denoise_preview_active: false,
            nr_method: NrMethod::Wavelet,
            nr_luma: 0.3,
            nr_color: 0.5,
            nr_detail: 0.5,
            nr_preview_active: false,
            heal_active: false,
            heal_radius: 30,
            heal_spots: Vec::new(),
            perspective_corners: [[0.0; 2]; 4],
            perspective_preview_active: false,
            color_space_conversion: ColorSpaceConversion::SrgbToDisplayP3,
            lut_op: None,
            lut_strength: 1.0,
            lut_name: String::new(),
            lut_preview_active: false,
            lut_dialog_requested: false,
            hue_degrees: 0.0,
            hue_preview_active: false,
            hl_highlights: 0.0,
            hl_shadows: 0.0,
            hl_preview_active: false,
            wb_temperature: 0.0,
            wb_tint: 0.0,
            wb_preview_active: false,
            vignette_strength: 0.5,
            vignette_radius: 0.65,
            vignette_feather: 0.5,
            vignette_preview_active: false,
            hdr_strength: 0.8,
            hdr_preview_active: false,
            grain_strength: 0.10,
            grain_size: 1.8,
            grain_seed: 42,
            grain_preview_active: false,
            cb_cyan_red: [0.0; 3],
            cb_magenta_green: [0.0; 3],
            cb_yellow_blue: [0.0; 3],
            cb_preview_active: false,
            hsl_hue: [0.0; 8],
            hsl_sat: [0.0; 8],
            hsl_lum: [0.0; 8],
            hsl_preview_active: false,
            levels_black: 0.0,
            levels_mid: 1.0,
            levels_white: 1.0,
            levels_preview_active: false,
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
            encode_opts: EncodeOptions::default(),
            export_resize_enabled: false,
            export_resize_w: 0,
            export_resize_h: 0,
            export_resize_mode: ResampleMode::Bicubic,
        }
    }

    /// True when any tool is showing a live preview overlay on the render.
    pub fn any_preview_active(&self) -> bool {
        self.levels_preview_active
            || self.bw_preview_active
            || self.vignette_preview_active
            || self.bc_preview_active
            || self.sat_preview_active
            || self.sepia_preview_active
            || self.sharpen_preview_active
            || self.clarity_preview_active
            || self.split_preview_active
            || self.lut_preview_active
            || self.curve_preview_active
            || self.hdr_preview_active
            || self.wb_preview_active
            || self.hl_preview_active
            || self.hue_preview_active
            || self.vibrance_preview_active
            || self.cb_preview_active
            || self.hsl_preview_active
            || self.blur_preview_active
            || self.denoise_preview_active
            || self.nr_preview_active
            || self.rotate_preview_active
            || self.flip_preview_active
            || self.straighten_preview_active
            || self.perspective_preview_active
    }

    /// Build the preview operation from current tool state, if any preview is
    /// active.  The result is applied on top of the committed pipeline but not
    /// cached.
    pub fn preview_op(&self) -> Option<Box<dyn Operation>> {
        if self.levels_preview_active {
            Some(Box::new(LevelsOp::new(
                self.levels_black,
                self.levels_white,
                self.levels_mid,
            )))
        } else if self.bw_preview_active {
            Some(self.make_bw_op())
        } else if self.bc_preview_active {
            Some(Box::new(BrightnessContrastOp::new(
                self.bc_brightness,
                self.bc_contrast,
            )))
        } else if self.sat_preview_active {
            Some(Box::new(SaturationOp::new(self.saturation)))
        } else if self.sepia_preview_active {
            Some(Box::new(SepiaOp::new(self.sepia_strength)))
        } else if self.sharpen_preview_active {
            Some(Box::new(SharpenOp::new(self.sharpen_strength)))
        } else if self.clarity_preview_active {
            Some(Box::new(ClarityTextureOp::new(self.clarity, self.texture)))
        } else if self.split_preview_active {
            Some(Box::new(SplitToneOp::new(
                self.split_shadow_hue,
                self.split_shadow_sat,
                self.split_highlight_hue,
                self.split_highlight_sat,
                self.split_balance,
            )))
        } else if self.lut_preview_active {
            self.lut_op.as_ref().map(|op| {
                let mut preview = op.clone();
                preview.strength = self.lut_strength;
                Box::new(preview) as Box<dyn Operation>
            })
        } else if self.curve_preview_active {
            Some(Box::new(CurvesOp {
                points: self.curve_points.clone(),
            }))
        } else if self.vignette_preview_active {
            Some(Box::new(VignetteOp::new(
                self.vignette_strength,
                self.vignette_radius,
                self.vignette_feather,
            )))
        } else if self.vibrance_preview_active {
            Some(Box::new(VibranceOp::new(self.vibrance)))
        } else if self.hue_preview_active {
            Some(Box::new(HueShiftOp::new(self.hue_degrees)))
        } else if self.hl_preview_active {
            Some(Box::new(HighlightsShadowsOp::new(
                self.hl_highlights,
                self.hl_shadows,
            )))
        } else if self.wb_preview_active {
            Some(Box::new(WhiteBalanceOp::new(
                self.wb_temperature,
                self.wb_tint,
            )))
        } else if self.hdr_preview_active {
            Some(Box::new(FauxHdrOp::new(self.hdr_strength)))
        } else if self.grain_preview_active {
            Some(Box::new(GrainOp::new(
                self.grain_strength,
                self.grain_size,
                self.grain_seed,
            )))
        } else if self.cb_preview_active {
            Some(Box::new(ColorBalanceOp::new(
                self.cb_cyan_red,
                self.cb_magenta_green,
                self.cb_yellow_blue,
            )))
        } else if self.hsl_preview_active {
            Some(Box::new(HslPanelOp::new(
                self.hsl_hue,
                self.hsl_sat,
                self.hsl_lum,
            )))
        } else if self.blur_preview_active {
            Some(Box::new(BlurOp::new(self.blur_radius)))
        } else if self.denoise_preview_active {
            Some(Box::new(DenoiseOp::new(
                self.denoise_strength,
                self.denoise_radius,
            )))
        } else if self.nr_preview_active {
            Some(Box::new(NoiseReductionOp {
                method: self.nr_method.clone(),
                luma_strength: self.nr_luma,
                color_strength: self.nr_color,
                detail_preservation: self.nr_detail,
            }))
        } else if self.rotate_preview_active {
            Some(Box::new(RotateOp::arbitrary(self.rotate_deg)))
        } else if self.flip_preview_active {
            match (self.flip_h_pending, self.flip_v_pending) {
                (true, false) => Some(Box::new(FlipOp::horizontal())),
                (false, true) => Some(Box::new(FlipOp::vertical())),
                // H then V is equivalent to a 180° rotation (lossless).
                (true, true) => Some(Box::new(RotateOp::cw180())),
                (false, false) => None,
            }
        } else if self.straighten_preview_active {
            Some(Box::new(RotateOp::arbitrary(self.straighten_angle)))
        } else if self.perspective_preview_active {
            Some(Box::new(PerspectiveOp::new(self.perspective_corners)))
        } else {
            None
        }
    }

    /// Silently dismiss every tool preview without committing any of them.
    ///
    /// Called automatically whenever the pipeline is mutated through any means
    /// other than a tool's own "Apply" button, so the committed state is always
    /// visible unobscured.  Slider/curve values are preserved so the user can
    /// resume adjusting after the other operation is complete.
    pub fn cancel_all_previews(&mut self) {
        self.levels_preview_active = false;
        self.bw_preview_active = false;
        self.bc_preview_active = false;
        self.sat_preview_active = false;
        self.sepia_preview_active = false;
        self.sharpen_preview_active = false;
        self.clarity_preview_active = false;
        self.split_preview_active = false;
        self.lut_preview_active = false;
        self.curve_preview_active = false;
        self.curve_dragging_idx = None;
        self.vignette_preview_active = false;
        self.vibrance_preview_active = false;
        self.hue_preview_active = false;
        self.hl_preview_active = false;
        self.wb_preview_active = false;
        self.hdr_preview_active = false;
        self.grain_preview_active = false;
        self.cb_preview_active = false;
        self.hsl_preview_active = false;
        self.blur_preview_active = false;
        self.denoise_preview_active = false;
        self.nr_preview_active = false;
        self.rotate_preview_active = false;
        self.flip_h_pending = false;
        self.flip_v_pending = false;
        self.flip_preview_active = false;
        self.straighten_preview_active = false;
        self.perspective_preview_active = false;
    }

    /// Build a `MaskShape` from the current mask UI state, or `None` if masking
    /// is disabled.  Used both by `push_op` and the canvas overlay renderer.
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
        match self.crop_aspect_idx {
            1 => Some((3.0, 2.0)),
            2 => Some((4.0, 3.0)),
            3 => Some((1.0, 1.0)),
            4 => Some((16.0, 9.0)),
            5 => Some((9.0, 16.0)),
            6 => Some((self.crop_custom_ratio, 1.0)),
            _ => None,
        }
    }

    pub fn make_bw_op(&self) -> Box<dyn Operation> {
        match self.bw_mode_idx {
            1 => Box::new(BlackAndWhiteOp::average()),
            2 => Box::new(BlackAndWhiteOp::perceptual()),
            3 => Box::new(BlackAndWhiteOp::channel_mixer(
                self.bw_mixer_r,
                self.bw_mixer_g,
                self.bw_mixer_b,
            )),
            _ => Box::new(BlackAndWhiteOp::luminance()),
        }
    }
}
