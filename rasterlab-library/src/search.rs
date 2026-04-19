use std::ops::RangeInclusive;

use crate::db_trait::CollectionId;

/// All fields are ANDed together. `None` means "no constraint on this field".
#[derive(Debug, Clone, Default)]
pub struct SearchFilter {
    /// Full-text match against keywords, original filename, and caption.
    pub text: Option<String>,

    /// Minimum star rating (inclusive).
    pub rating_min: Option<u8>,

    /// Pick/reject flag: `"pick"` or `"reject"`.
    pub flag: Option<String>,

    /// Aperture (f-number) range, e.g. `2.8..=2.8` for exactly f/2.8.
    pub aperture: Option<RangeInclusive<f32>>,

    /// ISO range.
    pub iso: Option<RangeInclusive<u32>>,

    /// "Faster than" threshold: upper bound on `shutter_sec`
    /// (e.g. `0.002` = 1/500 s, matches all photos ≤ 1/500).
    pub shutter_max_sec: Option<f64>,

    /// "Slower than" threshold: lower bound on `shutter_sec`.
    pub shutter_min_sec: Option<f64>,

    /// Substring match on camera model.
    pub camera_model: Option<String>,

    /// Substring match on lens model.
    pub lens_model: Option<String>,

    /// Capture date range (inclusive).  Compared against the `capture_date`
    /// column which stores the raw EXIF string `"YYYY:MM:DD HH:MM:SS"`.
    pub capture_date_from: Option<String>,
    pub capture_date_to: Option<String>,

    /// Filter to photos from a specific import session (UUID string).
    pub import_session: Option<String>,

    /// Filter to photos in a specific collection.
    pub collection_id: Option<CollectionId>,

    /// Color label: `"red"`, `"yellow"`, `"green"`, `"blue"`, `"purple"`.
    pub color_label: Option<String>,

    /// When `true`, only return photos with at least one committed edit.
    pub has_edits_only: bool,
}

impl SearchFilter {
    pub fn is_empty(&self) -> bool {
        self.text.is_none()
            && self.rating_min.is_none()
            && self.flag.is_none()
            && self.aperture.is_none()
            && self.iso.is_none()
            && self.shutter_max_sec.is_none()
            && self.shutter_min_sec.is_none()
            && self.camera_model.is_none()
            && self.lens_model.is_none()
            && self.capture_date_from.is_none()
            && self.capture_date_to.is_none()
            && self.import_session.is_none()
            && self.collection_id.is_none()
            && self.color_label.is_none()
            && !self.has_edits_only
    }
}
