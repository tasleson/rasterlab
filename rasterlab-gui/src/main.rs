mod app;
mod panels;
mod state;

fn main() -> eframe::Result<()> {
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
        ..Default::default()
    };

    eframe::run_native(
        "RasterLab",
        options,
        Box::new(|cc| Ok(Box::new(app::RasterLabApp::new(cc)))),
    )
}
