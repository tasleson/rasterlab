use std::path::Path;

use anyhow::{Context, Result};
use rasterlab_core::library_meta::LibraryMeta;
use stoolap::Value;
use stoolap::api::Database;

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
}

// ── Schema ────────────────────────────────────────────────────────────────────

const SCHEMA_STMTS: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS photos (
        id                INTEGER PRIMARY KEY,
        hash              TEXT    NOT NULL UNIQUE,
        lib_path          TEXT    NOT NULL,
        width             INTEGER,
        height            INTEGER,
        import_date       INTEGER,
        import_session    TEXT,
        capture_date      TEXT,
        original_filename TEXT,
        stack_id          TEXT,
        stack_is_primary  INTEGER NOT NULL DEFAULT 1
    )",
    "CREATE TABLE IF NOT EXISTS exif (
        photo_id          INTEGER PRIMARY KEY,
        camera_make       TEXT,
        camera_model      TEXT,
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
        id          TEXT PRIMARY KEY,
        name        TEXT NOT NULL,
        started_at  INTEGER,
        source_dir  TEXT,
        photo_count INTEGER NOT NULL DEFAULT 0
    )",
    "CREATE TABLE IF NOT EXISTS collections (
        id         INTEGER PRIMARY KEY,
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
    "CREATE INDEX IF NOT EXISTS exif_capture   ON exif(capture_date)",
    "CREATE INDEX IF NOT EXISTS photos_import  ON photos(import_date, import_session)",
    "CREATE INDEX IF NOT EXISTS photos_stack   ON photos(stack_id)",
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
    })
}

const PHOTO_SELECT: &str = "SELECT p.id, p.hash, p.lib_path, p.width, p.height,
            p.import_date, p.import_session, p.capture_date,
            p.original_filename, p.stack_id, p.stack_is_primary
     FROM photos p";

// ── LibraryDb impl ────────────────────────────────────────────────────────────

impl LibraryDb for StoolapDb {
    fn init(&self) -> Result<()> {
        for stmt in SCHEMA_STMTS {
            self.db
                .execute(stmt, ())
                .with_context(|| format!("schema: {}", &stmt[..40]))?;
        }
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
        let capture_date: Option<&str> = lmta.exif.as_ref().and_then(|e| e.capture_date.as_deref());

        self.db
            .execute(
                "INSERT INTO photos
             (hash, lib_path, width, height, import_date, import_session,
              capture_date, original_filename, stack_id, stack_is_primary)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
                (
                    hash,
                    lib_path,
                    width as i64,
                    height as i64,
                    lmta.import_date as i64,
                    lmta.import_session_id.as_str(),
                    capture_date,
                    lmta.original_filename.as_deref(),
                    stack_id,
                    if lmta.stack_is_primary { 1i64 } else { 0i64 },
                ),
            )
            .context("insert photo")?;

        let photo_id: i64 = self
            .db
            .query_one("SELECT last_insert_rowid()", ())
            .context("last_insert_rowid")?;

        // EXIF — 17 params exceeds tuple impl limit; use Vec<Value>
        if let Some(exif) = &lmta.exif {
            let opt_text =
                |s: Option<&str>| -> Value { s.map_or_else(Value::null_unknown, Value::text) };
            let opt_int =
                |v: Option<i64>| -> Value { v.map_or_else(Value::null_unknown, Value::integer) };
            let opt_f64 =
                |v: Option<f64>| -> Value { v.map_or_else(Value::null_unknown, Value::float) };

            let params: Vec<Value> = vec![
                Value::integer(photo_id),
                opt_text(exif.camera_make.as_deref()),
                opt_text(exif.camera_model.as_deref()),
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
            self.db
                .execute(
                    "INSERT INTO exif
                 (photo_id, camera_make, camera_model, lens_model, iso,
                  shutter_sec, shutter_display, aperture, focal_length,
                  focal_length_35mm, exposure_bias, exposure_program,
                  metering_mode, flash, gps_lat, gps_lon, gps_alt)
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17)",
                    params,
                )
                .context("insert exif")?;
        }

        // Ratings row
        self.db
            .execute(
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
            self.db
                .execute(
                    "INSERT INTO keywords (photo_id, keyword) VALUES ($1, $2)",
                    (photo_id, kw.as_str()),
                )
                .context("insert keyword")?;
        }

        // user_meta
        self.db
            .execute(
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
            self.db
                .execute(
                    "INSERT OR IGNORE INTO collections (name, created_at) VALUES ($1, $2)",
                    (coll_name.as_str(), unix_now() as i64),
                )
                .ok();
            if let Ok(coll_id) = self.db.query_one::<i64, _>(
                "SELECT id FROM collections WHERE name = $1",
                (coll_name.as_str(),),
            ) {
                self.db
                    .execute(
                        "INSERT OR IGNORE INTO collection_photos
                     (collection_id, photo_id, added_at) VALUES ($1,$2,$3)",
                        (coll_id, photo_id, unix_now() as i64),
                    )
                    .ok();
            }
        }

        Ok(photo_id)
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

    fn update_lmta(&self, photo_id: PhotoId, lmta: &LibraryMeta) -> Result<()> {
        self.db.execute(
            "UPDATE ratings SET rating=$1, color_label=$2, flag=$3 WHERE photo_id=$4",
            (
                lmta.rating as i64,
                lmta.color_label.as_deref(),
                lmta.flag.as_deref(),
                photo_id,
            ),
        )?;
        self.db.execute(
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
        self.db
            .execute("DELETE FROM keywords WHERE photo_id = $1", (photo_id,))?;
        for kw in &lmta.keywords {
            self.db.execute(
                "INSERT INTO keywords (photo_id, keyword) VALUES ($1,$2)",
                (photo_id, kw.as_str()),
            )?;
        }
        Ok(())
    }

    fn update_lmta_batch(&self, updates: &[(PhotoId, LibraryMeta)]) -> Result<()> {
        for (id, lmta) in updates {
            self.update_lmta(*id, lmta)?;
        }
        Ok(())
    }

    fn delete_photo(&self, photo_id: PhotoId) -> Result<()> {
        // Manual cascade since we dropped ON DELETE CASCADE
        for tbl in &[
            "keywords",
            "ratings",
            "exif",
            "user_meta",
            "collection_photos",
        ] {
            self.db.execute(
                &format!("DELETE FROM {} WHERE photo_id = $1", tbl),
                (photo_id,),
            )?;
        }
        self.db
            .execute("DELETE FROM photos WHERE id = $1", (photo_id,))?;
        Ok(())
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
        }

        if let Some(ref text) = filter.text {
            let pat = format!("%{}%", text);
            push!(
                "(p.original_filename LIKE {} OR um.caption LIKE {} OR k.keyword LIKE {})",
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
            push!("e.camera_model LIKE {}", Value::text(format!("%{}%", cam)));
        }
        if let Some(ref lens) = filter.lens_model {
            push!("e.lens_model LIKE {}", Value::text(format!("%{}%", lens)));
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

        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conditions.join(" AND "))
        };

        let sql = format!(
            "SELECT DISTINCT p.id, p.hash, p.lib_path, p.width, p.height,
                    p.import_date, p.import_session, p.capture_date,
                    p.original_filename, p.stack_id, p.stack_is_primary
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
            &format!(
                "SELECT p.id, p.hash, p.lib_path, p.width, p.height,
                        p.import_date, p.import_session, p.capture_date,
                        p.original_filename, p.stack_id, p.stack_is_primary
                 FROM photos p
                 JOIN collection_photos cp ON cp.photo_id = p.id
                 WHERE cp.collection_id = $1
                 ORDER BY p.capture_date DESC, p.id DESC"
            ),
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
                "INSERT OR IGNORE INTO import_sessions
             (id, name, started_at, source_dir, photo_count) VALUES ($1,$2,$3,$4,0)",
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
        self.db.execute(
            "INSERT INTO collections (name, created_at) VALUES ($1,$2)",
            (name, created_at as i64),
        )?;
        let id: i64 = self.db.query_one("SELECT last_insert_rowid()", ())?;
        Ok(id)
    }

    fn rename_collection(&self, id: CollectionId, name: &str) -> Result<()> {
        self.db
            .execute("UPDATE collections SET name=$1 WHERE id=$2", (name, id))?;
        Ok(())
    }

    fn delete_collection(&self, id: CollectionId) -> Result<()> {
        self.db.execute(
            "DELETE FROM collection_photos WHERE collection_id=$1",
            (id,),
        )?;
        self.db
            .execute("DELETE FROM collections WHERE id=$1", (id,))?;
        Ok(())
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
        for &pid in photo_ids {
            self.db.execute(
                "INSERT OR IGNORE INTO collection_photos
                 (collection_id, photo_id, added_at) VALUES ($1,$2,$3)",
                (collection_id, pid, now),
            )?;
        }
        Ok(())
    }

    fn remove_from_collection(
        &self,
        collection_id: CollectionId,
        photo_ids: &[PhotoId],
    ) -> Result<()> {
        for &pid in photo_ids {
            self.db.execute(
                "DELETE FROM collection_photos WHERE collection_id=$1 AND photo_id=$2",
                (collection_id, pid),
            )?;
        }
        Ok(())
    }

    // ── Bulk rebuild ──────────────────────────────────────────────────────

    fn clear_all(&self) -> Result<()> {
        for tbl in &[
            "collection_photos",
            "collections",
            "keywords",
            "user_meta",
            "ratings",
            "exif",
            "import_sessions",
            "photos",
        ] {
            self.db
                .execute(&format!("DELETE FROM {}", tbl), ())
                .with_context(|| format!("clear {}", tbl))?;
        }
        Ok(())
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
