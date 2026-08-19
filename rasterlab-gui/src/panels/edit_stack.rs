//! Edit stack panel — shows all pipeline operations with enable/disable, delete,
//! and drag-to-reorder controls.  A tab bar at the top lets the user switch
//! between virtual copies of the same source image.

use egui::{Color32, RichText, Ui};

use crate::state::AppState;

/// Renders the edit stack panel.
pub fn ui(ui: &mut Ui, state: &mut AppState) {
    ui.heading("Edit Stack");
    ui.separator();

    let lock = state.editing.is_some();

    // ── Virtual copy tab bar ──────────────────────────────────────────────
    ui.add_enabled_ui(!lock, |ui| {
        virtual_copy_tabs(ui, state);
    });

    ui.separator();

    // ── Undo / Redo controls ──────────────────────────────────────────────
    ui.horizontal(|ui| {
        let can_undo = state.can_undo() && !lock;
        let can_redo = state.can_redo() && !lock;
        if ui
            .add_enabled(can_undo, egui::Button::new("⟵ Undo"))
            .clicked()
        {
            state.undo();
        }
        if ui
            .add_enabled(can_redo, egui::Button::new("Redo ⟶"))
            .clicked()
        {
            state.redo();
        }
    });

    ui.separator();

    // ── Rename popup (shown when rename_pending is set) ───────────────────
    rename_popup(ui, state);

    // ── Op list ──────────────────────────────────────────────────────────
    let Some(pipeline) = state.pipeline() else {
        ui.label(
            RichText::new("No image loaded")
                .color(Color32::from_gray(150))
                .italics(),
        );
        return;
    };

    let ops = pipeline.ops();
    if ops.is_empty() {
        ui.label(
            RichText::new("(no edits yet)")
                .color(Color32::from_gray(150))
                .italics(),
        );
        return;
    }

    let cursor = pipeline.cursor();
    let mut remove_idx: Option<usize> = None;
    let mut reorder: Option<(usize, usize)> = None;
    let mut toggle_idx: Option<usize> = None;
    let mut edit_idx: Option<usize> = None;

    let editing = state.editing;

    for (i, entry) in ops.iter().enumerate() {
        let is_active = i < cursor;
        let desc = entry.operation.describe();
        let (name, details) = split_description(&desc);
        let is_editing_this = editing.is_some_and(|s| s.op_index == i);
        let editable = crate::state::editing_tool_for_op(entry.operation.as_ref()).is_some();
        // The pipeline entry is temporarily disabled while its tool preview
        // substitutes for it.  Keep showing the committed enabled state so
        // this implementation detail does not look like a user toggle.
        let displayed_enabled = editing
            .filter(|session| session.op_index == i)
            .map_or(entry.enabled, |session| session.was_enabled);

        // Dimmed rows are in the "redo" area (after the cursor)
        let row_color = if !is_active {
            ui.visuals().text_color().gamma_multiply(0.35)
        } else if displayed_enabled {
            ui.visuals().text_color()
        } else {
            ui.visuals().text_color().gamma_multiply(0.55) // disabled
        };

        // Stack mutations are locked for the whole edit session. In
        // particular, the op under edit is temporarily unchecked for preview
        // purposes; letting that checkbox or its delete/reorder buttons remain
        // interactive would turn temporary state into a real stack mutation.
        // Its pencil remains independently enabled below so edit mode can end.
        let row_enabled = editing.is_none();

        ui.horizontal(|ui| {
            // ── Drag handle ──────────────────────────────────────────────
            ui.label(RichText::new("⣿").color(Color32::from_gray(80)));

            ui.add_enabled_ui(row_enabled, |ui| {
                // ── Enable / disable checkbox ─────────────────────────────
                let mut enabled = displayed_enabled;
                if ui.checkbox(&mut enabled, "").changed() {
                    toggle_idx = Some(i);
                }
            });

            // ── Operation name + description ──────────────────────────
            let name_color = if is_editing_this {
                Color32::from_rgb(90, 160, 255)
            } else {
                row_color
            };
            let prefix = if is_editing_this { "✎ " } else { "" };
            let text = RichText::new(format!("{}{}.  {}", prefix, i + 1, name))
                .color(name_color)
                .monospace();
            ui.label(text);

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_enabled_ui(row_enabled, |ui| {
                    // ── Delete ───────────────────────────────────────────
                    if ui
                        .button(RichText::new("🗙").color(Color32::from_rgb(220, 80, 80)))
                        .on_hover_text("Remove this operation")
                        .clicked()
                    {
                        remove_idx = Some(i);
                    }

                    // ── Move up ──────────────────────────────────────────
                    if i > 0 && ui.button("⏶").on_hover_text("Move up").clicked() {
                        reorder = Some((i, i - 1));
                    }

                    // ── Move down ────────────────────────────────────────
                    if i + 1 < ops.len() && ui.button("⏷").on_hover_text("Move down").clicked() {
                        reorder = Some((i, i + 1));
                    }
                });

                // ── Edit (pencil) ────────────────────────────────────
                // Keep the active row highlighted and clickable, but treat
                // another click as the same edit request. Apply and the
                // banner's Cancel Edit button are the explicit ways to leave
                // the session, so a double-click cannot rapidly exit/re-enter.
                let pencil_enabled =
                    is_editing_this || (editing.is_none() && editable && is_active);
                let pencil_color = if is_editing_this {
                    Color32::from_rgb(90, 160, 255)
                } else {
                    ui.visuals().text_color()
                };
                let hover = if is_editing_this {
                    "Already editing this operation"
                } else if !editable {
                    "This op type cannot be edited"
                } else {
                    "Edit this operation"
                };
                if ui
                    .add_enabled(
                        pencil_enabled,
                        egui::Button::new(RichText::new("📝").color(pencil_color)),
                    )
                    .on_hover_text(hover)
                    .clicked()
                {
                    edit_idx = Some(i);
                }
            });
        });

        // Parameter-heavy descriptions used to share the header line and
        // force the whole side panel to their preferred width. Keep the
        // operation name compact and let its details wrap within the panel.
        if let Some(details) = details {
            ui.horizontal(|ui| {
                ui.add_space(44.0);
                ui.add(
                    egui::Label::new(
                        RichText::new(details)
                            .color(row_color.gamma_multiply(0.75))
                            .monospace()
                            .small(),
                    )
                    .wrap(),
                );
            });
        }

        ui.separator();
    }

    // ── Apply deferred mutations ──────────────────────────────────────────
    if let Some(idx) = edit_idx {
        state.begin_edit(idx);
    } else if let Some(idx) = remove_idx {
        state.remove_op(idx);
    } else if let Some((from, to)) = reorder {
        state.reorder_op(from, to);
    } else if let Some(idx) = toggle_idx {
        state.toggle_op(idx);
    }
}

// ── Tab bar ──────────────────────────────────────────────────────────────────

/// Split the conventional `Name  details` operation description into the two
/// lines used by the edit stack. Descriptions without details remain a single
/// header line, including descriptions supplied by plugins.
fn split_description(description: &str) -> (&str, Option<&str>) {
    match description.split_once("  ") {
        Some((name, details)) if !name.trim().is_empty() && !details.trim().is_empty() => {
            (name.trim(), Some(details.trim()))
        }
        _ => (description, None),
    }
}

fn virtual_copy_tabs(ui: &mut Ui, state: &mut AppState) {
    let Some(store) = &state.copies else {
        return;
    };

    let count = store.len();
    let active = store.active_index();
    let names: Vec<String> = store.names().map(String::from).collect();

    let mut switch_to: Option<usize> = None;
    let mut remove_idx: Option<usize> = None;
    let mut rename_idx: Option<(usize, egui::Pos2)> = None;
    let mut add_copy = false;
    let mut duplicate = false;

    ui.horizontal(|ui| {
        for (i, name) in names.iter().enumerate() {
            let selected = i == active;

            let label_color = if selected {
                Color32::WHITE
            } else {
                Color32::from_gray(170)
            };

            let resp = ui.add(
                egui::Button::new(RichText::new(name).color(label_color))
                    .selected(selected)
                    .min_size(egui::vec2(0.0, 0.0)),
            );

            if resp.clicked() && !selected {
                switch_to = Some(i);
            }

            let tab_pos = resp.rect.left_bottom();
            resp.context_menu(|ui| {
                if ui.button("Rename…").clicked() {
                    rename_idx = Some((i, tab_pos));
                    ui.close();
                }
                if ui.button("Duplicate").clicked() {
                    duplicate = true;
                    ui.close();
                }
                if count > 1 && ui.button("Delete").clicked() {
                    remove_idx = Some(i);
                    ui.close();
                }
            });
        }

        if ui.button("+").on_hover_text("Add virtual copy").clicked() {
            add_copy = true;
        }
    });

    // ── Deferred mutations ────────────────────────────────────────────────
    if let Some(idx) = switch_to {
        state.switch_copy(idx);
    }
    if let Some(idx) = remove_idx {
        state.remove_virtual_copy(idx);
    }
    if add_copy {
        state.add_virtual_copy();
    }
    if duplicate {
        state.duplicate_virtual_copy();
    }
    if let Some((idx, pos)) = rename_idx {
        // Seed the rename dialog with the current name.
        let current = state
            .copies
            .as_ref()
            .and_then(|s| s.names().nth(idx))
            .unwrap_or("")
            .to_string();
        state.rename_pending = Some((idx, current, pos));
    }
}

// ── Inline rename dialog ──────────────────────────────────────────────────────

fn rename_popup(ui: &mut Ui, state: &mut AppState) {
    let Some((idx, _, anchor)) = state.rename_pending.clone() else {
        return;
    };

    let mut commit_name: Option<String> = None;
    let mut do_cancel = false;
    let mut open = true;

    egui::Window::new("Rename copy")
        .collapsible(false)
        .resizable(false)
        .fixed_pos(anchor)
        .open(&mut open)
        .show(ui.ctx(), |ui| {
            // Borrow the text field, edit it, then drop the borrow before
            // the inner closures so `state` is not held across button checks.
            let (name, response) = {
                let Some((_, text, _)) = &mut state.rename_pending else {
                    return;
                };
                let resp = ui.text_edit_singleline(text);
                (text.clone(), resp)
            };

            let pressed_enter =
                response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));

            ui.horizontal(|ui| {
                if ui.button("OK").clicked() || pressed_enter {
                    commit_name = Some(name.clone());
                }
                if ui.button("Cancel").clicked() {
                    do_cancel = true;
                }
            });
        });

    if let Some(name) = commit_name {
        state.rename_virtual_copy(idx, name);
        state.rename_pending = None;
    } else if do_cancel || !open {
        state.rename_pending = None;
    }
}

#[cfg(test)]
mod tests {
    use super::split_description;

    #[test]
    fn splits_operation_name_from_details() {
        assert_eq!(
            split_description("Channel Levels  R 0.00/1.00/1.00  G 0.00/1.00/1.00"),
            ("Channel Levels", Some("R 0.00/1.00/1.00  G 0.00/1.00/1.00"))
        );
    }

    #[test]
    fn leaves_name_only_descriptions_on_one_line() {
        assert_eq!(split_description("Perspective"), ("Perspective", None));
    }
}
