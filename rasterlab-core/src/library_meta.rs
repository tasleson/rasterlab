use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// One collection a photo belongs to, as recorded in its `.rlab`.
///
/// Membership is keyed on [`id`](Self::id) rather than on the name, so that
/// renaming a collection is a single index update instead of a rewrite of
/// every file in it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CollectionRef {
    /// UUID minted when the collection was created.  Two files are in the same
    /// collection exactly when this matches.
    pub id: String,
    /// The collection's name as of the last time this file was written.
    ///
    /// Not authoritative, and stale in every member file that has not been
    /// touched since a rename — the library index owns the name.  It is here
    /// so that rebuilding an index that has been lost outright restores named
    /// collections rather than anonymous groups of photos.
    #[serde(default)]
    pub name: String,
}

/// User-defined and EXIF metadata embedded in the `LMTA` chunk of a `.rlab`
/// library file. All fields are optional so existing `.rlab` files that were
/// created before the library feature will deserialize cleanly with defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryMeta {
    // ── Identity ──────────────────────────────────────────────────────────────
    /// Original filename before import (e.g. `"DSC_001.NEF"`).
    pub original_filename: Option<String>,
    /// UUID of the import session this file belongs to.
    pub import_session_id: String,
    /// Unix timestamp (seconds) when this file was imported into the library.
    pub import_date: u64,

    // ── Stack (RAW+JPEG pairs) ─────────────────────────────────────────────
    /// Blake3 hex hash of the paired file in a RAW+JPEG stack, if any.
    pub stack_peer_hash: Option<String>,
    /// `true` if this is the primary file in a stack (RAW, or the only file).
    #[serde(default = "default_true")]
    pub stack_is_primary: bool,

    // ── User metadata ─────────────────────────────────────────────────────
    /// User-assigned keywords / tags.
    #[serde(default)]
    pub keywords: Vec<String>,
    /// Star rating 0–5 (0 = unrated).
    #[serde(default)]
    pub rating: u8,
    /// Color label: `"red"`, `"yellow"`, `"green"`, `"blue"`, `"purple"`.
    pub color_label: Option<String>,
    /// Pick/reject flag: `"pick"` or `"reject"`.
    pub flag: Option<String>,
    /// `true` if the user has marked this photo as protected. Protected photos
    /// cannot be deleted from the library and their `.rlab` file is locked on
    /// the filesystem (best-effort, per-OS) so it cannot go missing.
    #[serde(default)]
    pub protected: bool,
    /// Collections this photo belongs to.
    #[serde(default)]
    pub collection_refs: Vec<CollectionRef>,
    /// Collection names as written by versions that identified a collection by
    /// its name alone.
    ///
    /// Read so that a file which has not been touched since keeps its
    /// memberships through a rebuild, and migrated into
    /// [`collection_refs`](Self::collection_refs) the next time the file is
    /// written for any other reason.  Never written back.
    #[serde(default, rename = "collections", skip_serializing_if = "Vec::is_empty")]
    pub legacy_collections: Vec<String>,
    /// User-supplied caption / description.
    pub caption: Option<String>,
    /// Copyright notice (e.g. `"© 2025 Jane Smith"`).
    pub copyright: Option<String>,
    /// Creator / photographer name.
    pub creator: Option<String>,
    /// City where the photo was taken.
    pub location_city: Option<String>,
    /// Country where the photo was taken.
    pub location_country: Option<String>,

    // ── Source file timestamps ────────────────────────────────────────────
    /// Original source-file path at import time.
    #[serde(default)]
    pub source_path: Option<String>,
    /// Size in bytes of the original source file at import time. Stored
    /// alongside `source_path`/`source_mtime` to form a cheap fingerprint that
    /// lets an interrupted import resume without re-reading already-imported
    /// files (see `import::import_one`).
    #[serde(default)]
    pub source_size: Option<u64>,
    /// Modification time of the original source file at import time.
    /// Preserved so that the original bytes can be exported with matching mtime.
    #[serde(default)]
    pub source_mtime: Option<FileTimeStamp>,
    /// Access time of the original source file at import time.
    #[serde(default)]
    pub source_atime: Option<FileTimeStamp>,
    /// Creation/birth time of the original source file at import time (where
    /// the OS reports one). On Linux this is usually unavailable.
    #[serde(default)]
    pub source_ctime: Option<FileTimeStamp>,

    // ── EXIF snapshot ─────────────────────────────────────────────────────
    /// Cached EXIF values extracted at import time. Stored here so that
    /// reconstruction (`rebuild_index`) never requires a full image decode.
    pub exif: Option<LibraryExif>,
}

/// Wall-clock timestamp stored as seconds + nanoseconds past the Unix epoch.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileTimeStamp {
    pub secs: i64,
    pub nanos: u32,
}

impl FileTimeStamp {
    /// Convert from a [`SystemTime`] (clamped to non-negative seconds from epoch).
    pub fn from_system_time(t: SystemTime) -> Self {
        let d = t.duration_since(UNIX_EPOCH).unwrap_or_default();
        Self {
            secs: d.as_secs() as i64,
            nanos: d.subsec_nanos(),
        }
    }

    /// Convert to a [`SystemTime`]. Negative seconds are clamped to the epoch.
    pub fn to_system_time(self) -> SystemTime {
        let secs = self.secs.max(0) as u64;
        UNIX_EPOCH + Duration::new(secs, self.nanos)
    }
}

fn default_true() -> bool {
    true
}

impl Default for LibraryMeta {
    fn default() -> Self {
        Self {
            original_filename: None,
            import_session_id: String::new(),
            import_date: 0,
            stack_peer_hash: None,
            stack_is_primary: true,
            keywords: Vec::new(),
            rating: 0,
            color_label: None,
            flag: None,
            protected: false,
            collection_refs: Vec::new(),
            legacy_collections: Vec::new(),
            caption: None,
            copyright: None,
            creator: None,
            location_city: None,
            location_country: None,
            source_path: None,
            source_size: None,
            source_mtime: None,
            source_atime: None,
            source_ctime: None,
            exif: None,
        }
    }
}

/// EXIF values extracted at import time and cached inside [`LibraryMeta`].
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LibraryExif {
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub lens_make: Option<String>,
    pub lens_model: Option<String>,
    /// DateTimeOriginal from EXIF, stored as-is (e.g. `"2025:06:03 14:22:00"`).
    pub capture_date: Option<String>,

    pub iso: Option<u32>,
    /// Shutter speed as decimal seconds (`1/2000` → `0.0005`) for range queries.
    pub shutter_sec: Option<f64>,
    /// Human-readable shutter speed string for display (e.g. `"1/2000"`).
    pub shutter_display: Option<String>,
    /// Aperture as f-number (e.g. `2.8`).
    pub aperture: Option<f32>,
    pub focal_length: Option<f32>,
    pub focal_length_35mm: Option<f32>,
    pub exposure_bias: Option<f32>,
    /// Subject distance in metres.
    pub subject_distance: Option<f32>,
    pub exposure_program: Option<String>,
    pub metering_mode: Option<String>,
    pub flash: Option<bool>,

    pub gps_lat: Option<f64>,
    pub gps_lon: Option<f64>,
    pub gps_alt: Option<f32>,
}

impl LibraryExif {
    /// Build a [`LibraryExif`] from the [`crate::image::ImageMetadata`] that
    /// the format decoders populate at load time.
    pub fn from_image_metadata(m: &crate::image::ImageMetadata) -> Self {
        Self {
            camera_make: m.camera_make.clone(),
            camera_model: m.camera_model.clone(),
            lens_make: m.lens_make.clone(),
            lens_model: m.lens_model.clone(),
            capture_date: m.date_time.clone(),
            iso: m.iso,
            shutter_sec: m.shutter_speed.as_deref().and_then(parse_shutter_sec),
            shutter_display: m.shutter_speed.clone(),
            aperture: m.aperture,
            focal_length: m.focal_length,
            focal_length_35mm: m.focal_length_35mm.map(|v| v as f32),
            exposure_bias: m.exposure_bias,
            subject_distance: m.subject_distance,
            exposure_program: m.exposure_program.clone(),
            metering_mode: m.metering_mode.clone(),
            // Normalise flash string to bool: present + not "No Flash" → true
            flash: m
                .flash
                .as_deref()
                .map(|s| !s.eq_ignore_ascii_case("no flash") && !s.eq_ignore_ascii_case("0")),
            gps_lat: m.gps_lat,
            gps_lon: m.gps_lon,
            gps_alt: m.gps_alt,
        }
    }
}

/// Parse a shutter-speed string to decimal seconds.
///
/// Accepts `"1/2000"`, `"1/500 s"`, `"0.5"`, `"0.5 s"`, `"2 s"`, etc.
/// The trailing ` s` unit (produced by `format_rational`) is stripped before parsing.
pub fn parse_shutter_sec(s: &str) -> Option<f64> {
    let s = s.trim();
    let s = s.strip_suffix(" s").unwrap_or(s).trim_end();
    if let Some((num, den)) = s.split_once('/') {
        let n: f64 = num.trim().parse().ok()?;
        let d: f64 = den.trim().parse().ok()?;
        if d == 0.0 {
            return None;
        }
        Some(n / d)
    } else {
        s.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_shutter_fraction() {
        let v = parse_shutter_sec("1/2000").unwrap();
        assert!((v - 0.0005).abs() < 1e-10);
    }

    #[test]
    fn parse_shutter_decimal() {
        let v = parse_shutter_sec("0.5").unwrap();
        assert!((v - 0.5).abs() < 1e-10);
    }

    #[test]
    fn parse_shutter_whole() {
        let v = parse_shutter_sec("2").unwrap();
        assert!((v - 2.0).abs() < 1e-10);
    }

    #[test]
    fn parse_shutter_with_unit_suffix() {
        // format_rational appends " s" — parser must strip it
        let v = parse_shutter_sec("1/200 s").unwrap();
        assert!((v - 0.005).abs() < 1e-10);
        let v = parse_shutter_sec("0.5 s").unwrap();
        assert!((v - 0.5).abs() < 1e-10);
        let v = parse_shutter_sec("2 s").unwrap();
        assert!((v - 2.0).abs() < 1e-10);
    }

    #[test]
    fn parse_shutter_invalid() {
        assert!(parse_shutter_sec("abc").is_none());
        assert!(parse_shutter_sec("1/0").is_none());
    }

    #[test]
    fn library_meta_default_roundtrip() {
        let meta = LibraryMeta {
            import_session_id: "test-session".into(),
            rating: 3,
            keywords: vec!["travel".into(), "sunset".into()],
            ..Default::default()
        };
        let json = serde_json::to_string(&meta).unwrap();
        let back: LibraryMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(back.rating, 3);
        assert_eq!(back.keywords, vec!["travel", "sunset"]);
        assert!(back.stack_is_primary); // default_true
        assert!(!back.protected); // defaults to false
    }

    /// Files written before collections had ids carry a bare list of names.
    /// They have to keep their memberships — a rebuild reads the legacy list —
    /// and the field must not be written back out, or a migrated file would
    /// carry its membership twice.
    #[test]
    fn legacy_collection_names_are_read_but_never_rewritten() {
        let json = r#"{"original_filename":null,"import_session_id":"s",
            "import_date":0,"stack_peer_hash":null,"exif":null,
            "collections":["Portfolio","Prints"]}"#;
        let meta: LibraryMeta = serde_json::from_str(json).unwrap();
        assert_eq!(meta.legacy_collections, ["Portfolio", "Prints"]);
        assert!(meta.collection_refs.is_empty());

        let migrated = LibraryMeta {
            collection_refs: vec![CollectionRef {
                id: "d1f7c0de-0000-4000-8000-000000000001".to_owned(),
                name: "Portfolio".to_owned(),
            }],
            ..Default::default()
        };
        let json = serde_json::to_string(&migrated).unwrap();
        assert!(
            !json.contains("\"collections\""),
            "the legacy list must not be written back: {json}"
        );

        let back: LibraryMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(back.collection_refs, migrated.collection_refs);
        assert!(back.legacy_collections.is_empty());
    }

    /// A name hint is only a hint, so a file written by a version that did not
    /// record one still names its collection something.
    #[test]
    fn a_collection_ref_without_a_name_defaults_to_empty() {
        let refs: Vec<CollectionRef> =
            serde_json::from_str(r#"[{"id":"d1f7c0de-0000-4000-8000-000000000001"}]"#).unwrap();
        assert_eq!(refs[0].name, "");
    }

    #[test]
    fn protected_defaults_false_on_old_files() {
        // A pre-protection .rlab whose LMTA JSON has no `protected` key must
        // still deserialize, defaulting the flag to false.
        let json = r#"{"original_filename":null,"import_session_id":"s",
            "import_date":0,"stack_peer_hash":null,"exif":null}"#;
        let meta: LibraryMeta = serde_json::from_str(json).unwrap();
        assert!(!meta.protected);

        let meta = LibraryMeta {
            protected: true,
            ..Default::default()
        };
        let back: LibraryMeta =
            serde_json::from_str(&serde_json::to_string(&meta).unwrap()).unwrap();
        assert!(back.protected);
    }
}
