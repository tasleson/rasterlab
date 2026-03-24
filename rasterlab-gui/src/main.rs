mod app;
mod panels;
mod state;

fn main() -> eframe::Result<()> {
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
