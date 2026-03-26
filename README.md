# RasterLab

> *How good of an image editor can you build with $20 worth of Claude Code?*

We're here to find out!  This is still a work in progress!

## What is this?

RasterLab is a non-destructive RAW image editor written in Rust, built almost entirely by Claude Code as an experiment in AI-assisted software development. It has a real-time preview pipeline, intermediate result caching, parallelized image processing, undo/redo, a histogram panel, and support for JPEG, PNG, and Nikon NEF files.

Currently we're a couple days in, and we've burned 19% of our weekly usage.

## Features

- **Non-destructive editing pipeline** — operations stack on top of the original, nothing is ever overwritten
- **Intermediate result caching** — changing the last op in a 10-op stack doesn't re-run the first 9
- **Downsampled live preview** — slider feedback renders at 25% resolution (~16× faster) while a full-res render queues behind it
- **Parallelized ops** via rayon — crop, rotate, sharpen, levels, and B&W all use parallel pixel/row iteration
- **Undo/redo** — with cache-aware shortcuts so undo is often instant
- **Histogram** — per-channel R/G/B + luma, computed in the render thread
- **Arbitrary rotation** — with bilinear interpolation, the slow one
- **Plugin API** — because why not

## Supported operations

| Op | What it does |
|---|---|
| Crop | Crops. Parallel row copy. |
| Rotate | 90°/180°/270° lossless, or arbitrary angle with bilinear interp |
| Sharpen | Unsharp mask convolution, optionally luma-only |
| Black & White | Luminance, average, perceptual, or channel mixer |
| Levels | Black/mid/white point with LUT-based remapping |

## Building

```sh
cargo build --release
cargo run --release -p rasterlab-gui
```

It's still not as fast as I would like.  Claude thinks it's fast, but I disagree.

## Architecture

```
rasterlab-core/       # Image type, ops, pipeline, histogram
rasterlab-gui/        # egui/eframe frontend
rasterlab-cli/        # Headless batch processing
rasterlab-plugin-api/ # Trait definitions for external ops
plugins/              # Example plugin
```

The GUI render path is fully async: mutations serialize op parameters to JSON, a background thread deserializes and applies them, results come back via mpsc. The main thread never blocks on pixel work.

## How the experiment went

**Things Claude got right immediately:**
- The overall architecture (non-destructive pipeline, background render thread, Arc-based image sharing)
- Rayon parallelism patterns for each op
- The step cache design and generation counter for stale-write prevention

**Things that required a few rounds:**
- Performance improvements, mouse wheel scroll is still painfully jumpy
- Merge conflict resolution after a parallel branch tried a simpler cache approach

**Things Claude was confidently wrong about:**
- The initial estimate that the step cache would make levels-slider dragging fast even without the downsampled preview. It would have, eventually. But 800ms per render is 800ms per render.

**Things that cost more tokens than expected:**
- Explaining why `Arc<Image>` can be passed to a function expecting `&Image` via Deref coercion (twice)

## Status

Works. Suspiciously well for what we've done so far.  I'm interested in seeing what we end up with.

The plugin system exists and has an example. Nobody has written a plugin. The arbitrary rotation is slow on large images because bilinear interpolation has terrible cache locality and nobody has fixed it.

# Changelog

## 2026-03-25 (19% of our first week usage)

- **Levels tool** — black/mid/white point sliders with live preview; changes are non-destructive and can be committed to the pipeline or discarded
- **Render timing** — status bar now shows how long the last render took in milliseconds
- **Intermediate result caching** — the pipeline caches the rendered image after each committed op; changing op N no longer re-runs ops 0 through N-1, and undo/redo are often instant
- **Downsampled live preview** — while a levels slider is being dragged, ops run on a 25% resolution image (~16× fewer pixels) for immediate feedback; a full-res render queues automatically once the preview is displayed
- **Dev build optimization** — `opt-level = 1` added to the dev profile so pixel-processing loops aren't painfully slow during development (was ~800ms per render, now reasonable)
- **CLI file argument** — the GUI now accepts a file path on the command line so you can open an image directly without using the file dialog
- **Crop fix** — corrected an off-by-one in crop start/end coordinate handling

## 2026-03-24 (14% of our first week usage)

- **Initial release** — non-destructive editing pipeline, background render thread, undo/redo, histogram panel, crop, rotate (90°/180°/270° + arbitrary angle with bilinear interpolation), sharpen, and black & white conversion
- **Zoom & pan** — canvas supports scroll-to-zoom and drag-to-pan; fit-to-view behavior fixed to work as expected
- **Smoother zoom** — zoom now centers on the cursor position rather than the canvas origin
- **Stability** — fixed a segfault on startup
- **Plugin API** — trait definitions and an example plugin for extending the op set


## License

MIT OR Apache-2.0
