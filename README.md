<img width="1472" height="741" alt="rasterlab" src="https://github.com/user-attachments/assets/bbbd7756-9b7a-4431-a1e7-03ffbae72b30" />

# RasterLab

RasterLab is a cross-platform, non-destructive photo editor and photo library written in Rust. It can open JPEG, PNG, and many camera RAW formats; build an editable operation stack; manage virtual copies; and export rendered JPEG or PNG files without changing the imported source image.

RasterLab began as a one-month AI-assisted development experiment and has continued beyond that original scope. The [one-month milestone README](docs/README-2026-04-23.md) preserves the project description as it stood on April 23, 2026. Current changes are tracked through releases rather than in this README.

> [!WARNING]
> RasterLab is experimental, pre-release software. Keep independent backups of important images and libraries. The recovery features described below reduce several corruption and accident risks, but they are not a replacement for a tested backup.

## Highlights

- Non-destructive, ordered edit stacks with editable operations, per-operation enable/disable controls, undo/redo, and virtual copies.
- Background rendering with intermediate-result caching and generation checks that discard stale render results.
- Quarter-scale live previews for interactive tools, followed by full-resolution rendering.
- CPU parallelism through Rayon and optional `wgpu` acceleration for supported operations, with CPU execution for the rest.
- Per-channel RGB and luma histograms, before/after split view, interactive crop, heal, and straighten overlays.
- JPEG, PNG, and broad camera RAW input; JPEG and PNG rendered export; EXIF preservation or removal on export.
- Export resizing and presentation borders with custom text and optional EXIF-derived camera settings.
- A managed photo library with thumbnails, collections, import sessions, search/filtering, metadata editing, duplicate detection, and batch export.
- A C-ABI plugin API and an example plugin.

## Current limitations

- **Adaptive Enhance needs more work.** It is an experimental analysis-driven tool whose correction choices and regional adjustments still need tuning. Treat its output as a starting point and inspect the individual operations it adds to the edit stack.
- Camera RAW support depends on `rawler`; support can vary by camera model and file variant even when the extension is recognized.
- Non-Local Means noise reduction and some geometric or multi-image operations can be slow on large inputs.
- The plugin interface exists and an example plugin is included, but the third-party plugin ecosystem is not established. Plugins are loaded through the library API only; there is no command-line flag or user interface for loading them, and a loaded plugin is trusted native code (see [Plugins](#plugins)).

## Supported files

| Purpose | Formats |
|---|---|
| Open/import | JPEG, PNG, and camera RAW |
| Recognized RAW extensions | `3fr`, `arw`, `cr2`, `cr3`, `dng`, `erf`, `iiq`, `nef`, `nrw`, `orf`, `pef`, `raf`, `raw`, `rw2`, `sr2`, `srf`, `srw` |
| Rendered export | JPEG and PNG |
| Native project | `.rlab` |
| Library export | Rendered JPEG/PNG, the verbatim imported original, or the `.rlab` project |

RAW files are decoded to an editable sRGB image. RasterLab retains the original file bytes in a saved `.rlab` project or managed library item; it does not write changes back into a RAW file.

## Supported operations

### One-click actions and looks

| Action | What it does | Status |
|---|---|---|
| Auto Enhance | Applies a histogram-based levels stretch, a small saturation boost, and mild sharpening as separate editable operations. | Available |
| Adaptive Enhance | Analyzes color cast, tone, chroma, sharpness, borders, and regional lighting, then adds the corrections it selects as editable operations. | **Experimental; needs more work** |
| Old Photo Restore | Uses the global analysis path, without Adaptive Enhance's regional decisions, to propose color, tone, saturation, and sharpness corrections for faded prints and scans. | Available |
| Classic B&W | Applies a channel-mixed monochrome conversion, a brightness/contrast adjustment, and a vignette. | Available |
| 35mm Sprocket Panorama | Applies a positionable 2:1 crop and a 35 mm film-frame treatment with selectable film stock and randomized markings. | Available |

### Editing tools

The table follows the tool order in the GUI.

| Tool | What it does |
|---|---|
| Airplane Window | Reduces aircraft-window color cast, haze, and reflections with an overall strength control. |
| Black & White | Converts with luminance, average, perceptual, or channel-mixer modes and presets. |
| Blur | Applies a Gaussian blur with a configurable radius. |
| Brightness / Contrast | Adjusts linear brightness and contrast. |
| Channel Levels | Sets independent black, gamma, and white points for the red, green, and blue channels. |
| Clarity / Texture | Adjusts local contrast at midtone and fine-detail spatial scales. |
| Color Balance | Shifts cyan/red, magenta/green, and yellow/blue independently in shadows, midtones, and highlights. |
| Color Space | Converts pixel values between sRGB and Display P3. |
| Crop | Crops interactively or by coordinates, with free, 3:2, 4:3, 1:1, 16:9, 9:16, and custom aspect ratios. |
| Curves | Applies an interactive tone curve with draggable control points. |
| Denoise | Applies a bilateral filter with configurable strength and radius. |
| Faux HDR | Builds virtual ±1 EV brackets from one image and exposure-fuses them. |
| Focus Stack | Fuses multiple focus-bracketed images using a Sum-Modified-Laplacian focus measure, correcting the magnification difference focus breathing leaves between frames. |
| Grain | Adds configurable film grain. |
| HDR Merge | Merges two or more bracketed exposures using exposure estimation, radiance merging, and Reinhard tone mapping. |
| Heal | Performs spot healing with an automatically selected source patch or a clone source. |
| Highlights / Shadows | Adjusts highlight and shadow regions independently. |
| HSL Panel | Adjusts hue, saturation, and luminance in eight color bands. |
| Hue Shift | Rotates hue globally. |
| Levels | Sets black, mid, and white points with LUT-based remapping. |
| Local Tone | Uses edge-aware local Laplacian filtering to compress large-scale contrast while retaining or boosting texture. |
| LUT / Color Grading | Applies a `.cube` 3D LUT with adjustable blend strength. |
| Noise Reduction | Provides wavelet and Non-Local Means methods with separate luminance, color, and detail controls. |
| Panorama | Stitches multiple images using feature matching, RANSAC homography estimation, and feather blending. |
| Perspective | Applies four-corner keystone and perspective correction. |
| Resize | Resamples with nearest-neighbor, bilinear, or bicubic interpolation. |
| Rotate | Rotates by right angles or an arbitrary angle, optionally crops the result, and provides horizontal/vertical flip controls. |
| Saturation | Applies a global saturation multiplier. |
| Sepia | Adds a sepia tone with adjustable strength. |
| Shadow Exposure | Changes shadow exposure in EV while leaving highlights substantially untouched. |
| Sharpen | Applies unsharp-mask sharpening. |
| Split Tone | Tints shadows and highlights with independent hue/saturation and a balance control. |
| Straighten | Rotates by a numeric angle or draggable horizon line, with optional crop-to-rectangle. |
| Vibrance | Boosts lower-saturation colors while protecting colors that are already saturated. |
| Vignette | Applies radial darkening with strength, radius, and feather controls. |
| White Balance | Adjusts temperature and tint. |

A linear or radial gradient mask can wrap the next applied operation. Multi-image tools such as Focus Stack, HDR Merge, and Panorama prompt for additional source files and add their result to the same non-destructive pipeline. They accept managed-library photos as source frames, reading each one's embedded original.

## Protecting images and projects

RasterLab uses several independent mechanisms because no single checksum, undo stack, or filesystem flag covers every way work can be lost.

### Non-destructive editing and recovery from user mistakes

- Editing operations are stored as parameters in a pipeline; the source pixels are not overwritten.
- A `.rlab` file embeds the original source-file bytes verbatim, plus every virtual copy's pipeline and undo cursor. Library export can write those original bytes back out with the original filename and recorded timestamps, or copy out the whole `.rlab` — edits included — named after the imported file rather than its content hash.
- Undo/redo, operation enable/disable controls, editable stack entries, and virtual copies make edits reversible without duplicating the source image.
- Pipeline state is autosaved after changes. Previous unsaved sessions can be restored from **File > Previously Unsaved Work**.
- Opening another file or library photo while edits are unsaved requires confirmation.
- Library deletion requires confirmation and moves photos to RasterLab's **Recently Deleted** view, where they can be restored, deleted permanently, or removed together with **Empty Recently Deleted**.
- All four, and deleting collections, run in the background with a progress count and a **Stop** button, so a large batch on a network-mounted library never blocks the window. One runs at a time. Stopping keeps whatever has already been done; what could not be done is listed by name.
- A library photo can be marked **Protected**. RasterLab then refuses to delete it (even into Recently Deleted) until it is unprotected.

### Detecting corruption in `.rlab` files

New editor projects and library items are written as `.rlab` format v5. The format is a chunked container holding project metadata, the verbatim original, virtual-copy edit stacks, an optional preview, and optional library metadata.

- Every chunk has a BLAKE3 digest, so damage can be associated with a specific chunk.
- A trailing BLAKE3 digest covers the complete container, including chunk framing and parity data.
- The recovery data stores a BLAKE3 digest for each data shard. This lets repair identify the damaged shards instead of discarding an entire large chunk because one byte changed.
- These hashes detect accidental changes. They are not a signature or authentication mechanism: someone deliberately modifying a file can recompute unkeyed hashes.

### Repairing bit rot and unreadable sectors

Each v5 file contains Reed–Solomon recovery data in two `RECC` chunks, one before and one after the protected content.

- Files that fit in 4 KiB data shards target roughly 10% parity, with a minimum of one parity shard. Larger files use larger shards and target roughly 20% parity. The parity set is stored twice so damage to one recovery copy does not necessarily remove the ability to repair the content.
- The two copies bracket the content. If bytes are truncated from either end, the surviving copy records the protected length and alignment needed to treat the missing part as erased shards.
- Recovery locates `RECC` candidates by scanning for their signatures and validating them, rather than trusting the ordinary chunk chain. A damaged chunk-length field therefore cannot by itself hide all recovery data.
- A degraded reader retries failed bulk reads in 4 KiB blocks, zero-fills only unreadable regions, and passes those regions to Reed–Solomon as erasures. One unreadable disk sector need not make the whole project unreadable.
- Repair succeeds only while the number of damaged data shards is within the parity-shard budget. The percentage is an approximate shard budget, not a guarantee that the same percentage of arbitrary byte damage can always be recovered.

### Verifying saves and scrubbing a library

- A v5 save is staged in a hidden file beside its destination, flushed with `fsync`, read back, and compared byte-for-byte with the in-memory file before being renamed into place. Cache-bypass/eviction requests are advisory on platforms that support them, so the exact storage layer exercised by the read-back is OS-dependent.
- Every other file RasterLab writes — exports, preferences, autosaves, pipeline JSON — is staged and renamed the same way, without the read-back. A crash therefore leaves the previous file whole rather than a truncated one. On Unix the containing directory is flushed after the rename, and directories created along the way are flushed into their own parents, so a newly created library shard cannot take a verified file down with it.
- **File > Start Integrity Scrub** verifies every `.rlab` in the open library. It leaves clean v5 files alone, upgrades clean v3/v4 files to v5, and repairs correctable damage.
- Before a scrub replaces a damaged file, it copies the damaged original into the library's `recovered/` tree, verifying that backup the same way a save is verified. The repaired temporary file is then renamed over the live file on the same filesystem.
- Library files are addressed by the BLAKE3 hash of their embedded original bytes. A scrub also compares that identity with the file's name and directory, detecting a valid but misplaced or misdirected file that internal checksums alone would accept.
- The Stoolap database is an index, not the only copy of library metadata. Ratings, flags, labels, captions, keywords, collections, EXIF snapshots, and edit state are embedded in `.rlab` files, allowing **File > Rebuild Library Index** to reconstruct the catalog. A rebuild walks every photo, so it can be stopped from the same menu item or from the progress line in the library toolbar; it keeps whatever it re-indexed, and running it again finishes the job.

To verify an individual project from the source tree:

```sh
cargo run --release -p rasterlab-core --example rlab_verify -- photo.rlab
```

Write recovery to a separate file so the damaged input remains available:

```sh
cargo run --release -p rasterlab-core --example rlab_verify -- \
  --repair-to repaired.rlab photo.rlab
```

### What these mechanisms do not protect against

- Loss of the device, deletion of the whole library, ransomware, fire, theft, or corruption beyond the parity budget.
- Damage to standalone source images or exported JPEG/PNG files that have not been saved inside `.rlab`.
- A stale but internally valid older version replacing a newer project.
- Failures that are never checked: library scrubbing is user-initiated, not a continuous background service.

Keep at least one independent backup on another device or service, and periodically test that it can be restored.

## Photo library

The managed library imports each photo into a content-addressed v5 `.rlab` file under `files/ab/cd/<blake3>.rlab`. A Stoolap database indexes the embedded information for fast browsing and search.

Current library features include:

- File or recursive folder import, duplicate detection, RAW+JPEG pairing, and 512 px thumbnails.
- Import-session grouping; folder imports use capture dates, with filesystem timestamps as fallback, to rebuild a useful historical timeline. Consecutive shooting days merge into one session, except that a day of more than 100 photos is kept as a session of its own. A **Recent Imports** list in the sidebar shows the last 10 sessions in the order they were imported, so a folder of old photographs is one click away instead of buried under its capture date.
- Ratings, pick/reject flags, color labels, captions, keywords, and batch metadata edits.
- Collections: create one from the sidebar, then right-click a selection in the grid and use **Collections** to put photos in or take them out. Right-clicking a collection in the sidebar renames or deletes it, and selecting a photo lists the collections it belongs to. Ctrl-click and shift-click mark several collections in the sidebar the way they select several photos in the grid, so a batch of them can be deleted in one confirmation, which runs in the background like the Recently Deleted operations. A photo can be in as many collections as you like, and each membership is stored in that photo's own `.rlab`, so it survives an index rebuild. Deleting a collection leaves its photos alone. Files record a collection's identity rather than its name, so renaming one is instant however many photos are in it; the name each file carries is only a fallback for rebuilding an index that has been lost outright, and may be a rename behind.
- Import-time collections: choosing **File > Import Photos > Select Folder…** asks, before the import starts, whether the photos should be filed into a collection — one per folder, named after the folder that directly holds them, or a single collection you name for the whole import. A collection that already goes by that name is used as it is, so importing the same folder again adds only what is new. The choice is remembered between imports.
- Filtering by text, rating, flag, color label, camera, lens, capture date, aperture, shutter speed, ISO, pixel dimensions, and edited state.
- Sorting by import date, capture date, rating, or filename.
- Batch rendered export with resize constraints and presentation borders, or verbatim export of imported originals or of the `.rlab` projects themselves.
- Focus stacking from the grid: select the frames, right-click, and **Focus Stack** opens the first one in the editor with the whole selection loaded as source frames.
- Index rebuilding, integrity scrubbing, protected-photo deletion guards, and a library-owned Recently Deleted area that works consistently on local and network filesystems.

## Plugins

RasterLab can load additional operations from shared libraries that implement the C ABI in `rasterlab-plugin-api`. `plugins/example-plugin` is a complete working example (a sepia tone filter).

> [!WARNING]
> **A plugin is trusted native code.** It is loaded into the RasterLab process with `dlopen` and runs with the full privileges of the user running the application: it can read and write any file that user can, open network connections, and corrupt any memory in the process. The plugin ABI is not a security boundary — there is no sandbox and no separate address space, and a plugin can bypass the ABI entirely. Install plugins only from sources you would trust to run as an ordinary program.

The loader does check what a plugin reports, so that an honest plugin bug produces an error rather than a crash or a corrupted image: the ABI version must match exactly, plugin metadata must be short, null-terminated UTF-8 with a non-empty name, and every image an operation returns must carry a known pixel format and a byte length that matches its dimensions. Image sizes are computed in 64-bit arithmetic on both sides of the boundary, and a buffer a plugin allocates is always released by that plugin's own deallocator, since host and plugin have separate allocators.

Loading is a library-level API — `rasterlab_core::plugin_loader::PluginRegistry`, which can load one library or scan a directory. There is no `--plugin` command-line flag and no user interface for loading plugins yet.

## Building and running

RasterLab uses Rust 2024 edition and its dependencies require Rust 1.92 or newer; that minimum is recorded as `rust-version` in the workspace manifest and built by CI. Platform packages required by `eframe`, `wgpu`, and native file dialogs may also be needed.

```sh
cargo build --release
cargo run --release -p rasterlab-gui
```

An image path may be passed to the GUI:

```sh
cargo run --release -p rasterlab-gui -- photo.nef
```

Run the test suite with:

```sh
cargo test --workspace
```

The GPU kernel tests are marked `#[ignore]` because they need a working `wgpu`
adapter. Run them explicitly on a machine that has one:

```sh
cargo test -p rasterlab-gpu -- --ignored
```

## Command-line interface

The `rasterlab` CLI provides single-image processing, parallel directory batches, metadata/histogram inspection, JSON pipeline save/load, and library creation, import and maintenance. Its direct operation flags currently cover crop, rotate, black and white, Airplane Window correction, and sharpen; loading a saved pipeline can apply a broader serialized edit stack.

```sh
# Show all commands and options
cargo run --release -p rasterlab-cli -- --help

# Process one image
cargo run --release -p rasterlab-cli -- process photo.nef \
  -o output.jpg --rotate 90 --sharpen 0.8

# Inspect metadata and histograms
cargo run --release -p rasterlab-cli -- info photo.jpg
```

### Libraries

Creating a library, importing into it, rebuilding its index and scrubbing its
files are all CLI commands, so a library on a headless machine can be filled
and maintained over ssh or from cron rather than being mounted on a desktop
first. They all take the library root and stop cleanly on Ctrl-C after the file
they are on; a second Ctrl-C quits immediately, which is safe because every
`.rlab` write is staged and renamed into place.

On a terminal they show a spinner with the counts the GUI shows — files done of
total, imported, repaired, upgraded, errors, and an estimate of the time left —
over the file currently being read. Redirected to a log or a cron mail that
becomes one whole line every thirty seconds instead, and `--quiet` leaves only
the final tally.

```sh
# Create an empty library
rasterlab library create /srv/photos

# Import a card, a shoot tree, or loose files; folders are searched recursively
rasterlab library import /srv/photos ~/cards/DCIM
rasterlab library import /srv/photos ~/shoots --collection-per-folder
rasterlab library import /srv/photos iceland/*.nef --collection "Iceland 2024"

# Re-index the .rlab files on disk, recovering rows the index has lost
rasterlab library rebuild /srv/photos

# Verify every file and repair what its parity can recover
rasterlab library scrub /srv/photos --quiet
```

An import groups what it brings in into back-dated sessions by capture date,
the same way the GUI groups a folder import, so importing an existing archive
reconstructs its history instead of landing it all under today. Photos already
in the library are skipped by content hash, which makes re-running an import
over the same source cheap — and is why an interrupted import is finished by
simply running it again. `--create` makes the library as part of the import for
the first run.

Every command exits non-zero if any file failed, so a scheduled scrub is worth
running under a job that reports failures. Uncorrectable corruption is listed
on stderr with the file that carries it; the damaged original of anything
repaired is kept under `recovered/`.

## Architecture

```text
rasterlab-core/       Image type, formats, operations, pipeline, .rlab format
rasterlab-render/     Background rendering, preview scheduling, GPU/CPU routing
rasterlab-gpu/        wgpu compute kernels for supported operations
rasterlab-gui/        egui/eframe desktop application
rasterlab-library/    Managed library, Stoolap index, import/export, integrity scrub
rasterlab-cli/        Headless single-image, batch, inspection, and library management
rasterlab-plugin-api/ Stable C-ABI types for external operations
plugins/              Example plugin
```

The render path serializes operation parameters for background execution. Intermediate images are cached by pipeline step, and generation counters prevent an older render from replacing a newer request. Supported adjacent operations may remain on the GPU as a batch; unsupported operations fall back to the CPU pipeline.

## License

MIT OR Apache-2.0
