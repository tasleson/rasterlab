# RasterLab — Feature Ideas

Feature gap analysis vs. commercial offerings (Lightroom, Capture One, DxO PhotoLab, ON1, darktable).

---

## Tier 1 — Table Stakes (blocking adoption)

1. **Lens corrections** — Automatic geometric distortion, chromatic aberration (purple/green fringing),
   and vignette correction using lens profiles. CA in particular is highly visible in RAW files.

---

## Tier 2 — Strong Differentiators

2. **Real HDR merge** — Merge 2–7 bracketed exposures into a true HDR image (vs. current single-image
   faux HDR). Feature Lightroom, ON1, and darktable all offer and photographers actively seek.

---

## Tier 3 — Valuable but Not Blockers

3. **Library / catalog view** — Grid view for browsing a folder with star ratings, color labels,
   and pick/reject flags. Without this, RasterLab is file-at-a-time rather than a workflow tool.
   Even a minimal folder browser + rating implementation dramatically improves daily use.

4. **Export presets** — Save named export configurations (size, format, quality, color space,
   watermark) for one-click reuse. Currently every export requires re-entering settings.

5. **Watermarking on export** — Text or image watermark composited at export time. Standard in
   Lightroom; frequently requested by photographers sharing work online.

6. **Defringe / chromatic aberration tool** — Manual purple/green fringe removal with eyedropper
   selection. Handles edge cases and non-profiled lenses beyond automatic lens corrections.

7. **Soft proofing** — Simulate how the image will look with a target ICC profile (sRGB, AdobeRGB,
   printer profile). Critical for print workflows. Lightroom, darktable, and RawTherapee all have this.

---

## Tier 4 — Nice to Have

8. **Tethered capture** — Live import from a connected camera during a shoot. Capture One's strongest
   card. Complex to implement but a professional studio workflow must-have.

9. **AI subject/sky masking** — Auto-detect and mask sky, subject, background without manual brush
   work. Lightroom's most-praised recent feature. Requires an ML model (ONNX Runtime in Rust is
   feasible).

10. **Map / GPS view** — Show geotagged photos on a map. Nice for travel photographers; not a
    workflow blocker.
