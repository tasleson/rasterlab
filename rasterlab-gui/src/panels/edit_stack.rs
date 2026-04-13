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
        let is_editing_this = editing.is_some_and(|s| s.op_index == i);
        let editable = entry.operation.as_any().is_some();

        // Dimmed rows are in the "redo" area (after the cursor)
        let row_color = if !is_active {
            ui.visuals().text_color().gamma_multiply(0.35)
        } else if entry.enabled {
            ui.visuals().text_color()
        } else {
            ui.visuals().text_color().gamma_multiply(0.55) // disabled
        };

        // While editing a different op, disable this row so the user can only
        // interact with the op under edit (its own pencil still works, to exit).
        let row_enabled = editing.is_none() || is_editing_this;

        ui.horizontal(|ui| {
            // ── Drag handle ──────────────────────────────────────────────
            ui.label(RichText::new("⣿").color(Color32::from_gray(80)));

            ui.add_enabled_ui(row_enabled, |ui| {
                // ── Enable / disable checkbox ─────────────────────────────
                let mut enabled = entry.enabled;
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
            let text = RichText::new(format!("{}{}.  {}", prefix, i + 1, desc))
                .color(name_color)
                .monospace();
            ui.label(text);

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_enabled_ui(row_enabled, |ui| {
                    // ── Delete ───────────────────────────────────────────
                    if ui
                        .button(RichText::new("✕").color(Color32::from_rgb(220, 80, 80)))
                        .on_hover_text("Remove this operation")
                        .clicked()
                    {
                        remove_idx = Some(i);
                    }

                    // ── Move up ──────────────────────────────────────────
                    if i > 0 && ui.button("▲").on_hover_text("Move up").clicked() {
                        reorder = Some((i, i - 1));
                    }

                    // ── Move down ────────────────────────────────────────
                    if i + 1 < ops.len() && ui.button("▼").on_hover_text("Move down").clicked() {
                        reorder = Some((i, i + 1));
                    }
                });

                // ── Edit (pencil) ────────────────────────────────────
                // Always enabled for the row under edit (so the user can
                // exit the session); otherwise only when no other edit is
                // active and the op type supports editing.
                let pencil_enabled =
                    is_editing_this || (editing.is_none() && editable && is_active);
                let pencil_color = if is_editing_this {
                    Color32::from_rgb(90, 160, 255)
                } else {
                    ui.visuals().text_color()
                };
                let hover = if is_editing_this {
                    "Exit edit mode"
                } else if !editable {
                    "This op type cannot be edited"
                } else {
                    "Edit this operation"
                };
                if ui
                    .add_enabled(
                        pencil_enabled,
                        egui::Button::new(RichText::new("✎").color(pencil_color)),
                    )
                    .on_hover_text(hover)
                    .clicked()
                {
                    edit_idx = Some(i);
                }
            });
        });

        ui.separator();
    }

    // ── Apply deferred mutations ──────────────────────────────────────────
    if let Some(idx) = edit_idx {
        if state.editing.is_some_and(|s| s.op_index == idx) {
            state.end_edit();
        } else {
            state.begin_edit(idx);
        }
    } else if let Some(idx) = remove_idx {
        state.remove_op(idx);
    } else if let Some((from, to)) = reorder {
        state.reorder_op(from, to);
    } else if let Some(idx) = toggle_idx {
        state.toggle_op(idx);
    }
}

// ── Tab bar ──────────────────────────────────────────────────────────────────

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
