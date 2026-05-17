//! EXIF extraction and metadata-preserving export helpers.

use std::path::Path;

use crate::image::ImageMetadata;

// ---------------------------------------------------------------------------
// Public: read EXIF from JPEG bytes
// ---------------------------------------------------------------------------

/// Extract EXIF from a JPEG byte slice, populating an [`ImageMetadata`].
///
/// The raw APP1 bytes are stashed in `raw_exif` so the encoder can
/// re-attach them verbatim during a metadata-preserving export.
pub fn read_exif_from_bytes(data: &[u8]) -> ImageMetadata {
    let mut meta = ImageMetadata::default();

    // ── Capture raw APP1 bytes for re-attachment on export ────────────────
    use img_parts::{ImageEXIF, jpeg::Jpeg};
    if let Ok(jpeg) = Jpeg::from_bytes(data.to_vec().into())
        && let Some(exif_bytes) = jpeg.exif()
    {
        meta.raw_exif = Some(exif_bytes.to_vec());
    }

    // ── Parse EXIF fields ─────────────────────────────────────────────────
    if let Ok(exif) = exif::Reader::new().read_from_container(&mut std::io::Cursor::new(data)) {
        populate_metadata(&mut meta, &exif);
    }

    meta
}

// ---------------------------------------------------------------------------
// Public: read EXIF from TIFF-based RAW file
// ---------------------------------------------------------------------------

/// Extract EXIF from a TIFF-based RAW file (NEF, CR2, ARW, ORF, DNG, …).
pub fn read_exif_from_file(path: &Path) -> ImageMetadata {
    let mut meta = ImageMetadata::default();

    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(_) => return meta,
    };

    if let Ok(exif) = exif::Reader::new().read_raw(data.clone()) {
        populate_metadata(&mut meta, &exif);
        meta.raw_exif = Some(data);
    }

    meta
}

// ---------------------------------------------------------------------------
// Public: attach EXIF to an already-encoded JPEG
// ---------------------------------------------------------------------------

/// Rotate/flip an RGBA8 buffer to upright per the EXIF Orientation value
/// (1–8), returning the new buffer and its dimensions.
///
/// Orientation `1` (or any out-of-range value) is a no-op — the input is
/// returned unchanged.  For 90°/270° rotations the dimensions swap.
///
/// EXIF values follow the TIFF 6.0 spec:
///
/// | val | meaning                          | dims    |
/// |-----|----------------------------------|---------|
/// | 1   | normal                           | (w, h)  |
/// | 2   | mirror horizontal                | (w, h)  |
/// | 3   | rotate 180°                      | (w, h)  |
/// | 4   | mirror vertical                  | (w, h)  |
/// | 5   | transpose (mirror across `\`)    | (h, w)  |
/// | 6   | rotate 90° CW                    | (h, w)  |
/// | 7   | transverse (mirror across `/`)   | (h, w)  |
/// | 8   | rotate 90° CCW                   | (h, w)  |
pub fn apply_orientation(
    data: Vec<u8>,
    width: u32,
    height: u32,
    orientation: u16,
) -> (Vec<u8>, u32, u32) {
    if orientation <= 1 || orientation > 8 {
        return (data, width, height);
    }
    let w = width as usize;
    let h = height as usize;
    let (new_w, new_h) = match orientation {
        5..=8 => (height, width),
        _ => (width, height),
    };
    let nw = new_w as usize;
    let mut out = vec![0u8; data.len()];
    for sy in 0..h {
        let src_row = sy * w * 4;
        for sx in 0..w {
            let (dx, dy) = match orientation {
                2 => (w - 1 - sx, sy),
                3 => (w - 1 - sx, h - 1 - sy),
                4 => (sx, h - 1 - sy),
                5 => (sy, sx),
                6 => (h - 1 - sy, sx),
                7 => (h - 1 - sy, w - 1 - sx),
                8 => (sy, w - 1 - sx),
                _ => (sx, sy),
            };
            let so = src_row + sx * 4;
            let dop = (dy * nw + dx) * 4;
            out[dop..dop + 4].copy_from_slice(&data[so..so + 4]);
        }
    }
    (out, new_w, new_h)
}

/// Patch the EXIF Orientation tag (0x0112) to `1` in a TIFF byte blob.
///
/// `bytes` may be the raw TIFF data inside a JPEG APP1 segment (the
/// `Exif\0\0` prefix already stripped) or a TIFF-based RAW file.  Walks
/// IFD0 and any chained IFDs (including the IFD1 thumbnail) and rewrites
/// any Orientation entry it finds in place.  Returns silently on
/// malformed input — orientation normalisation is best-effort.
///
/// Called after [`apply_orientation`] has rotated the pixel buffer so
/// that a metadata-preserving export (which re-attaches these bytes
/// verbatim) does not double-rotate the image in downstream viewers.
pub fn normalize_tiff_orientation(bytes: &mut [u8]) {
    if bytes.len() < 8 {
        return;
    }
    let little_endian = match &bytes[..2] {
        b"II" => true,
        b"MM" => false,
        _ => return,
    };
    let read_u16 = |b: &[u8], off: usize| -> u16 {
        let bs = [b[off], b[off + 1]];
        if little_endian {
            u16::from_le_bytes(bs)
        } else {
            u16::from_be_bytes(bs)
        }
    };
    let read_u32 = |b: &[u8], off: usize| -> u32 {
        let bs = [b[off], b[off + 1], b[off + 2], b[off + 3]];
        if little_endian {
            u32::from_le_bytes(bs)
        } else {
            u32::from_be_bytes(bs)
        }
    };
    let write_u16 = |b: &mut [u8], off: usize, val: u16| {
        let bs = if little_endian {
            val.to_le_bytes()
        } else {
            val.to_be_bytes()
        };
        b[off] = bs[0];
        b[off + 1] = bs[1];
    };

    if read_u16(bytes, 2) != 0x002A {
        return;
    }
    let mut ifd_off = read_u32(bytes, 4) as usize;
    // Bound the IFD chain in case of a corrupt loop.
    for _ in 0..8 {
        if ifd_off == 0 || ifd_off + 2 > bytes.len() {
            return;
        }
        let count = read_u16(bytes, ifd_off) as usize;
        let entries_start = ifd_off + 2;
        let entries_end = entries_start + count * 12;
        if entries_end + 4 > bytes.len() {
            return;
        }
        for i in 0..count {
            let e = entries_start + i * 12;
            if read_u16(bytes, e) == 0x0112 {
                // SHORT (type=3) count=1 → value occupies the first 2 bytes
                // of the 4-byte value/offset field.  Upper 2 bytes are
                // padding; leave them alone to minimise the chance of
                // touching anything we don't fully understand.
                write_u16(bytes, e + 8, 1);
            }
        }
        ifd_off = read_u32(bytes, entries_end) as usize;
    }
}

/// Re-attach original EXIF bytes to a freshly encoded JPEG.
///
/// The `image` crate strips all metadata on encode; this function inserts
/// the original APP1 segment back so EXIF/IPTC/XMP is preserved.
/// Returns the modified JPEG bytes, or the original buffer on failure.
pub fn attach_exif_to_jpeg(encoded: Vec<u8>, exif_bytes: &[u8]) -> Vec<u8> {
    use img_parts::{ImageEXIF, jpeg::Jpeg};
    let Ok(mut jpeg) = Jpeg::from_bytes(encoded.clone().into()) else {
        return encoded;
    };
    jpeg.set_exif(Some(exif_bytes.to_vec().into()));
    jpeg.encoder().bytes().to_vec()
}

// ---------------------------------------------------------------------------
// Private: populate ImageMetadata from a parsed exif::Exif
// ---------------------------------------------------------------------------

fn populate_metadata(meta: &mut ImageMetadata, exif: &exif::Exif) {
    use exif::{Tag, Value};

    for field in exif.fields() {
        match field.tag {
            Tag::Make => {
                meta.camera_make = ascii_string(&field.value);
            }
            Tag::Model => {
                meta.camera_model = ascii_string(&field.value);
            }
            Tag::LensModel => {
                meta.lens_model = ascii_string(&field.value);
            }
            Tag::Software => {
                meta.software = ascii_string(&field.value);
            }
            Tag::Orientation => {
                if let Value::Short(v) = &field.value
                    && let Some(&o) = v.first()
                    && (1..=8).contains(&o)
                {
                    meta.orientation = o;
                }
            }
            Tag::DateTimeOriginal | Tag::DateTime if meta.date_time.is_none() => {
                meta.date_time = ascii_string(&field.value);
            }
            Tag::PhotographicSensitivity => {
                if let Value::Short(v) = &field.value
                    && let Some(&iso) = v.first()
                {
                    meta.iso = Some(iso as u32);
                }
            }
            Tag::ExposureTime => {
                if let Value::Rational(v) = &field.value
                    && let Some(r) = v.first()
                {
                    meta.shutter_speed = Some(format_rational(r.num, r.denom));
                }
            }
            Tag::FNumber => {
                if let Value::Rational(v) = &field.value
                    && let Some(r) = v.first()
                {
                    meta.aperture = Some(r.num as f32 / r.denom.max(1) as f32);
                }
            }
            Tag::FocalLength => {
                if let Value::Rational(v) = &field.value
                    && let Some(r) = v.first()
                {
                    meta.focal_length = Some(r.num as f32 / r.denom.max(1) as f32);
                }
            }
            Tag::FocalLengthIn35mmFilm => {
                if let Value::Short(v) = &field.value
                    && let Some(&fl) = v.first()
                {
                    meta.focal_length_35mm = Some(fl as u32);
                }
            }
            Tag::ExposureBiasValue => {
                if let Value::SRational(v) = &field.value
                    && let Some(r) = v.first()
                {
                    let denom = if r.denom == 0 { 1 } else { r.denom };
                    meta.exposure_bias = Some(r.num as f32 / denom as f32);
                }
            }
            Tag::ExposureProgram => {
                meta.exposure_program = Some(field.display_value().to_string());
            }
            Tag::MeteringMode => {
                meta.metering_mode = Some(field.display_value().to_string());
            }
            Tag::Flash => {
                meta.flash = Some(field.display_value().to_string());
            }
            Tag::GPSLatitude => {
                meta.gps_lat = gps_dms_to_decimal(&field.value);
            }
            Tag::GPSLongitude => {
                meta.gps_lon = gps_dms_to_decimal(&field.value);
            }
            Tag::GPSLatitudeRef => {
                if let Some(s) = ascii_string(&field.value)
                    && (s.starts_with('S') || s.starts_with('s'))
                    && let Some(lat) = meta.gps_lat
                {
                    meta.gps_lat = Some(-lat.abs());
                }
            }
            Tag::GPSLongitudeRef => {
                if let Some(s) = ascii_string(&field.value)
                    && (s.starts_with('W') || s.starts_with('w'))
                    && let Some(lon) = meta.gps_lon
                {
                    meta.gps_lon = Some(-lon.abs());
                }
            }
            Tag::GPSAltitude => {
                if let Value::Rational(v) = &field.value
                    && let Some(r) = v.first()
                {
                    meta.gps_alt = Some(r.num as f32 / r.denom.max(1) as f32);
                }
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn ascii_string(val: &exif::Value) -> Option<String> {
    if let exif::Value::Ascii(v) = val {
        let s: String = v
            .iter()
            .flat_map(|bytes| bytes.iter().copied().map(char::from))
            .collect::<String>()
            .trim()
            .to_owned();
        if s.is_empty() { None } else { Some(s) }
    } else {
        None
    }
}

/// Format a rational exposure time (e.g. 1/1000 → "1/1000 s", 2/1 → "2 s").
fn format_rational(num: u32, denom: u32) -> String {
    if denom == 0 || num == 0 {
        return "0 s".into();
    }
    if num >= denom {
        let secs = num as f32 / denom as f32;
        if (secs - secs.round()).abs() < 0.001 {
            format!("{} s", secs.round() as u32)
        } else {
            format!("{:.1} s", secs)
        }
    } else {
        let g = gcd(num, denom);
        format!("{}/{} s", num / g, denom / g)
    }
}

fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        let t = b;
        b = a % b;
        a = t;
    }
    a
}

/// Convert GPS DMS rational triplet to decimal degrees.
fn gps_dms_to_decimal(val: &exif::Value) -> Option<f64> {
    if let exif::Value::Rational(v) = val
        && v.len() >= 3
    {
        let deg = v[0].num as f64 / v[0].denom.max(1) as f64;
        let min = v[1].num as f64 / v[1].denom.max(1) as f64;
        let sec = v[2].num as f64 / v[2].denom.max(1) as f64;
        return Some(deg + min / 60.0 + sec / 3600.0);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a 3 (wide) × 2 (tall) RGBA8 image with unique per-pixel R values
    /// so we can assert exactly where each source pixel ends up after a
    /// transform.  Layout (R channel only, G=B=0, A=255):
    ///
    /// ```text
    ///   x=0  x=1  x=2
    /// y=0  1    2    3
    /// y=1  4    5    6
    /// ```
    fn sample_3x2() -> (Vec<u8>, u32, u32) {
        let mut data = Vec::with_capacity(3 * 2 * 4);
        for r in 1u8..=6 {
            data.extend_from_slice(&[r, 0, 0, 255]);
        }
        (data, 3, 2)
    }

    fn r_channels(data: &[u8]) -> Vec<u8> {
        data.chunks_exact(4).map(|p| p[0]).collect()
    }

    #[test]
    fn orientation_1_is_identity() {
        let (data, w, h) = sample_3x2();
        let (out, ow, oh) = apply_orientation(data.clone(), w, h, 1);
        assert_eq!((ow, oh), (3, 2));
        assert_eq!(out, data);
    }

    #[test]
    fn orientation_2_mirrors_horizontally() {
        let (data, w, h) = sample_3x2();
        let (out, ow, oh) = apply_orientation(data, w, h, 2);
        assert_eq!((ow, oh), (3, 2));
        assert_eq!(r_channels(&out), vec![3, 2, 1, 6, 5, 4]);
    }

    #[test]
    fn orientation_3_rotates_180() {
        let (data, w, h) = sample_3x2();
        let (out, ow, oh) = apply_orientation(data, w, h, 3);
        assert_eq!((ow, oh), (3, 2));
        assert_eq!(r_channels(&out), vec![6, 5, 4, 3, 2, 1]);
    }

    #[test]
    fn orientation_4_mirrors_vertically() {
        let (data, w, h) = sample_3x2();
        let (out, ow, oh) = apply_orientation(data, w, h, 4);
        assert_eq!((ow, oh), (3, 2));
        assert_eq!(r_channels(&out), vec![4, 5, 6, 1, 2, 3]);
    }

    #[test]
    fn orientation_5_transposes() {
        let (data, w, h) = sample_3x2();
        let (out, ow, oh) = apply_orientation(data, w, h, 5);
        assert_eq!((ow, oh), (2, 3));
        assert_eq!(r_channels(&out), vec![1, 4, 2, 5, 3, 6]);
    }

    #[test]
    fn orientation_6_rotates_90_cw() {
        let (data, w, h) = sample_3x2();
        let (out, ow, oh) = apply_orientation(data, w, h, 6);
        assert_eq!((ow, oh), (2, 3));
        // Top-left (R=1) lands at top-right; bottom-left (R=4) lands at top-left.
        assert_eq!(r_channels(&out), vec![4, 1, 5, 2, 6, 3]);
    }

    #[test]
    fn orientation_7_transverses() {
        let (data, w, h) = sample_3x2();
        let (out, ow, oh) = apply_orientation(data, w, h, 7);
        assert_eq!((ow, oh), (2, 3));
        assert_eq!(r_channels(&out), vec![6, 3, 5, 2, 4, 1]);
    }

    #[test]
    fn orientation_8_rotates_90_ccw() {
        let (data, w, h) = sample_3x2();
        let (out, ow, oh) = apply_orientation(data, w, h, 8);
        assert_eq!((ow, oh), (2, 3));
        // Top-right (R=3) lands at top-left; bottom-left (R=4) lands at bottom-right.
        assert_eq!(r_channels(&out), vec![3, 6, 2, 5, 1, 4]);
    }

    #[test]
    fn orientation_invalid_is_noop() {
        let (data, w, h) = sample_3x2();
        let (out, ow, oh) = apply_orientation(data.clone(), w, h, 0);
        assert_eq!((ow, oh), (3, 2));
        assert_eq!(out, data);

        let (data, w, h) = sample_3x2();
        let (out, ow, oh) = apply_orientation(data.clone(), w, h, 99);
        assert_eq!((ow, oh), (3, 2));
        assert_eq!(out, data);
    }

    /// Build a minimal little-endian TIFF with a single IFD containing one
    /// SHORT entry: Orientation (0x0112) = `value`.  No image data, just
    /// enough to exercise the patcher.
    fn minimal_tiff_le(value: u16) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(b"II"); // little-endian
        b.extend_from_slice(&0x002Au16.to_le_bytes());
        b.extend_from_slice(&8u32.to_le_bytes()); // IFD0 at offset 8
        b.extend_from_slice(&1u16.to_le_bytes()); // 1 entry
        // Entry: tag=0x0112, type=3 (SHORT), count=1, value=value (2 bytes + 2 padding)
        b.extend_from_slice(&0x0112u16.to_le_bytes());
        b.extend_from_slice(&3u16.to_le_bytes());
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&value.to_le_bytes());
        b.extend_from_slice(&0u16.to_le_bytes()); // padding
        b.extend_from_slice(&0u32.to_le_bytes()); // next IFD = 0
        b
    }

    #[test]
    fn normalize_orientation_patches_le_tiff() {
        let mut bytes = minimal_tiff_le(8);
        normalize_tiff_orientation(&mut bytes);
        // The value sits at IFD0(8) + count(2) + tag(2) + type(2) + count(4) = offset 18.
        assert_eq!(u16::from_le_bytes([bytes[18], bytes[19]]), 1);
    }

    #[test]
    fn normalize_orientation_handles_big_endian() {
        let mut b = Vec::new();
        b.extend_from_slice(b"MM");
        b.extend_from_slice(&0x002Au16.to_be_bytes());
        b.extend_from_slice(&8u32.to_be_bytes());
        b.extend_from_slice(&1u16.to_be_bytes());
        b.extend_from_slice(&0x0112u16.to_be_bytes());
        b.extend_from_slice(&3u16.to_be_bytes());
        b.extend_from_slice(&1u32.to_be_bytes());
        b.extend_from_slice(&6u16.to_be_bytes());
        b.extend_from_slice(&0u16.to_be_bytes());
        b.extend_from_slice(&0u32.to_be_bytes());

        normalize_tiff_orientation(&mut b);
        assert_eq!(u16::from_be_bytes([b[18], b[19]]), 1);
    }

    #[test]
    fn normalize_orientation_ignores_garbage() {
        let mut bytes = vec![0u8; 32];
        // No "II"/"MM" header — should return cleanly without panicking.
        normalize_tiff_orientation(&mut bytes);
        // Empty input
        let mut empty: Vec<u8> = Vec::new();
        normalize_tiff_orientation(&mut empty);
    }
}
