<img width="1408" height="768" alt="vibe_this" src="https://github.com/user-attachments/assets/bcf3af6b-4982-4dad-b460-99cca597660a" />

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
| Auto Enhance | One-click levels stretch + saturation boost + mild sharpen |
| Black & White | Luminance, average, perceptual, or channel mixer with presets |
| Blur | Gaussian blur with configurable radius |
| Brightness / Contrast | Linear brightness and contrast adjustment |
| Color Balance | Cyan↔Red, Magenta↔Green, Yellow↔Blue per shadows/midtones/highlights |
| Color Space | sRGB ↔ Display P3 conversion |
| Crop | Axis-aligned crop with canvas drag-to-select. Parallel row copy. |
| Curves | Interactive curve editor with draggable control points |
| Denoise | Bilateral filter noise reduction |
| Faux HDR | Exposure fusion from ±1 stop virtual brackets |
| Grain | Film grain with presets modelled on classic 35 mm stocks |
| Highlights / Shadows | Independent highlight and shadow recovery |
| HSL Panel | Per-hue-band hue, saturation, and luminance (8 bands) |
| Hue Shift | Global hue rotation in degrees |
| Levels | Black/mid/white point with LUT-based remapping |
| LUT / Color Grading | Apply a .cube 3D LUT with blend strength |
| Perspective | Four-corner keystone/perspective correction |
| Resize | Nearest-neighbour, bilinear, or bicubic resampling |
| Rotate | 90°/180°/270° lossless, or arbitrary angle with bilinear interp |
| Saturation | Global saturation multiplier |
| Sepia | Sepia tone with adjustable strength |
| Sharpen | Unsharp mask convolution |
| Vibrance | Saturation boost that protects already-saturated colours |
| Vignette | Radial darkening with strength, radius, and feather controls |
| White Balance | Temperature and tint sliders |

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

## How the experiment is going

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

## 2026-03-26 (40% of our first week usage)

### Native File Format (.rlab) 
  - **New .rlab binary format** — chunked layout with per-chunk and file-level Blake3 integrity hashes. Stores the
  original source image verbatim, the full edit stack, metadata (timestamps, source path, dimensions, app version), and
  an optional thumbnail.
  - **Save / Save As (Ctrl+S / Ctrl+⇧S)** — saves and reopens the full editing session including undo history. Title bar
  shows filename and a ● dirty indicator when there are unsaved changes.
  - **Export… (Ctrl+E)** — renamed from the old Save; writes the rendered result as JPEG or PNG.
  - **Open dialog now lists .rlab** files alongside images.

### CLI Improvements

  - **Export Edit Stack as JSON (File menu)** — writes the current pipeline as a JSON file compatible with the CLI's `--load-pipeline` argument.
  - **rasterlab batch** --load-pipeline — batch processing can now load a pipeline JSON exported from the GUI and apply it
  to an entire directory in parallel.

### Tools Panel

  - **Sepia tool** — added live 1/4-scale preview on slider change, plus Cancel and Reset buttons, matching all other
  preview-capable tools.
  - **Alphabetical ordering** — all tool sections are now sorted A–Z with Auto Enhance pinned at the top.
  - **Tool panel state persistence** — open/close state of each tool section is saved to a YAML prefs file and restored on next launch (default: all collapsed).

### Canvas
  - **Ctrl+scroll to zoom** — scroll wheel alone no longer hijacks the canvas; hold Ctrl to zoom, pivoting around the cursor.
  - **Magnifier cursor** — cursor changes to a magnifying glass when Ctrl is held over the canvas.

### Bug Fixes, I'm sure there is many more :-)
  - Removed duplicate Save As… entry from the File menu.
  - Resolved 3 pre-existing test failures.

## 2026-03-25 (19% of our first week usage)

- **Levels tool** — black/mid/white point sliders with live preview; changes are non-destructive and can be committed to the pipeline or discarded
- **Render timing** — status bar now shows how long the last render took in milliseconds
- **Intermediate result caching** — the pipeline caches the rendered image after each committed op; changing op N no longer re-runs ops 0 through N-1, and undo/redo are often instant
- **Downsampled live preview** — while a levels slider is being dragged, ops run on a 25% resolution image (~16× fewer pixels) for immediate feedback; a full-res render queues automatically once the preview is displayed
- **CLI file argument** — the GUI now accepts a file path on the command line so you can open an image directly without using the file dialog
- **Crop fix** — corrected crop start/end coordinate handling when panning

## 2026-03-24 (14% of our first week usage)

- **Initial release** — non-destructive editing pipeline, background render thread, undo/redo, histogram panel, crop, rotate (90°/180°/270° + arbitrary angle with bilinear interpolation), sharpen, and black & white conversion
- **Zoom & pan** — canvas supports scroll-to-zoom and drag-to-pan; fit-to-view behavior fixed to work as expected
- **Smoother zoom** — zoom now centers on the cursor position rather than the canvas origin
- **Stability** — fixed a segfault on startup
- **Plugin API** — trait definitions and an example plugin for extending the op set

## Screen shot
<img width="1405" height="933" alt="Screenshot 2026-03-26 at 12 19 59 AM" src="https://github.com/user-attachments/assets/1650d897-232c-44de-80c5-64ee0450a135" />


## License

MIT OR Apache-2.0
