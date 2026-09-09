//! Compare two libraries and report where they disagree.
//!
//! The question this answers is "did that change alter what ends up in the
//! library?" — import the same sources with the old code and the new code,
//! then compare the two libraries.  A clean run means the new code produced
//! the same library the old one did.
//!
//! # What "the same" means here
//!
//! Two libraries are never byte-identical: ids, uuids and timestamps are
//! minted per run, so comparing raw rows or raw `.rlab` bytes would report a
//! difference for every photo and say nothing.  What is compared instead is
//! everything that would survive being written out and read back somewhere
//! else — the photograph, what the user set on it, and how it is filed:
//!
//! * **Photos are keyed by content hash**, which is both the `.rlab`'s name
//!   and, on the way in, a check that the embedded original is intact —
//!   [`RlabFile::read`] verifies it.  A photo in one library and not the other
//!   is reported as such.
//! * **Every `LMTA` field is compared**, by serialising the chunk and diffing
//!   it key by key, so a field added to [`LibraryMeta`] later is covered
//!   without touching this module.
//! * **Edit stacks are compared**, including each virtual copy's name, its
//!   serialised operations and its undo cursor.
//! * **Collections and import sessions are compared by name**, as sets of the
//!   photos in them.  The uuid a collection is really keyed by is minted per
//!   run and cannot match across two libraries; the name and the membership
//!   are what a user would call the same.
//! * **Both stores are read.**  The index is compared as well as the files,
//!   because a photo whose file is right and whose row is wrong is exactly the
//!   kind of divergence this is meant to catch — as is a `.rlab` on disk that
//!   no row mentions.
//!
//! Deliberately ignored, because they differ between two correct runs:
//! row ids, collection and session uuids, import timestamps, the `.rlab`
//! header's `app_version`/`created_at`/`modified_at`, and the source file's
//! path and access/creation times.  The source's size and mtime *are*
//! compared: the same source file has to look the same to both runs.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use anyhow::Result;
use rasterlab_core::{library_meta::LibraryMeta, project::RlabFile};
use serde_json::Value;

use crate::{
    db_trait::SortOrder,
    import::{thumb_path, walk_rlab_files},
    library::Library,
};

/// `LMTA` fields left out of the comparison.
///
/// The first three are minted or stamped per run.  `source_atime` moves
/// whenever anything reads the source, and `source_ctime` is unavailable on
/// Linux and set by the copy elsewhere, so neither says anything about the
/// import.  Collection references are handled separately: they carry a uuid
/// that cannot match across libraries, so they are compared by name.
const IGNORED_LMTA_FIELDS: &[&str] = &[
    "import_date",
    "import_session_id",
    "source_path",
    "source_atime",
    "source_ctime",
    "collection_refs",
    // serde name of `legacy_collections`.
    "collections",
];

/// How many differing photos a collection or session difference names before
/// the rest are left to the per-photo lines.
const MEMBERS_NAMED: usize = 3;

/// Characters of a hash shown when a photo has no filename to go by.
const HASH_SHOWN: usize = 12;

// ── Results ──────────────────────────────────────────────────────────────────

/// Which of the two libraries a one-sided difference is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

impl Side {
    fn label(self) -> &'static str {
        match self {
            Side::Left => "left",
            Side::Right => "right",
        }
    }
}

/// What a difference is about, for a summary that does not have to re-read
/// every line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Photo,
    Collection,
    Session,
}

/// One way in which the two libraries disagree.
#[derive(Debug, Clone)]
pub struct Difference {
    pub scope: Scope,
    /// What disagrees: `photo DSC_0001.NEF (3f2a…)`.
    pub subject: String,
    /// How, in one line: `rating: 3 (left) vs 0 (right)`.
    pub detail: String,
}

/// Live progress for a running comparison.
#[derive(Debug, Clone)]
pub struct CompareProgress {
    /// Which library is being read; the two are read one after the other.
    pub side: Side,
    pub total: usize,
    pub done: usize,
    /// Library-relative path of the file being read, empty when a side is done.
    pub current: PathBuf,
}

/// Final tally once a comparison finishes (or is cancelled).
#[derive(Debug, Clone, Default)]
pub struct CompareOutcome {
    /// Photos found in each library, whether or not they matched.
    pub photos_left: usize,
    pub photos_right: usize,
    /// Every disagreement found, photos first and each in hash order.
    pub differences: Vec<Difference>,
    /// Files neither library could be read from: `(path, message)`.
    pub errors: Vec<(PathBuf, String)>,
    pub cancelled: bool,
}

impl CompareOutcome {
    /// True when the two libraries hold the same photos, filed the same way —
    /// and the run actually got far enough to say so.
    pub fn is_match(&self) -> bool {
        self.differences.is_empty() && self.errors.is_empty() && !self.cancelled
    }

    /// How many differences fall in each scope, for a one-line summary.
    pub fn counts(&self) -> (usize, usize, usize) {
        let count = |scope| self.differences.iter().filter(|d| d.scope == scope).count();
        (
            count(Scope::Photo),
            count(Scope::Collection),
            count(Scope::Session),
        )
    }
}

/// What to read while comparing.
#[derive(Debug, Clone, Copy, Default)]
pub struct CompareOptions {
    /// Compare the index rows alone, without opening a single `.rlab`.
    ///
    /// Much faster on a large or network-mounted library, and enough to catch
    /// a change that files photos differently.  It cannot see a difference in
    /// what was actually written — metadata, edit stacks, thumbnails — so the
    /// full comparison is the default.
    pub index_only: bool,
}

// ── Comparison ───────────────────────────────────────────────────────────────

/// Read both libraries and report every way they differ.
///
/// Both are opened read-only in the sense that nothing here writes to them,
/// but each does take the index's exclusive lock while it is read, so neither
/// library may be open elsewhere.  The libraries are read one after the other
/// rather than together, so only one lock is held at a time.
///
/// `cancel` is polled before each file; a cancelled run reports what it had
/// compared so far, which is a partial answer, so [`CompareOutcome::is_match`]
/// is false for it however few differences it found.
pub fn compare(
    left: &Path,
    right: &Path,
    options: CompareOptions,
    cancel: Arc<AtomicBool>,
    progress_cb: &dyn Fn(CompareProgress),
) -> Result<CompareOutcome> {
    let left_snapshot = snapshot(left, Side::Left, options, &cancel, progress_cb)?;
    let right_snapshot = snapshot(right, Side::Right, options, &cancel, progress_cb)?;

    let mut differences = Vec::new();
    diff_photos(&left_snapshot, &right_snapshot, &mut differences);
    diff_groups(
        Scope::Collection,
        "collection",
        &left_snapshot.collections,
        &right_snapshot.collections,
        &left_snapshot,
        &right_snapshot,
        &mut differences,
    );
    diff_groups(
        Scope::Session,
        "import session",
        &left_snapshot.sessions,
        &right_snapshot.sessions,
        &left_snapshot,
        &right_snapshot,
        &mut differences,
    );

    let mut errors = left_snapshot.errors;
    errors.extend(right_snapshot.errors);
    Ok(CompareOutcome {
        photos_left: left_snapshot.photos.len(),
        photos_right: right_snapshot.photos.len(),
        differences,
        errors,
        cancelled: cancel.load(Ordering::Relaxed),
    })
}

/// Everything about one photo that is stable across two runs, as
/// `field → value` so that two photos diff key by key.
type Facts = BTreeMap<String, String>;

/// One library, reduced to what can be compared with another.
struct Snapshot {
    /// Photo hash → what both stores say about it.
    photos: BTreeMap<String, Facts>,
    /// Collection name → the hashes of the photos in it.
    collections: BTreeMap<String, BTreeSet<String>>,
    /// Session name → the hashes of the photos in it.
    sessions: BTreeMap<String, BTreeSet<String>>,
    errors: Vec<(PathBuf, String)>,
}

impl Snapshot {
    /// How to name a photo in a report: its filename where it has one, and
    /// enough of its hash to find it on disk.
    fn name(&self, hash: &str) -> String {
        let short = &hash[..hash.len().min(HASH_SHOWN)];
        // The index's copy of the name where there is a row, the file's where
        // the photo is only on disk.
        let facts = self.photos.get(hash);
        let name = facts
            .and_then(|facts| facts.get("index.original_filename"))
            .or_else(|| facts.and_then(|facts| facts.get("file.lmta.original_filename")));
        match name {
            Some(name) if name != "none" => format!("{name} ({short}…)"),
            _ => format!("{short}…"),
        }
    }
}

/// Read one library into the form the comparison works on.
fn snapshot(
    root: &Path,
    side: Side,
    options: CompareOptions,
    cancel: &AtomicBool,
    progress_cb: &dyn Fn(CompareProgress),
) -> Result<Snapshot> {
    let library = Library::open_existing(root)?;
    let files_dir = root.join("files");
    let deleted_dir = root.join("recently_deleted/files");

    // Driven by the union of both stores rather than by the index alone: a
    // `.rlab` no row mentions is a real difference between two libraries, and
    // an interrupted import is exactly how one gets there.
    let mut on_disk: BTreeMap<String, (PathBuf, &'static str)> = BTreeMap::new();
    for (dir, state) in [(&files_dir, "active"), (&deleted_dir, "deleted")] {
        for path in walk_rlab_files(dir) {
            let Some(hash) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            // A file on both sides is one whose move into or out of Recently
            // Deleted was interrupted; where it ended up is what counts, and
            // `deleted` is walked second.
            on_disk.insert(hash.to_owned(), (path.clone(), state));
        }
    }

    let sessions_by_id: HashMap<String, String> = library
        .all_sessions()?
        .into_iter()
        .map(|row| (row.id, row.name))
        .collect();
    let collections = library.all_collections()?;
    let collection_names: HashMap<i64, String> = collections
        .iter()
        .map(|row| (row.id, row.name.clone()))
        .collect();

    let rows: Vec<_> = library
        .all_photos(SortOrder::default())?
        .into_iter()
        .map(|row| (row, "active"))
        .chain(
            library
                .recently_deleted()?
                .into_iter()
                .map(|row| (row.photo, "deleted")),
        )
        .collect();

    // Membership is read by photo id, which is meaningless outside this
    // library, so it is turned into hashes on the way in.
    let hash_by_id: HashMap<i64, String> = rows
        .iter()
        .map(|(row, _)| (row.id, row.hash.clone()))
        .collect();
    let mut collection_members: BTreeMap<String, BTreeSet<String>> = collections
        .iter()
        .map(|row| (row.name.clone(), BTreeSet::new()))
        .collect();
    let mut photo_collections: HashMap<String, BTreeSet<String>> = HashMap::new();
    for (collection_id, photo_id) in library.collection_memberships()? {
        let (Some(name), Some(hash)) = (
            collection_names.get(&collection_id),
            hash_by_id.get(&photo_id),
        ) else {
            continue;
        };
        collection_members
            .entry(name.clone())
            .or_default()
            .insert(hash.clone());
        photo_collections
            .entry(hash.clone())
            .or_default()
            .insert(name.clone());
    }

    let mut snapshot = Snapshot {
        photos: BTreeMap::new(),
        collections: collection_members,
        sessions: BTreeMap::new(),
        errors: Vec::new(),
    };

    let mut rows_by_hash: HashMap<String, _> = HashMap::with_capacity(rows.len());
    for (row, state) in rows {
        rows_by_hash.insert(row.hash.clone(), (row, state));
    }

    let hashes: BTreeSet<String> = on_disk
        .keys()
        .cloned()
        .chain(rows_by_hash.keys().cloned())
        .collect();
    let total = hashes.len();

    for (done, hash) in hashes.into_iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let file = on_disk.get(&hash);
        progress_cb(CompareProgress {
            side,
            total,
            done,
            current: file
                .map(|(path, _)| relative(root, path))
                .unwrap_or_default(),
        });

        let mut facts = Facts::new();
        match rows_by_hash.get(&hash) {
            Some((row, state)) => {
                facts.insert("index.present".into(), "yes".into());
                facts.insert("index.state".into(), (*state).into());
                facts.insert("index.width".into(), row.width.to_string());
                facts.insert("index.height".into(), row.height.to_string());
                facts.insert("index.capture_date".into(), optional(&row.capture_date));
                facts.insert(
                    "index.original_filename".into(),
                    optional(&row.original_filename),
                );
                facts.insert(
                    "index.stack_is_primary".into(),
                    row.stack_is_primary.to_string(),
                );
                facts.insert("index.has_edits".into(), row.has_edits.to_string());
                facts.insert("index.protected".into(), row.protected.to_string());
                facts.insert(
                    "index.session".into(),
                    sessions_by_id
                        .get(&row.import_session)
                        .cloned()
                        // A row pointing at a session that is not there is a
                        // difference worth seeing, not one to paper over.
                        .unwrap_or_else(|| "<unknown session>".into()),
                );
                facts.insert(
                    "index.collections".into(),
                    join(
                        photo_collections
                            .get(&hash)
                            .into_iter()
                            .flatten()
                            .map(String::as_str),
                    ),
                );
                if let Some(name) = sessions_by_id.get(&row.import_session) {
                    snapshot
                        .sessions
                        .entry(name.clone())
                        .or_default()
                        .insert(hash.clone());
                }
            }
            None => {
                facts.insert("index.present".into(), "no".into());
            }
        }

        match file {
            Some((path, state)) => {
                facts.insert("file.present".into(), "yes".into());
                facts.insert("file.state".into(), (*state).into());
                if !options.index_only
                    && let Err(e) = read_file_facts(root, path, &hash, &mut facts)
                {
                    snapshot.errors.push((path.clone(), e.to_string()));
                }
            }
            None => {
                facts.insert("file.present".into(), "no".into());
            }
        }
        snapshot.photos.insert(hash, facts);
    }

    progress_cb(CompareProgress {
        side,
        total,
        done: snapshot.photos.len(),
        current: PathBuf::new(),
    });
    Ok(snapshot)
}

/// Add everything the photo's `.rlab` — and the thumbnail beside it — say.
fn read_file_facts(root: &Path, path: &Path, hash: &str, facts: &mut Facts) -> Result<()> {
    // A full read rather than a summary: it verifies the embedded original
    // against the hash the file is named for, which is the one check that says
    // the two libraries hold the same photograph and not just the same row.
    let rlab = RlabFile::read(path)?;
    facts.insert(
        "file.format_version".into(),
        rlab.format_version.to_string(),
    );
    facts.insert("file.width".into(), rlab.meta.width.to_string());
    facts.insert("file.height".into(), rlab.meta.height.to_string());
    facts.insert(
        "file.original_bytes".into(),
        rlab.original_bytes.len().to_string(),
    );
    facts.insert("file.has_edits".into(), rlab.has_edits().to_string());
    facts.insert(
        "file.active_copy".into(),
        rlab.active_copy_index.to_string(),
    );
    // Hashed rather than sized: the thumbnail is regenerated on import, so its
    // bytes are what says whether a change altered how photos are previewed.
    facts.insert(
        "file.thumbnail".into(),
        match &rlab.thumbnail {
            Some(bytes) => blake3::hash(bytes).to_hex().to_string(),
            None => "none".into(),
        },
    );
    facts.insert(
        "file.thumbnail_on_disk".into(),
        thumb_path(root, hash).exists().to_string(),
    );

    flatten("file.copies", &serde_json::to_value(&rlab.copies)?, facts);
    lmta_facts(rlab.lmta.as_ref(), facts)?;
    Ok(())
}

/// Add the `LMTA` chunk, field by field, minus what is minted per run.
fn lmta_facts(lmta: Option<&LibraryMeta>, facts: &mut Facts) -> Result<()> {
    let Some(lmta) = lmta else {
        facts.insert("file.lmta".into(), "none".into());
        return Ok(());
    };
    let mut value = serde_json::to_value(lmta)?;
    if let Some(fields) = value.as_object_mut() {
        for ignored in IGNORED_LMTA_FIELDS {
            fields.remove(*ignored);
        }
    }
    flatten("file.lmta", &value, facts);

    // Collection membership as the file records it, by name: the uuid it is
    // really keyed by is minted per run.  Pre-uuid files name their
    // collections and nothing else, and belong in the same list.
    let names: BTreeSet<&str> = lmta
        .collection_refs
        .iter()
        .map(|reference| reference.name.as_str())
        .chain(lmta.legacy_collections.iter().map(String::as_str))
        .filter(|name| !name.is_empty())
        .collect();
    facts.insert("file.lmta.collection_names".into(), join(names));
    Ok(())
}

// ── Diffing ──────────────────────────────────────────────────────────────────

fn diff_photos(left: &Snapshot, right: &Snapshot, out: &mut Vec<Difference>) {
    for hash in union(left.photos.keys(), right.photos.keys()) {
        match (left.photos.get(&hash), right.photos.get(&hash)) {
            (Some(a), Some(b)) => {
                for (field, values) in collapse_missing_stores(fields_differing(a, b)) {
                    out.push(Difference {
                        scope: Scope::Photo,
                        subject: format!("photo {}", left.name(&hash)),
                        detail: format!("{field}: {values}"),
                    });
                }
            }
            (Some(_), None) => out.push(only_in(
                Scope::Photo,
                "photo",
                &left.name(&hash),
                Side::Left,
            )),
            (None, Some(_)) => out.push(only_in(
                Scope::Photo,
                "photo",
                &right.name(&hash),
                Side::Right,
            )),
            (None, None) => unreachable!("hash came from one of the two maps"),
        }
    }
}

/// Compare two named groupings of photos — collections, or import sessions.
fn diff_groups(
    scope: Scope,
    noun: &str,
    left: &BTreeMap<String, BTreeSet<String>>,
    right: &BTreeMap<String, BTreeSet<String>>,
    left_snapshot: &Snapshot,
    right_snapshot: &Snapshot,
    out: &mut Vec<Difference>,
) {
    for name in union(left.keys(), right.keys()) {
        let subject = format!("{noun} \"{name}\"");
        match (left.get(&name), right.get(&name)) {
            (Some(a), Some(b)) if a != b => {
                for (side, snapshot, missing) in [
                    (Side::Left, left_snapshot, a.difference(b)),
                    (Side::Right, right_snapshot, b.difference(a)),
                ] {
                    let only: Vec<String> = missing.map(|hash| snapshot.name(hash)).collect();
                    if only.is_empty() {
                        continue;
                    }
                    out.push(Difference {
                        scope,
                        subject: subject.clone(),
                        detail: format!(
                            "{} only in {}: {}",
                            count_of(only.len(), "photo"),
                            side.label(),
                            summarise(&only)
                        ),
                    });
                }
            }
            (Some(_), Some(_)) => {}
            (Some(_), None) => out.push(only_in(scope, noun, &name, Side::Left)),
            (None, Some(_)) => out.push(only_in(scope, noun, &name, Side::Right)),
            (None, None) => unreachable!("name came from one of the two maps"),
        }
    }
}

/// Every field the two photos disagree on, as `left vs right`.
fn fields_differing(left: &Facts, right: &Facts) -> Vec<(String, String)> {
    let missing = "<absent>".to_owned();
    union(left.keys(), right.keys())
        .into_iter()
        .filter_map(|field| {
            let a = left.get(&field).unwrap_or(&missing);
            let b = right.get(&field).unwrap_or(&missing);
            (a != b).then(|| (field.clone(), format!("{a} (left) vs {b} (right)")))
        })
        .collect()
}

/// Reduce a photo one library holds in only one of its two stores to the one
/// line that says so.
///
/// A `.rlab` that is on one side and not the other disagrees about every field
/// the file carries, which is fifty lines saying the same thing as
/// `file.present`.  The store that is there on both sides still reports
/// normally: a photo whose file matches and whose row does not is exactly what
/// this command is for.
fn collapse_missing_stores(fields: Vec<(String, String)>) -> Vec<(String, String)> {
    let absent = |store: &str| {
        fields
            .iter()
            .any(|(field, _)| field == &format!("{store}.present"))
    };
    let missing: Vec<&str> = ["file", "index"]
        .into_iter()
        .filter(|store| absent(store))
        .collect();
    if missing.is_empty() {
        return fields;
    }
    fields
        .into_iter()
        .filter(|(field, _)| {
            missing
                .iter()
                .all(|store| field == &format!("{store}.present") || !field.starts_with(store))
        })
        .collect()
}

fn only_in(scope: Scope, noun: &str, name: &str, side: Side) -> Difference {
    let subject = if scope == Scope::Photo {
        format!("{noun} {name}")
    } else {
        format!("{noun} \"{name}\"")
    };
    Difference {
        scope,
        subject,
        detail: format!("only in {}", side.label()),
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Flatten JSON into `prefix.field = value` lines, so that two structures diff
/// field by field and a field added to either one is compared without this
/// module having to learn about it.
fn flatten(prefix: &str, value: &Value, out: &mut Facts) {
    match value {
        Value::Object(fields) if !fields.is_empty() => {
            for (name, field) in fields {
                flatten(&format!("{prefix}.{name}"), field, out);
            }
        }
        Value::Array(items) if !items.is_empty() => {
            for (i, item) in items.iter().enumerate() {
                flatten(&format!("{prefix}[{i}]"), item, out);
            }
        }
        Value::Object(_) => {
            out.insert(prefix.to_owned(), "{}".into());
        }
        Value::Array(_) => {
            out.insert(prefix.to_owned(), "[]".into());
        }
        Value::Null => {
            out.insert(prefix.to_owned(), "none".into());
        }
        other => {
            out.insert(prefix.to_owned(), other.to_string());
        }
    }
}

fn union<'a, T: Ord + Clone + 'a>(
    left: impl Iterator<Item = &'a T>,
    right: impl Iterator<Item = &'a T>,
) -> Vec<T> {
    let set: BTreeSet<T> = left.chain(right).cloned().collect();
    set.into_iter().collect()
}

fn optional(value: &Option<String>) -> String {
    value.clone().unwrap_or_else(|| "none".into())
}

fn join<'a>(names: impl IntoIterator<Item = &'a str>) -> String {
    let joined = names.into_iter().collect::<Vec<_>>().join(", ");
    if joined.is_empty() {
        "none".into()
    } else {
        joined
    }
}

/// The first few names, and how many more there are: a difference of hundreds
/// of photos is a line, not a page.
fn summarise(names: &[String]) -> String {
    if names.len() <= MEMBERS_NAMED {
        return names.join(", ");
    }
    format!(
        "{}, and {} more",
        names[..MEMBERS_NAMED].join(", "),
        names.len() - MEMBERS_NAMED
    )
}

fn count_of(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

fn relative(root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(root).unwrap_or(path).to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rasterlab_core::library_meta::CollectionRef;
    use serde_json::json;

    fn flattened(value: Value) -> Facts {
        let mut facts = Facts::new();
        flatten("x", &value, &mut facts);
        facts
    }

    /// Every leaf gets a key, and an empty container gets one too — otherwise
    /// a field emptied by a change would look like a field that never existed.
    #[test]
    fn flattening_names_every_leaf_and_every_empty_container() {
        let facts = flattened(json!({
            "rating": 3,
            "exif": { "iso": 400, "lens": null },
            "keywords": ["sky", "sea"],
            "notes": [],
            "extra": {},
        }));
        assert_eq!(
            facts.into_iter().collect::<Vec<_>>(),
            vec![
                ("x.exif.iso".to_owned(), "400".to_owned()),
                ("x.exif.lens".to_owned(), "none".to_owned()),
                ("x.extra".to_owned(), "{}".to_owned()),
                ("x.keywords[0]".to_owned(), "\"sky\"".to_owned()),
                ("x.keywords[1]".to_owned(), "\"sea\"".to_owned()),
                ("x.notes".to_owned(), "[]".to_owned()),
                ("x.rating".to_owned(), "3".to_owned()),
            ]
        );
    }

    /// A field one side has and the other does not is a difference, not a
    /// field to skip: that is how a chunk gaining or losing one is caught.
    #[test]
    fn a_field_only_one_side_has_is_a_difference() {
        let left = Facts::from([("rating".to_owned(), "3".to_owned())]);
        let right = Facts::from([("caption".to_owned(), "\"hi\"".to_owned())]);
        assert_eq!(
            fields_differing(&left, &right),
            vec![
                (
                    "caption".to_owned(),
                    "<absent> (left) vs \"hi\" (right)".to_owned()
                ),
                (
                    "rating".to_owned(),
                    "3 (left) vs <absent> (right)".to_owned()
                ),
            ]
        );
    }

    /// The `LMTA` diff drops what is minted per run, and compares collection
    /// membership by name rather than by the uuid it is keyed on.
    #[test]
    fn lmta_comparison_ignores_what_differs_between_two_correct_runs() {
        let lmta = LibraryMeta {
            import_session_id: "3f9c-…".into(),
            import_date: 1_700_000_000,
            source_path: Some("/cards/DCIM/DSC_0001.NEF".into()),
            rating: 4,
            collection_refs: vec![CollectionRef {
                id: "uuid-of-the-run".into(),
                name: "Iceland".into(),
            }],
            legacy_collections: vec!["Old Trip".into()],
            ..LibraryMeta::default()
        };
        let mut facts = Facts::new();
        lmta_facts(Some(&lmta), &mut facts).unwrap();

        for ignored in IGNORED_LMTA_FIELDS {
            assert!(
                !facts.contains_key(&format!("file.lmta.{ignored}")),
                "{ignored} was compared"
            );
        }
        assert_eq!(facts.get("file.lmta.rating").map(String::as_str), Some("4"));
        assert_eq!(
            facts.get("file.lmta.collection_names").map(String::as_str),
            Some("Iceland, Old Trip")
        );
    }

    /// A photo one library has no `.rlab` for disagrees about every field that
    /// file carries; one line says it, and the index is still compared.
    #[test]
    fn a_missing_file_collapses_to_one_line() {
        let fields = vec![
            ("file.present".to_owned(), "yes vs no".to_owned()),
            ("file.lmta.rating".to_owned(), "3 vs <absent>".to_owned()),
            ("file.width".to_owned(), "640 vs <absent>".to_owned()),
            ("index.protected".to_owned(), "true vs false".to_owned()),
        ];
        assert_eq!(
            collapse_missing_stores(fields),
            vec![
                ("file.present".to_owned(), "yes vs no".to_owned()),
                ("index.protected".to_owned(), "true vs false".to_owned()),
            ]
        );
    }

    /// A collection two libraries disagree about by hundreds of photos is one
    /// line, not a page of them.
    #[test]
    fn a_long_list_of_photos_is_summarised() {
        let names: Vec<String> = (0..5).map(|n| format!("photo {n}")).collect();
        assert_eq!(summarise(&names[..2]), "photo 0, photo 1");
        assert_eq!(
            summarise(&names),
            "photo 0, photo 1, photo 2, and 2 more",
            "the cap is {MEMBERS_NAMED}"
        );
    }
}
