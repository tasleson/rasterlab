# RasterLab — development guidance

## Pre-commit checklist

Before every commit run:

```bash
cargo fmt
cargo clippy
cargo build --release
```

Benchmarks are not part of this checklist; run them only when the change
touches performance-sensitive code, as described below.

## Performance rules

### Always benchmark rayon changes before committing

Run the criterion suite before and after any change to parallel code:

```bash
cargo bench --package rasterlab-core -- --save-baseline main   # before
cargo bench --package rasterlab-core -- --baseline main        # after
```

For end-to-end pipeline timing (sepia apply → histogram → texture conversion):

```bash
cargo run --release --example render_timing -- exp.jpg
```

To break down the Open path instead (decode → EXIF → orientation →
histogram → texture upload), pass a large photo and, optionally, a copy
of it carrying EXIF orientation 6:

```bash
cargo run --release --example load_timing -- photo.jpg [photo_rotated.jpg]
```

### Measure memory-bandwidth-bound loops, do not assume

A simple map/copy over a large buffer is limited by memory bandwidth,
not compute — but that does **not** mean rayon cannot help, because a
single core usually cannot saturate the machine's bandwidth on its own.
Which way it goes depends on the buffer and the box, so measure.

Two worked examples, both 24 MP (~90 MiB), on a 16-core desktop:

- The `image_to_egui` RGBA8→Color32 conversion **does** benefit:
  ~42 ms serial → ~15 ms across the pool.  (A serial `memcpy` of the
  same buffer is ~45 ms, so the gain is parallelism, not skipping the
  per-pixel work — and skipping it is not an option anyway, since
  `Color32` is premultiplied.)
- The RGB→RGBA expansion likewise goes ~47 ms → ~13 ms, but chunk size
  barely matters there (4096 px/chunk beats one-pixel chunks by only
  ~8%), which is the signature of a bandwidth-bound loop.

The rule that still holds unconditionally is the accumulator one below.

### rayon fold accumulators must be small or chunked

`par_chunks(4).fold(large_acc, ...)` invokes the fold closure once per
pixel, moving the accumulator by value each time.  An 8 KiB accumulator
× 35 M pixels = ~143 GB of stack traffic, turning a 5 ms operation into
a 400 ms one.

**Rule:** if the fold accumulator exceeds ~64 bytes, use a larger chunk
size so the inner loop keeps the accumulator cache-hot:

```rust
// BAD  — one fold call per pixel, 8 KiB accumulator moved each time
data.par_chunks(4).fold(zero, |mut acc, pixel| { ... acc })

// GOOD — one fold call per 4096 pixels, accumulator stays in L1 cache
data.par_chunks_exact(4 * 4096).fold(zero, |mut acc, chunk| {
    for pixel in chunk.chunks_exact(4) { ... }
    acc
})
```

### Rayon worker stack size on macOS

macOS secondary threads default to 512 KiB.  Benchmarks and examples
that use rayon with large fold accumulators must initialise the global
pool before criterion or rayon first runs:

```rust
rayon::ThreadPoolBuilder::new()
    .stack_size(16 * 1024 * 1024)
    .build_global()
    .unwrap();
```

The GUI render thread already sets 32 MiB (`app_state.rs`).

## Tool ordering

When adding a new tool to the tools panel (`rasterlab-gui/src/panels/tools/`):

- **Auto Enhance** stays first.
- **Looks** stays second.
- All other tools are placed in **strict alphabetical order by display name** after those two.
- Update the **Supported operations** table in `README.md` to match the same order.

## Key files

| Purpose | Location |
|---|---|
| Core image processing ops | `rasterlab-core/src/ops/` |
| GUI render pipeline | `rasterlab-gui/src/state/app_state.rs` |
| Canvas / texture upload | `rasterlab-gui/src/panels/canvas.rs` |
| Criterion benchmarks | `rasterlab-core/benches/operations.rs` |
| End-to-end timing example | `rasterlab-core/examples/render_timing.rs` |
| Open-path (load) timing example | `rasterlab-core/examples/load_timing.rs` |
| Thread-scaling example | `rasterlab-core/examples/rayon_scaling.rs` |
