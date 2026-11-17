// On Windows release builds, don't open a console window behind the GUI.
#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

fn main() -> rasterlab_gui::eframe::Result<()> {
    // Optional positional argument: path to an image file to open on startup.
    let initial_file = std::env::args_os().nth(1).map(std::path::PathBuf::from);
    rasterlab_gui::run(rasterlab_gui::AppOptions {
        initial_file,
        extra_tools: Vec::new(),
    })
}
