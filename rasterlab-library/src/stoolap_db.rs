use std::{collections::HashSet, path::Path};

use anyhow::{Context, Result};
use rasterlab_core::library_meta::{LibraryExif, LibraryMeta};
use stoolap::Value;
use stoolap::api::{Database, Transaction};
use uuid::Uuid;

use crate::{
    db_trait::{
        CollectionId, CollectionRow, ImportSessionRow, LibraryDb, NewPhoto, PhotoId, PhotoRow,
        RecentlyDeletedRow, SortOrder,
    },
    library::LibraryBusy,
    search::SearchFilter,
};

pub struct StoolapDb {
    db: Database,
}

impl StoolapDb {
    pub fn open(library_root: &Path) -> Result<Self> {
        let db_path = library_root.join("library.db");
        let dsn = format!("file://{}", db_path.display());
        // A held lock is not a broken library, so it keeps its own type all
        // the way up rather than arriving as one more opaque open failure.
        let db = Database::open(&dsn).map_err(|e| match e {
            stoolap::Error::DatabaseLocked => anyhow::Error::new(LibraryBusy),
            other => anyhow::Error::new(other).context("open library.db"),
        })?;
        Ok(Self { db })
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        let db = Database::open_in_memory().context("open in-memory db")?;
        Ok(Self { db })
    }

    /// A collection's photos, in the caller's order.
    ///
    /// Two tables is the limit: a third turns this into the join that comes
    /// back empty. See `search` on this type, and STOOLAP_BUG.md.
    fn collection_photos_sorted(
        &self,
        collection_id: CollectionId,
        sort: SortOrder,
    ) -> Result<Vec<PhotoRow>> {
        let rows = self.db.query(
            &format!(
                "{} JOIN collection_photos cp ON cp.photo_id = p.id
                 WHERE cp.collection_id = $1 {}",
                PHOTO_SELECT,
                sort_clause(sort)
            ),
            (collection_id,),
        )?;
        let mut result = Vec::new();
        for row in rows {
            let row = row.context("collection_photos row")?;
            if row_is_active(&row)? {
                result.push(row_to_photo(&row)?);
            }
        }
        Ok(result)
    }

    /// [`LibraryDb::collection_member_ids`] as a set, for the caller that is
    /// filtering rows it has already decided are visible.
    fn collection_member_id_set(&self, collection_id: CollectionId) -> Result<HashSet<PhotoId>> {
        Ok(LibraryDb::collection_member_ids(self, collection_id)?
            .into_iter()
            .collect())
    }

    /// Mint a uuid for every collection row that predates them.
    ///
    /// Runs on every open and is a no-op once done.  A collection without a
    /// uuid cannot be written into a photo's `.rlab`, so this has to happen
    /// before anything reads the table, not lazily.
    fn backfill_collection_uuids(&self) -> Result<()> {
        let rows = self
            .db
            .query("SELECT id, uuid FROM collections", ())
            .context("read collections for uuid backfill")?;
        let mut missing = Vec::new();
        for row in rows {
            let row = row.context("collection row")?;
            let has_uuid = row
                .get::<String>(1)
                .is_ok_and(|uuid| !uuid.trim().is_empty());
            if !has_uuid {
                missing.push(row.get::<i64>(0).context("collection id")?);
            }
        }
        for id in missing {
            self.db.execute(
                "UPDATE collections SET uuid = $1 WHERE id = $2",
                (Uuid::new_v4().to_string(), id),
            )?;
        }
        Ok(())
    }

    /// Run `f` in one transaction, committing only if it returns `Ok`.
    ///
    /// One photo is spread over `photos`, `exif`, `ratings`, `keywords`,
    /// `user_meta` and `collection_photos`, so any mutation that writes more
    /// than one statement can land half-applied: a photo row with no rating,
    /// EXIF belonging to nothing, a keyword list emptied for a rewrite that
    /// never arrived.  A [`Transaction`] rolls back when it is dropped
    /// uncommitted, so an early `?` inside `f` takes its partial writes with
    /// it and the error the caller sees means nothing changed.
    fn in_transaction<T>(&self, f: impl FnOnce(&mut Transaction) -> Result<T>) -> Result<T> {
        let mut tx = self.db.begin().context("begin transaction")?;
        let value = f(&mut tx)?;
        tx.commit().context("commit transaction")?;
        Ok(value)
    }
}

// ── Schema ────────────────────────────────────────────────────────────────────

const SCHEMA_STMTS: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS photos (
        id                INTEGER PRIMARY KEY AUTOINCREMENT,
        hash              TEXT    NOT NULL UNIQUE,
        lib_path          TEXT    NOT NULL,
        width             INTEGER,
        height            INTEGER,
        import_date       INTEGER,
        import_session    TEXT,
        capture_date      TEXT,
        original_filename TEXT,
        stack_id          TEXT,
        stack_is_primary  INTEGER NOT NULL DEFAULT 1,
        has_edits         INTEGER NOT NULL DEFAULT 0,
        protected         INTEGER NOT NULL DEFAULT 0,
        source_path       TEXT,
        source_size       INTEGER,
        source_mtime      INTEGER,
        deleted_at        INTEGER NOT NULL DEFAULT 0
    )",
    "CREATE TABLE IF NOT EXISTS exif (
        photo_id          INTEGER PRIMARY KEY,
        camera_make       TEXT,
        camera_model      TEXT,
        lens_make         TEXT,
        lens_model        TEXT,
        iso               INTEGER,
        shutter_sec       REAL,
        shutter_display   TEXT,
        aperture          REAL,
        focal_length      REAL,
        focal_length_35mm REAL,
        exposure_bias     REAL,
        exposure_program  TEXT,
        metering_mode     TEXT,
        flash             INTEGER,
        gps_lat           REAL,
        gps_lon           REAL,
        gps_alt           REAL
    )",
    "CREATE TABLE IF NOT EXISTS ratings (
        photo_id    INTEGER PRIMARY KEY,
        rating      INTEGER NOT NULL DEFAULT 0,
        color_label TEXT,
        flag        TEXT
    )",
    "CREATE TABLE IF NOT EXISTS keywords (
        photo_id INTEGER,
        keyword  TEXT NOT NULL
    )",
    "CREATE TABLE IF NOT EXISTS user_meta (
        photo_id         INTEGER PRIMARY KEY,
        caption          TEXT,
        copyright        TEXT,
        creator          TEXT,
        location_city    TEXT,
        location_country TEXT
    )",
    "CREATE TABLE IF NOT EXISTS import_sessions (
        rowid       INTEGER PRIMARY KEY AUTOINCREMENT,
        id          TEXT NOT NULL UNIQUE,
        name        TEXT NOT NULL,
        started_at  INTEGER,
        imported_at INTEGER,
        source_dir  TEXT,
        photo_count INTEGER NOT NULL DEFAULT 0
    )",
    "CREATE TABLE IF NOT EXISTS collections (
        id         INTEGER PRIMARY KEY AUTOINCREMENT,
        uuid       TEXT NOT NULL UNIQUE,
        name       TEXT NOT NULL UNIQUE,
        created_at INTEGER
    )",
    "CREATE TABLE IF NOT EXISTS collection_photos (
        collection_id INTEGER,
        photo_id      INTEGER,
        added_at      INTEGER
    )",
    "CREATE INDEX IF NOT EXISTS exif_aperture  ON exif(aperture)",
    "CREATE INDEX IF NOT EXISTS exif_iso       ON exif(iso)",
    "CREATE INDEX IF NOT EXISTS exif_shutter   ON exif(shutter_sec)",
    "CREATE INDEX IF NOT EXISTS photos_capture ON photos(capture_date)",
    "CREATE INDEX IF NOT EXISTS photos_import  ON photos(import_date, import_session)",
    "CREATE INDEX IF NOT EXISTS photos_stack   ON photos(stack_id)",
    "CREATE INDEX IF NOT EXISTS photos_source  ON photos(source_path)",
    "CREATE INDEX IF NOT EXISTS keywords_kw    ON keywords(keyword)",
    "CREATE INDEX IF NOT EXISTS kw_photo       ON keywords(photo_id)",
    "CREATE INDEX IF NOT EXISTS cp_coll        ON collection_photos(collection_id)",
    "CREATE INDEX IF NOT EXISTS cp_photo       ON collection_photos(photo_id)",
];

// ── Helper: sort ORDER BY clause ──────────────────────────────────────────────

fn sort_clause(sort: SortOrder) -> &'static str {
    match sort {
        SortOrder::CaptureDateDesc => "ORDER BY p.capture_date DESC, p.id DESC",
        SortOrder::CaptureDateAsc => "ORDER BY p.capture_date ASC, p.id ASC",
        SortOrder::ImportDateDesc => "ORDER BY p.import_date DESC, p.id DESC",
        SortOrder::RatingDesc => "ORDER BY COALESCE(r.rating, 0) DESC, p.id DESC",
        SortOrder::FilenameAsc => "ORDER BY p.original_filename ASC, p.id ASC",
    }
}

// ── Row helper ────────────────────────────────────────────────────────────────

fn row_to_photo(row: &stoolap::api::rows::ResultRow) -> Result<PhotoRow> {
    Ok(PhotoRow {
        id: row.get::<i64>(0).context("id")?,
        hash: row.get::<String>(1).context("hash")?,
        lib_path: row.get::<String>(2).context("lib_path")?,
        width: row.get::<i64>(3).context("width")? as u32,
        height: row.get::<i64>(4).context("height")? as u32,
        import_date: row.get::<i64>(5).context("import_date")? as u64,
        import_session: row.get::<String>(6).context("import_session")?,
        capture_date: row.get::<Option<String>>(7).context("capture_date")?,
        original_filename: row.get::<Option<String>>(8).context("original_filename")?,
        stack_id: row.get::<Option<String>>(9).context("stack_id")?,
        stack_is_primary: row.get::<i64>(10).context("stack_is_primary")? != 0,
        has_edits: row.get::<i64>(11).unwrap_or(0) != 0,
        protected: row.get::<i64>(12).unwrap_or(0) != 0,
    })
}

const PHOTO_SELECT: &str = "SELECT p.id, p.hash, p.lib_path, p.width, p.height,
            p.import_date, p.import_session, p.capture_date,
            p.original_filename, p.stack_id, p.stack_is_primary, p.has_edits,
            p.protected, p.deleted_at
     FROM photos p";

fn row_is_active(row: &stoolap::api::rows::ResultRow) -> Result<bool> {
    Ok(row.get::<i64>(13).context("deleted_at")? == 0)
}

// ── LibraryDb impl ────────────────────────────────────────────────────────────

impl LibraryDb for StoolapDb {
    fn init(&self) -> Result<()> {
        for stmt in SCHEMA_STMTS {
            self.db
                .execute(stmt, ())
                .with_context(|| format!("schema: {}", &stmt[..40]))?;
        }
        // Migration: add has_edits to existing databases (ignore error if already present).
        let _ = self.db.execute(
            "ALTER TABLE photos ADD COLUMN has_edits INTEGER NOT NULL DEFAULT 0",
            (),
        );
        let _ = self
            .db
            .execute("ALTER TABLE exif ADD COLUMN lens_make TEXT", ());
        // Migration: add protected to existing databases (ignore error if present).
        let _ = self.db.execute(
            "ALTER TABLE photos ADD COLUMN protected INTEGER NOT NULL DEFAULT 0",
            (),
        );
        // Migration: collections used to be identified by name.  The column is
        // added without the UNIQUE the fresh schema carries — the rows that
        // exist have no uuid yet — and every one of them is given one below.
        let _ = self
            .db
            .execute("ALTER TABLE collections ADD COLUMN uuid TEXT", ());
        self.backfill_collection_uuids()
            .context("give existing collections a uuid")?;
        // Migration: source fingerprint columns for fast import resume.
        let _ = self
            .db
            .execute("ALTER TABLE photos ADD COLUMN source_path TEXT", ());
        let _ = self
            .db
            .execute("ALTER TABLE photos ADD COLUMN source_size INTEGER", ());
        let _ = self
            .db
            .execute("ALTER TABLE photos ADD COLUMN source_mtime INTEGER", ());
        // Migration: when the import actually ran, as distinct from the capture
        // date a folder import back-dates the session to.  NULL in existing
        // rows, which readers treat as "unknown".
        let _ = self.db.execute(
            "ALTER TABLE import_sessions ADD COLUMN imported_at INTEGER",
            (),
        );
        // Migration: library-owned Recently Deleted state. Zero means active;
        // positive values are deletion timestamps.
        let _ = self.db.execute(
            "ALTER TABLE photos ADD COLUMN deleted_at INTEGER NOT NULL DEFAULT 0",
            (),
        );
        let _ = self.db.execute(
            "UPDATE photos SET deleted_at=0 WHERE deleted_at IS NULL",
            (),
        );
        Ok(())
    }

    // ── Photos ────────────────────────────────────────────────────────────

    fn insert_photo(&self, photo: NewPhoto<'_>) -> Result<PhotoId> {
        self.in_transaction(|tx| insert_photo_tx(tx, photo))
    }

    fn insert_photo_with_collection(
        &self,
        photo: NewPhoto<'_>,
        collection_id: Option<CollectionId>,
    ) -> Result<PhotoId> {
        self.in_transaction(|tx| {
            let photo_id = insert_photo_tx(tx, photo)?;
            if let Some(collection_id) = collection_id {
                // A freshly allocated photo id cannot already be a member, so
                // unlike the general selection API this needs no collection-
                // wide read to protect against duplicate membership rows.
                tx.execute(
                    "INSERT INTO collection_photos
                     (collection_id, photo_id, added_at) VALUES ($1,$2,$3)",
                    (collection_id, photo_id, unix_now() as i64),
                )?;
            }
            Ok(photo_id)
        })
    }

    fn replace_photo(&self, photo_id: PhotoId, photo: NewPhoto<'_>) -> Result<()> {
        self.in_transaction(|tx| replace_photo_tx(tx, photo_id, photo))
    }

    fn photo_by_hash(&self, hash: &str) -> Result<Option<PhotoRow>> {
        let mut rows = self
            .db
            .query(&format!("{} WHERE p.hash = $1", PHOTO_SELECT), (hash,))?;
        if let Some(row) = rows.next() {
            let row = row.context("photo_by_hash row")?;
            return Ok(Some(row_to_photo(&row)?));
        }
        Ok(None)
    }

    fn source_already_imported(
        &self,
        source_path: &str,
        source_size: u64,
        source_mtime_secs: i64,
    ) -> Result<bool> {
        let mut rows = self.db.query(
            "SELECT 1 FROM photos
             WHERE source_path = $1 AND source_size = $2 AND source_mtime = $3",
            (source_path, source_size as i64, source_mtime_secs),
        )?;
        Ok(rows.next().is_some())
    }

    fn update_lmta(&self, photo_id: PhotoId, lmta: &LibraryMeta) -> Result<()> {
        self.in_transaction(|tx| update_lmta_tx(tx, photo_id, lmta))
    }

    fn set_has_edits(&self, photo_id: PhotoId, has_edits: bool) -> Result<()> {
        self.db.execute(
            "UPDATE photos SET has_edits=$1 WHERE id=$2",
            (has_edits as i64, photo_id),
        )?;
        Ok(())
    }

    fn set_protected(&self, photo_id: PhotoId, protected: bool) -> Result<()> {
        self.db.execute(
            "UPDATE photos SET protected=$1 WHERE id=$2",
            (protected as i64, photo_id),
        )?;
        Ok(())
    }

    /// One transaction for the whole batch: a rating applied to a selection of
    /// forty photos either lands on all of them or on none, so a failure never
    /// leaves the user guessing which half took.
    fn update_lmta_batch(&self, updates: &[(PhotoId, LibraryMeta)]) -> Result<()> {
        self.in_transaction(|tx| {
            for (id, lmta) in updates {
                update_lmta_tx(tx, *id, lmta)?;
            }
            Ok(())
        })
    }

    fn mark_photo_deleted(&self, photo_id: PhotoId, deleted_at: u64) -> Result<()> {
        self.in_transaction(|tx| set_photo_deleted_tx(tx, photo_id, Some(deleted_at)))
    }

    fn restore_photo(&self, photo_id: PhotoId) -> Result<()> {
        self.in_transaction(|tx| set_photo_deleted_tx(tx, photo_id, None))
    }

    fn delete_photo(&self, photo_id: PhotoId) -> Result<()> {
        self.in_transaction(|tx| delete_photo_tx(tx, photo_id))
    }

    fn all_photos(&self, sort: SortOrder) -> Result<Vec<PhotoRow>> {
        let sql = format!(
            "{} LEFT JOIN ratings r ON r.photo_id = p.id {}",
            PHOTO_SELECT,
            sort_clause(sort)
        );
        let rows = self.db.query(&sql, ())?;
        let mut result = Vec::new();
        for row in rows {
            let row = row.context("all_photos row")?;
            if row_is_active(&row)? {
                result.push(row_to_photo(&row)?);
            }
        }
        Ok(result)
    }

    fn recently_deleted(&self) -> Result<Vec<RecentlyDeletedRow>> {
        let rows = self.db.query(
            &format!(
                "{} WHERE p.deleted_at > 0 ORDER BY p.deleted_at DESC, p.id DESC",
                PHOTO_SELECT
            ),
            (),
        )?;
        let mut result = Vec::new();
        for row in rows {
            let row = row.context("recently_deleted row")?;
            result.push(RecentlyDeletedRow {
                photo: row_to_photo(&row)?,
                deleted_at: row.get::<i64>(13).context("deleted_at")? as u64,
            });
        }
        Ok(result)
    }

    // ── Search ────────────────────────────────────────────────────────────

    fn search(&self, filter: &SearchFilter, sort: SortOrder) -> Result<Vec<PhotoRow>> {
        // A collection scope cannot go into the statement below. Stoolap
        // returns *no rows at all*, silently, from a join of three or more
        // tables when the join column of one of them carries an index and the
        // rows are read back in a later session. `collection_photos(photo_id)`
        // is indexed and `photo_id` is what the join is on, so putting that
        // table in with the metadata tables empties the whole result.
        // STOOLAP_BUG.md has the reproduction. Membership is resolved with a
        // query of its own instead, and applied to the rows here.
        let members = match filter.collection_id {
            Some(id) => Some(self.collection_member_id_set(id)?),
            None => None,
        };

        // A collection and nothing else is what clicking one in the sidebar
        // asks for, and a join on its own is a shape the database does handle.
        // Worth its own path: it reads a collection's handful of rows instead
        // of every photo in the library.
        let others = SearchFilter {
            collection_id: None,
            ..filter.clone()
        };
        if let Some(id) = filter.collection_id
            && others.is_empty()
        {
            return self.collection_photos_sorted(id, sort);
        }

        let mut conditions: Vec<String> = Vec::new();
        let mut params: Vec<Value> = Vec::new();

        macro_rules! push {
            ($cond:expr, $val:expr) => {{
                let n = params.len() + 1;
                conditions.push($cond.replace("{}", &format!("${}", n)));
                params.push($val);
            }};
            ($cond:expr, $v1:expr, $v2:expr) => {{
                let n1 = params.len() + 1;
                let n2 = n1 + 1;
                let c = $cond.replacen("{}", &format!("${}", n1), 1).replacen(
                    "{}",
                    &format!("${}", n2),
                    1,
                );
                conditions.push(c);
                params.push($v1);
                params.push($v2);
            }};
            ($cond:expr, $v1:expr, $v2:expr, $v3:expr) => {{
                let n1 = params.len() + 1;
                let n2 = n1 + 1;
                let n3 = n2 + 1;
                let c = $cond
                    .replacen("{}", &format!("${}", n1), 1)
                    .replacen("{}", &format!("${}", n2), 1)
                    .replacen("{}", &format!("${}", n3), 1);
                conditions.push(c);
                params.push($v1);
                params.push($v2);
                params.push($v3);
            }};
            ($cond:expr, $v1:expr, $v2:expr, $v3:expr, $v4:expr) => {{
                let n1 = params.len() + 1;
                let n2 = n1 + 1;
                let n3 = n2 + 1;
                let n4 = n3 + 1;
                let c = $cond
                    .replacen("{}", &format!("${}", n1), 1)
                    .replacen("{}", &format!("${}", n2), 1)
                    .replacen("{}", &format!("${}", n3), 1)
                    .replacen("{}", &format!("${}", n4), 1);
                conditions.push(c);
                params.push($v1);
                params.push($v2);
                params.push($v3);
                params.push($v4);
            }};
        }

        if let Some(ref text) = filter.text {
            let pat = format!("%{}%", text);
            push!(
                "(p.original_filename ILIKE {} OR p.source_path ILIKE {} OR um.caption ILIKE {} OR k.keyword ILIKE {})",
                Value::text(pat.clone()),
                Value::text(pat.clone()),
                Value::text(pat.clone()),
                Value::text(pat)
            );
        }
        if let Some(min) = filter.rating_min {
            push!("COALESCE(r.rating, 0) >= {}", Value::integer(min as i64));
        }
        if let Some(ref flag) = filter.flag {
            push!("r.flag = {}", Value::text(flag.clone()));
        }
        if let Some(ref range) = filter.aperture {
            push!(
                "e.aperture BETWEEN {} AND {}",
                Value::float(*range.start() as f64),
                Value::float(*range.end() as f64)
            );
        }
        if let Some(ref range) = filter.iso {
            push!(
                "e.iso BETWEEN {} AND {}",
                Value::integer(*range.start() as i64),
                Value::integer(*range.end() as i64)
            );
        }
        if let Some(max_sec) = filter.shutter_max_sec {
            push!("e.shutter_sec <= {}", Value::float(max_sec));
        }
        if let Some(min_sec) = filter.shutter_min_sec {
            push!("e.shutter_sec >= {}", Value::float(min_sec));
        }
        if let Some(ref cam) = filter.camera_model {
            push!("e.camera_model ILIKE {}", Value::text(format!("%{}%", cam)));
        }
        if let Some(ref lens) = filter.lens_model {
            push!("e.lens_model ILIKE {}", Value::text(format!("%{}%", lens)));
        }
        if let Some(ref from) = filter.capture_date_from {
            push!("p.capture_date >= {}", Value::text(from.clone()));
        }
        if let Some(ref to) = filter.capture_date_to {
            push!("p.capture_date <= {}", Value::text(to.clone()));
        }
        if let Some(ref session) = filter.import_session {
            push!("p.import_session = {}", Value::text(session.clone()));
        }
        // Orientation-agnostic dimension bounds.  `max(width, height)` is not
        // portable SQL here, so the long/short-edge comparison is spelled out
        // as the two orientations it can take; the pair is equivalent to
        // "long edge within the limit's long edge, short within its short".
        if let Some(res) = filter.resolution_max {
            push!(
                "((p.width <= {} AND p.height <= {}) OR (p.width <= {} AND p.height <= {}))",
                Value::integer(res.long_edge as i64),
                Value::integer(res.short_edge as i64),
                Value::integer(res.short_edge as i64),
                Value::integer(res.long_edge as i64)
            );
        }
        if let Some(res) = filter.resolution_min {
            push!(
                "((p.width >= {} AND p.height >= {}) OR (p.width >= {} AND p.height >= {}))",
                Value::integer(res.long_edge as i64),
                Value::integer(res.short_edge as i64),
                Value::integer(res.short_edge as i64),
                Value::integer(res.long_edge as i64)
            );
        }
        if let Some(ref label) = filter.color_label {
            push!("r.color_label = {}", Value::text(label.clone()));
        }
        if filter.has_edits_only {
            conditions.push("p.has_edits = 1".to_string());
        }

        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conditions.join(" AND "))
        };

        let sql = format!(
            "SELECT DISTINCT p.id, p.hash, p.lib_path, p.width, p.height,
                    p.import_date, p.import_session, p.capture_date,
                    p.original_filename, p.stack_id, p.stack_is_primary, p.has_edits,
                    p.protected, p.deleted_at
             FROM photos p
             LEFT JOIN exif       e  ON e.photo_id  = p.id
             LEFT JOIN ratings    r  ON r.photo_id  = p.id
             LEFT JOIN user_meta  um ON um.photo_id = p.id
             LEFT JOIN keywords   k  ON k.photo_id  = p.id
             {} {}",
            where_clause,
            sort_clause(sort)
        );

        let rows = self.db.query(&sql, params.as_slice())?;
        let mut result = Vec::new();
        for row in rows {
            let row = row.context("search row")?;
            if !row_is_active(&row)? {
                continue;
            }
            let photo = row_to_photo(&row)?;
            if members.as_ref().is_none_or(|ids| ids.contains(&photo.id)) {
                result.push(photo);
            }
        }
        Ok(result)
    }

    fn photos_by_session(&self, session_id: &str) -> Result<Vec<PhotoRow>> {
        let rows = self.db.query(
            &format!(
                "{} WHERE p.import_session = $1
                 ORDER BY p.capture_date ASC, p.id ASC",
                PHOTO_SELECT
            ),
            (session_id,),
        )?;
        let mut result = Vec::new();
        for row in rows {
            let row = row.context("photos_by_session row")?;
            if row_is_active(&row)? {
                result.push(row_to_photo(&row)?);
            }
        }
        Ok(result)
    }

    fn collection_photos(&self, collection_id: CollectionId) -> Result<Vec<PhotoRow>> {
        self.collection_photos_sorted(collection_id, SortOrder::CaptureDateDesc)
    }

    // ── Import sessions ───────────────────────────────────────────────────

    fn insert_session(
        &self,
        id: &str,
        name: &str,
        started_at: u64,
        source_dir: Option<&str>,
    ) -> Result<()> {
        self.db
            .execute(
                "INSERT INTO import_sessions
             (id, name, started_at, source_dir, photo_count) VALUES ($1,$2,$3,$4,0) ON CONFLICT DO NOTHING",
                (id, name, started_at as i64, source_dir),
            )
            .context("insert_session")?;
        Ok(())
    }

    fn rename_session(&self, id: &str, name: &str) -> Result<()> {
        self.db
            .execute("UPDATE import_sessions SET name=$1 WHERE id=$2", (name, id))?;
        Ok(())
    }

    fn mark_session_imported(&self, id: &str, at: u64) -> Result<()> {
        self.db.execute(
            "UPDATE import_sessions SET imported_at=$1 WHERE id=$2",
            (at as i64, id),
        )?;
        Ok(())
    }

    fn update_session_count(&self, id: &str, count: i64) -> Result<()> {
        self.db.execute(
            "UPDATE import_sessions SET photo_count=$1 WHERE id=$2",
            (count, id),
        )?;
        Ok(())
    }

    fn session_photo_count(&self, session_id: &str) -> Result<i64> {
        self.db
            .query_one(
                "SELECT COUNT(*) FROM photos
                 WHERE deleted_at = 0 AND import_session = $1",
                (session_id,),
            )
            .context("count session photos")
    }

    fn active_photo_count(&self) -> Result<i64> {
        self.db
            .query_one("SELECT COUNT(*) FROM photos WHERE deleted_at = 0", ())
            .context("count active photos")
    }

    fn delete_empty_sessions(&self) -> Result<usize> {
        // Counted per session rather than with a `NOT IN` subquery: sessions
        // number in the hundreds at most, and an anti-join against a column
        // that can be NULL is the kind of thing that silently deletes
        // everything.
        let rows = self.db.query("SELECT id FROM import_sessions", ())?;
        let mut stale = Vec::new();
        for row in rows {
            let id = row.context("empty-session candidate")?.get::<String>(0)?;
            let count: i64 = self.db.query_one(
                "SELECT COUNT(*) FROM photos WHERE import_session = $1",
                (id.as_str(),),
            )?;
            if count == 0 {
                stale.push(id);
            }
        }
        self.in_transaction(|tx| {
            for id in &stale {
                tx.execute("DELETE FROM import_sessions WHERE id = $1", (id.as_str(),))?;
            }
            Ok(stale.len())
        })
    }

    fn all_sessions(&self) -> Result<Vec<ImportSessionRow>> {
        let rows = self.db.query(
            "SELECT id, name, started_at, imported_at, source_dir, photo_count
             FROM import_sessions
             WHERE photo_count > 0
             ORDER BY started_at DESC",
            (),
        )?;
        let mut result = Vec::new();
        for row in rows {
            let row = row.context("all_sessions row")?;
            result.push(ImportSessionRow {
                id: row.get::<String>(0)?,
                name: row.get::<String>(1)?,
                started_at: row.get::<i64>(2)? as u64,
                imported_at: row.get::<Option<i64>>(3)?.map(|t| t as u64),
                source_dir: row.get::<Option<String>>(4)?,
                photo_count: row.get::<i64>(5)?,
            });
        }
        Ok(result)
    }

    // ── Stacks ────────────────────────────────────────────────────────────

    fn photos_in_stack(&self, stack_id: &str) -> Result<Vec<PhotoRow>> {
        let rows = self.db.query(
            &format!(
                "{} WHERE p.stack_id = $1
                 ORDER BY p.stack_is_primary DESC, p.id ASC",
                PHOTO_SELECT
            ),
            (stack_id,),
        )?;
        let mut result = Vec::new();
        for row in rows {
            let row = row.context("photos_in_stack row")?;
            if row_is_active(&row)? {
                result.push(row_to_photo(&row)?);
            }
        }
        Ok(result)
    }

    // ── Collections ───────────────────────────────────────────────────────

    fn create_collection(&self, uuid: &str, name: &str, created_at: u64) -> Result<CollectionId> {
        let id: i64 = self.db.query_one(
            "INSERT INTO collections (uuid, name, created_at) VALUES ($1,$2,$3) RETURNING id",
            (uuid, name, created_at as i64),
        )?;
        Ok(id)
    }

    fn rename_collection(&self, id: CollectionId, name: &str) -> Result<()> {
        self.db
            .execute("UPDATE collections SET name=$1 WHERE id=$2", (name, id))?;
        Ok(())
    }

    fn delete_collection(&self, id: CollectionId) -> Result<()> {
        self.in_transaction(|tx| {
            tx.execute(
                "DELETE FROM collection_photos WHERE collection_id=$1",
                (id,),
            )?;
            tx.execute("DELETE FROM collections WHERE id=$1", (id,))?;
            Ok(())
        })
    }

    fn all_collections(&self) -> Result<Vec<CollectionRow>> {
        let rows = self.db.query(
            "SELECT id, uuid, name, created_at FROM collections ORDER BY name ASC",
            (),
        )?;
        let mut result = Vec::new();
        for row in rows {
            let row = row.context("all_collections row")?;
            result.push(CollectionRow {
                id: row.get::<i64>(0)?,
                uuid: row.get::<String>(1)?,
                name: row.get::<String>(2)?,
                created_at: row.get::<i64>(3)? as u64,
            });
        }
        Ok(result)
    }

    fn collection_member_ids(&self, collection_id: CollectionId) -> Result<Vec<PhotoId>> {
        let rows = self.db.query(
            "SELECT photo_id FROM collection_photos WHERE collection_id = $1",
            (collection_id,),
        )?;
        let mut ids = Vec::new();
        for row in rows {
            ids.push(row.context("collection member row")?.get::<i64>(0)?);
        }
        Ok(ids)
    }

    fn collection_memberships(&self) -> Result<Vec<(CollectionId, PhotoId)>> {
        let rows = self.db.query(
            "SELECT cp.collection_id, cp.photo_id, p.deleted_at
             FROM collection_photos cp JOIN photos p ON p.id = cp.photo_id",
            (),
        )?;
        let mut result = Vec::new();
        for row in rows {
            let row = row.context("collection_memberships row")?;
            if row.get::<i64>(2).context("deleted_at")? != 0 {
                continue;
            }
            result.push((
                row.get::<i64>(0).context("collection_id")?,
                row.get::<i64>(1).context("photo_id")?,
            ));
        }
        Ok(result)
    }

    fn add_to_collection(&self, collection_id: CollectionId, photo_ids: &[PhotoId]) -> Result<()> {
        let now = unix_now() as i64;
        self.in_transaction(|tx| {
            // `collection_photos` has no key to conflict on, so re-adding a
            // photo would insert a second membership row and the collection
            // would list it twice.  Skipping the photos already there is what
            // makes adding a selection that partly overlaps the collection —
            // the common case — do the obvious thing.
            let mut existing = HashSet::new();
            let rows = tx.query(
                "SELECT photo_id FROM collection_photos WHERE collection_id = $1",
                (collection_id,),
            )?;
            for row in rows {
                existing.insert(row.context("collection member row")?.get::<i64>(0)?);
            }
            for &pid in photo_ids {
                if !existing.insert(pid) {
                    continue;
                }
                tx.execute(
                    "INSERT INTO collection_photos
                     (collection_id, photo_id, added_at) VALUES ($1,$2,$3)",
                    (collection_id, pid, now),
                )?;
            }
            Ok(())
        })
    }

    fn remove_from_collection(
        &self,
        collection_id: CollectionId,
        photo_ids: &[PhotoId],
    ) -> Result<()> {
        self.in_transaction(|tx| {
            for &pid in photo_ids {
                tx.execute(
                    "DELETE FROM collection_photos WHERE collection_id=$1 AND photo_id=$2",
                    (collection_id, pid),
                )?;
            }
            Ok(())
        })
    }
}

// ── Transaction bodies ────────────────────────────────────────────────────────
//
// Written against `&mut Transaction` rather than `&self` so a caller can put
// several of them in one transaction — `update_lmta_batch` does exactly that.

/// Insert a photo row and every row that hangs off it.
#[allow(clippy::too_many_arguments)]
fn insert_photo_tx(tx: &mut Transaction, photo: NewPhoto<'_>) -> Result<PhotoId> {
    let NewPhoto {
        hash,
        lib_path,
        lmta,
        width,
        height,
        stack_id,
        has_edits,
    } = photo;
    let capture_date: Option<&str> = lmta.exif.as_ref().and_then(|e| e.capture_date.as_deref());

    let opt_text = |s: Option<&str>| -> Value { s.map_or_else(Value::null_unknown, Value::text) };
    let opt_int = |v: Option<i64>| -> Value { v.map_or_else(Value::null_unknown, Value::integer) };

    // 15 params exceeds the 12-tuple Params impl limit; use Vec<Value>.
    let photo_params: Vec<Value> = vec![
        Value::text(hash),
        Value::text(lib_path),
        Value::integer(width as i64),
        Value::integer(height as i64),
        Value::integer(lmta.import_date as i64),
        Value::text(lmta.import_session_id.as_str()),
        opt_text(capture_date),
        opt_text(lmta.original_filename.as_deref()),
        opt_text(stack_id),
        Value::integer(if lmta.stack_is_primary { 1 } else { 0 }),
        Value::integer(if lmta.protected { 1 } else { 0 }),
        opt_text(lmta.source_path.as_deref()),
        opt_int(lmta.source_size.map(|s| s as i64)),
        opt_int(lmta.source_mtime.map(|t| t.secs)),
        Value::integer(has_edits as i64),
    ];
    let photo_id: i64 = tx
        .query_one(
            "INSERT INTO photos
             (hash, lib_path, width, height, import_date, import_session,
              capture_date, original_filename, stack_id, stack_is_primary, protected,
              source_path, source_size, source_mtime, has_edits)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15)
             RETURNING id",
            photo_params,
        )
        .context("insert photo")?;

    write_photo_dependents_tx(tx, photo_id, lmta)?;

    // Collection membership is deliberately not restored here.  A photo's
    // `.rlab` names its collections only as a hint, and which name wins is a
    // question about the whole library rather than one photo, so
    // `reconstruct::rebuild` settles it in a pass of its own once every file
    // has been read.

    Ok(photo_id)
}

/// Rewrite one photo's row in place from what its `.rlab` says, keeping its id
/// and therefore its collection membership. See [`LibraryDb::replace_photo`].
fn replace_photo_tx(tx: &mut Transaction, photo_id: PhotoId, photo: NewPhoto<'_>) -> Result<()> {
    let NewPhoto {
        hash: _,
        lib_path,
        lmta,
        width,
        height,
        stack_id,
        has_edits,
    } = photo;
    let capture_date: Option<&str> = lmta.exif.as_ref().and_then(|e| e.capture_date.as_deref());

    let opt_text = |s: Option<&str>| -> Value { s.map_or_else(Value::null_unknown, Value::text) };
    let opt_int = |v: Option<i64>| -> Value { v.map_or_else(Value::null_unknown, Value::integer) };

    // `deleted_at` is reset along with everything else: the caller found this
    // row by the hash of a file sitting in `files/`, and a photo whose file is
    // there is an active one however the index came to think otherwise.
    let params: Vec<Value> = vec![
        Value::text(lib_path),
        Value::integer(width as i64),
        Value::integer(height as i64),
        Value::integer(lmta.import_date as i64),
        Value::text(lmta.import_session_id.as_str()),
        opt_text(capture_date),
        opt_text(lmta.original_filename.as_deref()),
        opt_text(stack_id),
        Value::integer(if lmta.stack_is_primary { 1 } else { 0 }),
        Value::integer(if lmta.protected { 1 } else { 0 }),
        opt_text(lmta.source_path.as_deref()),
        opt_int(lmta.source_size.map(|s| s as i64)),
        opt_int(lmta.source_mtime.map(|t| t.secs)),
        Value::integer(has_edits as i64),
        Value::integer(photo_id),
    ];
    // No row means the caller is working from an id the index no longer has;
    // writing the dependent rows anyway would attach them to nothing.
    if tx
        .execute(
            "UPDATE photos SET
             lib_path=$1, width=$2, height=$3, import_date=$4, import_session=$5,
             capture_date=$6, original_filename=$7, stack_id=$8, stack_is_primary=$9,
             protected=$10, source_path=$11, source_size=$12, source_mtime=$13,
             has_edits=$14, deleted_at=0
         WHERE id=$15",
            params,
        )
        .context("update photo")?
        == 0
    {
        anyhow::bail!("photo {photo_id} is not in the index");
    }

    write_photo_dependents_tx(tx, photo_id, lmta)?;

    // Keep the sidebar's cached count in step with the row just written, the
    // way every other photo mutation here does. A photo whose file moved it to
    // another session leaves the old session's count to the pass that ends a
    // rebuild.
    tx.execute(
        "UPDATE import_sessions
         SET photo_count = (
             SELECT COUNT(*) FROM photos
             WHERE import_session = import_sessions.id AND deleted_at = 0
         )
         WHERE id = (SELECT import_session FROM photos WHERE id = $1)",
        (photo_id,),
    )?;

    Ok(())
}

/// Write the rows that hang off a photo — EXIF, rating, keywords, user
/// metadata — from what its `.rlab` says.
///
/// Each row is updated where it exists and inserted where it does not, rather
/// than being cleared and rewritten: stoolap rejects an insert of a primary key
/// that was deleted earlier in the same transaction, and `exif`, `ratings` and
/// `user_meta` are all keyed by `photo_id`.  That is also what lets inserting a
/// photo and rewriting one in place share this.
fn write_photo_dependents_tx(
    tx: &mut Transaction,
    photo_id: PhotoId,
    lmta: &LibraryMeta,
) -> Result<()> {
    write_exif_tx(tx, photo_id, lmta.exif.as_ref())?;

    let rating_params = vec![
        Value::integer(photo_id),
        Value::integer(lmta.rating as i64),
        lmta.color_label
            .as_deref()
            .map_or_else(Value::null_unknown, Value::text),
        lmta.flag
            .as_deref()
            .map_or_else(Value::null_unknown, Value::text),
    ];
    if tx.execute(
        "UPDATE ratings SET rating=$2, color_label=$3, flag=$4 WHERE photo_id=$1",
        rating_params.clone(),
    )? == 0
    {
        tx.execute(
            "INSERT INTO ratings (photo_id, rating, color_label, flag) VALUES ($1,$2,$3,$4)",
            rating_params,
        )
        .context("insert rating")?;
    }

    let opt_text = |s: Option<&str>| -> Value { s.map_or_else(Value::null_unknown, Value::text) };
    let user_meta_params = vec![
        Value::integer(photo_id),
        opt_text(lmta.caption.as_deref()),
        opt_text(lmta.copyright.as_deref()),
        opt_text(lmta.creator.as_deref()),
        opt_text(lmta.location_city.as_deref()),
        opt_text(lmta.location_country.as_deref()),
    ];
    if tx.execute(
        "UPDATE user_meta SET caption=$2, copyright=$3, creator=$4,
             location_city=$5, location_country=$6
         WHERE photo_id=$1",
        user_meta_params.clone(),
    )? == 0
    {
        tx.execute(
            "INSERT INTO user_meta
             (photo_id, caption, copyright, creator, location_city, location_country)
             VALUES ($1,$2,$3,$4,$5,$6)",
            user_meta_params,
        )
        .context("insert user_meta")?;
    }

    // Keywords have no key to update against, so the list is replaced whole.
    // Both statements are in this transaction, so a failure between them cannot
    // leave the photo with none.
    tx.execute("DELETE FROM keywords WHERE photo_id = $1", (photo_id,))?;
    for kw in &lmta.keywords {
        tx.execute(
            "INSERT INTO keywords (photo_id, keyword) VALUES ($1, $2)",
            (photo_id, kw.as_str()),
        )
        .context("insert keyword")?;
    }

    Ok(())
}

/// Write a photo's EXIF row, or drop it for a file that carries no snapshot.
///
/// The parameters are ordered `photo_id` first so one vector serves both the
/// update and the insert.
fn write_exif_tx(
    tx: &mut Transaction,
    photo_id: PhotoId,
    exif: Option<&LibraryExif>,
) -> Result<()> {
    let Some(exif) = exif else {
        tx.execute("DELETE FROM exif WHERE photo_id = $1", (photo_id,))?;
        return Ok(());
    };

    let opt_text = |s: Option<&str>| -> Value { s.map_or_else(Value::null_unknown, Value::text) };
    let opt_int = |v: Option<i64>| -> Value { v.map_or_else(Value::null_unknown, Value::integer) };
    let opt_f64 = |v: Option<f64>| -> Value { v.map_or_else(Value::null_unknown, Value::float) };

    // 18 params exceeds the 12-tuple Params impl limit; use Vec<Value>.
    let params: Vec<Value> = vec![
        Value::integer(photo_id),
        opt_text(exif.camera_make.as_deref()),
        opt_text(exif.camera_model.as_deref()),
        opt_text(exif.lens_make.as_deref()),
        opt_text(exif.lens_model.as_deref()),
        opt_int(exif.iso.map(|v| v as i64)),
        opt_f64(exif.shutter_sec),
        opt_text(exif.shutter_display.as_deref()),
        opt_f64(exif.aperture.map(|v| v as f64)),
        opt_f64(exif.focal_length.map(|v| v as f64)),
        opt_f64(exif.focal_length_35mm.map(|v| v as f64)),
        opt_f64(exif.exposure_bias.map(|v| v as f64)),
        opt_text(exif.exposure_program.as_deref()),
        opt_text(exif.metering_mode.as_deref()),
        opt_int(exif.flash.map(|v| if v { 1i64 } else { 0i64 })),
        opt_f64(exif.gps_lat),
        opt_f64(exif.gps_lon),
        opt_f64(exif.gps_alt.map(|v| v as f64)),
    ];

    if tx.execute(
        "UPDATE exif SET
             camera_make=$2, camera_model=$3, lens_make=$4, lens_model=$5, iso=$6,
             shutter_sec=$7, shutter_display=$8, aperture=$9, focal_length=$10,
             focal_length_35mm=$11, exposure_bias=$12, exposure_program=$13,
             metering_mode=$14, flash=$15, gps_lat=$16, gps_lon=$17, gps_alt=$18
         WHERE photo_id=$1",
        params.clone(),
    )? == 0
    {
        tx.execute(
            "INSERT INTO exif
             (photo_id, camera_make, camera_model, lens_make, lens_model, iso,
              shutter_sec, shutter_display, aperture, focal_length,
              focal_length_35mm, exposure_bias, exposure_program,
              metering_mode, flash, gps_lat, gps_lon, gps_alt)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18)",
            params,
        )
        .context("insert exif")?;
    }
    Ok(())
}

/// Rewrite the mutable metadata of one photo: rating, user fields, keywords.
fn update_lmta_tx(tx: &mut Transaction, photo_id: PhotoId, lmta: &LibraryMeta) -> Result<()> {
    tx.execute(
        "UPDATE ratings SET rating=$1, color_label=$2, flag=$3 WHERE photo_id=$4",
        (
            lmta.rating as i64,
            lmta.color_label.as_deref(),
            lmta.flag.as_deref(),
            photo_id,
        ),
    )?;
    tx.execute(
        "UPDATE user_meta SET caption=$1, copyright=$2, creator=$3,
             location_city=$4, location_country=$5 WHERE photo_id=$6",
        (
            lmta.caption.as_deref(),
            lmta.copyright.as_deref(),
            lmta.creator.as_deref(),
            lmta.location_city.as_deref(),
            lmta.location_country.as_deref(),
            photo_id,
        ),
    )?;
    // Keywords are replaced wholesale; the delete and the re-insert have to be
    // in the same transaction or a failure between them loses the list.
    tx.execute("DELETE FROM keywords WHERE photo_id = $1", (photo_id,))?;
    for kw in &lmta.keywords {
        tx.execute(
            "INSERT INTO keywords (photo_id, keyword) VALUES ($1,$2)",
            (photo_id, kw.as_str()),
        )?;
    }
    Ok(())
}

/// Toggle a photo's Recently Deleted state and refresh its session's active
/// count in the same transaction.
fn set_photo_deleted_tx(
    tx: &mut Transaction,
    photo_id: PhotoId,
    deleted_at: Option<u64>,
) -> Result<()> {
    let deleted_value = Value::integer(deleted_at.unwrap_or(0) as i64);
    tx.execute(
        "UPDATE photos SET deleted_at = $1 WHERE id = $2",
        vec![deleted_value, Value::integer(photo_id)],
    )?;
    tx.execute(
        "UPDATE import_sessions
         SET photo_count = (
             SELECT COUNT(*) FROM photos
             WHERE import_session = import_sessions.id AND deleted_at = 0
         )
         WHERE id = (SELECT import_session FROM photos WHERE id = $1)",
        (photo_id,),
    )?;
    Ok(())
}

/// Remove a photo row and its dependents, keeping its import-session count in sync.
fn delete_photo_tx(tx: &mut Transaction, photo_id: PhotoId) -> Result<()> {
    // `photo_count` is cached for the sidebar, so derive the post-delete value
    // from the photo rows while the row (and therefore its session id) is still
    // available. Empty-session pruning happens at the end of the surrounding
    // operation: index rebuilding temporarily deletes and reinserts rows, and
    // must retain the session row (including a custom name) in between.
    tx.execute(
        "UPDATE import_sessions
         SET photo_count = (
             SELECT COUNT(*) FROM photos
             WHERE import_session = import_sessions.id
               AND deleted_at = 0 AND id <> $1
         )
         WHERE id = (SELECT import_session FROM photos WHERE id = $1)",
        (photo_id,),
    )?;
    // Manual cascade since we dropped ON DELETE CASCADE
    for tbl in &[
        "keywords",
        "ratings",
        "exif",
        "user_meta",
        "collection_photos",
    ] {
        tx.execute(
            &format!("DELETE FROM {} WHERE photo_id = $1", tbl),
            (photo_id,),
        )?;
    }
    tx.execute("DELETE FROM photos WHERE id = $1", (photo_id,))?;
    Ok(())
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> StoolapDb {
        let db = StoolapDb::open_in_memory().unwrap();
        db.init().unwrap();
        db
    }

    fn lmta(session: &str) -> LibraryMeta {
        LibraryMeta {
            import_session_id: session.to_owned(),
            import_date: 1_600_000_000,
            keywords: vec!["alpha".into(), "beta".into()],
            ..Default::default()
        }
    }

    /// A photo to insert, with everything a test does not care about filled in.
    fn new_photo<'a>(hash: &'a str, lib_path: &'a str, lmta: &'a LibraryMeta) -> NewPhoto<'a> {
        NewPhoto {
            hash,
            lib_path,
            lmta,
            width: 10,
            height: 10,
            stack_id: None,
            has_edits: false,
        }
    }

    fn count(db: &StoolapDb, sql: &str) -> i64 {
        db.db.query_one::<i64, _>(sql, ()).unwrap()
    }

    /// The point of wrapping `insert_photo`: `hash` is UNIQUE, so a second
    /// insert of the same photo fails — after the EXIF, rating, keyword and
    /// user_meta statements would have run.  Without the transaction those
    /// rows would survive, attached to a photo id that does not exist.
    #[test]
    fn a_failed_insert_leaves_no_rows_behind() {
        let db = db();
        db.insert_photo(new_photo("aabbcc", "aa/bb/aabbcc.rlab", &lmta("s1")))
            .unwrap();

        let before = count(&db, "SELECT COUNT(*) FROM keywords");
        db.insert_photo(new_photo("aabbcc", "aa/bb/aabbcc.rlab", &lmta("s1")))
            .expect_err("duplicate hash must be rejected");

        assert_eq!(count(&db, "SELECT COUNT(*) FROM photos"), 1);
        assert_eq!(
            count(&db, "SELECT COUNT(*) FROM keywords"),
            before,
            "the rolled-back insert left keyword rows behind"
        );
        assert_eq!(count(&db, "SELECT COUNT(*) FROM ratings"), 1);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM user_meta"), 1);
    }

    #[test]
    fn inserting_a_fresh_photo_with_a_collection_writes_one_membership() {
        let db = db();
        let collection = db
            .create_collection("uuid-favorites", "Favorites", 1_600_000_000)
            .unwrap();

        let id = db
            .insert_photo_with_collection(
                new_photo("aabbcc", "aa/bb/aabbcc.rlab", &lmta("s1")),
                Some(collection),
            )
            .unwrap();

        assert_eq!(count(&db, "SELECT COUNT(*) FROM photos"), 1);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM collection_photos"), 1);
        assert_eq!(db.collection_member_ids(collection).unwrap(), [id]);
    }

    #[test]
    fn failed_fresh_photo_insert_does_not_add_a_membership() {
        let db = db();
        let collection = db
            .create_collection("uuid-favorites", "Favorites", 1_600_000_000)
            .unwrap();
        db.insert_photo_with_collection(
            new_photo("aabbcc", "aa/bb/aabbcc.rlab", &lmta("s1")),
            Some(collection),
        )
        .unwrap();

        db.insert_photo_with_collection(
            new_photo("aabbcc", "aa/bb/aabbcc.rlab", &lmta("s1")),
            Some(collection),
        )
        .expect_err("duplicate hash must roll back the fresh insert");

        assert_eq!(count(&db, "SELECT COUNT(*) FROM photos"), 1);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM collection_photos"), 1);
    }

    #[test]
    fn replacing_a_photo_keeps_its_id_and_collections_and_rewrites_the_rest() {
        let db = db();
        let id = db
            .insert_photo(new_photo("aabbcc", "aa/bb/aabbcc.rlab", &lmta("s1")))
            .unwrap();
        let collection = db
            .create_collection("uuid-1", "Trip", 1_600_000_000)
            .unwrap();
        db.add_to_collection(collection, &[id]).unwrap();

        let updated = LibraryMeta {
            rating: 4,
            keywords: vec!["gamma".into()],
            caption: Some("after".into()),
            ..lmta("s1")
        };
        db.replace_photo(id, new_photo("aabbcc", "aa/bb/aabbcc.rlab", &updated))
            .unwrap();

        let row = db.photo_by_hash("aabbcc").unwrap().expect("photo row");
        assert_eq!(row.id, id, "the row must keep the id its members refer to");
        assert_eq!(
            db.collection_photos(collection).unwrap().len(),
            1,
            "membership hangs off the id, so it must survive the rewrite"
        );
        assert_eq!(
            count(&db, "SELECT COUNT(*) FROM keywords"),
            1,
            "keywords should be replaced by the file's, not added to them"
        );
        assert_eq!(count(&db, "SELECT COUNT(*) FROM ratings"), 1);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM user_meta"), 1);
        assert_eq!(count(&db, "SELECT rating FROM ratings"), 4);
    }

    /// A rebuild replaces rows one at a time with no delete in between, so the
    /// session's cached count — which the sidebar shows and `all_sessions`
    /// filters on — must never dip while it runs.
    #[test]
    fn replacing_a_photo_leaves_its_session_count_alone() {
        let db = db();
        db.insert_session("s1", "Session", 1_600_000_000, None)
            .unwrap();
        let id = db
            .insert_photo(new_photo("aabbcc", "aa/bb/aabbcc.rlab", &lmta("s1")))
            .unwrap();
        db.update_session_count("s1", db.session_photo_count("s1").unwrap())
            .unwrap();

        db.replace_photo(id, new_photo("aabbcc", "aa/bb/aabbcc.rlab", &lmta("s1")))
            .unwrap();

        assert_eq!(
            db.all_sessions().unwrap()[0].photo_count,
            1,
            "a session of one photo must not vanish mid-rebuild"
        );
    }

    /// The edited-only filter reads one column, and only an insert can put a
    /// photo that arrived already edited into it.
    #[test]
    fn an_edited_photo_is_indexed_as_edited_and_found_by_the_filter() {
        let db = db();
        db.insert_photo(NewPhoto {
            has_edits: true,
            ..new_photo("aabbcc", "aa/bb/aabbcc.rlab", &lmta("s1"))
        })
        .unwrap();
        db.insert_photo(new_photo("ddeeff", "dd/ee/ddeeff.rlab", &lmta("s1")))
            .unwrap();

        assert!(
            db.all_photos(SortOrder::default()).unwrap()[0].has_edits
                ^ db.all_photos(SortOrder::default()).unwrap()[1].has_edits
        );

        let filter = SearchFilter {
            import_session: Some("s1".into()),
            has_edits_only: true,
            ..Default::default()
        };
        let found = db.search(&filter, SortOrder::default()).unwrap();
        assert_eq!(
            found
                .iter()
                .map(|row| row.hash.as_str())
                .collect::<Vec<_>>(),
            ["aabbcc"],
            "the session's edited photo is the only one the filter should return"
        );
    }

    #[test]
    fn session_photo_count_reflects_the_rows() {
        let db = db();
        db.insert_session("s1", "Jun 3 2025", 1_600_000_000, None)
            .unwrap();
        assert_eq!(db.session_photo_count("s1").unwrap(), 0);

        let id = db
            .insert_photo(new_photo("aabbcc", "aa/bb/aabbcc.rlab", &lmta("s1")))
            .unwrap();
        db.insert_photo(new_photo("ddeeff", "dd/ee/ddeeff.rlab", &lmta("s1")))
            .unwrap();
        assert_eq!(db.session_photo_count("s1").unwrap(), 2);

        db.delete_photo(id).unwrap();
        assert_eq!(db.session_photo_count("s1").unwrap(), 1);
        assert_eq!(db.all_sessions().unwrap()[0].photo_count, 1);
        assert_eq!(db.session_photo_count("nonexistent").unwrap(), 0);
    }

    #[test]
    fn active_photo_count_counts_rows_not_cached_session_totals() {
        let db = db();
        db.insert_session("s1", "Jun 3 2025", 1_600_000_000, None)
            .unwrap();
        db.insert_session("s2", "Jun 4 2025", 1_600_086_400, None)
            .unwrap();
        assert_eq!(db.active_photo_count().unwrap(), 0);

        let id = db
            .insert_photo(new_photo("aabbcc", "aa/bb/aabbcc.rlab", &lmta("s1")))
            .unwrap();
        db.insert_photo(new_photo("ddeeff", "dd/ee/ddeeff.rlab", &lmta("s2")))
            .unwrap();
        // Neither session's cached count has been written yet — which is
        // exactly the mid-import state the sidebar used to under-report.
        assert_eq!(
            db.all_sessions()
                .unwrap()
                .iter()
                .map(|s| s.photo_count)
                .sum::<i64>(),
            0
        );
        assert_eq!(db.active_photo_count().unwrap(), 2);

        db.mark_photo_deleted(id, 1_600_000_100).unwrap();
        assert_eq!(
            db.active_photo_count().unwrap(),
            1,
            "Recently Deleted is not part of All Photos"
        );
    }

    #[test]
    fn deleting_a_sessions_last_photo_zeros_the_cached_count() {
        let db = db();
        db.insert_session("s1", "Jun 3 2025", 1_600_000_000, None)
            .unwrap();
        let id = db
            .insert_photo(new_photo("aabbcc", "aa/bb/aabbcc.rlab", &lmta("s1")))
            .unwrap();
        db.update_session_count("s1", 1).unwrap();

        db.delete_photo(id).unwrap();

        let count: i64 = db
            .db
            .query_one(
                "SELECT photo_count FROM import_sessions WHERE id = $1",
                ("s1",),
            )
            .unwrap();
        assert_eq!(count, 0);
        assert!(db.all_sessions().unwrap().is_empty());
    }

    #[test]
    fn recently_deleted_rows_are_separated_from_active_photos() {
        let db = db();
        db.insert_session("s1", "Jun 3 2025", 1_600_000_000, None)
            .unwrap();
        let id = db
            .insert_photo(new_photo("aabbcc", "aa/bb/aabbcc.rlab", &lmta("s1")))
            .unwrap();
        db.update_session_count("s1", 1).unwrap();
        assert_eq!(db.all_photos(SortOrder::default()).unwrap().len(), 1);

        db.mark_photo_deleted(id, 1_700_000_000).unwrap();

        let deleted_at: i64 = db
            .db
            .query_one("SELECT deleted_at FROM photos WHERE id = $1", (id,))
            .unwrap();
        assert_eq!(deleted_at, 1_700_000_000);
        let active_count: i64 = db
            .db
            .query_one("SELECT COUNT(*) FROM photos WHERE deleted_at = 0", ())
            .unwrap();
        assert_eq!(active_count, 0);
        assert!(db.all_photos(SortOrder::default()).unwrap().is_empty());
        assert_eq!(db.recently_deleted().unwrap().len(), 1);

        db.restore_photo(id).unwrap();
        assert_eq!(db.all_photos(SortOrder::default()).unwrap().len(), 1);
        assert!(db.recently_deleted().unwrap().is_empty());
    }

    #[test]
    fn delete_empty_sessions_keeps_the_ones_with_photos() {
        let db = db();
        db.insert_session("full", "Jun 3 2025", 1_600_000_000, None)
            .unwrap();
        db.insert_session("empty", "Jun 4 2025", 1_600_100_000, None)
            .unwrap();
        db.insert_photo(new_photo("aabbcc", "aa/bb/aabbcc.rlab", &lmta("full")))
            .unwrap();
        db.update_session_count("full", 1).unwrap();

        assert_eq!(db.delete_empty_sessions().unwrap(), 1);
        let ids: Vec<String> = db
            .all_sessions()
            .unwrap()
            .into_iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(ids, ["full"]);
    }
    /// `collection_photos` joins on the membership rows, so a second add would
    /// otherwise show the photo twice in the collection it is already in.
    #[test]
    fn re_adding_a_photo_leaves_one_membership_row() {
        let db = db();
        let a = db
            .insert_photo(new_photo("aabbcc", "aa/bb/aabbcc.rlab", &lmta("s1")))
            .unwrap();
        let b = db
            .insert_photo(new_photo("ddeeff", "dd/ee/ddeeff.rlab", &lmta("s1")))
            .unwrap();
        let coll = db
            .create_collection("uuid-favorites", "Favorites", 1_600_000_000)
            .unwrap();

        db.add_to_collection(coll, &[a]).unwrap();
        // The overlapping case: `a` is already in, `b` is not.
        db.add_to_collection(coll, &[a, b]).unwrap();

        assert_eq!(count(&db, "SELECT COUNT(*) FROM collection_photos"), 2);
        assert_eq!(db.collection_photos(coll).unwrap().len(), 2);
    }

    /// Libraries made before collections had ids have rows without one, and a
    /// collection with no uuid cannot be written into a photo's file at all.
    #[test]
    fn opening_an_older_index_gives_every_collection_a_uuid() {
        let db = db();
        // What the migration finds: a row from before the column existed.
        db.db
            .execute(
                "INSERT INTO collections (uuid, name, created_at) VALUES ('', 'Portfolio', 1)",
                (),
            )
            .unwrap();

        db.init().expect("init must be re-runnable");

        let collections = db.all_collections().unwrap();
        assert_eq!(collections.len(), 1);
        assert!(
            !collections[0].uuid.is_empty(),
            "the pre-uuid row was left without an id"
        );

        // Re-running must not mint a second one over the top of the first.
        let minted = collections[0].uuid.clone();
        db.init().unwrap();
        assert_eq!(db.all_collections().unwrap()[0].uuid, minted);
    }

    /// A soft-deleted photo keeps its membership row so restoring puts it back
    /// in the collection, but while it is in Recently Deleted it is not one of
    /// the collection's photos.
    #[test]
    fn collection_memberships_skip_deleted_photos() {
        let db = db();
        let a = db
            .insert_photo(new_photo("aabbcc", "aa/bb/aabbcc.rlab", &lmta("s1")))
            .unwrap();
        let b = db
            .insert_photo(new_photo("ddeeff", "dd/ee/ddeeff.rlab", &lmta("s1")))
            .unwrap();
        let favorites = db
            .create_collection("uuid-favorites", "Favorites", 1_600_000_000)
            .unwrap();
        let portfolio = db
            .create_collection("uuid-portfolio", "Portfolio", 1_600_000_000)
            .unwrap();
        db.add_to_collection(favorites, &[a, b]).unwrap();
        db.add_to_collection(portfolio, &[a]).unwrap();

        let mut pairs = db.collection_memberships().unwrap();
        pairs.sort_unstable();
        assert_eq!(pairs, [(favorites, a), (favorites, b), (portfolio, a)]);

        db.mark_photo_deleted(b, 1_700_000_000).unwrap();
        let mut pairs = db.collection_memberships().unwrap();
        pairs.sort_unstable();
        assert_eq!(pairs, [(favorites, a), (portfolio, a)]);

        db.restore_photo(b).unwrap();
        assert_eq!(db.collection_memberships().unwrap().len(), 3);
    }
}
