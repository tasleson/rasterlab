//! Tools panel — inputs for adding operations to the pipeline.

pub mod shared;
pub mod tool_trait;

pub mod auto_enhance;
pub mod blur;
pub mod brightness_contrast;
pub mod bw;
pub mod clarity_texture;
pub mod color_balance;
pub mod color_space;
pub mod crop;
pub mod curves;
pub mod denoise;
pub mod export;
pub mod faux_hdr;
pub mod focus_stack;
pub mod grain;
pub mod hdr_merge;
pub mod heal;
pub mod highlights_shadows;
pub mod hsl;
pub mod hue_shift;
pub mod levels;
pub mod looks;
pub mod lut;
pub mod masking;
pub mod metadata;
pub mod noise_reduction;
pub mod panorama;
pub mod perspective;
pub mod resize;
pub mod rotate;
pub mod saturation;
pub mod sepia;
pub mod shadow_exposure;
pub mod sharpen;
pub mod split_tone;
pub mod straighten;
pub mod vibrance;
pub mod vignette;
pub mod white_balance;

use egui::{Color32, Stroke, Ui};

use self::shared::header_for_tool;
use self::tool_trait::{ToolAction, ToolUiCtx};
use crate::state::AppState;

pub fn ui(ui: &mut Ui, state: &mut AppState) {
    ui.horizontal(|ui| {
        ui.heading("Tools");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .small_button("Collapse all")
                .on_hover_text("Collapse every tool section")
                .clicked()
            {
                state.tools_force_open = Some(false);
            }
            if ui
                .small_button("Expand all")
                .on_hover_text("Expand every tool section")
                .clicked()
            {
                state.tools_force_open = Some(true);
            }
        });
    });
    ui.separator();

    // Edit-session banner
    if let Some(session) = state.editing {
        let frame = egui::Frame::group(ui.style())
            .stroke(Stroke::new(2.0, Color32::from_rgb(90, 160, 255)))
            .fill(Color32::from_rgb(20, 35, 60));
        frame.show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!("✎ Editing op #{}", session.op_index + 1))
                        .color(Color32::from_rgb(140, 190, 255))
                        .strong(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Cancel Edit").clicked() {
                        state.end_edit();
                    }
                });
            });
            ui.label(
                egui::RichText::new(
                    "Other tools are locked. Adjust this tool, then press its Apply button \
                     to save changes, or Cancel Edit to discard.",
                )
                .color(Color32::from_gray(180))
                .small(),
            );
        });
        ui.separator();
    }

    let has_image = state.pipeline().is_some();

    // Auto Enhance (always first)
    auto_enhance::ui(ui, state, has_image);
    ui.separator();

    // Looks (always second)
    looks::ui(ui, state, has_image);

    ui.add_space(4.0);
    ui.separator();
    ui.add_space(4.0);

    // Trait-based tools — alphabetical by display name
    let tool_count = state.tools.tools.len();
    for idx in 0..tool_count {
        render_tool(ui, state, idx);
        ui.separator();
    }

    // Non-trait panels
    export::ui(ui, state, has_image);
    ui.separator();

    masking::ui(ui, state, has_image);
    ui.separator();

    metadata::ui(ui, state, has_image);

    // Clear one-frame force-open flag
    state.tools_force_open = None;
}

fn render_tool(ui: &mut Ui, state: &mut AppState, idx: usize) {
    // Take the tool out so we can borrow state freely for building ctx.
    let mut tool = std::mem::replace(&mut state.tools.tools[idx], Box::new(PlaceholderTool));

    let has_image = state.pipeline().is_some();
    let editing = state.editing;
    let force_open = state.tools_force_open;

    // Header with collapsing
    let id = tool.id();
    let display_name = tool.display_name();
    let editing_tool = tool.editing_tool();
    let default_open = state.prefs.is_tool_open(id);

    let header = if let Some(et) = editing_tool {
        header_for_tool(force_open, display_name, editing, et)
    } else {
        shared::header(force_open, display_name)
    };

    let resp = header
        .id_salt(id)
        .default_open(default_open)
        .show(ui, |ui| {
            // Disable if another tool is being edited
            if let Some(et) = editing_tool {
                if editing.is_some_and(|s| s.tool != et) {
                    ui.disable();
                }
            } else if editing.is_some() {
                ui.disable();
            }

            let ctx = ToolUiCtx {
                has_image,
                editing,
                histogram: state.histogram.as_ref(),
                last_path: state.last_path.as_deref(),
                nr_in_flight: state.nr_in_flight(),
                source_dims: state
                    .pipeline()
                    .map(|p| (p.source().width, p.source().height)),
                rendered_dims: state.rendered.as_ref().map(|r| (r.width, r.height)),
                rendered_scale: state.rendered_scale,
                force_open,
            };
            tool.render_ui(ui, &ctx)
        });

    if resp.header_response.clicked() {
        state.prefs.tools_open.insert(id.to_string(), !default_open);
    }

    // Put the tool back before handling action
    state.tools.tools[idx] = tool;

    // Handle the returned action
    let action = resp.body_returned.unwrap_or(ToolAction::None);
    match action {
        ToolAction::None => {}
        ToolAction::RequestRender => {
            state.request_render();
        }
        ToolAction::PushOp(op) => {
            state.push_op(op);
        }
        ToolAction::PushOps(ops) => {
            state.cancel_all_previews();
            if let Some(store) = &mut state.copies {
                let p = store.active_pipeline_mut();
                for op in ops {
                    p.push_op(op);
                }
            }
            if state.copies.is_some() {
                state.mark_dirty();
                state.request_render();
            }
        }
        ToolAction::RequestFileDialog(kind) => {
            state.tools.pending_dialog = Some(kind);
        }
    }
}

/// Minimal placeholder used during the `.take()` borrow-splitting pattern.
struct PlaceholderTool;
impl tool_trait::Tool for PlaceholderTool {
    fn id(&self) -> &'static str {
        "_placeholder"
    }
    fn display_name(&self) -> &'static str {
        ""
    }
    fn render_ui(&mut self, _ui: &mut egui::Ui, _ctx: &ToolUiCtx<'_>) -> ToolAction {
        ToolAction::None
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
