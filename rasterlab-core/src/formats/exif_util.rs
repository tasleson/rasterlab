//! EXIF extraction and metadata-preserving export helpers.

use std::path::Path;

use rayon::prelude::*;

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
///
/// `raw_exif` is populated with a *minimal* TIFF containing only the
/// metadata IFDs (see [`extract_exif_tiff`]) rather than the whole RAW
/// file, so it fits inside a JPEG APP1 segment for metadata-preserving
/// export.
pub fn read_exif_from_file(path: &Path) -> ImageMetadata {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(_) => return ImageMetadata::default(),
    };

    read_raw_exif_from_bytes(&data)
}

/// Extract EXIF from already-loaded TIFF-based RAW bytes.
///
/// This has the same metadata and export-attachment behavior as
/// [`read_exif_from_file`], without reopening the source file.
pub fn read_raw_exif_from_bytes(data: &[u8]) -> ImageMetadata {
    let mut meta = ImageMetadata::default();

    if let Ok(exif) = exif::Reader::new().read_raw(data.to_vec()) {
        populate_metadata(&mut meta, &exif);
        meta.raw_exif = extract_exif_tiff_from_parsed(data, &exif);
    }

    meta
}

/// Extract metadata from already-loaded TIFF-based RAW bytes without building
/// the compact EXIF attachment used only by metadata-preserving export.
///
/// Library import stores parsed fields in `LMTA`, while retaining the original
/// bytes in `ORIG`; it does not export the just-decoded image. Avoiding that
/// attachment prevents a second TIFF walk and allocation on this path.
pub fn read_raw_metadata_from_bytes(data: &[u8]) -> ImageMetadata {
    let mut meta = ImageMetadata::default();
    if let Ok(exif) = exif::Reader::new().read_raw(data.to_vec()) {
        populate_metadata(&mut meta, &exif);
    }
    meta
}

// ---------------------------------------------------------------------------
// Public: read just the capture date from a file prefix
// ---------------------------------------------------------------------------

/// Read only the EXIF capture date (`DateTimeOriginal`, falling back to
/// `DateTime`) from the leading bytes of a JPEG or TIFF-based RAW file.
///
/// Unlike [`read_exif_from_bytes`]/[`read_exif_from_file`] this builds neither
/// the full [`ImageMetadata`] nor the `raw_exif` re-attachment buffer, and it
/// operates on a truncated head of the file — EXIF lives near the start of both
/// container types.  The library's import capture-date scan uses it so it need
/// not stream entire multi-megabyte originals just to read one timestamp, which
/// is ruinously slow over a network filesystem.  Returns the raw EXIF datetime
/// string (`"YYYY:MM:DD HH:MM:SS"`), or `None` if the prefix carries no date
/// (e.g. it was truncated before the relevant IFD), so callers can fall back to
/// filesystem timestamps.
pub fn read_capture_date_from_prefix(prefix: &[u8], is_jpeg: bool) -> Option<String> {
    let reader = exif::Reader::new();
    let exif = if is_jpeg {
        reader
            .read_from_container(&mut std::io::Cursor::new(prefix))
            .ok()?
    } else {
        reader.read_raw(prefix.to_vec()).ok()?
    };
    let field = exif
        .get_field(exif::Tag::DateTimeOriginal, exif::In::PRIMARY)
        .or_else(|| exif.get_field(exif::Tag::DateTime, exif::In::PRIMARY))?;
    ascii_string(&field.value)
}

// ---------------------------------------------------------------------------
// Public: build a compact EXIF TIFF from a TIFF-based RAW
// ---------------------------------------------------------------------------

/// Maximum byte length of an extracted EXIF TIFF.
///
/// A JPEG APP1 segment's length field is 16-bit, so payload ≤ 65 533 bytes
/// (and EXIF additionally consumes 6 bytes for the `"Exif\0\0"` prefix
/// that [`img_parts`] prepends).  Cap conservatively to leave room.
const MAX_EXIF_TIFF_BYTES: usize = 65_000;

/// Rebuild a compact metadata-only TIFF from a TIFF-based RAW file's
/// bytes.
///
/// Walks the parsed EXIF, keeps only entries belonging to IFD0 (and the
/// ExifIFD / GPSIFD / InteropIFD sub-IFDs reached through it), and
/// re-serialises them with [`exif::experimental::Writer`].  Strip /
/// tile / JPEG-thumbnail offsets are dropped automatically by the
/// writer; the IFD1 thumbnail chain is skipped here.
///
/// If the first pass produces a blob larger than [`MAX_EXIF_TIFF_BYTES`]
/// — typically because of a multi-tens-of-KB MakerNote — the MakerNote
/// is dropped and we retry.  Returns `None` if the input cannot be
/// parsed or the output is still too large.
pub fn extract_exif_tiff(input: &[u8]) -> Option<Vec<u8>> {
    let exif = exif::Reader::new().read_raw(input.to_vec()).ok()?;

    extract_exif_tiff_from_parsed(input, &exif)
}

/// Build an export attachment from an already-parsed TIFF EXIF directory.
/// This keeps the normal RAW editor path to one parser pass.
fn extract_exif_tiff_from_parsed(input: &[u8], exif: &exif::Exif) -> Option<Vec<u8>> {
    let little_endian = matches!(input.first()?, b'I');

    if let Some(buf) = write_minimal_tiff(exif, little_endian, false)
        && buf.len() <= MAX_EXIF_TIFF_BYTES
    {
        return Some(buf);
    }
    let buf = write_minimal_tiff(exif, little_endian, true)?;
    (buf.len() <= MAX_EXIF_TIFF_BYTES).then_some(buf)
}

fn write_minimal_tiff(
    exif: &exif::Exif,
    little_endian: bool,
    drop_makernote: bool,
) -> Option<Vec<u8>> {
    use exif::{In, Tag, Value, experimental::Writer};

    let mut writer = Writer::new();
    for field in exif.fields() {
        // Drop the thumbnail IFD — it owns StripOffsets that point into
        // the source file and would dangle in the extracted blob.
        if field.ifd_num != In::PRIMARY {
            continue;
        }
        // The Writer cannot serialise Value::Unknown.
        if matches!(field.value, Value::Unknown(..)) {
            continue;
        }
        if drop_makernote && field.tag == Tag::MakerNote {
            continue;
        }
        writer.push_field(field);
    }

    let mut buf = std::io::Cursor::new(Vec::new());
    writer.write(&mut buf, little_endian).ok()?;
    Some(buf.into_inner())
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
    if orientation <= 1 || orientation > 8 || width == 0 || height == 0 {
        return (data, width, height);
    }
    rotate_pixels(&data, width, height, orientation, 4)
}

/// Like [`apply_orientation`] but for 3-byte-per-pixel RGB input, expanding to
/// RGBA8 (opaque) in the same pass.
///
/// The decode output of a standard JPEG is RGB8, so this fuses the RGB→RGBA
/// expansion with the orientation transform: one buffer pass instead of two.
/// Orientation `1` (or out-of-range) is expansion only.
pub fn apply_orientation_rgb(
    data: Vec<u8>,
    width: u32,
    height: u32,
    orientation: u16,
) -> (Vec<u8>, u32, u32) {
    if orientation <= 1 || orientation > 8 || width == 0 || height == 0 {
        return (rgb8_to_rgba8(&data), width, height);
    }
    rotate_pixels(&data, width, height, orientation, 3)
}

/// Expand an RGB8 buffer (3 bytes/pixel) to an opaque RGBA8 buffer.
///
/// A standard JPEG decodes to RGB8, so this pass sits directly on the Open
/// path.  It is memory-bandwidth-bound rather than compute-bound, but a
/// single core cannot saturate that bandwidth: splitting it across the rayon
/// pool takes a 24 MP image from ~47 ms to ~13 ms.
///
/// Chunked rather than per-pixel so the inner loop is a straight zip of two
/// contiguous slices, with no bounds check per channel.
pub fn rgb8_to_rgba8(rgb: &[u8]) -> Vec<u8> {
    /// Pixels per rayon task.  Large enough that scheduling overhead vanishes,
    /// small enough to keep every worker fed on a modest image.
    const CHUNK_PX: usize = 4096;

    let n = rgb.len() / 3;
    let mut out = vec![0u8; n * 4];
    out.par_chunks_mut(CHUNK_PX * 4)
        .zip(rgb.par_chunks(CHUNK_PX * 3))
        .for_each(|(dst, src)| {
            let src = src.as_chunks::<3>().0.iter();
            for (d, s) in dst.as_chunks_mut::<4>().0.iter_mut().zip(src) {
                *d = [s[0], s[1], s[2], 255];
            }
        });
    out
}

/// The orientation transform shared by [`apply_orientation`] and
/// [`apply_orientation_rgb`].
///
/// `src_bpp` is 3 or 4; the output is always RGBA8.  For 3-byte input the
/// alpha channel is filled with 255 in the same pass as the transform.
///
/// Output rows are distributed across the rayon pool.  For the mirror/180°
/// orientations each output row reads one contiguous source row; for the 90°
/// family each output row reads one source column, so the source access
/// strides while the output stays a clean stream — either way the whole
/// buffer is touched once, in parallel, instead of pixel-by-pixel.
fn rotate_pixels(
    src: &[u8],
    width: u32,
    height: u32,
    orientation: u16,
    src_bpp: usize,
) -> (Vec<u8>, u32, u32) {
    let w = width as usize;
    let h = height as usize;
    let (new_w, new_h) = match orientation {
        5..=8 => (height, width),
        _ => (width, height),
    };
    let nw = new_w as usize;
    let mut out = vec![0u8; nw * new_h as usize * 4];

    let row_bytes = w * src_bpp;
    let out_row_bytes = nw * 4;

    match orientation {
        2..=4 => {
            // Each output row maps to a contiguous source row.
            out.par_chunks_mut(out_row_bytes)
                .enumerate()
                .for_each(|(dy, drow)| {
                    let sy = if orientation == 2 { dy } else { h - 1 - dy };
                    let srow = &src[sy * row_bytes..(sy + 1) * row_bytes];
                    let rev = orientation != 4;
                    for (dx, d) in drow.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                        let sx = if rev { w - 1 - dx } else { dx };
                        expand_pixel(&srow[sx * src_bpp..(sx + 1) * src_bpp], src_bpp, d);
                    }
                });
        }
        5..=8 => {
            // Each output row maps to one source column; reads stride by the
            // source row pitch, the output row is written as a stream.
            out.par_chunks_mut(out_row_bytes)
                .enumerate()
                .for_each(|(dy, drow)| {
                    let sx = if matches!(orientation, 5 | 6) {
                        dy
                    } else {
                        w - 1 - dy
                    };
                    let rev = matches!(orientation, 6 | 7);
                    for (dx, d) in drow.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                        let sy = if rev { h - 1 - dx } else { dx };
                        let so = (sy * w + sx) * src_bpp;
                        expand_pixel(&src[so..so + src_bpp], src_bpp, d);
                    }
                });
        }
        _ => unreachable!("orientation 2..=8 is handled above"),
    }

    (out, new_w, new_h)
}

#[inline(always)]
fn expand_pixel(src: &[u8], src_bpp: usize, dst: &mut [u8; 4]) {
    match src_bpp {
        3 => {
            dst[0] = src[0];
            dst[1] = src[1];
            dst[2] = src[2];
            dst[3] = 255;
        }
        _ => dst.copy_from_slice(src),
    }
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
///
/// EXIF blobs that would overflow a JPEG APP1 segment
/// (> [`MAX_EXIF_TIFF_BYTES`]) are skipped rather than passed to
/// `img_parts`, which panics on oversized segments.  Callers should
/// already have shrunk RAW metadata via [`extract_exif_tiff`]; this is
/// a final safety net.
pub fn attach_exif_to_jpeg(encoded: Vec<u8>, exif_bytes: &[u8]) -> Vec<u8> {
    use img_parts::{ImageEXIF, jpeg::Jpeg};
    if exif_bytes.len() > MAX_EXIF_TIFF_BYTES {
        return encoded;
    }
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
            Tag::LensMake => {
                meta.lens_make = ascii_string(&field.value);
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
            Tag::SubjectDistance => {
                if let Value::Rational(v) = &field.value
                    && let Some(r) = v.first()
                    && r.denom != 0
                {
                    meta.subject_distance = Some(r.num as f32 / r.denom as f32);
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
        data.as_chunks::<4>().0.iter().map(|p| p[0]).collect()
    }

    fn subject_distance_tiff_le(num: u32, denom: u32) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(b"II");
        b.extend_from_slice(&0x002Au16.to_le_bytes());
        b.extend_from_slice(&8u32.to_le_bytes());

        b.extend_from_slice(&1u16.to_le_bytes());
        // IFD0 ExifIFDPointer -> offset 26.
        b.extend_from_slice(&0x8769u16.to_le_bytes());
        b.extend_from_slice(&4u16.to_le_bytes());
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&26u32.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());

        b.extend_from_slice(&1u16.to_le_bytes());
        // ExifIFD SubjectDistance (0x9206), RATIONAL, one value at offset 44.
        b.extend_from_slice(&0x9206u16.to_le_bytes());
        b.extend_from_slice(&5u16.to_le_bytes());
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&44u32.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());

        b.extend_from_slice(&num.to_le_bytes());
        b.extend_from_slice(&denom.to_le_bytes());
        b
    }

    #[test]
    fn read_exif_populates_subject_distance() {
        let tiff = subject_distance_tiff_le(375, 100);
        let exif = exif::Reader::new()
            .read_raw(tiff)
            .expect("fixture should parse");
        let mut meta = ImageMetadata::default();

        populate_metadata(&mut meta, &exif);

        assert_eq!(meta.subject_distance, Some(3.75));
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

    // -----------------------------------------------------------------
    // extract_exif_tiff
    // -----------------------------------------------------------------

    /// Build a little-endian TIFF that resembles a tiny TIFF-based RAW:
    /// IFD0 carries Make/Model + StripOffsets pointing at a large fake
    /// "image data" blob whose size dominates the file.  Used to verify
    /// the extractor drops the bulk-data tags and shrinks the output.
    fn fake_raw_tiff(image_bytes: usize) -> Vec<u8> {
        // Layout: header(8) | IFD0 | string pool | image data
        let make = b"TestCam\0";
        let model = b"FakeModel\0";

        let ifd_off = 8u32;
        let entry_count = 4u16; // Make, Model, StripOffsets, StripByteCounts
        let ifd_size = 2 + entry_count as usize * 12 + 4;
        let make_off = ifd_off as usize + ifd_size;
        let model_off = make_off + make.len();
        let image_off = model_off + model.len();

        let mut b = Vec::new();
        b.extend_from_slice(b"II");
        b.extend_from_slice(&0x002Au16.to_le_bytes());
        b.extend_from_slice(&ifd_off.to_le_bytes());

        b.extend_from_slice(&entry_count.to_le_bytes());

        // Make: ASCII (type=2), count = len, offset → make_off (indirect, > 4 bytes)
        b.extend_from_slice(&0x010Fu16.to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&(make.len() as u32).to_le_bytes());
        b.extend_from_slice(&(make_off as u32).to_le_bytes());

        // Model: ASCII
        b.extend_from_slice(&0x0110u16.to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&(model.len() as u32).to_le_bytes());
        b.extend_from_slice(&(model_off as u32).to_le_bytes());

        // StripOffsets: LONG (type=4), count=1, value = image_off (inline)
        b.extend_from_slice(&0x0111u16.to_le_bytes());
        b.extend_from_slice(&4u16.to_le_bytes());
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&(image_off as u32).to_le_bytes());

        // StripByteCounts: LONG, count=1, value = image_bytes
        b.extend_from_slice(&0x0117u16.to_le_bytes());
        b.extend_from_slice(&4u16.to_le_bytes());
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&(image_bytes as u32).to_le_bytes());

        // Next IFD = 0 (no IFD1 thumbnail; keeps the fixture simple).
        b.extend_from_slice(&0u32.to_le_bytes());

        b.extend_from_slice(make);
        b.extend_from_slice(model);
        b.resize(image_off + image_bytes, 0xAB); // fake pixels
        b
    }

    #[test]
    fn extract_drops_bulk_data_and_shrinks_output() {
        let raw = fake_raw_tiff(2_000_000);
        let extracted = extract_exif_tiff(&raw).expect("extraction should succeed");
        assert!(
            extracted.len() < 200,
            "extracted blob should be tiny (got {} bytes)",
            extracted.len(),
        );
        // Verify the metadata round-trips: re-parse and check Make/Model survive.
        let exif = exif::Reader::new()
            .read_raw(extracted.clone())
            .expect("extracted blob must parse");
        let make = exif
            .get_field(exif::Tag::Make, exif::In::PRIMARY)
            .expect("Make tag preserved");
        assert!(make.display_value().to_string().contains("TestCam"));
    }

    #[test]
    fn extract_output_fits_in_jpeg_segment() {
        // Even a huge "RAW" must produce output that fits in an APP1 segment.
        let raw = fake_raw_tiff(50_000_000);
        let extracted = extract_exif_tiff(&raw).expect("extraction should succeed");
        assert!(extracted.len() <= MAX_EXIF_TIFF_BYTES);
    }

    #[test]
    fn extract_returns_none_for_garbage() {
        assert!(extract_exif_tiff(b"not a tiff").is_none());
        assert!(extract_exif_tiff(&[]).is_none());
    }

    #[test]
    fn raw_import_metadata_omits_export_attachment() {
        let raw = fake_raw_tiff(2_000_000);

        let metadata = read_raw_metadata_from_bytes(&raw);
        let export_metadata = read_raw_exif_from_bytes(&raw);

        assert_eq!(metadata.camera_make.as_deref(), Some("TestCam"));
        assert_eq!(metadata.camera_model.as_deref(), Some("FakeModel"));
        assert!(metadata.raw_exif.is_none());
        assert_eq!(export_metadata.camera_make, metadata.camera_make);
        assert_eq!(export_metadata.camera_model, metadata.camera_model);
        assert!(export_metadata.raw_exif.is_some());
    }

    #[test]
    fn attach_exif_skips_oversized_blob() {
        // Construct a minimal valid JPEG: SOI + EOI.  attach_exif_to_jpeg
        // should refuse to attach an oversized EXIF rather than panic.
        let jpeg = vec![0xFF, 0xD8, 0xFF, 0xD9];
        let oversized = vec![0u8; MAX_EXIF_TIFF_BYTES + 1];
        let out = attach_exif_to_jpeg(jpeg.clone(), &oversized);
        assert_eq!(out, jpeg, "oversized EXIF should be skipped");
    }

    /// The pre-optimisation orientation transform, kept verbatim as a
    /// correctness oracle for the parallel rewrite.  Operates on `src_bpp`
    /// input and always emits RGBA8, matching [`rotate_pixels`].
    fn serial_reference(src: &[u8], w: usize, h: usize, orientation: u16, bpp: usize) -> Vec<u8> {
        let nw = if (5..=8).contains(&orientation) { h } else { w };
        let mut out = vec![0u8; w * h * 4];
        for sy in 0..h {
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
                let so = (sy * w + sx) * bpp;
                let dop = (dy * nw + dx) * 4;
                out[dop] = src[so];
                out[dop + 1] = src[so + 1];
                out[dop + 2] = src[so + 2];
                out[dop + 3] = if bpp == 4 { src[so + 3] } else { 255 };
            }
        }
        out
    }

    /// Deterministic non-uniform pixels, so a transform that silently does
    /// nothing (or transposes the wrong way) cannot pass.
    fn noise(len: usize) -> Vec<u8> {
        let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
        (0..len)
            .map(|_| {
                seed = seed
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (seed >> 33) as u8
            })
            .collect()
    }

    /// Every orientation, both input pixel formats, against the serial oracle.
    /// Non-square and odd dimensions so a width/height swap cannot go unnoticed.
    #[test]
    fn apply_orientation_matches_the_serial_reference() {
        for (w, h) in [(7usize, 5usize), (1, 6), (6, 1), (2, 2)] {
            for orientation in 1..=8u16 {
                let rgba = noise(w * h * 4);
                let (got, gw, gh) =
                    apply_orientation(rgba.clone(), w as u32, h as u32, orientation);
                let (ew, eh) = if (5..=8).contains(&orientation) {
                    (h as u32, w as u32)
                } else {
                    (w as u32, h as u32)
                };
                assert_eq!(
                    (gw, gh),
                    (ew, eh),
                    "rgba dims, {w}x{h} orient {orientation}"
                );
                let want = if orientation == 1 {
                    rgba.clone()
                } else {
                    serial_reference(&rgba, w, h, orientation, 4)
                };
                assert_eq!(got, want, "rgba pixels, {w}x{h} orient {orientation}");

                let rgb = noise(w * h * 3);
                let (got, gw, gh) =
                    apply_orientation_rgb(rgb.clone(), w as u32, h as u32, orientation);
                assert_eq!((gw, gh), (ew, eh), "rgb dims, {w}x{h} orient {orientation}");
                let want = if orientation == 1 {
                    rgb8_to_rgba8(&rgb)
                } else {
                    serial_reference(&rgb, w, h, orientation, 3)
                };
                assert_eq!(got, want, "rgb pixels, {w}x{h} orient {orientation}");
            }
        }
    }

    /// The RGB path must expand to opaque RGBA even when there is nothing to
    /// rotate, and an out-of-range tag must be treated as "no rotation".
    #[test]
    fn apply_orientation_rgb_expands_without_rotating() {
        let rgb = vec![1, 2, 3, 4, 5, 6];
        for orientation in [0u16, 1, 9, 65535] {
            let (out, w, h) = apply_orientation_rgb(rgb.clone(), 2, 1, orientation);
            assert_eq!((w, h), (2, 1));
            assert_eq!(
                out,
                vec![1, 2, 3, 255, 4, 5, 6, 255],
                "orient {orientation}"
            );
        }
    }

    /// A zero-dimension image must not reach the parallel path, where a
    /// zero-length chunk would panic.
    #[test]
    fn apply_orientation_tolerates_empty_images() {
        assert_eq!(apply_orientation(Vec::new(), 0, 0, 6), (Vec::new(), 0, 0));
        assert_eq!(apply_orientation(Vec::new(), 4, 0, 6), (Vec::new(), 4, 0));
        assert_eq!(
            apply_orientation_rgb(Vec::new(), 0, 3, 8),
            (Vec::new(), 0, 3)
        );
    }

    #[test]
    fn rgb8_to_rgba8_spans_the_chunk_boundary() {
        // Larger than one rayon chunk, and not a multiple of it, so the tail
        // block is exercised too.
        let px = 4096 * 2 + 37;
        let rgb = noise(px * 3);
        let out = rgb8_to_rgba8(&rgb);
        assert_eq!(out.len(), px * 4);
        for i in 0..px {
            assert_eq!(&out[i * 4..i * 4 + 3], &rgb[i * 3..i * 3 + 3], "pixel {i}");
            assert_eq!(out[i * 4 + 3], 255, "alpha {i}");
        }
    }
}
