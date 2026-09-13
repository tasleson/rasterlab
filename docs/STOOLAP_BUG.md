# Stoolap: a join loses every row when the join column is indexed

A query joining three or more tables returns **zero rows, with no error**, when
the join column of one of those tables carries a secondary index and the rows
are read back in a later session. The same query returns the correct rows in
the session that inserted them, and returns the correct rows in any session
once the index is dropped.

Silent wrong answers are the problem here rather than the missing rows: the
query succeeds, so nothing downstream can tell that it has been handed an empty
result instead of the three rows it asked for.

- **Crate:** `stoolap` 0.4.0 (crates.io), <https://github.com/stoolap/stoolap>
- **rustc:** 1.98.0
- **Platform:** Linux 7.1.10-200.fc44.x86_64
- **Found in:** [RasterLab](https://github.com/tasleson/rasterlab), whose photo
  library indexes ~50,000 photos and could not list the three photos in a
  collection.

## What triggers it

Three conditions have to hold together. Remove any one and the query is
correct.

1. A secondary index exists on the column a join is **on** — `CREATE INDEX
   cp_photo ON collection_photos(photo_id)` for a join `ON cp.photo_id = p.id`.
   An index on some other column of the same table (one used by the `WHERE`
   clause, say) does not do it.
2. The rows are read back from storage — any session *after* the one that
   inserted them. The writing session itself answers correctly.
3. The query joins **three or more tables**. Two-table joins are correct under
   the same conditions.

## Evidence

Counts from the reproduction below. Every cell should read 3.

| join-column index | session         | 2 tables | 3 tables (LEFT JOIN) | 3 tables (INNER JOIN) |
| ----------------- | --------------- | -------- | -------------------- | --------------------- |
| absent            | writing session | 3        | 3                    | 3                     |
| absent            | after reopen    | 3        | 3                    | 3                     |
| present           | writing session | 3        | 3                    | 3                     |
| present           | **after reopen**    | 3        | **0**                | **0**                 |

## Things that turned out not to matter

Ruled out by bisection, and listed because each one looked like the cause at
some point:

- **Table size.** Reproduces at 100 rows as readily as at 50,000. The library
  that first showed the bug held 50,461 photos, which sent the investigation
  down a scaling blind alley.
- **`LEFT JOIN` vs `JOIN`.** An all-inner three-table join fails identically.
  The first diagnosis here was "mixing inner and outer joins breaks it", which
  is wrong — it is the table count and the index.
- **Join order in the statement.** Naming the indexed table first or last makes
  no difference.
- **`SELECT DISTINCT`.** Present or absent, same outcome.
- **The `WHERE` clause.** A three-table join with no `WHERE` at all returns
  nothing just the same.
- **Index on a non-join column.** `CREATE INDEX ... ON collection_photos(collection_id)`,
  used by the `WHERE` clause rather than the `ON` clause, is harmless.
- **In-memory databases.** Not reproducible, which follows from condition 2:
  there is no later session to read the rows back in.

## Reproduction

`Cargo.toml`:

```toml
[package]
name = "stoolap-join-repro"
version = "0.1.0"
edition = "2021"

[dependencies]
stoolap = "0.4.0"
anyhow = "1"
```

`src/main.rs`:

```rust
use stoolap::api::Database;

const TWO_TABLES: &str = "SELECT p.id FROM photos p
     JOIN collection_photos cp ON cp.photo_id = p.id WHERE cp.collection_id = 1";
const THREE_TABLES: &str = "SELECT p.id FROM photos p
     JOIN exif e ON e.photo_id = p.id
     JOIN collection_photos cp ON cp.photo_id = p.id WHERE cp.collection_id = 1";

fn count(db: &Database, sql: &str) -> String {
    match db.query(sql, ()) {
        Ok(rows) => rows.into_iter().filter(|r| r.is_ok()).count().to_string(),
        Err(e) => format!("ERR({e})"),
    }
}

fn main() -> anyhow::Result<()> {
    for indexed in [false, true] {
        let dir = format!("/tmp/stoolap-repro-{indexed}");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        let dsn = format!("file://{dir}/db");

        {
            let db = Database::open(&dsn)?;
            db.execute(
                "CREATE TABLE photos (id INTEGER PRIMARY KEY AUTOINCREMENT, hash TEXT)",
                (),
            )?;
            db.execute("CREATE TABLE exif (photo_id INTEGER PRIMARY KEY, iso INTEGER)", ())?;
            db.execute(
                "CREATE TABLE collection_photos (collection_id INTEGER, photo_id INTEGER)",
                (),
            )?;
            if indexed {
                // An index on the column the join is ON.
                db.execute("CREATE INDEX cp_photo ON collection_photos(photo_id)", ())?;
            }

            let mut tx = db.begin()?;
            for i in 1..=100 {
                tx.execute("INSERT INTO photos (hash) VALUES ($1)", (format!("h{i}"),))?;
                tx.execute("INSERT INTO exif (photo_id, iso) VALUES ($1,100)", (i,))?;
            }
            for i in 1..=3 {
                tx.execute(
                    "INSERT INTO collection_photos (collection_id, photo_id) VALUES (1,$1)",
                    (i,),
                )?;
            }
            tx.commit()?;

            println!(
                "index={indexed:<5} writing session  2 tables={}  3 tables={}",
                count(&db, TWO_TABLES),
                count(&db, THREE_TABLES)
            );
        }

        let db = Database::open(&dsn)?;
        println!(
            "index={indexed:<5} after reopen     2 tables={}  3 tables={}",
            count(&db, TWO_TABLES),
            count(&db, THREE_TABLES)
        );
    }
    Ok(())
}
```

Output, where every number should be 3:

```
index=false writing session  2 tables=3  3 tables=3
index=false after reopen     2 tables=3  3 tables=3
index=true  writing session  2 tables=3  3 tables=3
index=true  after reopen     2 tables=3  3 tables=0
```

## Working around it

RasterLab does not put the affected table into the join at all. Membership is
resolved with a two-table query of its own, and either used directly or
intersected with the result of the larger joined query in application code
(`rasterlab-library/src/stoolap_db.rs`, `LibraryDb::search`). Dropping the index
on the join column also restores correct results, at whatever the scan costs.

## A second, unrelated limitation

The same feature first ran into a different wall, recorded here because anyone
scoping a query to a subquery in stoolap 0.4.0 will meet it:

```sql
SELECT ... FROM photos p
WHERE p.id IN (SELECT photo_id FROM collection_photos WHERE collection_id = 1)
```

fails to compile with:

```
Compile error: Unsupported expression: Dynamic IN list not yet supported in VM
```

This one at least fails loudly, and is a documented gap rather than a wrong
answer.

---

🤖 Assisted-by: Claude Code (Claude Opus 5)
