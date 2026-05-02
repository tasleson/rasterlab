use egui::Ui;

use crate::state::{EditSession, EditingTool};

/// Wrap `CollapsingHeader::new` so every header in this panel honours the
/// one-frame force-open flag that drives Expand-All / Collapse-All.
pub(super) fn header(
    force: Option<bool>,
    title: impl Into<egui::WidgetText>,
) -> egui::CollapsingHeader {
    let h = egui::CollapsingHeader::new(title);
    match force {
        Some(open) => h.open(Some(open)),
        None => h,
    }
}

/// Like `header`, but when `editing` matches `this_tool` the title is rendered
/// bold and the section is forced open so the user can immediately find the
/// tool they just started editing from the Edit Stack.
pub(super) fn header_for_tool(
    force: Option<bool>,
    title: &str,
    editing: Option<EditSession>,
    this_tool: EditingTool,
) -> egui::CollapsingHeader {
    let is_active = editing.is_some_and(|s| s.tool == this_tool);
    let widget_text: egui::WidgetText = if is_active {
        egui::RichText::new(title).strong().into()
    } else {
        title.into()
    };
    let h = egui::CollapsingHeader::new(widget_text);
    let effective_force = if is_active { Some(true) } else { force };
    match effective_force {
        Some(open) => h.open(Some(open)),
        None => h,
    }
}

pub(super) fn path_list_ui(ui: &mut Ui, paths: &[String], id_salt: &str) -> Option<usize> {
    let mut remove_idx: Option<usize> = None;
    egui::ScrollArea::vertical()
        .max_height(120.0)
        .id_salt(id_salt)
        .show(ui, |ui| {
            for (i, path) in paths.iter().enumerate() {
                ui.horizontal(|ui| {
                    let name = std::path::Path::new(path)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.clone());
                    ui.label(format!("{}. {}", i + 1, name));
                    if ui.small_button("✕").clicked() {
                        remove_idx = Some(i);
                    }
                });
            }
        });
    remove_idx
}
