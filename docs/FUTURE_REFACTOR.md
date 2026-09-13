# Future Refactors

## Render GPU Batch Application — done

The full-render and preview committed-op paths in `rasterlab-render/src/lib.rs`
now share a single loop, `run_committed_ops`, parameterised by a cache-recording
hook: full render records `(index, current)` per op position and `(end - 1,
current)` at a batch readback boundary, while preview and the overlay path pass
`no_intermediates`. Preview scaling and cache policy stay with the callers, so
the helper only owns batch eligibility, GPU fallback, and image validation.
