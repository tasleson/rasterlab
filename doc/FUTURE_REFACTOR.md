# Future Refactors

## Render GPU Batch Application

Medium: GPU batch selection/application is duplicated between the full render
and preview committed-op paths in `rasterlab-render/src/lib.rs`.

Full render currently adds sparse intermediate cache entries, while preview does
not, but both paths duplicate the same flow:

- find an adjacent run of GPU-supported operations
- batch the run when it contains more than one operation
- otherwise apply a single operation with optional GPU fallback

Extract the shared loop with an explicit cache-recording hook. Full render can
use the hook to record `(end - 1, current)` sparse intermediates, and preview can
pass a no-op hook. Keep preview scaling and cache policy outside the helper so
the refactor only centralizes batch eligibility, fallback, and validation
behavior.

Current duplication points:

- `rasterlab-render/src/lib.rs:318` full render committed-op loop
- `rasterlab-render/src/lib.rs:382` preview committed-op loop
