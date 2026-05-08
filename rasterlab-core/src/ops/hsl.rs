/// Convert linear RGB (each in `[0.0, 1.0]`) to HSL.
pub(super) fn rgb_to_hsl(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) * 0.5;

    if (max - min).abs() < 1e-9 {
        return (0.0, 0.0, l);
    }

    let d = max - min;
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };

    let h = if (max - r).abs() < 1e-9 {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if (max - g).abs() < 1e-9 {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };

    (h / 6.0, s, l)
}

/// Convert HSL back to linear RGB (each in `[0.0, 1.0]`).
pub(super) fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (f32, f32, f32) {
    if s < 1e-9 {
        return (l, l, l);
    }
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    (
        hue_to_rgb(p, q, h + 1.0 / 3.0),
        hue_to_rgb(p, q, h),
        hue_to_rgb(p, q, h - 1.0 / 3.0),
    )
}

pub(super) fn hue_to_rgb(p: f32, q: f32, mut t: f32) -> f32 {
    if t < 0.0 {
        t += 1.0;
    }
    if t > 1.0 {
        t -= 1.0;
    }
    if t < 1.0 / 6.0 {
        return p + (q - p) * 6.0 * t;
    }
    if t < 0.5 {
        return q;
    }
    if t < 2.0 / 3.0 {
        return p + (q - p) * (2.0 / 3.0 - t) * 6.0;
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f32 = 1e-5;

    #[test]
    fn black_is_achromatic() {
        let (_, s, l) = rgb_to_hsl(0.0, 0.0, 0.0);
        assert!((l).abs() < EPS);
        assert!((s).abs() < EPS);
    }

    #[test]
    fn white_is_achromatic() {
        let (_, s, l) = rgb_to_hsl(1.0, 1.0, 1.0);
        assert!((l - 1.0).abs() < EPS);
        assert!((s).abs() < EPS);
    }

    #[test]
    fn pure_red() {
        let (h, s, l) = rgb_to_hsl(1.0, 0.0, 0.0);
        assert!((h).abs() < EPS, "h={h}");
        assert!((s - 1.0).abs() < EPS, "s={s}");
        assert!((l - 0.5).abs() < EPS, "l={l}");
    }

    #[test]
    fn pure_green() {
        let (h, s, l) = rgb_to_hsl(0.0, 1.0, 0.0);
        assert!((h - 1.0 / 3.0).abs() < EPS, "h={h}");
        assert!((s - 1.0).abs() < EPS, "s={s}");
        assert!((l - 0.5).abs() < EPS, "l={l}");
    }

    #[test]
    fn pure_blue() {
        let (h, s, l) = rgb_to_hsl(0.0, 0.0, 1.0);
        assert!((h - 2.0 / 3.0).abs() < EPS, "h={h}");
        assert!((s - 1.0).abs() < EPS, "s={s}");
        assert!((l - 0.5).abs() < EPS, "l={l}");
    }

    #[test]
    fn round_trip() {
        let cases = [
            (0.8, 0.2, 0.5),
            (0.1, 0.9, 0.3),
            (0.5, 0.5, 0.5),
            (0.0, 0.4, 0.8),
            (1.0, 0.6, 0.0),
        ];
        for (r, g, b) in cases {
            let (h, s, l) = rgb_to_hsl(r, g, b);
            let (r2, g2, b2) = hsl_to_rgb(h, s, l);
            assert!((r - r2).abs() < EPS, "r mismatch for ({r},{g},{b}): {r2}");
            assert!((g - g2).abs() < EPS, "g mismatch for ({r},{g},{b}): {g2}");
            assert!((b - b2).abs() < EPS, "b mismatch for ({r},{g},{b}): {b2}");
        }
    }

    #[test]
    fn grey_stays_grey_round_trip() {
        for v in [0.0f32, 0.25, 0.5, 0.75, 1.0] {
            let (h, s, l) = rgb_to_hsl(v, v, v);
            assert!((s).abs() < EPS, "s should be 0 for grey v={v}: {s}");
            let (r2, g2, b2) = hsl_to_rgb(h, 0.0, l);
            assert!((r2 - l).abs() < EPS, "r2 should equal l for grey v={v}");
            assert!((g2 - l).abs() < EPS, "g2 should equal l for grey v={v}");
            assert!((b2 - l).abs() < EPS, "b2 should equal l for grey v={v}");
        }
    }

    #[test]
    fn hue_to_rgb_boundary_values() {
        let p = 0.2f32;
        let q = 0.8f32;
        // t slightly below 0 should wrap to near 1.0 — same result as t+1
        let below = hue_to_rgb(p, q, -0.1);
        let wrapped = hue_to_rgb(p, q, 0.9);
        assert!(
            (below - wrapped).abs() < EPS,
            "below={below} wrapped={wrapped}"
        );
        // t slightly above 1 should wrap to near 0 — same result as t-1
        let above = hue_to_rgb(p, q, 1.1);
        let wrapped2 = hue_to_rgb(p, q, 0.1);
        assert!(
            (above - wrapped2).abs() < EPS,
            "above={above} wrapped2={wrapped2}"
        );
    }
}
