# GPU Pipeline Migration Plan

Goal: minimize CPU/GPU transfers by keeping image data GPU-resident across
consecutive GPU-supported filters, while preserving the current CPU pipeline as
the correctness fallback.

## Step 1: Add a GPU-resident executor API

Add an API that uploads a CPU `Image` once, applies multiple supported
operations to a `GpuImage`, and reads back only when explicitly requested.

Initial shape:

- `GpuPipeline::from_image(ctx, image)`
- `GpuPipeline::apply_op(ctx, op)`
- `GpuPipeline::into_image(ctx)`

This step should not change render behavior yet; it only creates the API and
tests it against the existing one-op paths.

## Step 2: Batch adjacent GPU-supported ops in render

Update `rasterlab-render` so consecutive GPU-supported committed operations are
executed as one GPU run:

```text
CPU Image -> upload once -> GPU op A -> GPU op B -> readback once
```

Unsupported operations remain CPU boundaries:

```text
CPU ops -> GPU run -> CPU op -> GPU run -> final CPU image
```

## Step 3: Make readback/cache policy explicit

The existing edit pipeline stores CPU `Arc<Image>` intermediates. GPU runs need
an explicit policy for when to read back:

- read back at CPU-only op boundaries
- read back at the final render result
- initially, store CPU intermediates only at GPU-run boundaries
- later, optionally add GPU-side step cache entries

## Step 4: Move small reductions to GPU where useful

Histogram currently requires CPU image access. After GPU batching works, add an
optional GPU histogram path that reads back a small histogram buffer instead of
the full image when the display path no longer needs CPU pixels.

## Step 5: Display directly from GPU texture

Longer term, render directly into a wgpu texture consumed by egui. CPU readback
should then be limited to export, persistence, CPU-only filters, and fallback
paths.

## Step 6: Expand operation coverage

Add GPU kernels in priority order based on pipeline frequency and transfer
savings. Prefer chains of simple point/local ops first, because they benefit
most from staying GPU-resident.
