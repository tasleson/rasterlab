# GPU Work Remaining — Continuation Notes

Branch: `gpu`  
Last commit: `03f23ee` — "Add GPU ops: black & white, blur, color balance, color space, denoise, HSL panel, sharpen"

---

## Current state

22 of 35 ops have GPU support. All GPU code lives in `rasterlab-gpu/src/lib.rs` (one big file, ~5600 lines).

### Already GPU-accelerated
`BlackAndWhiteOp`, `BlurOp`, `BrightnessContrastOp`, `ClarityTextureOp`, `ColorBalanceOp`, `ColorSpaceOp`, `CurvesOp`, `DenoiseOp` (bilateral), `FauxHdrOp`, `HighlightsShadowsOp`, `HslPanelOp`, `HueShiftOp`, `LevelsOp`, `NoiseReductionOp` (NLM only), `SaturationOp`, `SepiaOp`, `ShadowExposureOp`, `SharpenOp`, `SplitToneOp`, `VibranceOp`, `VignetteOp`, `WhiteBalanceOp`

### Still CPU-only (13 ops — all in Tier 4 skip)
See priority table below.

---

## How GPU ops work — implementation pattern

Every GPU op requires changes in exactly one file: `rasterlab-gpu/src/lib.rs`.

### Five things to add per op

**1. Import** — add to the `use rasterlab_core::ops::{...}` block at the top of lib.rs.

**2. Kernel struct + field in `GpuContext`**
```rust
struct FooKernel {
    pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,
}
// Add to GpuContext struct:
foo: Arc<FooKernel>,
// Add to GpuContext::new():
let foo = Arc::new(FooKernel::new(&device));
// Add to Self { ... }:
foo,
```

**3. `supports()` and `apply_one()`** — add a downcast check in each.
```rust
// in supports():
if op.as_any().and_then(|any| any.downcast_ref::<FooOp>()).is_some() {
    return true;
}
// in apply_one():
} else if let Some(op) = op.as_any().and_then(|any| any.downcast_ref::<FooOp>()) {
    apply_foo(ctx, op, image)
```

**4. Param struct + `apply_foo()` function**

For a 3-binding op (no LUT — most ops):
```rust
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct FooParams {
    width: u32, height: u32, pixel_count: u32, _pad: u32,
    // float params here, padded to 16-byte multiples
    my_param: f32, _pad2: f32, _pad3: f32, _pad4: f32,
}

fn apply_foo(ctx: &GpuContext, op: &FooOp, image: GpuImage) -> Result<GpuImage, GpuError> {
    if /* identity check */ { return Ok(image); }
    let byte_len = expected_rgba_len(image.width, image.height) as u64;
    let output = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rasterlab foo output"),
        size: byte_len,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let params = FooParams { width: image.width, height: image.height,
        pixel_count: image.width.saturating_mul(image.height), _pad: 0, ... };
    let params_buffer = ctx.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("rasterlab foo params"),
        contents: bytemuck::bytes_of(&params),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    dispatch_3binding(ctx, &ctx.foo.pipeline, &ctx.foo.bind_group_layout,
        "rasterlab foo", &image.buffer, &output, &params_buffer,
        params.width, params.height)?;
    Ok(GpuImage { width: image.width, height: image.height, buffer: output })
}
```

For a 4-binding op (LUT-based — like `LevelsOp`): see `apply_levels()` for the pattern.  
Use `make_3binding_layout` / `make_4binding_layout` helpers already in lib.rs.  
Use `make_simple_pipeline` helper already in lib.rs for `KernelImpl::new()`.

**5. WGSL shader constant** — add before `NOISE_REDUCTION_NLM_WGSL`:
```rust
const FOO_WGSL: &str = r#"
struct Params { width: u32, height: u32, pixel_count: u32, _pad: u32, my_param: f32, ... };
@group(0) @binding(0) var<storage, read> input_pixels: array<u32>;
@group(0) @binding(1) var<storage, read_write> output_pixels: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;
@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) { return; }
    let i = gid.y * params.width + gid.x;
    if (i >= params.pixel_count) { return; }
    let px = input_pixels[i];
    // ... pixel math ...
    output_pixels[i] = ...;
}
"#;
```

### WGSL pixel pack/unpack idioms
```wgsl
// Unpack RGBA8 u32 → floats in [0,1]
let r = f32(px & 0xffu) / 255.0;
let g = f32((px >> 8u) & 0xffu) / 255.0;
let b = f32((px >> 16u) & 0xffu) / 255.0;
let a = px & 0xff000000u;

// Pack back (with rounding)
output_pixels[i] = u32(nr * 255.0 + 0.5) | (u32(ng * 255.0 + 0.5) << 8u)
    | (u32(nb * 255.0 + 0.5) << 16u) | a;

// HSL helpers are already in SATURATION_WGSL — copy them verbatim:
// hue_to_rgb(), rgb_to_hsl(), hsl_to_rgb(), unpack_rgb(), pack_rgba()
```

### Struct alignment rule
WGSL uniform structs must be 16-byte aligned in size. Always pad the float section to a multiple of 4 f32s.  
Pattern: `4×u32` header (16 bytes) + N×`vec4<f32>` worth of params. Use `_padN: f32` fields.

---

## Priority table — remaining ops

### Tier 1: ✅ Done (all 7 completed in commit 03f23ee)

`BlackAndWhiteOp`, `ColorBalanceOp`, `ColorSpaceOp`, `DenoiseOp` (bilateral), `HslPanelOp`, `BlurOp` (2-pass separable Gaussian), `SharpenOp` (5-tap cross kernel)

---

### Tier 4: Skip (not suitable for GPU)

| Op | Reason |
|---|---|
| `CropOp`, `FlipOp`, `RotateOp`, `ResizeOp`, `PerspectiveOp` | Geometric — output dimensions differ from input; would require texture sampling; low compute:bandwidth ratio |
| `GrainOp` | Needs per-pixel PRNG (possible with hash-based RNG, but low priority) |
| `HealOp` | Content-aware fill with mask; irregular access pattern |
| `FocusStackOp`, `HdrMergeOp`, `PanoramaOp` | Multi-image; GPU VRAM constraints; rarely applied repeatedly |
| `MaskedOp` | Wrapper op; inner op already dispatched on GPU |
| `HistogramOp` | Reduction, not a transform; tiny readback vs. full image; not in render hot path |
| `LutOp` | 3D LUT requires trilinear interpolation — doable but no 3D texture; would need to implement 3D lerp manually in shader |

---

## Pre-commit checklist (from CLAUDE.md)

Every commit must run these first:
```bash
cargo fmt
cargo clippy        # must be warning-free
cargo bench --package rasterlab-core
cargo build --release
```

---

## Key file locations

| File | Purpose |
|---|---|
| `rasterlab-gpu/src/lib.rs` | All GPU kernels, shaders, dispatch — the only file to edit for new GPU ops |
| `rasterlab-core/src/ops/` | CPU implementations to read for algorithm reference |
| `rasterlab-core/src/ops/mod.rs` | Op exports |
| `rasterlab-render/src/lib.rs` | Render pipeline that calls `rasterlab_gpu::supports()` and `apply_one()` — no changes needed for new ops |

---

## Useful helpers already in lib.rs

```rust
// Create a 3-binding layout (input storage, output storage, uniform params)
fn make_3binding_layout(device: &wgpu::Device, label: &str) -> wgpu::BindGroupLayout

// Create a 4-binding layout (adds a read-only storage for LUTs)
fn make_4binding_layout(device: &wgpu::Device, label: &str) -> wgpu::BindGroupLayout

// Compile shader, create pipeline layout, create compute pipeline
fn make_simple_pipeline(device, wgsl, bind_group_layout, shader_label, pipeline_label)
    -> wgpu::ComputePipeline

// Submit a single 3-binding compute pass and poll
#[allow(clippy::too_many_arguments)]
fn dispatch_3binding(ctx, pipeline, layout, label, input, output, params, width, height)
    -> Result<(), GpuError>

// Helper used by SplitToneOp — copy of CPU hue_to_rgb
fn hue_to_rgb_f32(hue: f32) -> (f32, f32, f32)

// Used by apply functions to compute byte length
fn expected_rgba_len(width: u32, height: u32) -> usize  // = width * height * 4
```

---

## GPU threshold / batching behaviour (no changes needed)

- Single op on image ≥ 2 MP → GPU (env var `RASTERLAB_GPU=0` disables, `=force` forces)
- Consecutive GPU-capable ops → single upload, N dispatches, single readback
- GPU failure → CPU fallback, no user-visible error
- Threshold and batching logic is in `rasterlab-render/src/lib.rs::gpu_skip_reason()`
