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
        && let Some(exif_bytes) = jpeg.exif() {
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
            Tag::DateTimeOriginal | Tag::DateTime => {
                if meta.date_time.is_none() {
                    meta.date_time = ascii_string(&field.value);
                }
            }
            Tag::PhotographicSensitivity => {
                if let Value::Short(v) = &field.value
                    && let Some(&iso) = v.first() {
                        meta.iso = Some(iso as u32);
                    }
            }
            Tag::ExposureTime => {
                if let Value::Rational(v) = &field.value
                    && let Some(r) = v.first() {
                        meta.shutter_speed = Some(format_rational(r.num, r.denom));
                    }
            }
            Tag::FNumber => {
                if let Value::Rational(v) = &field.value
                    && let Some(r) = v.first() {
                        meta.aperture = Some(r.num as f32 / r.denom.max(1) as f32);
                    }
            }
            Tag::FocalLength => {
                if let Value::Rational(v) = &field.value
                    && let Some(r) = v.first() {
                        meta.focal_length = Some(r.num as f32 / r.denom.max(1) as f32);
                    }
            }
            Tag::FocalLengthIn35mmFilm => {
                if let Value::Short(v) = &field.value
                    && let Some(&fl) = v.first() {
                        meta.focal_length_35mm = Some(fl as u32);
                    }
            }
            Tag::ExposureBiasValue => {
                if let Value::SRational(v) = &field.value
                    && let Some(r) = v.first() {
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
                        && let Some(lat) = meta.gps_lat {
                            meta.gps_lat = Some(-lat.abs());
                        }
            }
            Tag::GPSLongitudeRef => {
                if let Some(s) = ascii_string(&field.value)
                    && (s.starts_with('W') || s.starts_with('w'))
                        && let Some(lon) = meta.gps_lon {
                            meta.gps_lon = Some(-lon.abs());
                        }
            }
            Tag::GPSAltitude => {
                if let Value::Rational(v) = &field.value
                    && let Some(r) = v.first() {
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
        && v.len() >= 3 {
            let deg = v[0].num as f64 / v[0].denom.max(1) as f64;
            let min = v[1].num as f64 / v[1].denom.max(1) as f64;
            let sec = v[2].num as f64 / v[2].denom.max(1) as f64;
            return Some(deg + min / 60.0 + sec / 3600.0);
        }
    None
}
