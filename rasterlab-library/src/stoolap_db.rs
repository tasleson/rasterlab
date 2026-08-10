use std::path::Path;

use anyhow::{Context, Result};
use rasterlab_core::library_meta::LibraryMeta;
use stoolap::Value;
use stoolap::api::{Database, Transaction};

use crate::{
    db_trait::{
        CollectionId, CollectionRow, ImportSessionRow, LibraryDb, PhotoId, PhotoRow, SortOrder,
    },
    search::SearchFilter,
};

pub struct StoolapDb {
    db: Database,
}

impl StoolapDb {
    pub fn open(library_root: &Path) -> Result<Self> {
        let db_path = library_root.join("library.db");
        let dsn = format!("file://{}", db_path.display());
        let db = Database::open(&dsn).context("open library.db")?;
        Ok(Self { db })
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        let db = Database::open_in_memory().context("open in-memory db")?;
        Ok(Self { db })
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
        source_mtime      INTEGER
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
        source_dir  TEXT,
        photo_count INTEGER NOT NULL DEFAULT 0
    )",
    "CREATE TABLE IF NOT EXISTS collections (
        id         INTEGER PRIMARY KEY AUTOINCREMENT,
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
            p.protected
     FROM photos p";

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
        Ok(())
    }

    // ── Photos ────────────────────────────────────────────────────────────

    fn insert_photo(
        &self,
        hash: &str,
        lib_path: &str,
        lmta: &LibraryMeta,
        width: u32,
        height: u32,
        stack_id: Option<&str>,
    ) -> Result<PhotoId> {
        self.in_transaction(|tx| insert_photo_tx(tx, hash, lib_path, lmta, width, height, stack_id))
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
            result.push(row_to_photo(&row.context("all_photos row")?)?);
        }
        Ok(result)
    }

    // ── Search ────────────────────────────────────────────────────────────

    fn search(&self, filter: &SearchFilter, sort: SortOrder) -> Result<Vec<PhotoRow>> {
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
        if let Some(coll_id) = filter.collection_id {
            push!(
                "p.id IN (SELECT photo_id FROM collection_photos WHERE collection_id = {})",
                Value::integer(coll_id)
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
                    p.original_filename, p.stack_id, p.stack_is_primary, p.has_edits
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
            result.push(row_to_photo(&row.context("search row")?)?);
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
            result.push(row_to_photo(&row.context("photos_by_session row")?)?);
        }
        Ok(result)
    }

    fn collection_photos(&self, collection_id: CollectionId) -> Result<Vec<PhotoRow>> {
        let rows = self.db.query(
            "SELECT p.id, p.hash, p.lib_path, p.width, p.height,
                        p.import_date, p.import_session, p.capture_date,
                        p.original_filename, p.stack_id, p.stack_is_primary
                 FROM photos p
                 JOIN collection_photos cp ON cp.photo_id = p.id
                 WHERE cp.collection_id = $1
                 ORDER BY p.capture_date DESC, p.id DESC",
            (collection_id,),
        )?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row_to_photo(&row.context("collection_photos row")?)?);
        }
        Ok(result)
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
                "SELECT COUNT(*) FROM photos WHERE import_session = $1",
                (session_id,),
            )
            .context("count session photos")
    }

    fn delete_empty_sessions(&self) -> Result<usize> {
        // Counted per session rather than with a `NOT IN` subquery: sessions
        // number in the hundreds at most, and an anti-join against a column
        // that can be NULL is the kind of thing that silently deletes
        // everything.
        let stale: Vec<String> = self
            .all_sessions()?
            .into_iter()
            .map(|s| s.id)
            .filter(|id| self.session_photo_count(id).unwrap_or(1) == 0)
            .collect();
        self.in_transaction(|tx| {
            for id in &stale {
                tx.execute("DELETE FROM import_sessions WHERE id = $1", (id.as_str(),))?;
            }
            Ok(stale.len())
        })
    }

    fn all_sessions(&self) -> Result<Vec<ImportSessionRow>> {
        let rows = self.db.query(
            "SELECT id, name, started_at, source_dir, photo_count
             FROM import_sessions ORDER BY started_at DESC",
            (),
        )?;
        let mut result = Vec::new();
        for row in rows {
            let row = row.context("all_sessions row")?;
            result.push(ImportSessionRow {
                id: row.get::<String>(0)?,
                name: row.get::<String>(1)?,
                started_at: row.get::<i64>(2)? as u64,
                source_dir: row.get::<Option<String>>(3)?,
                photo_count: row.get::<i64>(4)?,
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
            result.push(row_to_photo(&row.context("photos_in_stack row")?)?);
        }
        Ok(result)
    }

    // ── Collections ───────────────────────────────────────────────────────

    fn create_collection(&self, name: &str, created_at: u64) -> Result<CollectionId> {
        let id: i64 = self.db.query_one(
            "INSERT INTO collections (name, created_at) VALUES ($1,$2) RETURNING id",
            (name, created_at as i64),
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
            "SELECT id, name, created_at FROM collections ORDER BY name ASC",
            (),
        )?;
        let mut result = Vec::new();
        for row in rows {
            let row = row.context("all_collections row")?;
            result.push(CollectionRow {
                id: row.get::<i64>(0)?,
                name: row.get::<String>(1)?,
                created_at: row.get::<i64>(2)? as u64,
            });
        }
        Ok(result)
    }

    fn add_to_collection(&self, collection_id: CollectionId, photo_ids: &[PhotoId]) -> Result<()> {
        let now = unix_now() as i64;
        self.in_transaction(|tx| {
            for &pid in photo_ids {
                tx.execute(
                    "INSERT INTO collection_photos
                     (collection_id, photo_id, added_at) VALUES ($1,$2,$3) ON CONFLICT DO NOTHING",
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
fn insert_photo_tx(
    tx: &mut Transaction,
    hash: &str,
    lib_path: &str,
    lmta: &LibraryMeta,
    width: u32,
    height: u32,
    stack_id: Option<&str>,
) -> Result<PhotoId> {
    let capture_date: Option<&str> = lmta.exif.as_ref().and_then(|e| e.capture_date.as_deref());

    let opt_text = |s: Option<&str>| -> Value { s.map_or_else(Value::null_unknown, Value::text) };
    let opt_int = |v: Option<i64>| -> Value { v.map_or_else(Value::null_unknown, Value::integer) };
    let opt_f64 = |v: Option<f64>| -> Value { v.map_or_else(Value::null_unknown, Value::float) };

    // 14 params exceeds the 12-tuple Params impl limit; use Vec<Value>.
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
    ];
    let photo_id: i64 = tx
        .query_one(
            "INSERT INTO photos
             (hash, lib_path, width, height, import_date, import_session,
              capture_date, original_filename, stack_id, stack_is_primary, protected,
              source_path, source_size, source_mtime)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)
             RETURNING id",
            photo_params,
        )
        .context("insert photo")?;

    // EXIF — 17 params exceeds tuple impl limit; use Vec<Value>
    if let Some(exif) = &lmta.exif {
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

    // Ratings row
    tx.execute(
        "INSERT INTO ratings (photo_id, rating, color_label, flag) VALUES ($1,$2,$3,$4)",
        (
            photo_id,
            lmta.rating as i64,
            lmta.color_label.as_deref(),
            lmta.flag.as_deref(),
        ),
    )
    .context("insert rating")?;

    // Keywords
    for kw in &lmta.keywords {
        tx.execute(
            "INSERT INTO keywords (photo_id, keyword) VALUES ($1, $2)",
            (photo_id, kw.as_str()),
        )
        .context("insert keyword")?;
    }

    // user_meta
    tx.execute(
        "INSERT INTO user_meta
             (photo_id, caption, copyright, creator, location_city, location_country)
             VALUES ($1,$2,$3,$4,$5,$6)",
        (
            photo_id,
            lmta.caption.as_deref(),
            lmta.copyright.as_deref(),
            lmta.creator.as_deref(),
            lmta.location_city.as_deref(),
            lmta.location_country.as_deref(),
        ),
    )
    .context("insert user_meta")?;

    // Collections
    for coll_name in &lmta.collections {
        tx.execute(
            "INSERT INTO collections (name, created_at) VALUES ($1, $2) ON CONFLICT DO NOTHING",
            (coll_name.as_str(), unix_now() as i64),
        )
        .ok();
        if let Ok(coll_id) = tx.query_one::<i64, _>(
            "SELECT id FROM collections WHERE name = $1",
            (coll_name.as_str(),),
        ) {
            tx.execute(
                "INSERT INTO collection_photos
                     (collection_id, photo_id, added_at) VALUES ($1,$2,$3) ON CONFLICT DO NOTHING",
                (coll_id, photo_id, unix_now() as i64),
            )
            .ok();
        }
    }

    Ok(photo_id)
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

/// Remove a photo row and its dependents.
fn delete_photo_tx(tx: &mut Transaction, photo_id: PhotoId) -> Result<()> {
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
        db.insert_photo("aabbcc", "aa/bb/aabbcc.rlab", &lmta("s1"), 100, 50, None)
            .unwrap();

        let before = count(&db, "SELECT COUNT(*) FROM keywords");
        db.insert_photo("aabbcc", "aa/bb/aabbcc.rlab", &lmta("s1"), 100, 50, None)
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
    fn session_photo_count_reflects_the_rows() {
        let db = db();
        db.insert_session("s1", "Jun 3 2025", 1_600_000_000, None)
            .unwrap();
        assert_eq!(db.session_photo_count("s1").unwrap(), 0);

        let id = db
            .insert_photo("aabbcc", "aa/bb/aabbcc.rlab", &lmta("s1"), 10, 10, None)
            .unwrap();
        db.insert_photo("ddeeff", "dd/ee/ddeeff.rlab", &lmta("s1"), 10, 10, None)
            .unwrap();
        assert_eq!(db.session_photo_count("s1").unwrap(), 2);

        db.delete_photo(id).unwrap();
        assert_eq!(db.session_photo_count("s1").unwrap(), 1);
        assert_eq!(db.session_photo_count("nonexistent").unwrap(), 0);
    }

    #[test]
    fn delete_empty_sessions_keeps_the_ones_with_photos() {
        let db = db();
        db.insert_session("full", "Jun 3 2025", 1_600_000_000, None)
            .unwrap();
        db.insert_session("empty", "Jun 4 2025", 1_600_100_000, None)
            .unwrap();
        db.insert_photo("aabbcc", "aa/bb/aabbcc.rlab", &lmta("full"), 10, 10, None)
            .unwrap();

        assert_eq!(db.delete_empty_sessions().unwrap(), 1);
        let ids: Vec<String> = db
            .all_sessions()
            .unwrap()
            .into_iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(ids, ["full"]);
    }
}
