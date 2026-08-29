use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use anyhow::Result;
use rasterlab_core::{formats::FormatRegistry, project::RlabFile};
use uuid::Uuid;
use walkdir::WalkDir;

use crate::{
    db_trait::{CollectionRow, LibraryDb, NewPhoto, PhotoId, SortOrder},
    import::{format_session_name, thumb_path, unix_now},
    thumbnail::{generate_thumbnail, write_thumbnail},
};

#[derive(Debug, Clone)]
pub struct RebuildProgress {
    pub total: usize,
    pub done: usize,
    pub current: std::path::PathBuf,
    pub errors: Vec<(std::path::PathBuf, String)>,
}

/// Final tally returned once a rebuild finishes (or is cancelled).
#[derive(Debug, Clone, Default)]
pub struct RebuildOutcome {
    /// `.rlab` files the walk found.
    pub total: usize,
    /// Files re-indexed before the walk ended.
    pub done: usize,
    /// Per-file failures: `(path, message)`.
    pub errors: Vec<(std::path::PathBuf, String)>,
    pub cancelled: bool,
}

/// Per-session aggregates gathered while re-indexing photos, used afterwards to
/// restore the `import_sessions` rows. Only the session id and each photo's
/// `import_date` survive in the LMTA chunks, so `started_at` and the display
/// name are both rebuilt from the import-date range.
struct SessionAgg {
    min_date: u64,
    max_date: u64,
}

/// What the files say about one collection, gathered as they are read.
///
/// The name is a hint each member file carries, and files that have not been
/// written since a rename carry an old one, so the most recently written file
/// wins.  Ties — the stamp has one-second resolution — go to the larger hash
/// rather than to whatever order the walk happened to take, so a rebuild of
/// the same library twice gives the same answer.
#[derive(Debug, Default)]
struct CollectionAgg {
    name: String,
    name_written_at: u64,
    name_from_hash: String,
    members: Vec<String>,
}

impl CollectionAgg {
    fn offer_name(&mut self, name: &str, written_at: u64, hash: &str) {
        if name.is_empty() {
            return;
        }
        let newer = (written_at, hash) > (self.name_written_at, self.name_from_hash.as_str());
        if self.name.is_empty() || newer {
            self.name = name.to_owned();
            self.name_written_at = written_at;
            self.name_from_hash = hash.to_owned();
        }
    }
}

/// Membership as gathered from the files: by uuid, plus whatever pre-uuid
/// files listed by name alone.
#[derive(Debug, Default)]
struct CollectionsFromFiles {
    by_uuid: HashMap<String, CollectionAgg>,
    by_legacy_name: HashMap<String, Vec<String>>,
}

/// Bring the database index back in line with the `.rlab` files on disk.
///
/// This is the reconciliation pass for everything the two-step writes elsewhere
/// can leave behind: photos whose file was written but never indexed (an import
/// killed part-way), rows whose file is gone (a delete that stopped after the
/// deletion), and metadata a file records that the index does not.
///
/// It is written to be safely re-runnable and to never leave the library worse
/// than it found it:
///
/// * Rows are refreshed in place, file by file, instead of the index being
///   emptied up front — so a rebuild that dies half-way leaves a library that
///   still mostly works, and running it again finishes the job.
/// * A session row that already exists keeps its name and `started_at`, so a
///   session the user renamed is not reverted to a generated date range.
/// * Rows for files that are no longer on disk are dropped only when the walk
///   actually found photos.  An empty `files/` directory is far more often an
///   unmounted volume than a library the user emptied, and wiping the index
///   over a mount failure is not recoverable from.
///
/// * An existing row is updated in place rather than deleted and re-inserted,
///   so it keeps the id its collection membership hangs off, and no photo is
///   ever momentarily without a row.
///
/// `cancel` is polled before each file so a rebuild over a large or slow
/// library can be stopped.  A cancelled run keeps the rows it refreshed and
/// leaves the rest of the reconciliation to the next full pass; see the
/// pruning step below for the one thing it must not do on partial evidence.
pub fn rebuild(
    library_root: &Path,
    db: &dyn LibraryDb,
    registry: &FormatRegistry,
    cancel: Arc<AtomicBool>,
    progress_cb: &dyn Fn(RebuildProgress),
) -> Result<RebuildOutcome> {
    let files_dir = library_root.join("files");
    if !files_dir.exists() {
        return Ok(RebuildOutcome::default());
    }

    // Collect all .rlab paths first so we can report total
    let rlab_paths: Vec<_> = WalkDir::new(&files_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file() && e.path().extension().is_some_and(|x| x == "rlab"))
        .map(|e| e.into_path())
        .collect();

    let total = rlab_paths.len();
    let mut errors: Vec<(std::path::PathBuf, String)> = Vec::new();
    let mut sessions: HashMap<String, SessionAgg> = HashMap::new();
    let mut collections = CollectionsFromFiles::default();
    let mut indexed: HashSet<String> = HashSet::with_capacity(total);

    let mut cancelled = false;
    let mut done = 0usize;

    for (i, rlab_file_path) in rlab_paths.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            cancelled = true;
            break;
        }
        progress_cb(RebuildProgress {
            total,
            done: i,
            current: rlab_file_path.clone(),
            errors: errors.clone(),
        });

        match reindex_one(
            library_root,
            db,
            registry,
            rlab_file_path,
            &mut sessions,
            &mut collections,
        ) {
            Ok(Some(hash)) => {
                indexed.insert(hash);
            }
            Ok(None) => {}
            Err(e) => errors.push((rlab_file_path.clone(), e.to_string())),
        }
        done = i + 1;
    }

    // Drop rows whose file is no longer there — a photo deleted outside the
    // app, or a delete that stopped after removing the file. A file that
    // failed to re-index is not evidence that it is gone, so anything that
    // errored above keeps its row.
    //
    // A cancelled walk never reached most of the library, so a row missing
    // from `indexed` says nothing about whether its file exists.  Pruning on
    // that evidence would delete photos that are still on disk, which is the
    // one thing this pass must never do.
    if !cancelled && !indexed.is_empty() {
        let unreadable: HashSet<&Path> = errors.iter().map(|(p, _)| p.as_path()).collect();
        for row in db.all_photos(SortOrder::default())? {
            if indexed.contains(&row.hash) {
                continue;
            }
            if unreadable.contains(files_dir.join(&row.lib_path).as_path()) {
                continue;
            }
            db.delete_photo(row.id)?;
        }
    }

    // Restore the session and collection rows.  Both run even for a cancelled
    // walk: they only add what the files already read described, so a partial
    // walk restores that much and the next run finishes the job.  Photos the
    // walk re-indexed keep the memberships they had either way, since their
    // rows are updated in place.
    //
    // Restore the session rows now that every photo's session membership and
    // import date are known.  `insert_session` is a no-op for a session that
    // still exists, which is what preserves a user's rename; the count comes
    // from the rows rather than from this run's tally, so a photo that kept its
    // row after failing to re-index is still counted.
    for (id, agg) in &sessions {
        db.insert_session(
            id,
            &format_session_name(agg.min_date, agg.max_date),
            agg.min_date,
            None,
        )?;
        db.update_session_count(id, db.session_photo_count(id)?)?;
    }
    db.delete_empty_sessions()?;

    restore_collections(db, collections)?;

    progress_cb(RebuildProgress {
        total,
        done,
        current: std::path::PathBuf::new(),
        errors: errors.clone(),
    });
    Ok(RebuildOutcome {
        total,
        done,
        errors,
        cancelled,
    })
}

/// Put collections and their membership back from what the files said.
///
/// The index owns collection names while it exists, so a collection this run
/// recognises by uuid keeps the name it has — otherwise every rebuild would
/// undo a rename by restoring the stale hints its member files still carry.
/// Hints name only the collections that have to be created from scratch, which
/// is the case this is really for: an index that was lost outright.
fn restore_collections(db: &dyn LibraryDb, found: CollectionsFromFiles) -> Result<()> {
    let mut known = db.all_collections()?;
    let photo_id =
        |hash: &str| -> Result<Option<PhotoId>> { Ok(db.photo_by_hash(hash)?.map(|row| row.id)) };

    // Settled in uuid order so that two collections wanting the same name are
    // resolved the same way every run.
    let mut by_uuid: Vec<(String, CollectionAgg)> = found.by_uuid.into_iter().collect();
    by_uuid.sort_by(|(a, _), (b, _)| a.cmp(b));

    for (uuid, agg) in by_uuid {
        let id = match known.iter().find(|row| row.uuid == uuid) {
            Some(row) => row.id,
            None => {
                let name = unique_name(&agg.name, &uuid, &known);
                let id = db.create_collection(&uuid, &name, unix_now())?;
                known.push(CollectionRow {
                    id,
                    uuid: uuid.clone(),
                    name,
                    created_at: unix_now(),
                });
                id
            }
        };
        let members: Vec<PhotoId> = agg
            .members
            .iter()
            .filter_map(|hash| photo_id(hash).transpose())
            .collect::<Result<_>>()?;
        db.add_to_collection(id, &members)?;
    }

    // Pre-uuid files name their collections and nothing else, so they join the
    // collection that goes by that name — the one a previous rebuild or the
    // hints above just created — and only start a new one if there is none.
    for (name, hashes) in found.by_legacy_name {
        let id = match known.iter().find(|row| row.name == name) {
            Some(row) => row.id,
            None => {
                let uuid = Uuid::new_v4().to_string();
                let id = db.create_collection(&uuid, &name, unix_now())?;
                known.push(CollectionRow {
                    id,
                    uuid,
                    name,
                    created_at: unix_now(),
                });
                id
            }
        };
        let members: Vec<PhotoId> = hashes
            .iter()
            .filter_map(|hash| photo_id(hash).transpose())
            .collect::<Result<_>>()?;
        db.add_to_collection(id, &members)?;
    }

    Ok(())
}

/// A name no existing collection is using, since the index requires them to be
/// distinct.
///
/// Two collections can genuinely want one name — a library rebuilt after its
/// index was lost, whose user made a second collection under a name the files
/// still remember. Suffixing keeps both, which is recoverable; failing the
/// rebuild is not.
fn unique_name(hint: &str, uuid: &str, known: &[CollectionRow]) -> String {
    let base = if hint.trim().is_empty() {
        // Nothing in any member file named it. The uuid at least tells the
        // user which collections are distinct, and can be renamed.
        format!("Collection {}", &uuid[..uuid.len().min(8)])
    } else {
        hint.to_owned()
    };
    let taken = |name: &str| known.iter().any(|row| row.name == name);
    if !taken(&base) {
        return base;
    }
    (2..)
        .map(|n| format!("{base} ({n})"))
        .find(|name| !taken(name))
        .unwrap_or(base)
}

/// Index one `.rlab`, replacing any row that already describes it.  Returns the
/// photo's hash, or `None` for a path the hash cannot be read from.
fn reindex_one(
    library_root: &Path,
    db: &dyn LibraryDb,
    registry: &FormatRegistry,
    rlab_file_path: &Path,
    sessions: &mut HashMap<String, SessionAgg>,
    collections: &mut CollectionsFromFiles,
) -> Result<Option<String>> {
    let rlab = RlabFile::read(rlab_file_path)?;

    // Derive hash from the path stem (files/ab/cd/{hash}.rlab)
    let hash = rlab_file_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_owned();

    if hash.is_empty() {
        return Ok(None);
    }

    let lmta = rlab.lmta.clone().unwrap_or_default();

    // Re-generate thumbnail if missing
    let tpath = thumb_path(library_root, &hash);
    if !tpath.exists()
        && let Ok(image) = registry.decode_bytes(&rlab.original_bytes, None)
        && let Ok(thumb) = generate_thumbnail(&image, 512)
    {
        write_thumbnail(&tpath, &thumb).ok();
    }

    let lib_path = rlab_file_path
        .strip_prefix(library_root.join("files"))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| format!("{}/{}/{}.rlab", &hash[0..2], &hash[2..4], hash));

    // Rewrite rather than skip: the file is the record, so whatever it says now
    // wins over whatever the index remembers.  An existing row is updated in
    // place instead of being deleted and re-inserted, which keeps its id — what
    // collection membership is keyed by — and leaves no window in which a photo
    // that is on disk has no row at all.
    let photo = NewPhoto {
        hash: &hash,
        lib_path: &lib_path,
        lmta: &lmta,
        width: rlab.meta.width,
        height: rlab.meta.height,
        stack_id: lmta
            .stack_peer_hash
            .as_deref()
            .map(|_| hash.as_str())
            .map(|_| {
                // Generate a stable stack_id from the sorted pair of hashes
                // (same logic used during import)
                hash.as_str()
            }),
        // Whether a photo carries edits lives in its virtual copies rather
        // than in the LMTA chunk, so it has to be read back off the file like
        // everything else here.  Left to the column default, a rebuild would
        // quietly empty the edited-only filter for the whole library.
        has_edits: rlab.has_edits(),
    };
    match db.photo_by_hash(&hash)? {
        Some(existing) => db.replace_photo(existing.id, photo)?,
        None => {
            db.insert_photo(photo)?;
        }
    }

    // Record collection membership for the pass that follows the walk. It
    // cannot be settled here: which name a collection ends up with is decided
    // by comparing files with each other, and a name-only membership from a
    // pre-uuid file can only be matched to a collection once every file that
    // might name it has been read.
    for held in &lmta.collection_refs {
        let agg = collections.by_uuid.entry(held.id.clone()).or_default();
        agg.offer_name(&held.name, rlab.meta.modified_at, &hash);
        agg.members.push(hash.clone());
    }
    for name in &lmta.legacy_collections {
        collections
            .by_legacy_name
            .entry(name.clone())
            .or_default()
            .push(hash.clone());
    }

    // Record session membership; the session rows themselves are restored in
    // one pass after all photos are indexed (see `rebuild`).
    if !lmta.import_session_id.is_empty() {
        let agg = sessions
            .entry(lmta.import_session_id.clone())
            .or_insert(SessionAgg {
                min_date: lmta.import_date,
                max_date: lmta.import_date,
            });
        agg.min_date = agg.min_date.min(lmta.import_date);
        agg.max_date = agg.max_date.max(lmta.import_date);
    }

    Ok(Some(hash))
}
