mod app;
#[cfg(not(target_arch = "wasm32"))]
mod file_chooser;
mod panels;
mod prefs;
mod state;

fn main() -> eframe::Result<()> {
    // Optional positional argument: path to an image file to open on startup.
    let initial_file: Option<std::path::PathBuf> =
        std::env::args_os().nth(1).map(std::path::PathBuf::from);

    // Rayon worker threads default to 8 MiB stack — not enough for the JPEG/PNG
    // decode chains + image-processing kernels.  Configure before first use.
    rayon::ThreadPoolBuilder::new()
        .stack_size(32 * 1024 * 1024) // 32 MiB per worker
        .build_global()
        .expect("failed to build rayon thread pool");
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("RasterLab")
            .with_inner_size([1400.0, 900.0])
            .with_min_inner_size([900.0, 600.0]),
        wgpu_options: eframe::egui_wgpu::WgpuConfiguration {
            wgpu_setup: eframe::egui_wgpu::WgpuSetup::CreateNew(
                eframe::egui_wgpu::WgpuSetupCreateNew {
                    device_descriptor: std::sync::Arc::new(|adapter| {
                        let base_limits = if adapter.get_info().backend == eframe::wgpu::Backend::Gl
                        {
                            eframe::wgpu::Limits::downlevel_webgl2_defaults()
                        } else {
                            eframe::wgpu::Limits::default()
                        };
                        eframe::wgpu::DeviceDescriptor {
                            label: Some("egui wgpu device"),
                            required_limits: eframe::wgpu::Limits {
                                max_texture_dimension_2d: 8192,
                                // Cap to what the driver exposes; some Linux drivers
                                // report fewer than wgpu's default of 8.
                                max_color_attachments: adapter.limits().max_color_attachments,
                                ..base_limits
                            },
                            ..Default::default()
                        }
                    }),
                    ..eframe::egui_wgpu::WgpuSetupCreateNew::without_display_handle()
                },
            ),
            ..Default::default()
        },
        ..Default::default()
    };

    eframe::run_native(
        "RasterLab",
        options,
        Box::new(move |cc| Ok(Box::new(app::RasterLabApp::new(cc, initial_file)))),
    )
}
