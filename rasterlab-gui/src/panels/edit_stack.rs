//! Edit stack panel — shows all pipeline operations with enable/disable, delete,
//! and drag-to-reorder controls.

use egui::{Color32, RichText, Ui};

use crate::state::AppState;

/// Renders the edit stack panel.
///
/// Returns `(remove_idx, reorder_from_to, toggle_idx)` — mutations to apply
/// after the panel finishes drawing so we don't borrow `state` mutably while
/// iterating.
pub fn ui(ui: &mut Ui, state: &mut AppState) {
    ui.heading("Edit Stack");
    ui.separator();

    // Undo / Redo controls
    ui.horizontal(|ui| {
        let can_undo = state.can_undo();
        let can_redo = state.can_redo();
        if ui.add_enabled(can_undo, egui::Button::new("⟵ Undo")).clicked() {
            state.undo();
        }
        if ui.add_enabled(can_redo, egui::Button::new("Redo ⟶")).clicked() {
            state.redo();
        }
    });

    ui.separator();

    let Some(pipeline) = &state.pipeline else {
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
    let mut remove_idx:  Option<usize>        = None;
    let mut reorder:     Option<(usize, usize)> = None;
    let mut toggle_idx:  Option<usize>        = None;

    for (i, entry) in ops.iter().enumerate() {
        let is_active = i < cursor;
        let desc      = entry.operation.describe();

        // Dimmed rows are in the "redo" area (after the cursor)
        let row_color = if !is_active {
            Color32::from_gray(100)
        } else if entry.enabled {
            Color32::from_rgb(220, 220, 220)
        } else {
            Color32::from_gray(130) // disabled
        };

        ui.horizontal(|ui| {
            // ── Drag handle ──────────────────────────────────────────────
            ui.label(RichText::new("⣿").color(Color32::from_gray(80)));

            // ── Enable / disable checkbox ─────────────────────────────
            let mut enabled = entry.enabled;
            if ui.checkbox(&mut enabled, "").changed() {
                toggle_idx = Some(i);
            }

            // ── Operation name + description ──────────────────────────
            let text = RichText::new(format!("{}.  {}", i + 1, desc))
                .color(row_color)
                .monospace();
            ui.label(text);

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // ── Delete ───────────────────────────────────────────
                if ui
                    .button(RichText::new("✕").color(Color32::from_rgb(220, 80, 80)))
                    .on_hover_text("Remove this operation")
                    .clicked()
                {
                    remove_idx = Some(i);
                }

                // ── Move up ──────────────────────────────────────────
                if i > 0
                    && ui
                        .button("▲")
                        .on_hover_text("Move up")
                        .clicked()
                {
                    reorder = Some((i, i - 1));
                }

                // ── Move down ────────────────────────────────────────
                if i + 1 < ops.len()
                    && ui
                        .button("▼")
                        .on_hover_text("Move down")
                        .clicked()
                {
                    reorder = Some((i, i + 1));
                }
            });
        });

        ui.separator();
    }

    // ── Apply deferred mutations ──────────────────────────────────────────
    if let Some(idx) = remove_idx {
        state.remove_op(idx);
    } else if let Some((from, to)) = reorder {
        state.reorder_op(from, to);
    } else if let Some(idx) = toggle_idx {
        state.toggle_op(idx);
    }
}
