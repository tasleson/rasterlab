//! The RasterLab desktop application as a library.
//!
//! The `rasterlab-gui` binary is a thin wrapper around [`run`].

use std::path::PathBuf;

mod app;
mod autosave;
#[cfg(not(target_arch = "wasm32"))]
mod file_chooser;
mod file_size;
mod panels;
mod prefs;
mod state;

/// The `eframe` this crate is built against, re-exported so a binary calling
/// [`run`] can name [`eframe::Result`] without tracking the version itself.
pub use eframe;

/// What [`run`] needs to know before the window opens.
#[derive(Debug, Default)]
pub struct AppOptions {
    /// Image or project to open on startup, typically the first positional
    /// command-line argument.
    pub initial_file: Option<PathBuf>,
}

/// Open the RasterLab window and run the application until it is closed.
///
/// This sets up everything the application needs — the shared rayon pool, the
/// wgpu renderer and the window icon — so a binary only has to build its
/// [`AppOptions`] and call it from `main`.
pub fn run(options: AppOptions) -> eframe::Result<()> {
    let AppOptions { initial_file } = options;
    let icon =
        eframe::icon_data::from_png_bytes(include_bytes!("../../assets/icon/rasterlab_icon.png"))
            .expect("embedded app icon must be a valid PNG");

    rasterlab_render::init_rayon_pool();
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("RasterLab")
            // Wayland app_id / X11 WM_CLASS.  Must match the installed
            // desktop file name (io.github.tasleson.RasterLab.desktop) so
            // the shell can show the proper name and icon in the switcher.
            .with_app_id("io.github.tasleson.RasterLab")
            .with_inner_size([1400.0, 900.0])
            .with_min_inner_size([900.0, 600.0])
            .with_icon(icon),
        renderer: eframe::Renderer::Wgpu,
        wgpu_options: eframe::egui_wgpu::WgpuConfiguration {
            wgpu_setup: eframe::egui_wgpu::WgpuSetup::CreateNew(
                eframe::egui_wgpu::WgpuSetupCreateNew {
                    device_descriptor: std::sync::Arc::new(|adapter| {
                        // Use the adapter's own limits directly.  wgpu's
                        // Limits::default() requests values (e.g. 8 color
                        // attachments, 8 storage buffers/stage) that some
                        // Linux drivers don't support.  Requesting exactly
                        // what the adapter reports avoids LimitsExceeded
                        // errors on every such field.
                        let mut limits = adapter.limits();
                        // egui needs at least 8192 for 4K+ display support.
                        limits.max_texture_dimension_2d = limits.max_texture_dimension_2d.max(8192);
                        eframe::wgpu::DeviceDescriptor {
                            label: Some("egui wgpu device"),
                            required_limits: limits,
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
        native_options,
        Box::new(move |cc| Ok(Box::new(app::RasterLabApp::new(cc, initial_file)))),
    )
}
