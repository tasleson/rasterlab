<img width="1408" height="768" alt="vibe_this" src="https://github.com/user-attachments/assets/bcf3af6b-4982-4dad-b460-99cca597660a" />

# RasterLab

> *How good of an image editor can you build with $20 worth of Claude Code Pro subscription?[2]*

We're here to find out!  This is still a work in progress!

## What is this?

RasterLab is a non-destructive RAW image editor written in Rust, built almost entirely by Claude Code as an experiment in AI-assisted software development. It has a real-time preview pipeline, intermediate result caching, parallelized image processing, undo/redo, a histogram panel, and support for JPEG, PNG, and a broad range of camera RAW formats (Nikon NEF/NRW, Canon CR2/CR3, Sony ARW, Fujifilm RAF, Panasonic RW2, Olympus ORF, Pentax PEF, Adobe DNG, and more).

See change log below for status.

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
| **Looks** | |
| Classic B&W | Channel-mixed B&W conversion with brightness lift and vignette |
| | |
| **——————————————** | **——————————————————————————————————————————** |
| | |
| Black & White | Luminance, average, perceptual, or channel mixer with presets |
| Blur | Gaussian blur with configurable radius |
| Brightness / Contrast | Linear brightness and contrast adjustment |
| Clarity / Texture | Local contrast at midtone (clarity) and fine-detail (texture) spatial scales |
| Color Balance | Cyan↔Red, Magenta↔Green, Yellow↔Blue per shadows/midtones/highlights |
| Color Space | sRGB ↔ Display P3 conversion |
| Crop | Axis-aligned crop with aspect-ratio presets (3:2, 4:3, 1:1, 16:9, 9:16, custom) |
| Curves | Interactive curve editor with draggable control points |
| Denoise | Bilateral filter noise reduction |
| Faux HDR | Exposure fusion from ±1 stop virtual brackets |
| Flip | Horizontal or vertical mirror |
| Focus Stack | Fuse multiple frames at different focus distances into one all-in-focus image (Sum-Modified-Laplacian focus measure) |
| Grain | Film grain with configurable strength and size |
| Heal | Content-aware spot heal / clone stamp; auto-detects source patch |
| Highlights / Shadows | Independent highlight and shadow recovery |
| HSL Panel | Per-hue-band hue, saturation, and luminance (8 bands: Reds … Magentas) |
| Hue Shift | Global hue rotation in degrees |
| Levels | Black/mid/white point with LUT-based remapping |
| Local Adjustments | Linear or radial gradient mask applied to any op |
| LUT / Color Grading | Apply a .cube 3D LUT with blend strength |
| Noise Reduction | Wavelet (fast) or Non-Local Means (quality); independent luma/chroma strength |
| Panorama | Stitch multiple images; Harris corners + normalised-patch matching + RANSAC homography + feather blend |
| Perspective | Four-corner keystone/perspective correction |
| Resize | Nearest-neighbour, bilinear, or bicubic resampling |
| Rotate / Straighten | 90°/180°/270° lossless; arbitrary angle with bilinear interp; horizon-line straighten with auto-crop |
| Saturation | Global saturation multiplier |
| Sepia | Sepia tone with adjustable strength |
| Shadow Exposure | Lift or crush shadows with an EV-stops gain in linear light, highlights untouched |
| Sharpen | Unsharp mask convolution |
| Split Tone | Shadow / highlight tinting with independent hue, saturation, and balance |
| Vibrance | Saturation boost that protects already-saturated colours |
| Vignette | Radial darkening with strength, radius, and feather controls |
| White Balance | Temperature and tint sliders |

## Building

```sh
cargo build --release
cargo run --release -p rasterlab-gui
```

It's still not as fast as I would like.  ~~Claude thinks it's fast, but I disagree.~~ It's much faster, but would like it just a little bit more :-)

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

# Week 3

## 2026-04-13 (Week 3 is a wrap, we used 72% of our total available usage[1])

### Features

- **UI scaling** — add UI Scale option to Preferences menu
- **Mouse pan** — add right-mouse button as an alternative pan gesture
- **Panorama Stitching** - load multiple images and stich them together as one
- **Focus stacking** - load multiple images to improve focus
- **More RAW** - allow the reading of all supported RAW file types by rawler crate
- **EXIF** - display, preserve, and optionally strip on export
- **Edit stack edits** - edit entries on the edit stack instead of deleting and creating a new edit
- **Split view option** - before/after using the before as the original file or anywhere in between
- **Shadow only exposure control** - Allows you to selectively control the exposure of shadows

### Tools Panel
- Added a expand all, collapse all ability

### Bug fixes

- Center the file dialog, instead of having it in the bottom left corner of screen
- Allow undo to restore an edit from the edit stack that was deleted
- Only send title viewport command when title changes (reduces idle CPU load)
- Stop continuous render loop when straighten tool is idle
- Make rotate work like other tools, apply/reset/cancel
- Fix button glyphs
- Make histograms more usable after editing


# Week 2

## 2026-04-04 (Week 2 is a wrap, we used 98% of our weekly usage)

### Features

- **Unsaved-changes protection** — opening a new file now prompts to discard unsaved changes. Pipeline state is autosaved after every edit so a crash or forced close never loses work; confirmation dialogs guard all destructive open actions.
- **Preview, cancel, and reset on all parameter tools** — every slider-based tool now has a consistent preview/cancel/reset workflow. Live 1/4-scale preview while adjusting; Cancel restores the previous state; Reset returns to defaults.
- **Cancel in-flight noise reduction** — long-running NR renders can now be cancelled mid-flight without waiting for completion.
- **Recently opened files** — File menu now lists recently opened images for quick re-access.
- **Build metadata in About dialog** — Help → About RasterLab now shows the exact build version and metadata.

### Tools Panel

- Reordered to enforce Auto Enhance first, Looks second, all others strictly alphabetical.

### Bug Fixes

- Edit stack panel now uses theme-aware colors; virtual-copy rename dialog anchors to its tab instead of the window.
- 1:1 zoom now centers on the current view center instead of jumping to the top-left corner.
- Cleared stale cached before-texture when opening a new image, preventing ghost pixels from the previous file bleeding through.
- Scaled crop coordinates correctly for the downsampled preview path, fixing misaligned crops at reduced resolution.
- Linux: capped wgpu `max_color_attachments` and used adapter limits to prevent `LimitsExceeded` panics on some drivers.

### Refactoring, don't repeast yourself aka. DRY

- Extracted `ToolState` from `AppState` to reduce coupling and improve testability.
- Consolidated HSL helpers, extracted `get_pixel`, and unified sample closures (DRY pass).
- I should also try to target the test code, lots of duplicates there


## 2026-04-02 (55% of our weekly usage)

### Virtual Copies

- **Virtual copies** — create any number of independent edit stacks for the same source image. Each copy has its own op list, undo/redo history, and name. The Edit Stack panel now shows a tab bar — click a tab to switch, `+` to add an empty copy, right-click for duplicate/rename/delete. All copies share the decoded source image in memory with zero pixel duplication. The `.rlab` format (now v2) saves every copy and restores the active one on open; v1 files load transparently as a single copy named "Copy 1".

### New Tools

- **Spot Heal / Clone Stamp** — Photoshop-style content-aware heal. Click to place a repair spot; RasterLab auto-detects the best source patch. Hover highlight shows which spot is selected. Multiple spots are committed as a single pipeline op.
- **High-quality Noise Reduction** — two algorithms selectable per image: Wavelet (fast Haar soft-thresholding, ~250 ms on 20 MP) and Non-Local Means (patch-based, highest quality). Independent luminance and chrominance strength controls; edge-preserving detail mask blends the result back toward real scene edges without protecting noise-induced false gradients.
- **Clarity / Texture** — local contrast enhancement at two spatial scales (clarity = midtone contrast, texture = fine surface detail). Live 1/4-scale preview while adjusting.
- **Local Adjustments** — non-destructive masks: linear gradient and radial gradient. Any tool op can be wrapped in a mask so adjustments apply only to part of the frame.
- **Straighten** — draw a horizon line on the canvas to set the rotation angle. Read-only angle display updates live as you drag; auto-crop option removes exposed corners after rotation.
- **Crop aspect ratio presets** — 3:2, 4:3, 1:1, 16:9, 9:16 and a custom ratio picker alongside the existing free-crop mode.
- **Classic B&W look** — one-click preset applying a channel-mixed B&W conversion, a subtle brightness/contrast lift, and a vignette.

### Canvas

- **Split before/after view** — draggable vertical divider shows the source (with geometric ops only) on the left and the fully edited result on the right. Correctly tracks rotation, flip, and crop so both sides share the same framing.
- **Large-image safety** — canvas textures are downsampled when the image exceeds the wgpu 8192 px per-side limit, preventing GPU upload failures on very high-resolution files.

### Performance

- **Vertical blur** — replaced the transpose-based pass with parallel column strips; measurably faster on large images.

### Infrastructure

- **egui / eframe 0.29 → 0.34** — framework upgrade; native file dialog integrated via egui-file-dialog with a fallback to the built-in chooser when the native dialog is unavailable.

### Bug Fixes

- Noise reduction: detail-preservation mask was computed from the noisy input, causing it to classify noise as "detail" and blend it back — making NR imperceptible at default settings. Mask now computed from the denoised output.
- Spot heal: spot selection hit-testing and hover highlight were broken; both fixed.

---

# Week 1

## 2026-03-30 (Week 1 is a wrap, we used 82% of our usage)

### Usage
- I didn't allocate enough dedicated time to leverage Claude effectively, so some resources went unused.

### Perf focus

- There's a performance-focused branch pending merge that needs cleanup. We've reduced latency from ~800ms to ~50ms for computing and displaying user-requested edits. Half the improvement came from hardware-specific optimizations; the rest from bug fixes, particularly in histogram generation. The goal is ≤42ms to achieve virtually real-time feel.


## 2026-03-27 (46% of our first week usage)

### New Tools

- **Split Tone** — tint shadows and highlights with independent hue and saturation controls. A balance slider shifts the crossover point between the two zones. Defaults to cool blue shadows / warm gold highlights — the classic darkroom look.

### Tools Panel

- **Sharpen** — added live 1/4-scale preview while dragging the strength slider, with a Cancel button to discard. Slider replaces the old drag-value widget for consistency with other preview tools.
- **Resize** — added MP preset dropdown that shows standard megapixel targets (1 MP – 24 MP) filtered to only those smaller than the current source image. Each entry shows the computed pixel dimensions for the image's aspect ratio. Selecting a preset populates the Width and Height fields; manual editing and lock-aspect still work as before.
- **LUT / Color Grading** — fixed the Load .cube file dialog hanging the event loop; now runs on a background thread like all other file dialogs.

### Canvas / Preview Performance

- **Viewport-restricted previews** — when zoomed in, preview renders now process only the pixels visible on screen rather than the entire image. The render thread receives the visible image region each frame and restricts work accordingly.
- **Overlay-based full-resolution preview** — when the pipeline is fully cached (the common slider-drag case), the preview op is applied at full resolution to just the visible viewport and drawn as an overlay on top of the base image. Sharp at any zoom level; base image is never replaced so the canvas never goes blank.
- **Stable zoom/pan during previews** — the canvas no longer resets zoom and pan position when a downsampled preview image arrives. Scale compensation ensures the preview fills the same screen area as the full-res image.

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


***[1]** The way Anthropic does the blocks of usage for a day, and then week etc. is annoying.  I understand why they do it, but it benefits them, not the paying customer*
***[2]** I wasn't diligent enough to get the full $20 worth, but that's Anthropic's loss as it diminishes what this could have been*
## License

MIT OR Apache-2.0
