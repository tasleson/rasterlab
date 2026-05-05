use crate::shaders::*;

pub(crate) struct BrightnessContrastKernel {
    pub(crate) pipeline: wgpu::ComputePipeline,
    pub(crate) bind_group_layout: wgpu::BindGroupLayout,
}

pub(crate) struct CurvesKernel {
    pub(crate) pipeline: wgpu::ComputePipeline,
    pub(crate) bind_group_layout: wgpu::BindGroupLayout,
}

pub(crate) struct HueShiftKernel {
    pub(crate) pipeline: wgpu::ComputePipeline,
    pub(crate) bind_group_layout: wgpu::BindGroupLayout,
}

pub(crate) struct SaturationKernel {
    pub(crate) pipeline: wgpu::ComputePipeline,
    pub(crate) bind_group_layout: wgpu::BindGroupLayout,
}

pub(crate) struct VibranceKernel {
    pub(crate) pipeline: wgpu::ComputePipeline,
    pub(crate) bind_group_layout: wgpu::BindGroupLayout,
}

pub(crate) struct WhiteBalanceKernel {
    pub(crate) pipeline: wgpu::ComputePipeline,
    pub(crate) bind_group_layout: wgpu::BindGroupLayout,
}

pub(crate) struct SepiaKernel {
    pub(crate) pipeline: wgpu::ComputePipeline,
    pub(crate) bind_group_layout: wgpu::BindGroupLayout,
}

pub(crate) struct LevelsKernel {
    pub(crate) pipeline: wgpu::ComputePipeline,
    pub(crate) bind_group_layout: wgpu::BindGroupLayout,
}

pub(crate) struct HighlightsShadowsKernel {
    pub(crate) pipeline: wgpu::ComputePipeline,
    pub(crate) bind_group_layout: wgpu::BindGroupLayout,
}

pub(crate) struct VignetteKernel {
    pub(crate) pipeline: wgpu::ComputePipeline,
    pub(crate) bind_group_layout: wgpu::BindGroupLayout,
}

pub(crate) struct ShadowExposureKernel {
    pub(crate) pipeline: wgpu::ComputePipeline,
    pub(crate) bind_group_layout: wgpu::BindGroupLayout,
}

pub(crate) struct SplitToneKernel {
    pub(crate) pipeline: wgpu::ComputePipeline,
    pub(crate) bind_group_layout: wgpu::BindGroupLayout,
}

pub(crate) struct BlackAndWhiteKernel {
    pub(crate) pipeline: wgpu::ComputePipeline,
    pub(crate) bind_group_layout: wgpu::BindGroupLayout,
}

pub(crate) struct BlurKernel {
    pub(crate) h_pipeline: wgpu::ComputePipeline,
    pub(crate) v_pipeline: wgpu::ComputePipeline,
    pub(crate) bind_group_layout: wgpu::BindGroupLayout,
}

pub(crate) struct ColorBalanceKernel {
    pub(crate) pipeline: wgpu::ComputePipeline,
    pub(crate) bind_group_layout: wgpu::BindGroupLayout,
}

pub(crate) struct ColorSpaceKernel {
    pub(crate) pipeline: wgpu::ComputePipeline,
    pub(crate) bind_group_layout: wgpu::BindGroupLayout,
}

pub(crate) struct DenoiseKernel {
    pub(crate) pipeline: wgpu::ComputePipeline,
    pub(crate) bind_group_layout: wgpu::BindGroupLayout,
}

pub(crate) struct HslPanelKernel {
    pub(crate) pipeline: wgpu::ComputePipeline,
    pub(crate) bind_group_layout: wgpu::BindGroupLayout,
}

pub(crate) struct SharpenKernel {
    pub(crate) pipeline: wgpu::ComputePipeline,
    pub(crate) bind_group_layout: wgpu::BindGroupLayout,
}

pub(crate) struct FauxHdrKernel {
    pub(crate) pipeline: wgpu::ComputePipeline,
    pub(crate) bind_group_layout: wgpu::BindGroupLayout,
}

pub(crate) struct ClarityTextureKernel {
    pub(crate) three_bind_layout: wgpu::BindGroupLayout,
    pub(crate) extract_luma_pipeline: wgpu::ComputePipeline,
    pub(crate) box_blur_h_pipeline: wgpu::ComputePipeline,
    pub(crate) box_blur_v_pipeline: wgpu::ComputePipeline,
    pub(crate) four_bind_layout: wgpu::BindGroupLayout,
    pub(crate) apply_detail_pipeline: wgpu::ComputePipeline,
}

impl BrightnessContrastKernel {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let bind_group_layout =
            make_4binding_layout(device, "rasterlab brightness_contrast bind group layout");
        let pipeline = make_simple_pipeline(
            device,
            BRIGHTNESS_CONTRAST_WGSL,
            &bind_group_layout,
            "rasterlab brightness_contrast shader",
            "rasterlab brightness_contrast pipeline",
        );
        Self {
            pipeline,
            bind_group_layout,
        }
    }
}

impl CurvesKernel {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let bind_group_layout = make_4binding_layout(device, "rasterlab curves bind group layout");
        let pipeline = make_simple_pipeline(
            device,
            CURVES_WGSL,
            &bind_group_layout,
            "rasterlab curves shader",
            "rasterlab curves pipeline",
        );
        Self {
            pipeline,
            bind_group_layout,
        }
    }
}

impl HueShiftKernel {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let bind_group_layout =
            make_3binding_layout(device, "rasterlab hue_shift bind group layout");
        let pipeline = make_simple_pipeline(
            device,
            HUE_SHIFT_WGSL,
            &bind_group_layout,
            "rasterlab hue_shift shader",
            "rasterlab hue_shift pipeline",
        );
        Self {
            pipeline,
            bind_group_layout,
        }
    }
}

impl SaturationKernel {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let bind_group_layout =
            make_3binding_layout(device, "rasterlab saturation bind group layout");
        let pipeline = make_simple_pipeline(
            device,
            SATURATION_WGSL,
            &bind_group_layout,
            "rasterlab saturation shader",
            "rasterlab saturation pipeline",
        );
        Self {
            pipeline,
            bind_group_layout,
        }
    }
}

impl VibranceKernel {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let bind_group_layout =
            make_3binding_layout(device, "rasterlab vibrance bind group layout");
        let pipeline = make_simple_pipeline(
            device,
            VIBRANCE_WGSL,
            &bind_group_layout,
            "rasterlab vibrance shader",
            "rasterlab vibrance pipeline",
        );
        Self {
            pipeline,
            bind_group_layout,
        }
    }
}

impl WhiteBalanceKernel {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let bind_group_layout =
            make_3binding_layout(device, "rasterlab white_balance bind group layout");
        let pipeline = make_simple_pipeline(
            device,
            WHITE_BALANCE_WGSL,
            &bind_group_layout,
            "rasterlab white_balance shader",
            "rasterlab white_balance pipeline",
        );
        Self {
            pipeline,
            bind_group_layout,
        }
    }
}

fn storage_entry(binding: u32, read_only: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn uniform_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn make_3binding_layout(device: &wgpu::Device, label: &str) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(label),
        entries: &[
            storage_entry(0, true),
            storage_entry(1, false),
            uniform_entry(2),
        ],
    })
}

fn make_4binding_layout(device: &wgpu::Device, label: &str) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(label),
        entries: &[
            storage_entry(0, true),
            storage_entry(1, false),
            uniform_entry(2),
            storage_entry(3, true),
        ],
    })
}

fn make_simple_pipeline(
    device: &wgpu::Device,
    wgsl: &str,
    bind_group_layout: &wgpu::BindGroupLayout,
    shader_label: &str,
    pipeline_label: &str,
) -> wgpu::ComputePipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(shader_label),
        source: wgpu::ShaderSource::Wgsl(wgsl.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(pipeline_label),
        bind_group_layouts: &[Some(bind_group_layout)],
        immediate_size: 0,
    });
    device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(pipeline_label),
        layout: Some(&layout),
        module: &shader,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: None,
    })
}

impl SepiaKernel {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let bind_group_layout = make_3binding_layout(device, "rasterlab sepia bind group layout");
        let pipeline = make_simple_pipeline(
            device,
            SEPIA_WGSL,
            &bind_group_layout,
            "rasterlab sepia shader",
            "rasterlab sepia pipeline",
        );
        Self {
            pipeline,
            bind_group_layout,
        }
    }
}

impl LevelsKernel {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let bind_group_layout = make_4binding_layout(device, "rasterlab levels bind group layout");
        let pipeline = make_simple_pipeline(
            device,
            LEVELS_WGSL,
            &bind_group_layout,
            "rasterlab levels shader",
            "rasterlab levels pipeline",
        );
        Self {
            pipeline,
            bind_group_layout,
        }
    }
}

impl HighlightsShadowsKernel {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let bind_group_layout =
            make_3binding_layout(device, "rasterlab highlights_shadows bind group layout");
        let pipeline = make_simple_pipeline(
            device,
            HIGHLIGHTS_SHADOWS_WGSL,
            &bind_group_layout,
            "rasterlab highlights_shadows shader",
            "rasterlab highlights_shadows pipeline",
        );
        Self {
            pipeline,
            bind_group_layout,
        }
    }
}

impl VignetteKernel {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let bind_group_layout =
            make_3binding_layout(device, "rasterlab vignette bind group layout");
        let pipeline = make_simple_pipeline(
            device,
            VIGNETTE_WGSL,
            &bind_group_layout,
            "rasterlab vignette shader",
            "rasterlab vignette pipeline",
        );
        Self {
            pipeline,
            bind_group_layout,
        }
    }
}

impl ShadowExposureKernel {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let bind_group_layout =
            make_3binding_layout(device, "rasterlab shadow_exposure bind group layout");
        let pipeline = make_simple_pipeline(
            device,
            SHADOW_EXPOSURE_WGSL,
            &bind_group_layout,
            "rasterlab shadow_exposure shader",
            "rasterlab shadow_exposure pipeline",
        );
        Self {
            pipeline,
            bind_group_layout,
        }
    }
}

impl SplitToneKernel {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let bind_group_layout =
            make_3binding_layout(device, "rasterlab split_tone bind group layout");
        let pipeline = make_simple_pipeline(
            device,
            SPLIT_TONE_WGSL,
            &bind_group_layout,
            "rasterlab split_tone shader",
            "rasterlab split_tone pipeline",
        );
        Self {
            pipeline,
            bind_group_layout,
        }
    }
}

impl BlackAndWhiteKernel {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let bind_group_layout =
            make_3binding_layout(device, "rasterlab black_and_white bind group layout");
        let pipeline = make_simple_pipeline(
            device,
            BLACK_AND_WHITE_WGSL,
            &bind_group_layout,
            "rasterlab black_and_white shader",
            "rasterlab black_and_white pipeline",
        );
        Self {
            pipeline,
            bind_group_layout,
        }
    }
}

impl BlurKernel {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let bind_group_layout = make_3binding_layout(device, "rasterlab blur bind group layout");
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rasterlab blur shader"),
            source: wgpu::ShaderSource::Wgsl(BLUR_WGSL.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("rasterlab blur pipeline layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let h_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("rasterlab blur h_pipeline"),
            layout: Some(&layout),
            module: &shader,
            entry_point: Some("main_h"),
            compilation_options: Default::default(),
            cache: None,
        });
        let v_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("rasterlab blur v_pipeline"),
            layout: Some(&layout),
            module: &shader,
            entry_point: Some("main_v"),
            compilation_options: Default::default(),
            cache: None,
        });
        Self {
            h_pipeline,
            v_pipeline,
            bind_group_layout,
        }
    }
}

impl ColorBalanceKernel {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let bind_group_layout =
            make_3binding_layout(device, "rasterlab color_balance bind group layout");
        let pipeline = make_simple_pipeline(
            device,
            COLOR_BALANCE_WGSL,
            &bind_group_layout,
            "rasterlab color_balance shader",
            "rasterlab color_balance pipeline",
        );
        Self {
            pipeline,
            bind_group_layout,
        }
    }
}

impl ColorSpaceKernel {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let bind_group_layout =
            make_3binding_layout(device, "rasterlab color_space bind group layout");
        let pipeline = make_simple_pipeline(
            device,
            COLOR_SPACE_WGSL,
            &bind_group_layout,
            "rasterlab color_space shader",
            "rasterlab color_space pipeline",
        );
        Self {
            pipeline,
            bind_group_layout,
        }
    }
}

impl DenoiseKernel {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let bind_group_layout = make_3binding_layout(device, "rasterlab denoise bind group layout");
        let pipeline = make_simple_pipeline(
            device,
            DENOISE_WGSL,
            &bind_group_layout,
            "rasterlab denoise shader",
            "rasterlab denoise pipeline",
        );
        Self {
            pipeline,
            bind_group_layout,
        }
    }
}

impl HslPanelKernel {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let bind_group_layout =
            make_3binding_layout(device, "rasterlab hsl_panel bind group layout");
        let pipeline = make_simple_pipeline(
            device,
            HSL_PANEL_WGSL,
            &bind_group_layout,
            "rasterlab hsl_panel shader",
            "rasterlab hsl_panel pipeline",
        );
        Self {
            pipeline,
            bind_group_layout,
        }
    }
}

impl SharpenKernel {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let bind_group_layout = make_3binding_layout(device, "rasterlab sharpen bind group layout");
        let pipeline = make_simple_pipeline(
            device,
            SHARPEN_WGSL,
            &bind_group_layout,
            "rasterlab sharpen shader",
            "rasterlab sharpen pipeline",
        );
        Self {
            pipeline,
            bind_group_layout,
        }
    }
}

impl FauxHdrKernel {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let bind_group_layout =
            make_3binding_layout(device, "rasterlab faux_hdr bind group layout");
        let pipeline = make_simple_pipeline(
            device,
            FAUX_HDR_WGSL,
            &bind_group_layout,
            "rasterlab faux_hdr shader",
            "rasterlab faux_hdr pipeline",
        );
        Self {
            pipeline,
            bind_group_layout,
        }
    }
}

impl ClarityTextureKernel {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let three_bind_layout = make_3binding_layout(device, "rasterlab clarity 3-bind layout");
        let four_bind_layout = make_4binding_layout(device, "rasterlab clarity 4-bind layout");
        let extract_luma_pipeline = make_simple_pipeline(
            device,
            CLARITY_EXTRACT_LUMA_WGSL,
            &three_bind_layout,
            "rasterlab clarity extract_luma shader",
            "rasterlab clarity extract_luma pipeline",
        );
        let box_blur_h_pipeline = make_simple_pipeline(
            device,
            CLARITY_BOX_BLUR_H_WGSL,
            &three_bind_layout,
            "rasterlab clarity box_blur_h shader",
            "rasterlab clarity box_blur_h pipeline",
        );
        let box_blur_v_pipeline = make_simple_pipeline(
            device,
            CLARITY_BOX_BLUR_V_WGSL,
            &three_bind_layout,
            "rasterlab clarity box_blur_v shader",
            "rasterlab clarity box_blur_v pipeline",
        );
        let apply_detail_pipeline = make_simple_pipeline(
            device,
            CLARITY_APPLY_DETAIL_WGSL,
            &four_bind_layout,
            "rasterlab clarity apply_detail shader",
            "rasterlab clarity apply_detail pipeline",
        );
        Self {
            three_bind_layout,
            extract_luma_pipeline,
            box_blur_h_pipeline,
            box_blur_v_pipeline,
            four_bind_layout,
            apply_detail_pipeline,
        }
    }
}

pub(crate) struct NoiseReductionNlmKernel {
    pub(crate) nlm_pipeline: wgpu::ComputePipeline,
    pub(crate) nlm_bind_group_layout: wgpu::BindGroupLayout,
    pub(crate) detail_pipeline: wgpu::ComputePipeline,
    pub(crate) detail_bind_group_layout: wgpu::BindGroupLayout,
}

impl NoiseReductionNlmKernel {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rasterlab noise_reduction_nlm shader"),
            source: wgpu::ShaderSource::Wgsl(NOISE_REDUCTION_NLM_WGSL.into()),
        });
        let nlm_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("rasterlab noise_reduction_nlm bind group layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: false },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            });
        let nlm_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("rasterlab noise_reduction_nlm pipeline layout"),
            bind_group_layouts: &[Some(&nlm_bind_group_layout)],
            immediate_size: 0,
        });
        let nlm_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("rasterlab noise_reduction_nlm pipeline"),
            layout: Some(&nlm_pipeline_layout),
            module: &shader,
            entry_point: Some("nlm_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        let detail_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("rasterlab noise_reduction_detail bind group layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: false },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            });
        let detail_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("rasterlab noise_reduction_detail pipeline layout"),
                bind_group_layouts: &[Some(&detail_bind_group_layout)],
                immediate_size: 0,
            });
        let detail_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("rasterlab noise_reduction_detail pipeline"),
            layout: Some(&detail_pipeline_layout),
            module: &shader,
            entry_point: Some("detail_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        Self {
            nlm_pipeline,
            nlm_bind_group_layout,
            detail_pipeline,
            detail_bind_group_layout,
        }
    }
}
