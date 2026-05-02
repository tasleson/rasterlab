# RasterLab refactor plan

A sequence of contained, low-risk refactors to take pressure off the largest
files and make future work easier. Each step is self-contained: clear context
between steps, open this file, follow the next one.

## Status

- [x] **Step 1 — Golden-image regression tests** (commit `2dee981`)
- [x] **Step 2 — Split `panels/tools.rs` into per-tool files** (commit `b4378f5`)
- [ ] **Step 3 — Extract `rasterlab-render` crate**
- [ ] **Step 4 — `Tool` trait abstraction**
- [ ] **Step 5 — Collapse the three file-dialog stacks**

## Conventions for every step

- **Pre-commit checks (hard requirement from `CLAUDE.md`):**
  `cargo fmt && cargo clippy && cargo bench && cargo build --release`.
  For test-only or pure-rename commits, `cargo bench` may be skipped if no
  production code changed — note that explicitly in the commit body.
- **Safety net:** `cargo test --package rasterlab-core --test golden` must
  stay green after every commit. If you intentionally change op output, run
  with `RASTERLAB_GOLDEN_UPDATE=1` and review the diff.
- **Tool-ordering rule (`CLAUDE.md`):** Auto Enhance first, Looks second, all
  others strictly alphabetical by display name. Mirror in `README.md` "Supported
  operations".
- **Commit style:** conventional commits (`feat(scope):`, `fix(scope):`,
  `refactor(scope):`, `test(scope):`). One-line subject, body explains the
  why. End with `Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>`.
- **Stage by name** — never `git add -A`. The working tree has unrelated WIP
  (perspective sliders, library work, untracked images). Touch only the files
  this step is about.
- **No behavior change inside a refactor commit.** If a step needs a behavior
  change to land, split it into a separate commit before/after.

## Repo cheat sheet

| Thing | Path |
|---|---|
| Workspace root | `Cargo.toml` (members listed) |
| Core image-processing ops | `rasterlab-core/src/ops/` (36 modules, flat) |
| `Operation` trait | `rasterlab-core/src/traits/operation.rs` |
| Pipeline | `rasterlab-core/src/pipeline.rs` |
| Golden tests | `rasterlab-core/tests/golden.rs` |
| GUI render thread + state | `rasterlab-gui/src/state/app_state.rs` (~2663 lines) |
| Per-tool fields blob | `rasterlab-gui/src/state/tool_state.rs` (~714 lines) |
| Tool panel UI | `rasterlab-gui/src/panels/tools.rs` (~3601 lines) |
| Canvas / texture upload | `rasterlab-gui/src/panels/canvas.rs` (~1889 lines) |
| File-dialog code | `rasterlab-gui/src/file_chooser.rs` + deps `rfd`, `egui-file-dialog` |
| Memory / preferences | `~/.claude/projects/-Users-tony-rasterlab/memory/MEMORY.md` |

---

## Step 2 — Split `panels/tools.rs` into per-tool files

**Goal:** turn one 3601-line file into one file per tool. **No behavior change.**
This is purely mechanical and sets up step 4 (trait abstraction).

### Current shape

`rasterlab-gui/src/panels/tools.rs` contains the entire tool panel UI: the
top-level dispatch (which tool is selected) plus a `ui_<tool>` function (or
inline block) for every tool — Crop, Rotate, Straighten, Sharpen, Clarity/
Texture, Sepia, B&W, Curves, Levels, HSL, Highlights/Shadows, Shadow Exposure,
White Balance, Color Balance, Saturation, Vibrance, Hue Shift, Split Tone,
Vignette, Grain, Blur, Resize, Noise Reduction, Denoise, Faux HDR, HDR Merge,
Panorama, Focus Stack, LUT, Heal, Perspective, Mask, Auto Enhance, Looks, etc.

`ToolState` (in `rasterlab-gui/src/state/tool_state.rs`) holds all the per-tool
fields and `*_preview_active` booleans the panel reads/writes. **Do not change
`ToolState` in this step** — only move UI code.

### Plan

1. Read `rasterlab-gui/src/panels/tools.rs` end-to-end and inventory every
   per-tool function/block. Make a list.
2. Create `rasterlab-gui/src/panels/tools/` directory with `mod.rs`.
3. For each tool, create `panels/tools/<snake_case_tool>.rs` and move its
   `ui_*` function (and any tool-private helpers) verbatim. Keep visibility
   the smallest that compiles (`pub(super)` is usually right).
4. The old `tools.rs` becomes `panels/tools/mod.rs` containing only the
   top-level dispatch (match on selected `EditingTool`), plus `mod` and `use`
   for each per-tool file.
5. Update `panels/mod.rs` if it referenced `tools` as a file rather than a
   module — rust handles either, but check imports.
6. Keep file ordering inside `mod.rs` matching the **tool ordering rule**
   (Auto Enhance, Looks, then alphabetical).

### Acceptance criteria

- `cargo build --release` succeeds.
- `cargo clippy` clean.
- Golden tests still pass.
- GUI launches, every tool is selectable, sliders move, previews fire, Apply
  commits to the edit stack — same as before. (Spot-check 5 tools across
  geometric / color / detail categories.)
- `panels/tools.rs` no longer exists; `panels/tools/mod.rs` is small (say
  <300 lines) and the largest per-tool file is <300 lines.

### Risks / watch-outs

- Per-tool helper functions (e.g. preset buttons, format helpers) shared by
  multiple tools should move to `panels/tools/shared.rs`, not be duplicated.
- Watch for `use` statements at the top of `tools.rs` — split them so each
  file imports only what it uses (clippy will flag unused imports).
- If a tool's UI mutates `AppState` directly (not just `ToolState`), keep the
  signature identical when moving — don't "improve" it here.

### Commit shape

One commit, message like:

```
refactor(gui): split tools.rs into per-tool files under panels/tools/

Pure move — no behavior change. Each tool's ui_* function now lives in its
own file (panels/tools/<tool>.rs); panels/tools/mod.rs keeps the top-level
dispatch. Sets up the planned Tool trait extraction in step 4 of REFACTOR.md.
```

`cargo bench` can be skipped (note in commit body) — no production code paths
changed.

---

## Step 3 — Extract `rasterlab-render` crate

**Goal:** move the render thread, message types, and pipeline cache out of the
GUI crate so rendering can be tested headlessly and `app_state.rs` shrinks.

### Current shape

`rasterlab-gui/src/state/app_state.rs` (~2663 lines) owns:

- `BgMessage` enum (`ImageLoaded`, `ProjectLoaded`, `RenderComplete`, …)
- The background render-thread spawn and channel plumbing (`mpsc`).
- Render-result handling: histogram side-channel, intermediates cache,
  `cache_gen`, downsampled previews, viewport overlay rendering.
- The 32 MiB-stack rayon pool init (search for `stack_size`).

`rasterlab-core/src/render_cache.rs` and `rasterlab-core/src/pipeline.rs`
already exist — the crate boundary is roughly: *core* knows how to apply ops;
*GUI* knows how to drive a background thread and feed an egui texture.
`rasterlab-render` would sit between them and own "given a pipeline + cancel
token, produce an image + histogram, with caching."

### Plan

1. Create `rasterlab-render/` crate (add to `[workspace] members` in root
   `Cargo.toml`).
2. Cargo.toml: depend on `rasterlab-core`, `rayon`, `image`, `serde` as
   needed. **No `egui` / `eframe` dep** — that's the point.
3. Move into `rasterlab-render/src/`:
   - `BgMessage` (or rename to `RenderMessage` / `RenderResult`).
   - Render-thread spawn + channel plumbing.
   - Step cache / intermediates handling (some lives in `core::render_cache`
     today — leave that, only move what's GUI-side now).
   - Rayon pool init helper.
4. In `rasterlab-gui/src/state/app_state.rs`, replace the inlined render
   plumbing with `rasterlab_render::Renderer::spawn(...)` (or whatever shape
   you settle on). The egui `Context` for repaint requests stays in the GUI
   crate — pass a `repaint: Arc<dyn Fn() + Send + Sync>` into the renderer
   so it stays UI-framework-agnostic.
5. Add a headless integration test in `rasterlab-render/tests/` that builds
   a small pipeline, renders it, and asserts the result hash. (Reuse the
   golden-test pattern.)

### Acceptance criteria

- `rasterlab-render` builds standalone (`cargo build -p rasterlab-render`).
- `rasterlab-render` does **not** depend on `egui` or `eframe`.
- `app_state.rs` shrinks by at least ~500 lines.
- GUI behavior unchanged: open image, edit, see live preview, full-res render
  arrives, histogram updates, viewport overlay still works.
- Golden tests still pass.
- New integration test in `rasterlab-render` passes.

### Risks / watch-outs

- `BgMessage::RenderComplete` carries `intermediates`, `start_index`,
  `cache_gen`, `is_preview`, `overlay_rect` — every field is load-bearing.
  Don't drop fields "to clean up" — that's behavior change. Move them as-is.
- Cancellation: `rasterlab_core::cancel` already exists. Make sure the
  renderer plumbs it through the same way the inlined code did.
- The egui repaint request (`ctx.request_repaint()`) is the hot edge between
  UI and renderer — abstract it through a callback, not a direct dep.
- Don't try to redesign the cache here. Step 3 is *extract*, not *redesign*.

### Commit shape

Two commits ideal:

1. `refactor(workspace): add empty rasterlab-render crate scaffold` — just
   Cargo.toml, lib.rs with a doc comment, workspace member entry. Ensures
   the crate boundary lands cleanly before any code moves.
2. `refactor: move render thread + BgMessage from rasterlab-gui to rasterlab-render`
   — the actual move.

---

## Step 4 — `Tool` trait abstraction

**Prerequisite:** step 2 must be done first.

**Goal:** kill the parallel field universe in `ToolState` and the
`any_preview_active` / `preview_op` / `cancel_all_previews` triplets that grow
linearly per tool.

### Current shape

`rasterlab-gui/src/state/tool_state.rs` — flat struct with field clusters per
tool: `crop_x, crop_y, crop_w, crop_h, crop_aspect_idx, crop_custom_ratio,
crop_portrait, rotate_deg, rotate_preview_active, rotate_crop, …`. Adding a
tool today means editing 4+ places (struct fields, `any_preview_active`,
`preview_op`, `cancel_all_previews`, the panel UI).

### Plan

1. Define a `Tool` trait in `rasterlab-gui/src/panels/tools/tool.rs`:

   ```rust
   pub trait Tool {
       fn id(&self) -> &'static str;
       fn display_name(&self) -> &'static str;
       fn render_ui(&mut self, ui: &mut egui::Ui, ctx: &mut ToolUiCtx<'_>);
       fn is_preview_active(&self) -> bool;
       fn cancel_preview(&mut self);
       fn preview_op(&self) -> Option<Box<dyn Operation>>;
       fn apply(&mut self, app: &mut AppState);
   }
   ```

   (Adjust signatures based on what the existing per-tool functions actually
   need from `AppState`. Pass a `ToolUiCtx` struct of borrows rather than the
   whole `AppState` to keep coupling visible.)

2. Migrate **one tool at a time** behind the existing struct:
   - Pick a small tool first (e.g. `Sepia` or `BlurOp`).
   - Create `panels/tools/sepia.rs`'s tool as a `pub struct SepiaTool {
     strength: f32, preview_active: bool }` implementing `Tool`.
   - Remove the corresponding fields from `ToolState`.
   - Wire the dispatch in `panels/tools/mod.rs` to call `tool.render_ui(...)`.
3. Repeat tool-by-tool. Each migration is its own commit.
4. Once all tools are migrated, `ToolState` should hold only a registry:

   ```rust
   pub struct ToolState {
       pub tools: BTreeMap<EditingTool, Box<dyn Tool>>,
   }
   ```

   And `any_preview_active` becomes
   `self.tools.values().any(|t| t.is_preview_active())`.

### Acceptance criteria

- `ToolState` no longer contains per-tool fields — only the registry.
- Adding a new tool means: add `panels/tools/<name>.rs`, register it in one
  place. Confirm by adding a trivial new tool (e.g. invert) and counting
  touchpoints.
- `any_preview_active`, `preview_op`, `cancel_all_previews` are 1-liners.
- All existing tools still work in the GUI. Golden tests still pass.

### Risks / watch-outs

- Some tools have shared state with `AppState` beyond their parameters
  (e.g. crop reads the rendered image dimensions). Pass that through
  `ToolUiCtx`, don't smuggle it into the trait.
- The "edit existing op from the stack" feature (the `as_any` downcast on
  `Operation`) currently rehydrates tool-panel sliders. Make sure the new
  trait has a `load_from_op(&mut self, op: &dyn Operation)` hook or
  equivalent before you delete the field-based version.
- Migrate incrementally — don't attempt big-bang.

### Commit shape

- 1 commit introducing the trait + `ToolUiCtx` (no migrations yet).
- 1 commit per tool migration (or batches of 2-3 small similar tools).
- 1 final commit removing dead `ToolState` fields and collapsing the helpers.

---

## Step 5 — Collapse the three file-dialog stacks

**Goal:** there's `rfd`, `egui-file-dialog`, and a hand-rolled
`rasterlab-gui/src/file_chooser.rs`. Pick one path per use case.

### Current shape

- `rfd` — native OS file pickers.
- `egui-file-dialog` — in-app file dialog (egui-rendered).
- `file_chooser.rs` — custom code, purpose unclear without audit.

### Plan

1. Audit. Grep for each in `rasterlab-gui/src/`:
   - `rg "rfd::"`
   - `rg "egui_file_dialog\|FileDialog"`
   - `rg "file_chooser"`
   For every callsite, note: native or in-app? Open or save? File or
   directory? Library navigation or one-off?
2. Decide the policy. Reasonable default:
   - Native OS dialogs (`rfd`) for: open image from disk, save export,
     pick a folder for "New Library".
   - In-app dialog (`egui-file-dialog`) for: nothing, unless there's a
     specific UX reason to stay inside the egui context.
   - Custom `file_chooser.rs`: probably delete.
3. Refactor each callsite to the chosen path. Delete unused dependencies
   from `Cargo.toml` (workspace + crate-level).

### Acceptance criteria

- At most two file-dialog dependencies remain in `Cargo.toml` (likely just
  `rfd`).
- `file_chooser.rs` either gone or substantially smaller with a clear sole
  purpose.
- All file-related flows still work: open image, save .rlab, export, set
  library root.

### Risks / watch-outs

- `rfd` dialogs block the calling thread on some platforms — make sure
  callsites that previously used a non-blocking in-app dialog still feel
  responsive. If something needs async, keep `egui-file-dialog` for that
  one callsite.
- Library navigation may have a custom dialog for a reason (thumbnails,
  metadata preview). Don't rip it out without checking.

### Commit shape

One commit per dialog system removed, or one commit total if the change is
small. Title: `refactor(gui): consolidate on rfd for file dialogs`.

---

## After step 5

What remains on the larger wishlist (NOT scheduled here):

- **GPU port** — rewrite of `ops/`, not a refactor. Separate roadmap. See
  `GPU.md` (untracked) for any prior thinking.
- **Linear-light pipeline** — every op converts to/from sRGB ad hoc today.
  A unified linear-float pipeline with sRGB encode at output would be cleaner
  but *will* shift pixels — gate on the golden tests existing (✓) and accept
  a hash-table refresh as part of the change.
- **Render pipeline redesign** — `RenderComplete`'s grab-bag of fields is the
  scar tissue of organic growth. Worth redesigning only when a new feature
  forces the issue.
