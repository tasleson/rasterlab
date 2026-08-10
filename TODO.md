# TODO

- [x] DONE Make project saves atomic. Write new `.rlab` data to a unique temporary file in the destination directory, sync and verify it, atomically replace the destination, and sync the parent directory. Reuse the safe staging approach already used by library scrubbing.

- [x] DONE Harden parsing and image allocation against invalid or hostile dimensions and lengths. Validate `.rlab` chunk lengths against the remaining file before allocating, impose sensible per-chunk limits, use checked integer conversions, and introduce a fallible image constructor with checked size arithmetic.

- [x] DONE Add continuous-integration quality gates for pushes and pull requests. Run formatting checks, Clippy with warnings denied, workspace tests, release compilation, and an MSRV build; run GPU-backed tests periodically on a suitable adapter.

- [ ] Define recoverable consistency semantics for filesystem and library-database updates. Use atomic file staging and database transactions where possible, make multi-step operations idempotent, and add reconciliation or retry handling for interrupted imports, metadata updates, and deletions.

- [ ] Decompose orchestration-heavy GUI modules along responsibility boundaries. Extract project/session persistence, render coordination, and library background tasks from `AppState`; split crop, mask, heal, straighten, split-view, and presentation-texture behavior out of the canvas module. Consolidate the duplicated full-render and preview GPU batching loops documented in `doc/FUTURE_REFACTOR.md`.

- [ ] Contain background-worker failures. Catch panics at worker boundaries and turn them into error results, make thread spawning fallible, handle disconnected channels, and ensure the GUI always clears loading state when a worker terminates unexpectedly.

- [ ] Make `ColorSpaceOp` reachable from the GPU dispatcher. It never overrides
  `Operation::as_any`, so `rasterlab_gpu::ops::classify` cannot downcast it and
  `apply_one` returns `UnsupportedOperation("color_space")`; the color_space
  kernel, its dispatch arm, and its shader are unreachable, and the render
  pipeline silently falls back to the CPU. Audit the other ops for the same gap.

- [ ] Give the GPU/CPU comparison tests an adapter-independent tolerance. Seven
  of the thirty tests in `rasterlab-gpu/src/tests.rs` fail on an AMD Radeon 780M:
  the hue-shift, saturation, vibrance, and pipeline-chaining tests assert exact
  byte equality against the CPU reference but drift by up to 3 LSB, and denoise
  reaches 19 against a tolerance of 2. Decide the per-op tolerance that still
  catches real breakage, so the scheduled GPU job means something.

- [ ] Clarify and harden the native plugin boundary. Document that plugins are trusted native code, use checked image-size arithmetic, define robust cross-library allocation/deallocation ownership, validate returned metadata where possible, add plugin-loader/ABI tests, and either implement or remove the documented `--plugin` loading path.
