# Revised GPU Filter Acceleration Plan

This replaces the older `GPU.md` plan, which predates the refactor that split
pipeline execution into `rasterlab-render`.

## Current Architecture

The relevant ownership boundaries are now:

```
rasterlab-core
  Image, Operation trait, built-in ops, EditPipeline, RenderCache

rasterlab-render
  RenderRequest, spawn_render(), render_pipeline(), preview/overlay paths,
  histogram generation, intermediate cache results

rasterlab-gui
  eframe/egui app, access to CreationContext::wgpu_render_state,
  canvas texture upload and drawing
```

This means GPU execution must be added to `rasterlab-render`, not by replacing a
loop in `AppState`. The GUI should only discover/share the wgpu device and queue;
the render crate should continue to own the execution policy.

## Key Constraint: Avoid a Crate Cycle

Do not add `GpuContext` or `GpuImage` methods directly to
`rasterlab-core::traits::Operation`.

That would create the dependency cycle:

```
rasterlab-core -> rasterlab-gpu -> rasterlab-core
```

Instead:

```
rasterlab-core      CPU image model and Operation trait only
rasterlab-gpu       GPU executor/adapters for known core ops
rasterlab-render    Chooses CPU vs GPU execution
rasterlab-gui       Supplies eframe's wgpu RenderState to AppState
```

The `Operation` trait stays stable for serialization, plugins, CLI, tests, and
headless rendering.

## Proposed Crate Layout

```
rasterlab-gpu/
  Cargo.toml
  src/
    lib.rs
    context.rs          # GpuContext: Arc<Device>, Arc<Queue>, limits, pipeline cache
    image.rs            # GpuImage and CPU upload/readback helpers
    executor.rs         # apply_gpu_runs(), capability checks, thresholds
    ops/
      mod.rs
      brightness_contrast.rs
      levels.rs
      curves.rs
      saturation.rs
    shaders/
      brightness_contrast.wgsl
      levels.wgsl
      curves.wgsl
      saturation.wgsl
```

Dependency direction:

```toml
# rasterlab-gpu/Cargo.toml
[dependencies]
rasterlab-core = { workspace = true }
wgpu = { workspace = true }
egui-wgpu = { workspace = true } # only if using egui_wgpu::RenderState here
bytemuck = { version = "1", features = ["derive"] }
pollster = "0.4"
```

Add `wgpu`, `egui-wgpu`, `bytemuck`, and `pollster` to workspace dependencies
when implementation begins. `eframe` re-exports wgpu, but a production crate
should depend on the concrete crates it uses.

## GPU Context

`eframe 0.34` exposes `egui_wgpu::RenderState` in `CreationContext`. In this
codebase `RenderState` currently contains owned `wgpu::Device` and `wgpu::Queue`
values that are `Clone`, so `GpuContext::from_eframe` can clone them into `Arc`s.

```rust
pub struct GpuContext {
    pub device: Arc<wgpu::Device>,
    pub queue: Arc<wgpu::Queue>,
    limits: wgpu::Limits,
    pipelines: Mutex<PipelineCache>,
}

impl GpuContext {
    pub fn from_eframe(rs: &egui_wgpu::RenderState) -> Self {
        Self {
            device: Arc::new(rs.device.clone()),
            queue: Arc::new(rs.queue.clone()),
            limits: rs.device.limits(),
            pipelines: Mutex::new(PipelineCache::new()),
        }
    }
}
```

`AppState` should store:

```rust
pub gpu: Option<Arc<GpuContext>>
```

`RasterLabApp::new(cc, initial_file)` should pass `cc.wgpu_render_state.as_ref()`
into `AppState::new(...)` or a follow-up initializer.

## RenderRequest Changes

Add an optional GPU context to `rasterlab-render`:

```rust
pub struct RenderRequest {
    pub start_image: Arc<Image>,
    pub committed_ops: Vec<Option<Box<dyn Operation>>>,
    pub preview_op: Option<Box<dyn Operation>>,
    pub preview_scale: Option<f32>,
    pub preview_viewport: Option<[u32; 4]>,
    pub overlay_viewport: Option<[u32; 4]>,
    pub gpu: Option<Arc<GpuContext>>,
}
```

`AppState::request_render_inner()` clones `self.gpu` into the request.

Important: keep all existing fields and paths. The render crate currently handles:

- Disabled operations via `Vec<Option<Box<dyn Operation>>>`.
- Preview scaling and `scaled_for_preview()`.
- Full-resolution viewport overlay previews.
- Histogram generation.
- Intermediate images returned for `EditPipeline::store_steps()`.
- Cancellation/error routing through `RenderResult`.

GPU support must preserve that contract.

## Execution Strategy

Start with GPU as an opportunistic accelerator inside the existing fallback path:

```rust
for maybe_op in committed_ops {
    ...
}
```

Replace only that loop with an executor that can process consecutive GPU-capable
runs and return the same intermediate vector shape expected today.

```rust
fn apply_committed_ops(
    current: Arc<Image>,
    ops: Vec<Option<Box<dyn Operation>>>,
    preview_scale: Option<f32>,
    gpu: Option<&GpuContext>,
    collect_intermediates: bool,
) -> Result<(Arc<Image>, Vec<Arc<Image>>), String>
```

Execution rules:

1. Disabled ops still push the unchanged current image into `intermediates` for
   full-resolution renders.
2. Preview renders still call `scaled_for_preview()` before capability checks.
3. If no GPU context exists, behavior is byte-for-byte the current CPU behavior.
4. If a GPU run is selected, upload once, dispatch all ops in the run, then
   read back only at cache boundaries.
5. If any GPU op fails to prepare/dispatch, fall back to CPU for that run and
   preserve the existing error behavior if CPU also fails.

## Cache Policy

The existing cache stores `Arc<Image>` after every committed operation. That
shape is valuable and should not be rewritten in Phase 1.

Phase 1 policy:

- Use GPU only for a consecutive run at the tail of the render, or when the plan
  is willing to read back each step.
- Default to tail-run acceleration first because it preserves the biggest win:
  one upload, N GPU ops, one readback.
- For non-tail GPU runs, either keep CPU execution or read back per step. Do not
  silently skip intermediate cache entries.
- Never store the final GPU result into earlier cache slots. A cache entry for
  op `i` must be the exact image after op `i`, or it must be absent.

Tail-run example:

```
ops from cache start:
  [CPU] [CPU] [GPU] [GPU] [GPU]

cache stores CPU intermediates for first two ops,
then either stores exact per-op GPU readbacks or stores only the final GPU slot.
```

For the first implementation, the simplest correct choice is:

- GPU-accelerate only if every remaining enabled committed op is GPU-capable.
- For full-resolution committed renders, read back after each GPU op if using the
  current `store_steps()` API unchanged.
- If avoiding per-op readback, add a sparse cache API first:

```rust
pub fn store_sparse_steps(&mut self, steps: Vec<(usize, Arc<Image>)>) {
    for (idx, image) in steps {
        self.cache.store(idx, image);
    }
}
```

- With sparse storage, a tail GPU run can store only the final op index and leave
  earlier GPU step slots empty.
- Prefer correctness and measurement over preserving every theoretical win.

Phase 2 can add a parallel GPU cache:

```rust
GpuRenderCache {
    source_key: ImageKey,
    steps: Vec<Option<Arc<GpuImage>>>,
    generation: u64,
}
```

That should wait until Phase 1 benchmarks prove that CPU readback/cache storage
is the limiting cost.

## GPU Image Representation

Use storage buffers for Phase 1 compute:

```rust
pub struct GpuImage {
    buffer: wgpu::Buffer,       // STORAGE | COPY_SRC | COPY_DST
    width: u32,
    height: u32,
    format: GpuFormat,          // start with Rgba8UnormBytes
}
```

Start with RGBA8 bytes, not RGBA32F.

Reasons:

- Current `Image` is RGBA8; golden tests and CPU parity are byte-oriented.
- Upload/readback is 4x smaller than RGBA32F.
- Many initial ops already quantize to 8-bit semantics.

Add RGBA32F later only for a deliberate high-precision pipeline mode. Mixing
RGBA8 CPU steps and RGBA32F GPU steps will change output and complicate parity
before the executor is proven.

## Operation Dispatch Without Changing Operation

`rasterlab-gpu` should inspect built-in operation names and downcast through the
existing `Operation::as_any()` hook where available.

```rust
pub enum GpuSupport {
    Supported,
    Unsupported,
}

pub fn supports(op: &dyn Operation) -> bool;

pub fn apply_one(
    ctx: &GpuContext,
    op: &dyn Operation,
    input: GpuImage,
) -> Result<GpuImage, GpuError>;
```

If an op does not expose `as_any()`, do not support it on GPU yet. Add downcast
hooks only to the specific built-in ops included in the rollout.

Plugins remain CPU-only until a separate plugin GPU ABI exists.

## Initial Operation Rollout

Start narrow. The goal is to prove the executor, not port the whole editor.

Phase 1:

| Op | Shader shape | Notes |
|---|---|---|
| `brightness_contrast` | per-pixel compute | Lowest-risk first op |
| `levels` | 256-entry LUT or direct formula | Existing CPU output should be easy to match |
| `curves` | LUT texture/buffer | Generate 256-entry LUT on CPU, apply on GPU |
| `saturation` | per-pixel compute | Simple RGB/luma math |

Do not start with a broad generic LUT covering every color op. Hue shift,
vibrance, split tone, white balance, color balance, HSL, masks, local contrast,
and LUT/color grading each need their own semantics and parity tests.

Phase 2:

| Op | Notes |
|---|---|
| `blur` | Separable two-pass compute; benchmark against current rayon path |
| `sharpen` | Reuse blur/intermediate machinery for unsharp mask |
| `resize` | Prefer texture sampling path after buffer executor is stable |

Phase 3:

Geometric ops, native display textures, and optional GPU step cache.

## Thresholds

Never default to "GPU whenever available".

Use a policy function:

```rust
fn should_use_gpu(image: &Image, run: &[&dyn Operation]) -> bool {
    let pixels = image.width as u64 * image.height as u64;
    pixels >= 2_000_000 && run.len() >= 2
}
```

Tune with benchmarks. GPU startup, upload, readback, and `device.poll()` can be
slower than rayon for small images, viewport previews, and single simple ops.

Add debug logging for the first implementation:

```
gpu run: ops=3 pixels=24.1MP upload=4.2ms dispatch=1.8ms readback=5.1ms total=11.1ms
cpu run: ops=3 pixels=24.1MP total=28.4ms
```

## Preview Paths

Keep GPU disabled for overlay viewport previews in Phase 1.

The overlay path currently extracts the visible region on CPU, applies the
preview op, and returns an overlay image plus `overlay_rect`. This path is
latency-sensitive and often processes far fewer pixels than a full image. It is
not the best first GPU target.

Phase 1 GPU should apply only to the fallback full-image/downsampled path.
After that is stable, consider GPU preview for:

- Full-image downsampled previews when the downsampled image is still large.
- Tail-run preview ops that are already GPU-supported.

## Display Path Optimization

Do not include native egui texture display in Phase 1.

`egui_wgpu::Renderer::register_native_texture()` registers a `wgpu::TextureView`,
not a storage buffer. It also expects a texture suitable for egui's texture bind
group, practically `Rgba8Unorm`.

To implement this later:

1. Add `GpuImage::to_display_texture()` that writes/normalizes into an
   `Rgba8Unorm` texture with `TEXTURE_BINDING | COPY_DST | RENDER_ATTACHMENT`
   as needed.
2. Keep a stable `TextureId` and use
   `update_egui_texture_from_wgpu_texture()` rather than registering a fresh id
   every frame.
3. Free old native textures through egui-wgpu renderer ownership rules.
4. Preserve the existing `image_to_egui()` CPU path for unsupported GPUs and
   large-texture fallback behavior.

## Synchronization

The render thread can submit compute work to the same queue eframe uses for UI
rendering. Keep synchronization conservative first:

```rust
queue.submit(Some(encoder.finish()));
device.poll(wgpu::MaintainBase::Wait);
```

Confirm the exact poll type for the workspace's wgpu version during
implementation. Avoid holding egui renderer locks in the render thread.

If blocking `poll(Wait)` causes UI jank, move to async map callbacks or a single
GPU worker thread that serializes GPU jobs.

## Testing

Add tests in layers:

1. CPU parity tests for each GPU op with deterministic small images.
2. Large-image smoke tests for buffer sizes, row pitch, and dispatch dimensions.
3. Executor tests where mixed supported/unsupported/disabled ops return the same
   result as pure CPU.
4. GUI smoke test only after native display textures are attempted.

Suggested ignored test gate:

```sh
cargo test -p rasterlab-gpu -- --ignored gpu
cargo test -p rasterlab-render gpu_executor -- --ignored
```

Use tolerances sparingly. Phase 1 RGBA8 shaders should aim for exact parity or
an explicitly documented off-by-one tolerance.

## Implementation Checklist

1. Add workspace deps and the new `rasterlab-gpu` crate.
2. Add optional `rasterlab-gpu` dependency to `rasterlab-render`.
3. Thread `Option<Arc<GpuContext>>` from `RasterLabApp::new()` into `AppState`
   and then into `RenderRequest`.
4. Implement upload/readback for RGBA8 `GpuImage`.
5. Implement brightness/contrast GPU op and parity tests.
6. Add the mixed CPU/GPU executor behind a conservative threshold.
7. Benchmark against `rasterlab-core` operation benches and the
   `render_timing` example.
8. Expand to levels, curves, and saturation only after the executor is stable.

## Non-Goals For Phase 1

- No change to `Operation` trait.
- No plugin GPU ABI.
- No display texture bypass.
- No GPU step cache.
- No RGBA32F pipeline.
- No broad "generic LUT covers all color ops" abstraction.
