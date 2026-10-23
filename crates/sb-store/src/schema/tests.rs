//! Acceptance tests for the migration runner and migration 1 (plan
//! M0.4). They live in-crate so they can reach `open_with` and the
//! migration list.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use rusqlite::{Connection, ffi};
use sb_test_support::{FixedClock, ScratchDir, SeededRandom, real_fs};
use sb_types::random::Random;
use sb_types::time::Timestamp;

use super::*;
use crate::StorePaths;
use crate::store::{PINNED_SQLITE, WRITER_PRAGMAS};
use crate::testing::{FIXED_NOW_MS, extra_migration, reading, scratch_paths, test_seams};

/// Seams with a different clock and seed, to prove a reopen rewrote
/// nothing it shouldn't have.
fn other_seams() -> Seams {
    Seams::new(
        real_fs(),
        Arc::new(FixedClock::new(Timestamp::from_millis(
            FIXED_NOW_MS + 3_600_000,
        ))),
        Arc::new(SeededRandom::new(7)),
    )
}

/// `store_meta` as a map.
fn store_meta(conn: &Connection) -> BTreeMap<String, String> {
    let mut stmt = conn.prepare("SELECT key, value FROM store_meta").unwrap();
    stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })
    .unwrap()
    .collect::<Result<BTreeMap<_, _>, _>>()
    .unwrap()
}

/// One integer answer, for pragma and count probes.
fn one_int(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |row| row.get(0)).unwrap()
}

/// A textual id is a lowercase hyphenated version-4 UUID (spec/02
/// rule 3): 8-4-4-4-12 lowercase hex, version nibble 4, RFC 9562
/// variant.
fn assert_v4_uuid(uuid: &str) {
    let bytes = uuid.as_bytes();
    assert_eq!(uuid.len(), 36, "{uuid}");
    for (slot, byte) in bytes.iter().enumerate() {
        match slot {
            8 | 13 | 18 | 23 => assert_eq!(*byte, b'-', "{uuid}: slot {slot} is not a hyphen"),
            _ => assert!(
                byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase(),
                "{uuid}"
            ),
        }
    }
    assert_eq!(bytes[14], b'4', "{uuid}: not version 4");
    assert!(
        matches!(bytes[19], b'8' | b'9' | b'a' | b'b'),
        "{uuid}: bad variant"
    );
}

/// 1. A fresh store reaches the latest schema with its metadata:
///    both version keys carry the workspace version, `created_at`
///    the clock, and `store_uuid`/`incarnation` two distinct v4
///    UUIDs.
#[test]
fn a_fresh_store_reaches_the_latest_version_with_its_meta() {
    let (_scratch, paths) = scratch_paths("schema-fresh");
    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    assert_eq!(
        reading(&store, |tx| Ok(one_int(tx, "PRAGMA user_version"))),
        i64::from(LATEST_SCHEMA_VERSION)
    );

    let meta =
        reading(&store, |tx| {
            assert_eq!(one_int(tx,
            "SELECT count(*) FROM accounts WHERE provider = 'local_archive' AND kind = 'local'
             AND display_name = 'Local Archive' AND state = 'active' AND sort_order = 0"), 1);
            Ok(store_meta(tx))
        });
    assert_eq!(
        meta.get("last_migrated_by").unwrap(),
        env!("CARGO_PKG_VERSION")
    );
    assert_eq!(
        meta.get("app_version_last_opened").unwrap(),
        env!("CARGO_PKG_VERSION")
    );
    assert_eq!(meta.get("created_at").unwrap(), &FIXED_NOW_MS.to_string());
    let store_uuid = meta.get("store_uuid").expect("store_uuid");
    let incarnation = meta.get("incarnation").expect("incarnation");
    assert_ne!(store_uuid, incarnation);
    assert_v4_uuid(store_uuid);
    assert_v4_uuid(incarnation);
    assert_eq!(meta.get("list_generation").unwrap(), "1");
    assert_eq!(meta.get("thread_inputs").unwrap(), "0");
    assert_eq!(meta.get("threads_built").unwrap(), "0");
    assert_eq!(meta.get("index_inputs").unwrap(), "0");
    assert_eq!(meta.get("search_generation").unwrap(), "1");
    assert_eq!(meta.get("search_extractor_version").unwrap(), "1");
    assert_eq!(meta.get("seen_address_target").unwrap(), "1");
    assert_eq!(meta.get("seen_address_inputs").unwrap(), "1");
    assert_eq!(meta.get("seen_address_swept").unwrap(), "0");
    assert_eq!(meta.get("mime_repair_target").unwrap(), "2");
    assert_eq!(meta.get("repair_inputs").unwrap(), "1");
    for key in ["repair_swept", "repair_deferred", "repair_unready"] {
        assert_eq!(meta.get(key).unwrap(), "0");
    }
    assert_eq!(meta.len(), 19, "unexpected extra store_meta keys: {meta:?}");
}

/// 2. Reopening runs no migration: `created_at` and both UUIDs
///    survive a reopen under a different clock and seed.
#[test]
fn reopening_runs_no_migration() {
    let (_scratch, paths) = scratch_paths("schema-reopen");
    let first = crate::open(&paths, &test_seams(real_fs())).unwrap();
    let (created_at, store_uuid, incarnation) = {
        let meta = reading(&first, |tx| Ok(store_meta(tx)));
        (
            meta["created_at"].clone(),
            meta["store_uuid"].clone(),
            meta["incarnation"].clone(),
        )
    };
    first.close().unwrap();

    let second = crate::open(&paths, &other_seams()).unwrap();
    assert_eq!(
        reading(&second, |tx| Ok(one_int(tx, "PRAGMA user_version"))),
        i64::from(LATEST_SCHEMA_VERSION)
    );
    let meta = reading(&second, |tx| Ok(store_meta(tx)));
    assert_eq!(meta.get("created_at").unwrap(), &created_at);
    assert_eq!(meta.get("store_uuid").unwrap(), &store_uuid);
    assert_eq!(meta.get("incarnation").unwrap(), &incarnation);
}

/// 3. After open, the checks the runner relies on are clean:
///    `foreign_key_check` answers no rows and `integrity_check`
///    answers exactly `ok`.
#[test]
fn checks_after_open_are_clean() {
    let (_scratch, paths) = scratch_paths("schema-clean");
    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    reading(&store, |tx| {
        let mut foreign_keys = tx.prepare("PRAGMA foreign_key_check").unwrap();
        let mut rows = foreign_keys.query([]).unwrap();
        assert!(
            rows.next().unwrap().is_none(),
            "foreign_key_check returned rows"
        );
        drop(rows);
        drop(foreign_keys);
        assert_eq!(
            pragma_strings(tx, "PRAGMA integrity_check").unwrap(),
            vec!["ok".to_owned()],
            "integrity_check did not answer ok"
        );
        Ok(())
    });
}

/// 4. Every application-owned ordinary table is `STRICT` (spec/02
///    rule 2), checked through `pragma_table_list`, which excludes
///    virtual, shadow and internal tables by construction.
#[test]
fn every_owned_ordinary_table_is_strict() {
    let (_scratch, paths) = scratch_paths("schema-strict");
    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    let (loose, tables) = reading(&store, |tx| {
        let loose: Vec<String> = tx
            .prepare(
                "SELECT name FROM pragma_table_list
                 WHERE schema = 'main' AND type = 'table'
                   AND name NOT GLOB 'sqlite_*' AND strict = 0",
            )
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        // The filter does see the store's tables, so the query isn't
        // answering over an empty set.
        let tables: Vec<String> = tx
            .prepare(
                "SELECT name FROM pragma_table_list
                 WHERE schema = 'main' AND type = 'table' AND name NOT GLOB 'sqlite_*'",
            )
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        Ok((loose, tables))
    });
    assert!(loose.is_empty(), "tables that are not STRICT: {loose:?}");
    assert!(!tables.is_empty());
    assert!(tables.contains(&"store_meta".to_owned()), "{tables:?}");
}

/// The ```` ```sql ```` blocks of a spec document, in document order,
/// joined with newlines.
fn sql_blocks(markdown: &str) -> String {
    let mut blocks: Vec<String> = Vec::new();
    let mut current: Option<Vec<&str>> = None;
    for line in markdown.lines() {
        let trimmed = line.trim_start();
        if let Some(lines) = current.as_mut() {
            if trimmed == "```" {
                blocks.push(lines.join("\n"));
                current = None;
            } else {
                lines.push(line);
            }
        } else if trimmed.starts_with("```sql") {
            current = Some(Vec::new());
        }
    }
    assert!(current.is_none(), "an unterminated ```sql block");
    blocks.join("\n")
}

/// Schema text the way the drift test compares it: `--` comments
/// removed, whitespace runs collapsed, no space next to `(`, `)` or
/// `,` — the differences that survive a verbatim copy.
fn normalise_sql(sql: &str) -> String {
    let without_comments: String = sql
        .lines()
        .map(|line| match line.split_once("--") {
            Some((before, _)) => before,
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let collapsed: Vec<&str> = without_comments.split_whitespace().collect();
    let mut squeezed = String::with_capacity(without_comments.len());
    let chars: Vec<char> = collapsed.join(" ").chars().collect();
    for (slot, &c) in chars.iter().enumerate() {
        if c == ' ' {
            let previous = if slot > 0 { chars[slot - 1] } else { ' ' };
            let next = chars.get(slot + 1).copied();
            if matches!(previous, '(' | ')' | ',')
                || next.is_some_and(|n| matches!(n, '(' | ')' | ','))
            {
                continue;
            }
        }
        squeezed.push(c);
    }
    squeezed
}

/// `sqlite_schema` as comparable rows, sorted, with `sql` normalised.
fn schema_rows(conn: &Connection) -> Vec<(String, String, String, Option<String>)> {
    let mut stmt = conn
        .prepare("SELECT type, name, tbl_name, sql FROM sqlite_schema ORDER BY type, name")
        .unwrap();
    stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    })
    .unwrap()
    .collect::<Result<Vec<_>, _>>()
    .unwrap()
    .into_iter()
    .map(|(kind, name, table, sql)| (kind, name, table, sql.map(|s| normalise_sql(&s))))
    .collect()
}

/// 5. Spec drift: the migrated store's schema equals one built from
///    the spec's own SQL blocks, so spec/02, spec/06 and `v1.sql`
///    can't drift apart (spec/02 "Migrations").
#[test]
fn migrated_schema_matches_the_spec_sql_blocks() {
    let spec_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../spec");
    let spec02 = fs::read_to_string(spec_dir.join("02-data-model.md")).unwrap();
    let spec06 = fs::read_to_string(spec_dir.join("06-search.md")).unwrap();
    let sql = format!("{}\n{}", sql_blocks(&spec02), sql_blocks(&spec06));
    assert!(!sql.is_empty(), "no ```sql blocks found in the specs");

    let spec_conn = Connection::open_in_memory().unwrap();
    spec_conn
        .execute_batch("PRAGMA foreign_keys = ON;")
        .and_then(|()| spec_conn.execute_batch(&sql))
        .expect("the spec's SQL blocks must execute");

    let (_scratch, paths) = scratch_paths("schema-drift");
    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    let migrated = reading(&store, |tx| Ok(schema_rows(tx)));
    assert_eq!(
        schema_rows(&spec_conn),
        migrated,
        "the migrated schema drifted from the spec's SQL blocks"
    );
}

/// A test-only migration 2 that leaves a foreign-key violation behind:
/// `defer_foreign_keys` lets the orphan insert through, so it is the
/// runner's in-transaction `foreign_key_check` that must catch it.
fn migration_2_orphan(tx: &rusqlite::Transaction, _seams: &Seams) -> Result<(), StoreError> {
    tx.execute_batch(
        "PRAGMA defer_foreign_keys = ON;
         CREATE TABLE m2_probe (tag_id TEXT NOT NULL REFERENCES tags(tag_id));
         INSERT INTO m2_probe VALUES ('no-such-tag');",
    )
    .map_err(bug)?;
    Ok(())
}

/// 6. A migration whose checks find a problem rolls back and yields
///    `Corrupt`: the production version remains and the orphan table is
///    absent (spec/02 "Migrations").
#[test]
fn a_migration_leaving_a_foreign_key_violation_rolls_back() {
    let (_scratch, paths) = scratch_paths("schema-fk-rollback");
    let failing = extra_migration(migration_2_orphan);
    match crate::store::open_with(
        &paths,
        &test_seams(real_fs()),
        WRITER_PRAGMAS,
        crate::store::READER_PRAGMAS,
        &PINNED_SQLITE,
        &failing,
    )
    .unwrap_err()
    {
        StoreError::Corrupt { detail } => {
            assert!(
                detail.contains(&format!("migration {}", LATEST_SCHEMA_VERSION + 1)),
                "{detail}"
            );
            assert!(detail.contains("foreign-key"), "{detail}");
            assert!(detail.contains("m2_probe"), "{detail}");
        }
        err => panic!("expected Corrupt, got {err:?}"),
    }

    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    assert_eq!(
        reading(&store, |tx| Ok(one_int(tx, "PRAGMA user_version"))),
        i64::from(LATEST_SCHEMA_VERSION)
    );
    assert_eq!(
        reading(&store, |tx| {
            Ok(one_int(
                tx,
                "SELECT count(*) FROM sqlite_schema WHERE name = 'm2_probe'",
            ))
        }),
        0,
        "the rolled-back migration's table is still there"
    );
}

/// A closed fresh store, ready for a raw connection to tamper with.
fn closed_store(label: &str) -> (ScratchDir, StorePaths) {
    let (scratch, paths) = scratch_paths(label);
    crate::open(&paths, &test_seams(real_fs()))
        .unwrap()
        .close()
        .unwrap();
    (scratch, paths)
}

/// Change a closed store's database through a raw connection, outside
/// the runner.
fn tamper(paths: &StorePaths, sql: &str) {
    let raw = Connection::open(paths.state_db()).unwrap();
    raw.execute_batch(sql).unwrap();
    raw.close().unwrap();
}

/// Replace a closed store's database with a new one built by `sql`,
/// keeping the layout, so only the database differs.
fn replace_state_db(paths: &StorePaths, sql: &str) {
    for name in ["state.sqlite", "state.sqlite-wal", "state.sqlite-shm"] {
        let path = paths.data_root().join(name);
        if path.exists() {
            fs::remove_file(path).unwrap();
        }
    }
    tamper(paths, sql);
}

/// Open refuses with `Corrupt` naming `needle`, and `state.sqlite` is
/// byte-for-byte unchanged (spec/02 "Migrations"), and so is its WAL
/// if it has one: frames a crash left there aren't lost either.
fn assert_corrupt_untouched(paths: &StorePaths, needle: &str) {
    let wal = paths.data_root().join("state.sqlite-wal");
    let before = fs::read(paths.state_db()).unwrap();
    let wal_before = fs::read(&wal).ok();
    match crate::open(paths, &test_seams(real_fs())).unwrap_err() {
        StoreError::Corrupt { detail } => assert!(detail.contains(needle), "{detail}"),
        err => panic!("expected Corrupt, got {err:?}"),
    }
    assert_eq!(
        fs::read(paths.state_db()).unwrap(),
        before,
        "state.sqlite changed"
    );
    if let Some(wal_before) = wal_before {
        assert_eq!(fs::read(&wal).unwrap(), wal_before, "the WAL changed");
    }
}

/// 7. A `user_version` above the list's latest is `NewerStore` naming
///    `last_migrated_by`, or `unknown` without the row or the table,
///    and `state.sqlite`'s bytes are unchanged (spec/02 "Migrations").
#[test]
fn a_newer_user_version_is_newer_store_and_touches_nothing() {
    let (_scratch, paths) = closed_store("schema-newer");
    for (sql, named) in [
        (
            format!(
                "PRAGMA user_version = {};
                 UPDATE store_meta SET value = '9.9.9' WHERE key = 'last_migrated_by';",
                LATEST_SCHEMA_VERSION + 1
            ),
            "9.9.9",
        ),
        (
            format!(
                "PRAGMA user_version = {}; DELETE FROM store_meta;",
                LATEST_SCHEMA_VERSION + 2
            ),
            "unknown",
        ),
        (
            format!(
                "PRAGMA user_version = {}; DROP TABLE store_meta;",
                LATEST_SCHEMA_VERSION + 3
            ),
            "unknown",
        ),
    ] {
        tamper(&paths, &sql);
        let before = fs::read(paths.state_db()).unwrap();
        match crate::open(&paths, &test_seams(real_fs())).unwrap_err() {
            StoreError::NewerStore { app_version } => assert_eq!(app_version, named),
            err => panic!("expected NewerStore, got {err:?}"),
        }
        assert_eq!(
            fs::read(paths.state_db()).unwrap(),
            before,
            "state.sqlite changed"
        );
    }
}

/// 8. `user_version` 0 with a foreign schema is not a Sweetbriar
///    store, whichever journal mode it uses: in WAL the runner refuses
///    it, in another mode the constructor does, before switching it to
///    WAL would rewrite it.
#[test]
fn a_foreign_schema_at_version_0_is_corrupt_and_untouched() {
    for (mode, refusal) in [("WAL", "holds a schema"), ("DELETE", "journal mode delete")] {
        let (_scratch, paths) = closed_store("schema-foreign");
        replace_state_db(
            &paths,
            &format!("PRAGMA journal_mode = {mode}; CREATE TABLE t(x);"),
        );
        assert_corrupt_untouched(&paths, refusal);
    }
}

/// A store switched out of WAL isn't one Sweetbriar created (spec/03):
/// `Corrupt`, and the constructor doesn't switch it back.
#[test]
fn a_store_out_of_wal_is_corrupt_and_untouched() {
    let (_scratch, paths) = closed_store("schema-not-wal");
    tamper(&paths, "PRAGMA journal_mode = DELETE;");
    assert_corrupt_untouched(&paths, "journal mode delete");
}

/// The refusal message for a supported version without identity.
const NO_IDENTITY: &str = "no Sweetbriar store_meta holds";

/// A supported `user_version` doesn't make a database a Sweetbriar
/// store: without the identity keys in a `store_meta` shaped like
/// migration 1's, it's `Corrupt`, untouched. Covers a foreign or absent
/// `store_meta`, repeated keys, a view, and a table without the
/// primary key open's upserts need.
#[test]
fn a_supported_version_without_identity_is_corrupt_and_untouched() {
    const IDENTITY_ROWS: &str =
        "('store_uuid', 'a'), ('incarnation', 'b'), ('last_migrated_by', 'c')";
    for schema in [
        "CREATE TABLE store_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL) STRICT;
         INSERT INTO store_meta VALUES ('foreign-owner', 'unrelated-app');"
            .to_owned(),
        "CREATE TABLE t(x);".to_owned(),
        "CREATE TABLE store_meta (key TEXT NOT NULL, value TEXT NOT NULL) STRICT;
         INSERT INTO store_meta VALUES ('store_uuid', 'a'), ('store_uuid', 'b'), ('store_uuid', 'c');"
            .to_owned(),
        format!("CREATE TABLE store_meta (key TEXT, value TEXT); INSERT INTO store_meta VALUES {IDENTITY_ROWS};"),
        format!(
            "CREATE TABLE m (key TEXT PRIMARY KEY, value TEXT NOT NULL); INSERT INTO m VALUES {IDENTITY_ROWS};
             CREATE VIEW store_meta AS SELECT key, value FROM m;"
        ),
        // Close to migration 1's table, but not it: the value's type,
        // NOT NULL, STRICT, an extra generated column.
        "CREATE TABLE store_meta (key TEXT PRIMARY KEY, value INTEGER NOT NULL) STRICT;
         INSERT INTO store_meta VALUES ('store_uuid', 1), ('incarnation', 2), ('last_migrated_by', 3);"
            .to_owned(),
        format!("CREATE TABLE store_meta (key TEXT PRIMARY KEY, value TEXT) STRICT; INSERT INTO store_meta VALUES {IDENTITY_ROWS};"),
        format!("CREATE TABLE store_meta (key TEXT NOT NULL PRIMARY KEY, value TEXT NOT NULL); INSERT INTO store_meta VALUES {IDENTITY_ROWS};"),
        format!(
            "CREATE TABLE store_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL,
               extra TEXT GENERATED ALWAYS AS (value) VIRTUAL) STRICT;
             INSERT INTO store_meta (key, value) VALUES {IDENTITY_ROWS};"
        ),
        // Same columns, different table: a generated value, an extra
        // constraint, then migration 1's table with an added index or
        // trigger.
        "CREATE TABLE store_meta (key TEXT PRIMARY KEY,
           value TEXT GENERATED ALWAYS AS ('foreign') STORED NOT NULL) STRICT;
         INSERT INTO store_meta (key) VALUES ('store_uuid'), ('incarnation'), ('last_migrated_by');"
            .to_owned(),
        format!("CREATE TABLE store_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL UNIQUE) STRICT; INSERT INTO store_meta VALUES {IDENTITY_ROWS};"),
        format!(
            "CREATE TABLE store_meta (\n  key   TEXT PRIMARY KEY,\n  value TEXT NOT NULL\n) STRICT;
             INSERT INTO store_meta VALUES {IDENTITY_ROWS};
             CREATE UNIQUE INDEX foreign_meta_value ON store_meta (value);"
        ),
        format!(
            "CREATE TABLE store_meta (\n  key   TEXT PRIMARY KEY,\n  value TEXT NOT NULL\n) STRICT;
             INSERT INTO store_meta VALUES {IDENTITY_ROWS};
             CREATE TRIGGER foreign_meta BEFORE INSERT ON store_meta BEGIN SELECT RAISE(ABORT, 'no'); END;"
        ),
        // Triggers naming the table in another case still fire on it:
        // one aborting, one quietly rewriting the metadata.
        format!(
            "CREATE TABLE store_meta (\n  key   TEXT PRIMARY KEY,\n  value TEXT NOT NULL\n) STRICT;
             INSERT INTO store_meta VALUES {IDENTITY_ROWS};
             CREATE TRIGGER foreign_meta BEFORE INSERT ON STORE_META BEGIN SELECT RAISE(ABORT, 'no'); END;"
        ),
        format!(
            "CREATE TABLE store_meta (\n  key   TEXT PRIMARY KEY,\n  value TEXT NOT NULL\n) STRICT;
             INSERT INTO store_meta VALUES {IDENTITY_ROWS};
             CREATE TRIGGER foreign_meta AFTER UPDATE ON StOrE_mEtA BEGIN
               UPDATE store_meta SET value = 'rewritten' WHERE key = 'last_migrated_by';
             END;"
        ),
    ] {
        let (_scratch, paths) = closed_store("schema-no-identity");
        replace_state_db(
            &paths,
            &format!("PRAGMA journal_mode = WAL; PRAGMA user_version = 1; {schema}"),
        );
        assert_corrupt_untouched(&paths, NO_IDENTITY);
    }
}

#[test]
fn a_negative_user_version_is_corrupt_and_untouched() {
    let (_scratch, paths) = closed_store("schema-negative");
    tamper(&paths, "PRAGMA user_version = -1;");
    assert_corrupt_untouched(&paths, "negative");
}

/// Open with `migrations` and expect the runner's error.
fn open_err(paths: &StorePaths, migrations: &[Migration]) -> StoreError {
    crate::store::open_with(
        paths,
        &test_seams(real_fs()),
        WRITER_PRAGMAS,
        crate::store::READER_PRAGMAS,
        &PINNED_SQLITE,
        migrations,
    )
    .unwrap_err()
}

/// A test-only migration 2 that leaves a CHECK violation, which
/// `foreign_key_check` can't see and `quick_check` does.
fn migration_2_bad_check(tx: &rusqlite::Transaction, _seams: &Seams) -> Result<(), StoreError> {
    tx.execute_batch(
        "PRAGMA ignore_check_constraints = ON;
         CREATE TABLE m2_check (x INTEGER CHECK (x = 1)) STRICT;
         INSERT INTO m2_check VALUES (2);
         PRAGMA ignore_check_constraints = OFF;",
    )
    .map_err(bug)
}

/// A `quick_check` finding rolls the migration back: the schema,
/// `user_version` and a prior `last_migrated_by` all survive, so the
/// runner wrote nothing outside the transaction.
#[test]
fn a_quick_check_finding_rolls_back_schema_and_metadata() {
    let (_scratch, paths) = closed_store("schema-quick-rollback");
    tamper(
        &paths,
        "UPDATE store_meta SET value = 'previous-app' WHERE key = 'last_migrated_by';",
    );
    let migrations = extra_migration(migration_2_bad_check);
    match open_err(&paths, &migrations) {
        StoreError::Corrupt { detail } => {
            assert!(
                detail.contains(&format!(
                    "migration {}'s quick_check",
                    LATEST_SCHEMA_VERSION + 1
                )),
                "{detail}"
            );
        }
        err => panic!("expected Corrupt, got {err:?}"),
    }

    let raw = Connection::open(paths.state_db()).unwrap();
    assert_eq!(
        one_int(&raw, "PRAGMA user_version"),
        i64::from(LATEST_SCHEMA_VERSION)
    );
    assert_eq!(store_meta(&raw)["last_migrated_by"], "previous-app");
    assert_eq!(
        one_int(
            &raw,
            "SELECT count(*) FROM sqlite_schema WHERE name = 'm2_check'"
        ),
        0
    );
}

/// A test-only migration 2 with invalid SQL after a valid statement.
fn migration_2_sql_bug(tx: &rusqlite::Transaction, _seams: &Seams) -> Result<(), StoreError> {
    tx.execute_batch("CREATE TABLE m2_partial (x INTEGER) STRICT; not_sql;")
        .map_err(bug)
}

/// A failed open doesn't checkpoint: WAL frames a crash left in a
/// valid store stay in the WAL, and `state.sqlite` is unchanged, when a
/// later step fails (here a migration bug).
#[test]
fn a_failed_open_leaves_crashed_wal_frames_in_the_wal() {
    let (_scratch, paths) = closed_store("schema-failed-open-crash");
    crash_write(&paths, "INSERT INTO store_meta VALUES ('crash', 'frame');");
    let before = fs::read(paths.state_db()).unwrap();
    let migrations = extra_migration(migration_2_sql_bug);
    let err = open_err(&paths, &migrations);
    assert!(matches!(err, StoreError::Internal { .. }), "{err:?}");
    assert_eq!(
        fs::read(paths.state_db()).unwrap(),
        before,
        "state.sqlite changed"
    );
}

/// A successful open lifts that: closing the store checkpoints, and
/// SQLite removes the emptied WAL.
#[test]
fn closing_an_opened_store_checkpoints() {
    let (_scratch, paths) = closed_store("schema-close-checkpoints");
    assert!(!paths.data_root().join("state.sqlite-wal").exists());
}

/// A bug in a migration is `Internal` and rolls back its partial work.
#[test]
fn a_migration_bug_is_internal_and_rolls_back() {
    let (_scratch, paths) = closed_store("schema-sql-bug");
    let migrations = extra_migration(migration_2_sql_bug);
    let err = open_err(&paths, &migrations);
    assert!(matches!(err, StoreError::Internal { .. }), "{err:?}");

    let raw = Connection::open(paths.state_db()).unwrap();
    assert_eq!(
        one_int(&raw, "PRAGMA user_version"),
        i64::from(LATEST_SCHEMA_VERSION)
    );
    assert_eq!(
        one_int(
            &raw,
            "SELECT count(*) FROM sqlite_schema WHERE name = 'm2_partial'"
        ),
        0
    );
}

/// A test-only migration 2 that changes nothing.
fn migration_2_noop(_tx: &rusqlite::Transaction, _seams: &Seams) -> Result<(), StoreError> {
    Ok(())
}

/// The full `integrity_check` after the run catches what `quick_check`
/// can't: an index whose entries don't match its table (made by
/// rewriting the index definition behind SQLite's back).
#[test]
fn the_full_integrity_check_runs_after_the_migrations() {
    let (_scratch, paths) = closed_store("schema-full-integrity");
    tamper(
        &paths,
        "CREATE TABLE m2_data (x INTEGER NOT NULL) STRICT;
         INSERT INTO m2_data VALUES (1), (2);
         CREATE INDEX m2_index ON m2_data (x);
         PRAGMA writable_schema = ON;
         UPDATE sqlite_schema SET sql = 'CREATE INDEX m2_index ON m2_data (-x)'
           WHERE name = 'm2_index';
         PRAGMA writable_schema = RESET;",
    );
    let raw = Connection::open(paths.state_db()).unwrap();
    assert_eq!(pragma_strings(&raw, "PRAGMA quick_check").unwrap(), ["ok"]);
    raw.close().unwrap();

    let migrations = extra_migration(migration_2_noop);
    match open_err(&paths, &migrations) {
        StoreError::Corrupt { detail } => {
            assert!(
                detail.contains("post-migration integrity_check"),
                "{detail}"
            );
            assert!(detail.contains("m2_index"), "{detail}");
        }
        err => panic!("expected Corrupt, got {err:?}"),
    }
}

/// Randomness that fails, as an exhausted entropy source would.
struct FailingRandom;

impl Random for FailingRandom {
    fn fill(&self, _dest: &mut [u8]) -> io::Result<()> {
        Err(io::Error::other("no entropy"))
    }
}

/// Randomness that always returns zeros: every UUID drawn is the same.
struct ConstantRandom;

impl Random for ConstantRandom {
    fn fill(&self, dest: &mut [u8]) -> io::Result<()> {
        dest.fill(0);
        Ok(())
    }
}

fn seams_with(random: impl Random + 'static) -> Seams {
    Seams::new(
        real_fs(),
        Arc::new(FixedClock::new(Timestamp::from_millis(FIXED_NOW_MS))),
        Arc::new(random),
    )
}

/// Without randomness migration 1 can't make its UUIDs: `Unavailable`,
/// rolled back, so a later open with working randomness succeeds.
#[test]
fn failing_randomness_is_unavailable_and_retryable() {
    let (_scratch, paths) = scratch_paths("schema-no-entropy");
    let err = crate::open(&paths, &seams_with(FailingRandom)).unwrap_err();
    assert!(matches!(err, StoreError::Unavailable { .. }), "{err:?}");

    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    assert_eq!(
        reading(&store, |tx| Ok(one_int(tx, "PRAGMA user_version"))),
        i64::from(LATEST_SCHEMA_VERSION)
    );
}

/// `store_uuid` and `incarnation` must differ; a seam that repeats
/// itself is a bug in the seam: `Internal`.
#[test]
fn repeating_randomness_is_internal() {
    let (_scratch, paths) = scratch_paths("schema-constant-random");
    let err = crate::open(&paths, &seams_with(ConstantRandom)).unwrap_err();
    assert!(matches!(err, StoreError::Internal { .. }), "{err:?}");
}

/// Each identity key is required on its own: missing any one is
/// `Corrupt`, untouched, also when the deletion sits in WAL frames a
/// crash left, which only a read-only verdict leaves in place.
#[test]
fn a_store_missing_one_identity_key_is_corrupt_and_untouched() {
    for key in IDENTITY_KEYS {
        let (_scratch, paths) = closed_store("schema-partial-identity");
        crash_write(
            &paths,
            &format!("DELETE FROM store_meta WHERE key = '{key}';"),
        );
        assert_corrupt_untouched(&paths, NO_IDENTITY);
    }

    // Foreign `store_meta` tables that a writer would fail on, with
    // crashed frames: a repeated key, a value of the wrong type.
    for (schema, row) in [
        (
            "CREATE TABLE store_meta (key TEXT NOT NULL, value TEXT NOT NULL) STRICT;
             INSERT INTO store_meta VALUES ('store_uuid', 'a'), ('store_uuid', 'b'), ('store_uuid', 'c');",
            "VALUES ('other', 'd')",
        ),
        (
            "CREATE TABLE store_meta (key TEXT PRIMARY KEY, value INTEGER NOT NULL) STRICT;
             INSERT INTO store_meta VALUES ('store_uuid', 1), ('incarnation', 2), ('last_migrated_by', 3);",
            "VALUES ('other', 4)",
        ),
        (
            "CREATE TABLE store_meta (key TEXT PRIMARY KEY,
               value TEXT GENERATED ALWAYS AS ('foreign') VIRTUAL NOT NULL) STRICT;
             INSERT INTO store_meta (key) VALUES ('store_uuid'), ('incarnation'), ('last_migrated_by');",
            "(key) VALUES ('other')",
        ),
    ] {
        let (_scratch, paths) = closed_store("schema-foreign-meta");
        replace_state_db(
            &paths,
            &format!("PRAGMA journal_mode = WAL; PRAGMA user_version = 1; {schema}"),
        );
        crash_write(&paths, &format!("INSERT INTO store_meta {row};"));
        assert_corrupt_untouched(&paths, NO_IDENTITY);
    }
}

/// The env vars that turn this test binary into a crashing writer: the
/// database to write and the SQL to commit. The harness itself moved
/// to `testing.rs` when the read-only open tests needed it too.
pub(crate) use crate::testing::crash_write;

/// A refused database stays untouched even when a crash left WAL
/// frames: open judges it read-only, and a read-only connection never
/// checkpoints them into the file.
#[test]
fn a_refused_database_with_crashed_wal_frames_stays_untouched() {
    let (_scratch, paths) = closed_store("schema-newer-crash");
    crash_write(
        &paths,
        &format!(
            "PRAGMA user_version = {};
             UPDATE store_meta SET value = '9.9.9' WHERE key = 'last_migrated_by';",
            LATEST_SCHEMA_VERSION + 1
        ),
    );
    let before = fs::read(paths.state_db()).unwrap();
    match crate::open(&paths, &test_seams(real_fs())).unwrap_err() {
        StoreError::NewerStore { app_version } => assert_eq!(app_version, "9.9.9"),
        err => panic!("expected NewerStore, got {err:?}"),
    }
    assert_eq!(
        fs::read(paths.state_db()).unwrap(),
        before,
        "state.sqlite changed"
    );

    let (_scratch, paths) = closed_store("schema-foreign-crash");
    replace_state_db(&paths, "PRAGMA journal_mode = WAL; CREATE TABLE t(x);");
    crash_write(&paths, "INSERT INTO t VALUES (1);");
    assert_corrupt_untouched(&paths, "holds a schema");
}

/// SQLite failures map onto spec/07's errors the same way in the runner
/// and in a migration: corruption, environment, bug.
#[test]
fn sqlite_failures_map_to_corrupt_unavailable_or_internal() {
    let failure = |code| rusqlite::Error::SqliteFailure(ffi::Error::new(code), None);
    for mapper in [step_err, bug] {
        for code in [ffi::SQLITE_CORRUPT, ffi::SQLITE_NOTADB] {
            let err = mapper(failure(code));
            assert!(matches!(err, StoreError::Corrupt { .. }), "{code}: {err:?}");
        }
        for code in [
            ffi::SQLITE_FULL,
            ffi::SQLITE_IOERR,
            ffi::SQLITE_IOERR_FSYNC,
            ffi::SQLITE_READONLY,
            ffi::SQLITE_CANTOPEN,
            ffi::SQLITE_PERM,
            ffi::SQLITE_BUSY,
            ffi::SQLITE_LOCKED,
            ffi::SQLITE_NOMEM,
            ffi::SQLITE_PROTOCOL,
        ] {
            let err = mapper(failure(code));
            assert!(
                matches!(err, StoreError::Unavailable { .. }),
                "{code}: {err:?}"
            );
        }
        for code in [ffi::SQLITE_ERROR, ffi::SQLITE_CONSTRAINT] {
            let err = mapper(failure(code));
            assert!(
                matches!(err, StoreError::Internal { .. }),
                "{code}: {err:?}"
            );
        }
    }
}

/// A store the runner can't write is `Unavailable`, not a bug.
#[test]
fn a_store_the_runner_cannot_write_is_unavailable() {
    let (_scratch, paths) = closed_store("schema-read-only");
    let mut raw =
        Connection::open_with_flags(paths.state_db(), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let err = run_migrations(&mut raw, &test_seams(real_fs()), MIGRATIONS).unwrap_err();
    assert!(matches!(err, StoreError::Unavailable { .. }), "{err:?}");
}

/// Appending a migration must not reinterpret an existing store version.
#[test]
fn historical_migration_ids_are_pinned() {
    assert_eq!([MIGRATIONS[0].version, MIGRATIONS[1].version], [1, 2]);
}

/// The runner relies on its list being strictly increasing; an
/// unordered or repeated version is a bug in the list: `Internal`.
#[test]
fn an_unordered_migration_list_is_internal() {
    let repeat = Migration {
        version: 1,
        run: migration_2_noop,
    };
    let two = Migration {
        version: 2,
        run: migration_2_noop,
    };
    for list in [&[MIGRATIONS[0], repeat][..], &[two, MIGRATIONS[0]][..]] {
        assert!(matches!(
            check_ordered(list),
            Err(StoreError::Internal { .. })
        ));
    }
    assert_eq!(check_ordered(MIGRATIONS).unwrap(), LATEST_SCHEMA_VERSION);
}

/// An index or trigger added to a real store's `store_meta` through a
/// crashed writer, so it exists only in WAL frames, is refused
/// untouched: neither `state.sqlite` nor the WAL changes.
#[test]
fn objects_added_to_store_meta_in_crashed_wal_are_refused() {
    for sql in [
        "CREATE UNIQUE INDEX foreign_index ON store_meta (value);",
        "CREATE INDEX foreign_index ON STORE_META (value);",
        "CREATE TRIGGER foreign_meta BEFORE INSERT ON StOrE_mEtA
           BEGIN SELECT RAISE(ABORT, 'no'); END;",
        "CREATE TRIGGER foreign_meta AFTER UPDATE ON STORE_META BEGIN
           UPDATE store_meta SET value = 'rewritten' WHERE key = 'last_migrated_by';
         END;",
    ] {
        let (_scratch, paths) = closed_store("schema-crashed-meta-object");
        // Distinct values, so the unique index can be built. The
        // clean-shutdown marker shares the fixed clock's `created_at`
        // value, and the thread watermarks repeat other counters, so
        // they move too.
        tamper(
            &paths,
            "UPDATE store_meta SET value = 'prior-opener' WHERE key = 'app_version_last_opened';
             UPDATE store_meta SET value = 'prior-shutdown' WHERE key = 'clean_shutdown_at';
             UPDATE store_meta SET value = 'prior-threads' WHERE key = 'thread_inputs';
             UPDATE store_meta SET value = 'prior-built' WHERE key = 'threads_built';
             UPDATE store_meta SET value = 'prior-index' WHERE key = 'index_inputs';
             UPDATE store_meta SET value = 'prior-search' WHERE key = 'search_generation';
             UPDATE store_meta SET value = 'prior-extractor' WHERE key = 'search_extractor_version';
             UPDATE store_meta SET value = 'prior-seen-target' WHERE key = 'seen_address_target';
             UPDATE store_meta SET value = 'prior-seen-inputs' WHERE key = 'seen_address_inputs';
              UPDATE store_meta SET value = 'prior-seen-swept' WHERE key = 'seen_address_swept';
               UPDATE store_meta SET value = 'prior-mime-target' WHERE key = 'mime_repair_target';
               UPDATE store_meta SET value = 'prior-' || key WHERE key IN('repair_inputs','repair_swept','repair_deferred','repair_unready');",
        );
        crash_write(&paths, sql);
        assert_corrupt_untouched(&paths, NO_IDENTITY);
    }
}

/// SQLite environment failures carry the `io::ErrorKind` their code
/// names; corruption and internal failures carry none, and an
/// ambiguous environment code is `Other`, never a cause guessed from
/// message text (spec/03 "Search writes").
#[test]
fn sqlite_environment_failures_carry_their_io_kind() {
    let failure = |code| rusqlite::Error::SqliteFailure(ffi::Error::new(code), None);
    for (code, kind) in [
        (ffi::SQLITE_FULL, io::ErrorKind::StorageFull),
        (ffi::SQLITE_PERM, io::ErrorKind::PermissionDenied),
        (ffi::SQLITE_READONLY, io::ErrorKind::ReadOnlyFilesystem),
        (ffi::SQLITE_BUSY, io::ErrorKind::ResourceBusy),
        (ffi::SQLITE_LOCKED, io::ErrorKind::ResourceBusy),
        (ffi::SQLITE_NOMEM, io::ErrorKind::OutOfMemory),
        (ffi::SQLITE_IOERR, io::ErrorKind::Other),
        (ffi::SQLITE_IOERR_FSYNC, io::ErrorKind::Other),
        (ffi::SQLITE_CANTOPEN, io::ErrorKind::Other),
        (ffi::SQLITE_PROTOCOL, io::ErrorKind::Other),
    ] {
        let err = classify(failure(code), "a test write");
        assert!(
            matches!(err, StoreError::Unavailable { .. }),
            "{code}: {err:?}"
        );
        assert_eq!(err.io_kind(), Some(kind), "{code}: {err:?}");
    }
    for code in [
        ffi::SQLITE_CORRUPT,
        ffi::SQLITE_NOTADB,
        ffi::SQLITE_ERROR,
        ffi::SQLITE_CONSTRAINT,
    ] {
        let err = classify(failure(code), "a test write");
        assert_eq!(err.io_kind(), None, "{code}: {err:?}");
    }
    let misleading = rusqlite::Error::SqliteFailure(
        ffi::Error::new(ffi::SQLITE_IOERR),
        Some("permission denied and disk is full".to_owned()),
    );
    assert_eq!(
        classify(misleading, "a test write").io_kind(),
        Some(io::ErrorKind::Other)
    );
}
