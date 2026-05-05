pub(crate) const BRIGHTNESS_CONTRAST_WGSL: &str = r#"
struct Params {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
};

@group(0) @binding(0) var<storage, read> input_pixels: array<u32>;
@group(0) @binding(1) var<storage, read_write> output_pixels: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> lut: array<u32>;

fn channel(byte: u32) -> u32 {
    return lut[byte] & 0xffu;
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) {
        return;
    }

    let i = gid.y * params.width + gid.x;
    if (i >= params.pixel_count) {
        return;
    }

    let px = input_pixels[i];
    let r = channel(px & 0xffu);
    let g = channel((px >> 8u) & 0xffu);
    let b = channel((px >> 16u) & 0xffu);
    let a = px & 0xff000000u;
    output_pixels[i] = r | (g << 8u) | (b << 16u) | a;
}
"#;

pub(crate) const CURVES_WGSL: &str = r#"
struct Params {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
};

@group(0) @binding(0) var<storage, read> input_pixels: array<u32>;
@group(0) @binding(1) var<storage, read_write> output_pixels: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> lut: array<u32>;

fn channel(byte: u32) -> u32 {
    return lut[byte] & 0xffu;
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) {
        return;
    }

    let i = gid.y * params.width + gid.x;
    if (i >= params.pixel_count) {
        return;
    }

    let px = input_pixels[i];
    let r = channel(px & 0xffu);
    let g = channel((px >> 8u) & 0xffu);
    let b = channel((px >> 16u) & 0xffu);
    let a = px & 0xff000000u;
    output_pixels[i] = r | (g << 8u) | (b << 16u) | a;
}
"#;

pub(crate) const HUE_SHIFT_WGSL: &str = r#"
struct Params {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
    shift: f32,
    _pad2: f32,
    _pad3: f32,
    _pad4: f32,
};

@group(0) @binding(0) var<storage, read> input_pixels: array<u32>;
@group(0) @binding(1) var<storage, read_write> output_pixels: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

fn hue_to_rgb(p: f32, q: f32, t_in: f32) -> f32 {
    var t = t_in;
    if (t < 0.0) {
        t = t + 1.0;
    }
    if (t > 1.0) {
        t = t - 1.0;
    }
    if (t < 1.0 / 6.0) {
        return p + (q - p) * 6.0 * t;
    }
    if (t < 0.5) {
        return q;
    }
    if (t < 2.0 / 3.0) {
        return p + (q - p) * (2.0 / 3.0 - t) * 6.0;
    }
    return p;
}

fn rgb_to_hsl(rgb: vec3<f32>) -> vec3<f32> {
    let max_c = max(max(rgb.r, rgb.g), rgb.b);
    let min_c = min(min(rgb.r, rgb.g), rgb.b);
    let l = (max_c + min_c) * 0.5;

    if (abs(max_c - min_c) < 1e-9) {
        return vec3<f32>(0.0, 0.0, l);
    }

    let d = max_c - min_c;
    let s = select(d / (max_c + min_c), d / (2.0 - max_c - min_c), l > 0.5);

    var h: f32;
    if (abs(max_c - rgb.r) < 1e-9) {
        h = (rgb.g - rgb.b) / d;
        if (rgb.g < rgb.b) {
            h = h + 6.0;
        }
    } else if (abs(max_c - rgb.g) < 1e-9) {
        h = (rgb.b - rgb.r) / d + 2.0;
    } else {
        h = (rgb.r - rgb.g) / d + 4.0;
    }

    return vec3<f32>(h / 6.0, s, l);
}

fn hsl_to_rgb(hsl: vec3<f32>) -> vec3<f32> {
    let h = hsl.x;
    let s = hsl.y;
    let l = hsl.z;
    if (s < 1e-9) {
        return vec3<f32>(l, l, l);
    }
    let q = select(l + s - l * s, l * (1.0 + s), l < 0.5);
    let p = 2.0 * l - q;
    return vec3<f32>(
        hue_to_rgb(p, q, h + 1.0 / 3.0),
        hue_to_rgb(p, q, h),
        hue_to_rgb(p, q, h - 1.0 / 3.0)
    );
}

fn unpack_rgb(px: u32) -> vec3<f32> {
    return vec3<f32>(
        f32(px & 0xffu) / 255.0,
        f32((px >> 8u) & 0xffu) / 255.0,
        f32((px >> 16u) & 0xffu) / 255.0
    );
}

fn pack_rgba(rgb: vec3<f32>, alpha: u32) -> u32 {
    let scaled = clamp(rgb * 255.0, vec3<f32>(0.0), vec3<f32>(255.0));
    let r = u32(scaled.r);
    let g = u32(scaled.g);
    let b = u32(scaled.b);
    return r | (g << 8u) | (b << 16u) | alpha;
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) {
        return;
    }

    let i = gid.y * params.width + gid.x;
    if (i >= params.pixel_count) {
        return;
    }

    let px = input_pixels[i];
    let hsl = rgb_to_hsl(unpack_rgb(px));
    let hue = hsl.x + params.shift;
    let wrapped_hue = hue - floor(hue);
    let rgb = hsl_to_rgb(vec3<f32>(wrapped_hue, hsl.y, hsl.z));
    output_pixels[i] = pack_rgba(rgb, px & 0xff000000u);
}
"#;

pub(crate) const SATURATION_WGSL: &str = r#"
struct Params {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
    saturation: f32,
    _pad2: f32,
    _pad3: f32,
    _pad4: f32,
};

@group(0) @binding(0) var<storage, read> input_pixels: array<u32>;
@group(0) @binding(1) var<storage, read_write> output_pixels: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

fn hue_to_rgb(p: f32, q: f32, t_in: f32) -> f32 {
    var t = t_in;
    if (t < 0.0) {
        t = t + 1.0;
    }
    if (t > 1.0) {
        t = t - 1.0;
    }
    if (t < 1.0 / 6.0) {
        return p + (q - p) * 6.0 * t;
    }
    if (t < 0.5) {
        return q;
    }
    if (t < 2.0 / 3.0) {
        return p + (q - p) * (2.0 / 3.0 - t) * 6.0;
    }
    return p;
}

fn rgb_to_hsl(rgb: vec3<f32>) -> vec3<f32> {
    let max_c = max(max(rgb.r, rgb.g), rgb.b);
    let min_c = min(min(rgb.r, rgb.g), rgb.b);
    let l = (max_c + min_c) * 0.5;

    if (abs(max_c - min_c) < 1e-9) {
        return vec3<f32>(0.0, 0.0, l);
    }

    let d = max_c - min_c;
    let s = select(d / (max_c + min_c), d / (2.0 - max_c - min_c), l > 0.5);

    var h: f32;
    if (abs(max_c - rgb.r) < 1e-9) {
        h = (rgb.g - rgb.b) / d;
        if (rgb.g < rgb.b) {
            h = h + 6.0;
        }
    } else if (abs(max_c - rgb.g) < 1e-9) {
        h = (rgb.b - rgb.r) / d + 2.0;
    } else {
        h = (rgb.r - rgb.g) / d + 4.0;
    }

    return vec3<f32>(h / 6.0, s, l);
}

fn hsl_to_rgb(hsl: vec3<f32>) -> vec3<f32> {
    let h = hsl.x;
    let s = hsl.y;
    let l = hsl.z;
    if (s < 1e-9) {
        return vec3<f32>(l, l, l);
    }
    let q = select(l + s - l * s, l * (1.0 + s), l < 0.5);
    let p = 2.0 * l - q;
    return vec3<f32>(
        hue_to_rgb(p, q, h + 1.0 / 3.0),
        hue_to_rgb(p, q, h),
        hue_to_rgb(p, q, h - 1.0 / 3.0)
    );
}

fn unpack_rgb(px: u32) -> vec3<f32> {
    return vec3<f32>(
        f32(px & 0xffu) / 255.0,
        f32((px >> 8u) & 0xffu) / 255.0,
        f32((px >> 16u) & 0xffu) / 255.0
    );
}

fn pack_rgba(rgb: vec3<f32>, alpha: u32) -> u32 {
    let scaled = clamp(rgb * 255.0, vec3<f32>(0.0), vec3<f32>(255.0));
    let r = u32(scaled.r);
    let g = u32(scaled.g);
    let b = u32(scaled.b);
    return r | (g << 8u) | (b << 16u) | alpha;
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) {
        return;
    }

    let i = gid.y * params.width + gid.x;
    if (i >= params.pixel_count) {
        return;
    }

    let px = input_pixels[i];
    let hsl = rgb_to_hsl(unpack_rgb(px));
    let new_s = clamp(hsl.y * params.saturation, 0.0, 1.0);
    let rgb = hsl_to_rgb(vec3<f32>(hsl.x, new_s, hsl.z));
    output_pixels[i] = pack_rgba(rgb, px & 0xff000000u);
}
"#;

pub(crate) const VIBRANCE_WGSL: &str = r#"
struct Params {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
    strength: f32,
    _pad2: f32,
    _pad3: f32,
    _pad4: f32,
};

@group(0) @binding(0) var<storage, read> input_pixels: array<u32>;
@group(0) @binding(1) var<storage, read_write> output_pixels: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

fn hue_to_rgb(p: f32, q: f32, t_in: f32) -> f32 {
    var t = t_in;
    if (t < 0.0) {
        t = t + 1.0;
    }
    if (t > 1.0) {
        t = t - 1.0;
    }
    if (t < 1.0 / 6.0) {
        return p + (q - p) * 6.0 * t;
    }
    if (t < 0.5) {
        return q;
    }
    if (t < 2.0 / 3.0) {
        return p + (q - p) * (2.0 / 3.0 - t) * 6.0;
    }
    return p;
}

fn rgb_to_hsl(rgb: vec3<f32>) -> vec3<f32> {
    let max_c = max(max(rgb.r, rgb.g), rgb.b);
    let min_c = min(min(rgb.r, rgb.g), rgb.b);
    let l = (max_c + min_c) * 0.5;

    if (abs(max_c - min_c) < 1e-9) {
        return vec3<f32>(0.0, 0.0, l);
    }

    let d = max_c - min_c;
    let s = select(d / (max_c + min_c), d / (2.0 - max_c - min_c), l > 0.5);

    var h: f32;
    if (abs(max_c - rgb.r) < 1e-9) {
        h = (rgb.g - rgb.b) / d;
        if (rgb.g < rgb.b) {
            h = h + 6.0;
        }
    } else if (abs(max_c - rgb.g) < 1e-9) {
        h = (rgb.b - rgb.r) / d + 2.0;
    } else {
        h = (rgb.r - rgb.g) / d + 4.0;
    }

    return vec3<f32>(h / 6.0, s, l);
}

fn hsl_to_rgb(hsl: vec3<f32>) -> vec3<f32> {
    let h = hsl.x;
    let s = hsl.y;
    let l = hsl.z;
    if (s < 1e-9) {
        return vec3<f32>(l, l, l);
    }
    let q = select(l + s - l * s, l * (1.0 + s), l < 0.5);
    let p = 2.0 * l - q;
    return vec3<f32>(
        hue_to_rgb(p, q, h + 1.0 / 3.0),
        hue_to_rgb(p, q, h),
        hue_to_rgb(p, q, h - 1.0 / 3.0)
    );
}

fn unpack_rgb(px: u32) -> vec3<f32> {
    return vec3<f32>(
        f32(px & 0xffu) / 255.0,
        f32((px >> 8u) & 0xffu) / 255.0,
        f32((px >> 16u) & 0xffu) / 255.0
    );
}

fn pack_rgba(rgb: vec3<f32>, alpha: u32) -> u32 {
    let scaled = clamp(rgb * 255.0, vec3<f32>(0.0), vec3<f32>(255.0));
    let r = u32(scaled.r);
    let g = u32(scaled.g);
    let b = u32(scaled.b);
    return r | (g << 8u) | (b << 16u) | alpha;
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) {
        return;
    }

    let i = gid.y * params.width + gid.x;
    if (i >= params.pixel_count) {
        return;
    }

    let px = input_pixels[i];
    let hsl = rgb_to_hsl(unpack_rgb(px));
    if (hsl.y < 1e-6) {
        output_pixels[i] = px;
        return;
    }

    let weight = (1.0 - hsl.y) * (1.0 - hsl.y);
    let new_s = clamp(hsl.y + params.strength * weight, 0.0, 1.0);
    let rgb = hsl_to_rgb(vec3<f32>(hsl.x, new_s, hsl.z));
    output_pixels[i] = pack_rgba(rgb, px & 0xff000000u);
}
"#;

pub(crate) const WHITE_BALANCE_WGSL: &str = r#"
struct Params {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
    r_scale: f32,
    g_scale: f32,
    b_scale: f32,
    _pad2: f32,
};

@group(0) @binding(0) var<storage, read> input_pixels: array<u32>;
@group(0) @binding(1) var<storage, read_write> output_pixels: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

fn scaled_channel(byte: u32, scale: f32) -> u32 {
    return u32(clamp(f32(byte) * scale, 0.0, 255.0));
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) {
        return;
    }

    let i = gid.y * params.width + gid.x;
    if (i >= params.pixel_count) {
        return;
    }

    let px = input_pixels[i];
    let r = scaled_channel(px & 0xffu, params.r_scale);
    let g = scaled_channel((px >> 8u) & 0xffu, params.g_scale);
    let b = scaled_channel((px >> 16u) & 0xffu, params.b_scale);
    let a = px & 0xff000000u;
    output_pixels[i] = r | (g << 8u) | (b << 16u) | a;
}
"#;

pub(crate) const SEPIA_WGSL: &str = r#"
struct Params {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
    strength: f32,
    _pad2: f32,
    _pad3: f32,
    _pad4: f32,
};

@group(0) @binding(0) var<storage, read> input_pixels: array<u32>;
@group(0) @binding(1) var<storage, read_write> output_pixels: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) { return; }
    let i = gid.y * params.width + gid.x;
    if (i >= params.pixel_count) { return; }

    let px = input_pixels[i];
    let r = f32(px & 0xffu);
    let g = f32((px >> 8u) & 0xffu);
    let b = f32((px >> 16u) & 0xffu);
    let a = px & 0xff000000u;

    let sr = min(r * 0.393 + g * 0.769 + b * 0.189, 255.0);
    let sg = min(r * 0.349 + g * 0.686 + b * 0.168, 255.0);
    let sb = min(r * 0.272 + g * 0.534 + b * 0.131, 255.0);

    let s = params.strength;
    let nr = u32(r + (sr - r) * s);
    let ng = u32(g + (sg - g) * s);
    let nb = u32(b + (sb - b) * s);
    output_pixels[i] = nr | (ng << 8u) | (nb << 16u) | a;
}
"#;

pub(crate) const LEVELS_WGSL: &str = r#"
struct Params {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
};

@group(0) @binding(0) var<storage, read> input_pixels: array<u32>;
@group(0) @binding(1) var<storage, read_write> output_pixels: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> lut: array<u32>;

fn channel(byte: u32) -> u32 {
    return lut[byte] & 0xffu;
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) { return; }
    let i = gid.y * params.width + gid.x;
    if (i >= params.pixel_count) { return; }

    let px = input_pixels[i];
    let r = channel(px & 0xffu);
    let g = channel((px >> 8u) & 0xffu);
    let b = channel((px >> 16u) & 0xffu);
    let a = px & 0xff000000u;
    output_pixels[i] = r | (g << 8u) | (b << 16u) | a;
}
"#;

pub(crate) const HIGHLIGHTS_SHADOWS_WGSL: &str = r#"
struct Params {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
    highlights: f32,
    shadows: f32,
    _pad2: f32,
    _pad3: f32,
};

@group(0) @binding(0) var<storage, read> input_pixels: array<u32>;
@group(0) @binding(1) var<storage, read_write> output_pixels: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) { return; }
    let i = gid.y * params.width + gid.x;
    if (i >= params.pixel_count) { return; }

    let px = input_pixels[i];
    let r = f32(px & 0xffu) / 255.0;
    let g = f32((px >> 8u) & 0xffu) / 255.0;
    let b = f32((px >> 16u) & 0xffu) / 255.0;
    let a = px & 0xff000000u;

    let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let hl_weight = pow(max((luma - 0.5) * 2.0, 0.0), 2.0);
    let sh_weight = pow(max((0.5 - luma) * 2.0, 0.0), 2.0);
    let delta = params.highlights * hl_weight * 0.5 + params.shadows * sh_weight * 0.5;

    let nr = u32(clamp((r + delta) * 255.0, 0.0, 255.0));
    let ng = u32(clamp((g + delta) * 255.0, 0.0, 255.0));
    let nb = u32(clamp((b + delta) * 255.0, 0.0, 255.0));
    output_pixels[i] = nr | (ng << 8u) | (nb << 16u) | a;
}
"#;

pub(crate) const VIGNETTE_WGSL: &str = r#"
struct Params {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
    strength: f32,
    inner: f32,
    zone: f32,
    _pad2: f32,
};

@group(0) @binding(0) var<storage, read> input_pixels: array<u32>;
@group(0) @binding(1) var<storage, read_write> output_pixels: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

pub(crate) const INV_SQRT2: f32 = 0.70710678118654752;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) { return; }
    let i = gid.y * params.width + gid.x;
    if (i >= params.pixel_count) { return; }

    let half_w = f32(params.width) * 0.5;
    let half_h = f32(params.height) * 0.5;
    let dx = (f32(gid.x) + 0.5 - half_w) / half_w;
    let dy = (f32(gid.y) + 0.5 - half_h) / half_h;
    let d = sqrt(dx * dx + dy * dy) * INV_SQRT2;

    let t = clamp((d - params.inner) / params.zone, 0.0, 1.0);
    let t_smooth = t * t * (3.0 - 2.0 * t);
    let factor = 1.0 - params.strength * t_smooth;

    let px = input_pixels[i];
    let r = u32(clamp(f32(px & 0xffu) * factor, 0.0, 255.0));
    let g = u32(clamp(f32((px >> 8u) & 0xffu) * factor, 0.0, 255.0));
    let b = u32(clamp(f32((px >> 16u) & 0xffu) * factor, 0.0, 255.0));
    let a = px & 0xff000000u;
    output_pixels[i] = r | (g << 8u) | (b << 16u) | a;
}
"#;

pub(crate) const SHADOW_EXPOSURE_WGSL: &str = r#"
struct Params {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
    ev: f32,
    falloff: f32,
    _pad2: f32,
    _pad3: f32,
};

@group(0) @binding(0) var<storage, read> input_pixels: array<u32>;
@group(0) @binding(1) var<storage, read_write> output_pixels: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

fn srgb_to_linear(c: f32) -> f32 {
    if (c <= 0.04045) {
        return c / 12.92;
    }
    return pow((c + 0.055) / 1.055, 2.4);
}

fn linear_to_srgb(c: f32) -> f32 {
    if (c <= 0.0031308) {
        return 12.92 * c;
    }
    return 1.055 * pow(c, 1.0 / 2.4) - 0.055;
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) { return; }
    let i = gid.y * params.width + gid.x;
    if (i >= params.pixel_count) { return; }

    let px = input_pixels[i];
    let r = f32(px & 0xffu) / 255.0;
    let g = f32((px >> 8u) & 0xffu) / 255.0;
    let b = f32((px >> 16u) & 0xffu) / 255.0;
    let a = px & 0xff000000u;

    let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let weight = pow(clamp(1.0 - luma, 0.0, 1.0), params.falloff);
    let gain = exp2(params.ev * weight);

    let rl = srgb_to_linear(r) * gain;
    let gl = srgb_to_linear(g) * gain;
    let bl = srgb_to_linear(b) * gain;

    let nr = u32(clamp(linear_to_srgb(rl) * 255.0, 0.0, 255.0));
    let ng = u32(clamp(linear_to_srgb(gl) * 255.0, 0.0, 255.0));
    let nb = u32(clamp(linear_to_srgb(bl) * 255.0, 0.0, 255.0));
    output_pixels[i] = nr | (ng << 8u) | (nb << 16u) | a;
}
"#;

pub(crate) const SPLIT_TONE_WGSL: &str = r#"
struct Params {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
    sh_r: f32,
    sh_g: f32,
    sh_b: f32,
    shadow_sat: f32,
    hi_r: f32,
    hi_g: f32,
    hi_b: f32,
    highlight_sat: f32,
    balance: f32,
    _pad2: f32,
    _pad3: f32,
    _pad4: f32,
};

@group(0) @binding(0) var<storage, read> input_pixels: array<u32>;
@group(0) @binding(1) var<storage, read_write> output_pixels: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) { return; }
    let i = gid.y * params.width + gid.x;
    if (i >= params.pixel_count) { return; }

    let px = input_pixels[i];
    let r = f32(px & 0xffu) / 255.0;
    let g = f32((px >> 8u) & 0xffu) / 255.0;
    let b = f32((px >> 16u) & 0xffu) / 255.0;
    let a = px & 0xff000000u;

    let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let luma_b = clamp(luma + params.balance, 0.0, 1.0);

    let shadow_w = (1.0 - luma_b) * (1.0 - luma_b) * params.shadow_sat;
    let highlight_w = luma_b * luma_b * params.highlight_sat;

    let nr = clamp(r + (params.sh_r - r) * shadow_w + (params.hi_r - r) * highlight_w, 0.0, 1.0);
    let ng = clamp(g + (params.sh_g - g) * shadow_w + (params.hi_g - g) * highlight_w, 0.0, 1.0);
    let nb = clamp(b + (params.sh_b - b) * shadow_w + (params.hi_b - b) * highlight_w, 0.0, 1.0);

    output_pixels[i] = u32(nr * 255.0 + 0.5) | (u32(ng * 255.0 + 0.5) << 8u)
        | (u32(nb * 255.0 + 0.5) << 16u) | a;
}
"#;

pub(crate) const BLACK_AND_WHITE_WGSL: &str = r#"
struct Params {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
    mode: u32,
    _pad2: u32,
    _pad3: u32,
    _pad4: u32,
    rw: f32,
    gw: f32,
    bw: f32,
    _pad5: f32,
};

@group(0) @binding(0) var<storage, read> input_pixels: array<u32>;
@group(0) @binding(1) var<storage, read_write> output_pixels: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) { return; }
    let i = gid.y * params.width + gid.x;
    if (i >= params.pixel_count) { return; }

    let px = input_pixels[i];
    let r = f32(px & 0xffu) / 255.0;
    let g = f32((px >> 8u) & 0xffu) / 255.0;
    let b = f32((px >> 16u) & 0xffu) / 255.0;
    let a = px & 0xff000000u;

    var gray: f32;
    if (params.mode == 0u) {
        gray = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    } else if (params.mode == 1u) {
        gray = (r + g + b) / 3.0;
    } else if (params.mode == 2u) {
        gray = 0.299 * r + 0.587 * g + 0.114 * b;
    } else {
        gray = params.rw * r + params.gw * g + params.bw * b;
    }
    gray = clamp(gray, 0.0, 1.0);
    let out = u32(gray * 255.0 + 0.5);
    output_pixels[i] = out | (out << 8u) | (out << 16u) | a;
}
"#;

pub(crate) const BLUR_WGSL: &str = r#"
struct Params {
    width: u32,
    height: u32,
    pixel_count: u32,
    kernel_radius: u32,
    sigma: f32,
    _pad: f32,
    _pad2: f32,
    _pad3: f32,
};

@group(0) @binding(0) var<storage, read> input_pixels: array<u32>;
@group(0) @binding(1) var<storage, read_write> output_pixels: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(16, 16)
fn main_h(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) { return; }
    let i = gid.y * params.width + gid.x;
    if (i >= params.pixel_count) { return; }

    var sum_r = 0.0;
    var sum_g = 0.0;
    var sum_b = 0.0;
    var sum_a = 0.0;
    var weight_sum = 0.0;

    let sigma2 = params.sigma * params.sigma;
    let r = i32(params.kernel_radius);
    for (var ki: i32 = -r; ki <= r; ki = ki + 1) {
        let sx = clamp(i32(gid.x) + ki, 0, i32(params.width) - 1);
        let src_px = input_pixels[gid.y * params.width + u32(sx)];
        let kv = exp(-0.5 * f32(ki * ki) / sigma2);
        sum_r += kv * f32(src_px & 0xffu);
        sum_g += kv * f32((src_px >> 8u) & 0xffu);
        sum_b += kv * f32((src_px >> 16u) & 0xffu);
        sum_a += kv * f32((src_px >> 24u) & 0xffu);
        weight_sum += kv;
    }

    let nr = u32(clamp(sum_r / weight_sum, 0.0, 255.0));
    let ng = u32(clamp(sum_g / weight_sum, 0.0, 255.0));
    let nb = u32(clamp(sum_b / weight_sum, 0.0, 255.0));
    let na = u32(clamp(sum_a / weight_sum, 0.0, 255.0));
    output_pixels[i] = nr | (ng << 8u) | (nb << 16u) | (na << 24u);
}

@compute @workgroup_size(16, 16)
fn main_v(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) { return; }
    let i = gid.y * params.width + gid.x;
    if (i >= params.pixel_count) { return; }

    var sum_r = 0.0;
    var sum_g = 0.0;
    var sum_b = 0.0;
    var sum_a = 0.0;
    var weight_sum = 0.0;

    let sigma2 = params.sigma * params.sigma;
    let r = i32(params.kernel_radius);
    for (var ki: i32 = -r; ki <= r; ki = ki + 1) {
        let sy = clamp(i32(gid.y) + ki, 0, i32(params.height) - 1);
        let src_px = input_pixels[u32(sy) * params.width + gid.x];
        let kv = exp(-0.5 * f32(ki * ki) / sigma2);
        sum_r += kv * f32(src_px & 0xffu);
        sum_g += kv * f32((src_px >> 8u) & 0xffu);
        sum_b += kv * f32((src_px >> 16u) & 0xffu);
        sum_a += kv * f32((src_px >> 24u) & 0xffu);
        weight_sum += kv;
    }

    let nr = u32(clamp(sum_r / weight_sum, 0.0, 255.0));
    let ng = u32(clamp(sum_g / weight_sum, 0.0, 255.0));
    let nb = u32(clamp(sum_b / weight_sum, 0.0, 255.0));
    let na = u32(clamp(sum_a / weight_sum, 0.0, 255.0));
    output_pixels[i] = nr | (ng << 8u) | (nb << 16u) | (na << 24u);
}
"#;

pub(crate) const COLOR_BALANCE_WGSL: &str = r#"
struct Params {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
    cr0: f32,
    cr1: f32,
    cr2: f32,
    _pad2: f32,
    mg0: f32,
    mg1: f32,
    mg2: f32,
    _pad3: f32,
    yb0: f32,
    yb1: f32,
    yb2: f32,
    _pad4: f32,
};

@group(0) @binding(0) var<storage, read> input_pixels: array<u32>;
@group(0) @binding(1) var<storage, read_write> output_pixels: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) { return; }
    let i = gid.y * params.width + gid.x;
    if (i >= params.pixel_count) { return; }

    let px = input_pixels[i];
    let r = f32(px & 0xffu) / 255.0;
    let g = f32((px >> 8u) & 0xffu) / 255.0;
    let b = f32((px >> 16u) & 0xffu) / 255.0;
    let a = px & 0xff000000u;

    let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let sh = (1.0 - luma) * (1.0 - luma);
    let mt = 4.0 * luma * (1.0 - luma);
    let hl = luma * luma;

    let dr = (params.cr0 * sh + params.cr1 * mt + params.cr2 * hl) * 0.4;
    let dg = (params.mg0 * sh + params.mg1 * mt + params.mg2 * hl) * 0.4;
    let db = (params.yb0 * sh + params.yb1 * mt + params.yb2 * hl) * 0.4;

    let nr = u32(clamp((r + dr) * 255.0, 0.0, 255.0));
    let ng = u32(clamp((g + dg) * 255.0, 0.0, 255.0));
    let nb = u32(clamp((b + db) * 255.0, 0.0, 255.0));
    output_pixels[i] = nr | (ng << 8u) | (nb << 16u) | a;
}
"#;

pub(crate) const COLOR_SPACE_WGSL: &str = r#"
struct Params {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
    m0: f32,
    m1: f32,
    m2: f32,
    _pad2: f32,
    m3: f32,
    m4: f32,
    m5: f32,
    _pad3: f32,
    m6: f32,
    m7: f32,
    m8: f32,
    _pad4: f32,
};

@group(0) @binding(0) var<storage, read> input_pixels: array<u32>;
@group(0) @binding(1) var<storage, read_write> output_pixels: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

fn srgb_to_linear(c: f32) -> f32 {
    if (c <= 0.04045) {
        return c / 12.92;
    }
    return pow((c + 0.055) / 1.055, 2.4);
}

fn linear_to_srgb(c: f32) -> f32 {
    if (c <= 0.0031308) {
        return 12.92 * c;
    }
    return 1.055 * pow(c, 1.0 / 2.4) - 0.055;
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) { return; }
    let i = gid.y * params.width + gid.x;
    if (i >= params.pixel_count) { return; }

    let px = input_pixels[i];
    let r = f32(px & 0xffu) / 255.0;
    let g = f32((px >> 8u) & 0xffu) / 255.0;
    let b = f32((px >> 16u) & 0xffu) / 255.0;
    let a = px & 0xff000000u;

    let rl = srgb_to_linear(r);
    let gl = srgb_to_linear(g);
    let bl = srgb_to_linear(b);

    let out_rl = clamp(params.m0 * rl + params.m1 * gl + params.m2 * bl, 0.0, 1.0);
    let out_gl = clamp(params.m3 * rl + params.m4 * gl + params.m5 * bl, 0.0, 1.0);
    let out_bl = clamp(params.m6 * rl + params.m7 * gl + params.m8 * bl, 0.0, 1.0);

    let nr = u32(clamp(linear_to_srgb(out_rl) * 255.0, 0.0, 255.0));
    let ng = u32(clamp(linear_to_srgb(out_gl) * 255.0, 0.0, 255.0));
    let nb = u32(clamp(linear_to_srgb(out_bl) * 255.0, 0.0, 255.0));
    output_pixels[i] = nr | (ng << 8u) | (nb << 16u) | a;
}
"#;

pub(crate) const DENOISE_WGSL: &str = r#"
struct Params {
    width: u32,
    height: u32,
    pixel_count: u32,
    radius: u32,
    sigma_r2: f32,
    sigma_s2: f32,
    _pad: f32,
    _pad2: f32,
};

@group(0) @binding(0) var<storage, read> input_pixels: array<u32>;
@group(0) @binding(1) var<storage, read_write> output_pixels: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) { return; }
    let i = gid.y * params.width + gid.x;
    if (i >= params.pixel_count) { return; }

    let px = input_pixels[i];
    let cr = f32(px & 0xffu);
    let cg = f32((px >> 8u) & 0xffu);
    let cb = f32((px >> 16u) & 0xffu);
    let a = px & 0xff000000u;

    var sum_r = 0.0;
    var sum_g = 0.0;
    var sum_b = 0.0;
    var sum_w = 0.0;

    for (var dy: i32 = -i32(params.radius); dy <= i32(params.radius); dy = dy + 1) {
        for (var dx: i32 = -i32(params.radius); dx <= i32(params.radius); dx = dx + 1) {
            let nx = clamp(i32(gid.x) + dx, 0, i32(params.width) - 1);
            let ny = clamp(i32(gid.y) + dy, 0, i32(params.height) - 1);
            let npx = input_pixels[u32(ny) * params.width + u32(nx)];
            let nr = f32(npx & 0xffu);
            let ng = f32((npx >> 8u) & 0xffu);
            let nb = f32((npx >> 16u) & 0xffu);

            let spatial_d = f32(dx * dx + dy * dy);
            let s_w = exp(-spatial_d / params.sigma_s2);

            let dr = nr - cr;
            let dg = ng - cg;
            let db = nb - cb;
            let color_d = dr * dr + dg * dg + db * db;
            let r_w = exp(-color_d / params.sigma_r2);

            let w = s_w * r_w;
            sum_r += w * nr;
            sum_g += w * ng;
            sum_b += w * nb;
            sum_w += w;
        }
    }

    if (sum_w > 1e-9) {
        let out_r = u32(clamp(sum_r / sum_w, 0.0, 255.0));
        let out_g = u32(clamp(sum_g / sum_w, 0.0, 255.0));
        let out_b = u32(clamp(sum_b / sum_w, 0.0, 255.0));
        output_pixels[i] = out_r | (out_g << 8u) | (out_b << 16u) | a;
    } else {
        output_pixels[i] = px;
    }
}
"#;

pub(crate) const HSL_PANEL_WGSL: &str = r#"
struct Params {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
    hue: array<f32, 8>,
    sat: array<f32, 8>,
    lum: array<f32, 8>,
};

@group(0) @binding(0) var<storage, read> input_pixels: array<u32>;
@group(0) @binding(1) var<storage, read_write> output_pixels: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

fn hue_to_rgb(p: f32, q: f32, t_in: f32) -> f32 {
    var t = t_in;
    if (t < 0.0) { t = t + 1.0; }
    if (t > 1.0) { t = t - 1.0; }
    if (t < 1.0 / 6.0) { return p + (q - p) * 6.0 * t; }
    if (t < 0.5) { return q; }
    if (t < 2.0 / 3.0) { return p + (q - p) * (2.0 / 3.0 - t) * 6.0; }
    return p;
}

fn rgb_to_hsl(rgb: vec3<f32>) -> vec3<f32> {
    let max_c = max(max(rgb.r, rgb.g), rgb.b);
    let min_c = min(min(rgb.r, rgb.g), rgb.b);
    let l = (max_c + min_c) * 0.5;
    if (abs(max_c - min_c) < 1e-9) { return vec3<f32>(0.0, 0.0, l); }
    let d = max_c - min_c;
    let s = select(d / (max_c + min_c), d / (2.0 - max_c - min_c), l > 0.5);
    var h: f32;
    if (abs(max_c - rgb.r) < 1e-9) {
        h = (rgb.g - rgb.b) / d;
        if (rgb.g < rgb.b) { h = h + 6.0; }
    } else if (abs(max_c - rgb.g) < 1e-9) {
        h = (rgb.b - rgb.r) / d + 2.0;
    } else {
        h = (rgb.r - rgb.g) / d + 4.0;
    }
    return vec3<f32>(h / 6.0, s, l);
}

fn hsl_to_rgb(hsl: vec3<f32>) -> vec3<f32> {
    let h = hsl.x;
    let s = hsl.y;
    let l = hsl.z;
    if (s < 1e-9) { return vec3<f32>(l, l, l); }
    let q = select(l + s - l * s, l * (1.0 + s), l < 0.5);
    let p = 2.0 * l - q;
    return vec3<f32>(
        hue_to_rgb(p, q, h + 1.0 / 3.0),
        hue_to_rgb(p, q, h),
        hue_to_rgb(p, q, h - 1.0 / 3.0)
    );
}

fn unpack_rgb(px: u32) -> vec3<f32> {
    return vec3<f32>(
        f32(px & 0xffu) / 255.0,
        f32((px >> 8u) & 0xffu) / 255.0,
        f32((px >> 16u) & 0xffu) / 255.0
    );
}

fn pack_rgba(rgb: vec3<f32>, alpha: u32) -> u32 {
    let scaled = clamp(rgb * 255.0, vec3<f32>(0.0), vec3<f32>(255.0));
    let r = u32(scaled.r);
    let g = u32(scaled.g);
    let b = u32(scaled.b);
    return r | (g << 8u) | (b << 16u) | alpha;
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) { return; }
    let i = gid.y * params.width + gid.x;
    if (i >= params.pixel_count) { return; }

    let px = input_pixels[i];
    let hsl = rgb_to_hsl(unpack_rgb(px));
    let h = hsl.x;
    let s = hsl.y;
    let l = hsl.z;

    let centres = array<f32, 8>(0.0, 0.125, 0.25, 0.375, 0.5, 0.625, 0.75, 0.875);
    let half_width = 0.125;

    var dh = 0.0;
    var ds = 0.0;
    var dl = 0.0;
    var w_sum = 0.0;

    for (var bi: i32 = 0; bi < 8; bi = bi + 1) {
        let centre = centres[bi];
        let raw_d = abs(h - centre);
        let d = select(raw_d, 1.0 - raw_d, raw_d > 0.5);
        let w = max(0.0, 1.0 - d / half_width);
        dh += w * params.hue[bi];
        ds += w * params.sat[bi];
        dl += w * params.lum[bi];
        w_sum += w;
    }

    if (w_sum < 1e-6) {
        output_pixels[i] = px;
        return;
    }

    let new_h = fract(h + dh / (360.0 * w_sum));
    let new_s = clamp(s + ds / w_sum, 0.0, 1.0);
    let new_l = clamp(l + dl / w_sum, 0.0, 1.0);
    let rgb = hsl_to_rgb(vec3<f32>(new_h, new_s, new_l));
    output_pixels[i] = pack_rgba(rgb, px & 0xff000000u);
}
"#;

pub(crate) const SHARPEN_WGSL: &str = r#"
struct Params {
    width: u32,
    height: u32,
    pixel_count: u32,
    luminance_only: u32,
    strength: f32,
    _pad: f32,
    _pad2: f32,
    _pad3: f32,
};

@group(0) @binding(0) var<storage, read> input_pixels: array<u32>;
@group(0) @binding(1) var<storage, read_write> output_pixels: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

fn read_pixel(x: i32, y: i32) -> u32 {
    let cx = u32(clamp(x, 0, i32(params.width) - 1));
    let cy = u32(clamp(y, 0, i32(params.height) - 1));
    return input_pixels[cy * params.width + cx];
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) { return; }
    let i = gid.y * params.width + gid.x;
    if (i >= params.pixel_count) { return; }

    let xi = i32(gid.x);
    let yi = i32(gid.y);

    let c_px = read_pixel(xi, yi);
    let t_px = read_pixel(xi, yi - 1);
    let b_px = read_pixel(xi, yi + 1);
    let l_px = read_pixel(xi - 1, yi);
    let r_px = read_pixel(xi + 1, yi);

    let a = c_px & 0xff000000u;
    let s = params.strength;

    if (params.luminance_only == 0u) {
        let c_r = f32(c_px & 0xffu);
        let c_g = f32((c_px >> 8u) & 0xffu);
        let c_b = f32((c_px >> 16u) & 0xffu);

        let t_r = f32(t_px & 0xffu);
        let t_g = f32((t_px >> 8u) & 0xffu);
        let t_b = f32((t_px >> 16u) & 0xffu);

        let b_r = f32(b_px & 0xffu);
        let b_g = f32((b_px >> 8u) & 0xffu);
        let b_b = f32((b_px >> 16u) & 0xffu);

        let l_r = f32(l_px & 0xffu);
        let l_g = f32((l_px >> 8u) & 0xffu);
        let l_b = f32((l_px >> 16u) & 0xffu);

        let r_r = f32(r_px & 0xffu);
        let r_g = f32((r_px >> 8u) & 0xffu);
        let r_b = f32((r_px >> 16u) & 0xffu);

        let nr = u32(clamp((1.0 + 4.0 * s) * c_r - s * (t_r + b_r + l_r + r_r), 0.0, 255.0));
        let ng = u32(clamp((1.0 + 4.0 * s) * c_g - s * (t_g + b_g + l_g + r_g), 0.0, 255.0));
        let nb = u32(clamp((1.0 + 4.0 * s) * c_b - s * (t_b + b_b + l_b + r_b), 0.0, 255.0));
        output_pixels[i] = nr | (ng << 8u) | (nb << 16u) | a;
    } else {
        let c_r = f32(c_px & 0xffu);
        let c_g = f32((c_px >> 8u) & 0xffu);
        let c_b = f32((c_px >> 16u) & 0xffu);
        let luma_c = 0.2126 * c_r + 0.7152 * c_g + 0.0722 * c_b;

        let t_r = f32(t_px & 0xffu);
        let t_g = f32((t_px >> 8u) & 0xffu);
        let t_b = f32((t_px >> 16u) & 0xffu);
        let luma_t = 0.2126 * t_r + 0.7152 * t_g + 0.0722 * t_b;

        let b_r = f32(b_px & 0xffu);
        let b_g = f32((b_px >> 8u) & 0xffu);
        let b_b = f32((b_px >> 16u) & 0xffu);
        let luma_b = 0.2126 * b_r + 0.7152 * b_g + 0.0722 * b_b;

        let l_r = f32(l_px & 0xffu);
        let l_g = f32((l_px >> 8u) & 0xffu);
        let l_b = f32((l_px >> 16u) & 0xffu);
        let luma_l = 0.2126 * l_r + 0.7152 * l_g + 0.0722 * l_b;

        let r_r = f32(r_px & 0xffu);
        let r_g = f32((r_px >> 8u) & 0xffu);
        let r_b = f32((r_px >> 16u) & 0xffu);
        let luma_r = 0.2126 * r_r + 0.7152 * r_g + 0.0722 * r_b;

        let sharpened_luma = clamp((1.0 + 4.0 * s) * luma_c - s * (luma_t + luma_b + luma_l + luma_r), 0.0, 255.0);
        let delta = sharpened_luma - luma_c;

        let nr = u32(clamp(c_r + delta, 0.0, 255.0));
        let ng = u32(clamp(c_g + delta, 0.0, 255.0));
        let nb = u32(clamp(c_b + delta, 0.0, 255.0));
        output_pixels[i] = nr | (ng << 8u) | (nb << 16u) | a;
    }
}
"#;

pub(crate) const NOISE_REDUCTION_NLM_WGSL: &str = r#"
struct Params {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
    luma_h2: f32,
    color_h2: f32,
    detail: f32,
    _pad2: f32,
};

@group(0) @binding(0) var<storage, read> input_pixels: array<u32>;
@group(0) @binding(1) var<storage, read_write> denoised_ycc: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;

fn clamp_coord(v: i32, hi: u32) -> u32 {
    return u32(clamp(v, 0, i32(hi) - 1));
}

fn pixel_at(x: u32, y: u32) -> u32 {
    return input_pixels[y * params.width + x];
}

fn unpack_rgb(px: u32) -> vec3<f32> {
    return vec3<f32>(
        f32(px & 0xffu),
        f32((px >> 8u) & 0xffu),
        f32((px >> 16u) & 0xffu)
    );
}

fn rgb_to_ycbcr(rgb: vec3<f32>) -> vec3<f32> {
    let y = 0.299 * rgb.r + 0.587 * rgb.g + 0.114 * rgb.b;
    let cb = -0.16874 * rgb.r - 0.33126 * rgb.g + 0.5 * rgb.b + 128.0;
    let cr = 0.5 * rgb.r - 0.41869 * rgb.g - 0.08131 * rgb.b + 128.0;
    return vec3<f32>(y, cb, cr);
}

fn ycbcr_at(x: u32, y: u32) -> vec3<f32> {
    return rgb_to_ycbcr(unpack_rgb(pixel_at(x, y)));
}

fn ycbcr_to_rgb(ycc: vec3<f32>) -> vec3<f32> {
    let y = ycc.x;
    let cb = ycc.y;
    let cr = ycc.z;
    return clamp(vec3<f32>(
        y + 1.402 * (cr - 128.0),
        y - 0.34414 * (cb - 128.0) - 0.71414 * (cr - 128.0),
        y + 1.772 * (cb - 128.0)
    ), vec3<f32>(0.0), vec3<f32>(255.0));
}

fn pack_rgba(rgb: vec3<f32>, alpha: u32) -> u32 {
    let r = u32(clamp(rgb.r, 0.0, 255.0));
    let g = u32(clamp(rgb.g, 0.0, 255.0));
    let b = u32(clamp(rgb.b, 0.0, 255.0));
    return r | (g << 8u) | (b << 16u) | alpha;
}

@compute @workgroup_size(8, 8)
fn nlm_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) {
        return;
    }

    let px = gid.x;
    let py = gid.y;
    let src = pixel_at(px, py);
    let patch_norm = 1.0 / 49.0;

    var sum_wy = 0.0;
    var sum_wc = 0.0;
    var acc_y = 0.0;
    var acc_cb = 0.0;
    var acc_cr = 0.0;

    let qy_lo = max(i32(py) - 7, 0);
    let qy_hi = min(i32(py) + 7, i32(params.height) - 1);
    let qx_lo = max(i32(px) - 7, 0);
    let qx_hi = min(i32(px) + 7, i32(params.width) - 1);

    for (var qyi = qy_lo; qyi <= qy_hi; qyi = qyi + 1) {
        for (var qxi = qx_lo; qxi <= qx_hi; qxi = qxi + 1) {
            let qx = u32(qxi);
            let qy = u32(qyi);
            var dist_y = 0.0;
            var dist_c = 0.0;

            for (var dy = -3; dy <= 3; dy = dy + 1) {
                for (var dx = -3; dx <= 3; dx = dx + 1) {
                    let pr = clamp_coord(i32(py) + dy, params.height);
                    let pc = clamp_coord(i32(px) + dx, params.width);
                    let qr = clamp_coord(i32(qy) + dy, params.height);
                    let qc = clamp_coord(i32(qx) + dx, params.width);

                    let p_ycc = ycbcr_at(pc, pr);
                    let q_ycc = ycbcr_at(qc, qr);
                    let dy_val = p_ycc.x - q_ycc.x;
                    dist_y = dist_y + dy_val * dy_val;

                    let dcb = p_ycc.y - q_ycc.y;
                    let dcr = p_ycc.z - q_ycc.z;
                    dist_c = dist_c + dcb * dcb + dcr * dcr;
                }
            }

            dist_y = dist_y * patch_norm;
            dist_c = dist_c * patch_norm;

            let wy = exp(-dist_y / max(params.luma_h2, 1e-9));
            let wc = exp(-dist_c / max(params.color_h2, 1e-9));
            let q_ycc = ycbcr_at(qx, qy);

            acc_y = acc_y + wy * q_ycc.x;
            sum_wy = sum_wy + wy;

            acc_cb = acc_cb + wc * q_ycc.y;
            acc_cr = acc_cr + wc * q_ycc.z;
            sum_wc = sum_wc + wc;
        }
    }

    let orig_ycc = ycbcr_at(px, py);
    let out_ycc = vec3<f32>(
        select(orig_ycc.x, acc_y / sum_wy, sum_wy > 1e-9),
        select(orig_ycc.y, acc_cb / sum_wc, sum_wc > 1e-9),
        select(orig_ycc.z, acc_cr / sum_wc, sum_wc > 1e-9)
    );

    denoised_ycc[py * params.width + px] = vec4<f32>(out_ycc, 0.0);
}

@group(0) @binding(0) var<storage, read> detail_input_pixels: array<u32>;
@group(0) @binding(1) var<storage, read> detail_denoised_ycc: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> output_pixels: array<u32>;
@group(0) @binding(3) var<uniform> detail_params: Params;

fn detail_clamp_coord(v: i32, hi: u32) -> u32 {
    return u32(clamp(v, 0, i32(hi) - 1));
}

fn detail_pixel_at(x: u32, y: u32) -> u32 {
    return detail_input_pixels[y * detail_params.width + x];
}

fn detail_unpack_rgb(px: u32) -> vec3<f32> {
    return vec3<f32>(
        f32(px & 0xffu),
        f32((px >> 8u) & 0xffu),
        f32((px >> 16u) & 0xffu)
    );
}

fn detail_rgb_to_ycbcr(rgb: vec3<f32>) -> vec3<f32> {
    let y = 0.299 * rgb.r + 0.587 * rgb.g + 0.114 * rgb.b;
    let cb = -0.16874 * rgb.r - 0.33126 * rgb.g + 0.5 * rgb.b + 128.0;
    let cr = 0.5 * rgb.r - 0.41869 * rgb.g - 0.08131 * rgb.b + 128.0;
    return vec3<f32>(y, cb, cr);
}

fn detail_orig_ycc_at(x: u32, y: u32) -> vec3<f32> {
    return detail_rgb_to_ycbcr(detail_unpack_rgb(detail_pixel_at(x, y)));
}

fn denoised_at(x: u32, y: u32) -> vec3<f32> {
    return detail_denoised_ycc[y * detail_params.width + x].xyz;
}

fn denoised_y_at_i(r: i32, c: i32) -> f32 {
    let y = detail_clamp_coord(r, detail_params.height);
    let x = detail_clamp_coord(c, detail_params.width);
    return denoised_at(x, y).x;
}

fn detail_ycbcr_to_rgb(ycc: vec3<f32>) -> vec3<f32> {
    let y = ycc.x;
    let cb = ycc.y;
    let cr = ycc.z;
    return clamp(vec3<f32>(
        y + 1.402 * (cr - 128.0),
        y - 0.34414 * (cb - 128.0) - 0.71414 * (cr - 128.0),
        y + 1.772 * (cb - 128.0)
    ), vec3<f32>(0.0), vec3<f32>(255.0));
}

fn detail_pack_rgba(rgb: vec3<f32>, alpha: u32) -> u32 {
    let r = u32(clamp(rgb.r, 0.0, 255.0));
    let g = u32(clamp(rgb.g, 0.0, 255.0));
    let b = u32(clamp(rgb.b, 0.0, 255.0));
    return r | (g << 8u) | (b << 16u) | alpha;
}

@compute @workgroup_size(8, 8)
fn detail_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= detail_params.width || gid.y >= detail_params.height) {
        return;
    }

    let px = gid.x;
    let py = gid.y;
    let r = i32(py);
    let c = i32(px);
    let gx = -denoised_y_at_i(r - 1, c - 1) + denoised_y_at_i(r - 1, c + 1)
        - 2.0 * denoised_y_at_i(r, c - 1) + 2.0 * denoised_y_at_i(r, c + 1)
        - denoised_y_at_i(r + 1, c - 1) + denoised_y_at_i(r + 1, c + 1);
    let gy = -denoised_y_at_i(r - 1, c - 1) - 2.0 * denoised_y_at_i(r - 1, c)
        - denoised_y_at_i(r - 1, c + 1) + denoised_y_at_i(r + 1, c - 1)
        + 2.0 * denoised_y_at_i(r + 1, c) + denoised_y_at_i(r + 1, c + 1);
    let grad = sqrt(gx * gx + gy * gy);
    let mask = clamp(grad / 128.0, 0.0, 1.0) * clamp(detail_params.detail, 0.0, 1.0);

    let orig_ycc = detail_orig_ycc_at(px, py);
    let out_ycc = denoised_at(px, py);
    let masked_ycc = out_ycc + mask * (orig_ycc - out_ycc);

    let rgb = detail_ycbcr_to_rgb(masked_ycc);
    output_pixels[py * detail_params.width + px] =
        detail_pack_rgba(rgb, detail_pixel_at(px, py) & 0xff000000u);
}
"#;

pub(crate) const FAUX_HDR_WGSL: &str = r#"
struct Params {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
    strength: f32,
    _pad2: f32,
    _pad3: f32,
    _pad4: f32,
};

@group(0) @binding(0) var<storage, read> input_pixels: array<u32>;
@group(0) @binding(1) var<storage, read_write> output_pixels: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) { return; }
    let i = gid.y * params.width + gid.x;
    if (i >= params.pixel_count) { return; }

    let px = input_pixels[i];
    let r = f32(px & 0xffu) / 255.0;
    let g = f32((px >> 8u) & 0xffu) / 255.0;
    let b = f32((px >> 16u) & 0xffu) / 255.0;
    let a = px & 0xff000000u;

    let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let luma_over = min(luma * 2.0, 1.0);
    let luma_under = luma * 0.5;

    // well-exposedness: exp(-0.5 * ((luma - 0.5) / 0.35)^2)
    let inv_sigma2 = 1.0 / (2.0 * 0.35 * 0.35);
    let dv0 = luma_over - 0.5;
    let dv1 = luma - 0.5;
    let dv2 = luma_under - 0.5;
    let w0 = exp(-dv0 * dv0 * inv_sigma2);
    let w1 = exp(-dv1 * dv1 * inv_sigma2);
    let w2 = exp(-dv2 * dv2 * inv_sigma2);
    let wsum = w0 + w1 + w2 + 1e-6;

    let luma_fused = (w0 * luma_over + w1 * luma + w2 * luma_under) / wsum;

    var scale = 1.0;
    if (luma > 1e-6) {
        scale = min(luma_fused / luma, 4.0);
    }

    let s = params.strength;
    let nr = clamp(r + (r * scale - r) * s, 0.0, 1.0);
    let ng = clamp(g + (g * scale - g) * s, 0.0, 1.0);
    let nb = clamp(b + (b * scale - b) * s, 0.0, 1.0);

    output_pixels[i] = u32(nr * 255.0 + 0.5) | (u32(ng * 255.0 + 0.5) << 8u)
        | (u32(nb * 255.0 + 0.5) << 16u) | a;
}
"#;

pub(crate) const CLARITY_EXTRACT_LUMA_WGSL: &str = r#"
struct Params { width: u32, height: u32, pixel_count: u32, _pad: u32 };

@group(0) @binding(0) var<storage, read> input_pixels: array<u32>;
@group(0) @binding(1) var<storage, read_write> luma: array<f32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) { return; }
    let i = gid.y * params.width + gid.x;
    if (i >= params.pixel_count) { return; }
    let px = input_pixels[i];
    let r = f32(px & 0xffu) / 255.0;
    let g = f32((px >> 8u) & 0xffu) / 255.0;
    let b = f32((px >> 16u) & 0xffu) / 255.0;
    luma[i] = 0.2126 * r + 0.7152 * g + 0.0722 * b;
}
"#;

pub(crate) const CLARITY_BOX_BLUR_H_WGSL: &str = r#"
struct Params { width: u32, height: u32, pixel_count: u32, radius: u32 };

@group(0) @binding(0) var<storage, read> input_luma: array<f32>;
@group(0) @binding(1) var<storage, read_write> output_luma: array<f32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) { return; }
    let r = params.radius;
    let x0 = u32(max(i32(gid.x) - i32(r), 0));
    let x1 = min(gid.x + r, params.width - 1u);
    var sum = 0.0;
    for (var x = x0; x <= x1; x += 1u) {
        sum += input_luma[gid.y * params.width + x];
    }
    output_luma[gid.y * params.width + gid.x] = sum / f32(x1 - x0 + 1u);
}
"#;

pub(crate) const CLARITY_BOX_BLUR_V_WGSL: &str = r#"
struct Params { width: u32, height: u32, pixel_count: u32, radius: u32 };

@group(0) @binding(0) var<storage, read> input_luma: array<f32>;
@group(0) @binding(1) var<storage, read_write> output_luma: array<f32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) { return; }
    let r = params.radius;
    let y0 = u32(max(i32(gid.y) - i32(r), 0));
    let y1 = min(gid.y + r, params.height - 1u);
    var sum = 0.0;
    for (var y = y0; y <= y1; y += 1u) {
        sum += input_luma[y * params.width + gid.x];
    }
    output_luma[gid.y * params.width + gid.x] = sum / f32(y1 - y0 + 1u);
}
"#;

pub(crate) const CLARITY_APPLY_DETAIL_WGSL: &str = r#"
struct Params {
    width: u32,
    height: u32,
    pixel_count: u32,
    midtone_weight: u32,
    amount: f32,
    _pad1: f32,
    _pad2: f32,
    _pad3: f32,
};

@group(0) @binding(0) var<storage, read> input_pixels: array<u32>;
@group(0) @binding(1) var<storage, read_write> output_pixels: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> blurred_luma: array<f32>;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) { return; }
    let i = gid.y * params.width + gid.x;
    if (i >= params.pixel_count) { return; }

    let px = input_pixels[i];
    let r = f32(px & 0xffu) / 255.0;
    let g = f32((px >> 8u) & 0xffu) / 255.0;
    let b = f32((px >> 16u) & 0xffu) / 255.0;
    let a = px & 0xff000000u;

    let l = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let detail = l - blurred_luma[i];
    let weight = select(1.0, 4.0 * l * (1.0 - l), params.midtone_weight != 0u);
    let boost = params.amount * detail * weight;

    let nr = clamp(r + boost, 0.0, 1.0);
    let ng = clamp(g + boost, 0.0, 1.0);
    let nb = clamp(b + boost, 0.0, 1.0);

    output_pixels[i] = u32(nr * 255.0 + 0.5) | (u32(ng * 255.0 + 0.5) << 8u)
        | (u32(nb * 255.0 + 0.5) << 16u) | a;
}
"#;
