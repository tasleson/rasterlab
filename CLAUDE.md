# RasterLab — development guidance

## Pre-commit checklist

Before every commit run:

```bash
cargo fmt
cargo clippy
cargo bench
cargo build --release
```

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

### Memory-bandwidth-bound loops do not benefit from rayon

Operations that are a simple map/copy over a large buffer (e.g. the
`image_to_egui` RGBA8→Color32 conversion) are limited by memory
bandwidth, not compute.  Adding `par_chunks` coordination overhead
makes them slower, not faster.  Benchmark before assuming parallel =
better.

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

When adding a new tool to the tools panel (`rasterlab-gui/src/panels/tools.rs`):

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
| Thread-scaling example | `rasterlab-core/examples/rayon_scaling.rs` |
