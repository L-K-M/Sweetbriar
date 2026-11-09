use super::*;
use crate::backups::BackupDestination;
use crate::backups::tests::{draft_rows, open};

fn staged(test: impl FnOnce(&mut Connection, &Quarantine<'_>)) {
    let (_scratch, paths, store) = open("quarantine-corrupt-index");
    draft_rows(&store);
    let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    let path = paths.data_root().with_extension("corrupt-index.sqlite");
    std::fs::copy(snapshot.state_db(), &path).unwrap();
    let mut conn = Connection::open(path).unwrap();
    conn.execute_batch("INSERT INTO send_attempts(attempt_id,draft_id,revision_id,account_id,config_generation,identity_id,mime_sha256,message_id_header,envelope_from,state,sent_copy,created_at,updated_at) SELECT 'attempt',draft_id,revision_id,account_id,1,identity_id,'sha','<m@x>','a@b','submitting','pending',0,0 FROM draft_revisions; INSERT INTO intents(intent_id,account_id,config_generation,op,target_json,args_json,preconditions_json,state,idempotency_key,origin,created_at,updated_at) SELECT 'intent',account_id,1,'move','{}','{}','{}','pending','intent','user',0,0 FROM accounts WHERE provider='generic_imap'; INSERT INTO object_pins VALUES('sha','reservation','old',0);").unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let input = Quarantine {
        new_incarnation: "00000000-0000-4000-8000-000000000001",
        generation: snapshot.generation_id(),
        source_location: "/external/source",
        current_root: paths.data_root(),
        preserved_root: Path::new("/preserved"),
        manifest_sha256: "digest",
        object_count: 0,
        total_bytes: 0,
        completed_at: 1,
        cancel: &cancel,
        max_work_calls: 2_000,
    };
    test(&mut conn, &input);
}

// Keep the declared index schema intact while replacing its B-tree with a
// self-consistent but incomplete tree, as an inherited corrupt image can do.
fn redirect_index(conn: &Connection, index: &str, replacement: &str) {
    let root: i64 = conn
        .query_row(
            "SELECT rootpage FROM sqlite_schema WHERE name=?1",
            [replacement],
            |row| row.get(0),
        )
        .unwrap();
    let original: i64 = conn
        .query_row(
            "SELECT rootpage FROM sqlite_schema WHERE name=?1",
            [index],
            |row| row.get(0),
        )
        .unwrap();
    conn.execute_batch("PRAGMA writable_schema=ON").unwrap();
    assert_eq!(
        conn.execute(
            "UPDATE sqlite_schema SET rootpage=CASE name WHEN ?1 THEN ?2 ELSE ?3 END WHERE name IN (?1,?4)",
            params![index, root, original, replacement]
        )
        .unwrap(),
        2
    );
    conn.execute_batch("PRAGMA writable_schema=RESET").unwrap();
}

fn filtered_write(
    table: &str,
    index: &str,
    column: &str,
    predicate: &str,
    unsafe_rows: &str,
    existing: bool,
) {
    staged(|conn, input| {
        if !existing {
            conn.execute_batch(&format!("CREATE INDEX {index} ON {table}({column})"))
                .unwrap();
        }
        conn.execute_batch(&format!(
            "CREATE INDEX empty_tree ON {table}({column}) WHERE 0"
        ))
        .unwrap();
        redirect_index(conn, index, "empty_tree");
        let physical: i64 = conn
            .query_row(
                &format!("SELECT count(*) FROM {table} NOT INDEXED WHERE {predicate}"),
                [],
                |row| row.get(0),
            )
            .unwrap();
        let indexed: i64 = conn
            .query_row(
                &format!("SELECT count(*) FROM {table} INDEXED BY {index} WHERE {predicate}"),
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(physical > 0);
        assert_eq!(indexed, 0, "the inherited index must hide real rows");
        // Refusing the corrupt image is safe. Success must transform all
        // physical rows, never merely the subset an index claims exists.
        if apply(conn, input).is_ok() {
            let remaining: i64 = conn
                .query_row(
                    &format!("SELECT count(*) FROM {table} NOT INDEXED WHERE {unsafe_rows}"),
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(remaining, 0, "unsafe quarantine skipped {table}");
        }
    });
}

#[test]
fn incomplete_live_attempt_index_cannot_hide_submitting_mail() {
    filtered_write(
        "send_attempts",
        "send_attempts_live",
        "revision_id",
        "state IN ('held','queued','submitting') OR (state='outcome_unknown' AND resolution IS NULL)",
        "state IN ('held','queued','submitting')",
        true,
    );
}

#[test]
fn incomplete_intent_index_cannot_preserve_runnable_work() {
    filtered_write(
        "intents",
        "inherited_intents",
        "state",
        "state IN ('pending','in_flight','parked')",
        "state IN ('pending','in_flight','parked')",
        false,
    );
}

#[test]
fn incomplete_pin_index_cannot_preserve_publication_authority() {
    filtered_write(
        "object_pins",
        "inherited_pins",
        "pin_kind",
        "pin_kind IN ('reservation','backup_in_progress')",
        "pin_kind IN ('reservation','backup_in_progress')",
        false,
    );
}

#[test]
fn incomplete_generation_index_cannot_preserve_in_progress_rows() {
    filtered_write(
        "backup_generations",
        "inherited_states",
        "state",
        "state='in_progress'",
        "state='in_progress'",
        false,
    );
}

#[test]
fn shadowed_rowid_cannot_redirect_location_updates() {
    for alias in ["rowid", "_rowid_", "oid"] {
        staged(|conn, input| {
            // SQLite 3.54 refuses ALTER TABLE ... ADD COLUMN of a rowid
            // alias, but a hostile image can still declare one: write it
            // into the schema directly, as ADD COLUMN would (no row is
            // rewritten), and reload.
            conn.execute_batch(&format!(
                "PRAGMA writable_schema=ON;
                 UPDATE sqlite_schema
                 SET sql=replace(sql, ') STRICT', ', {alias} INTEGER DEFAULT 100) STRICT')
                 WHERE type='table' AND name='backup_generations';
                 PRAGMA writable_schema=RESET;"
            ))
            .unwrap();
            let shadowed = conn
                .prepare("SELECT * FROM backup_generations")
                .unwrap()
                .column_names()
                .any(|name| name == alias);
            assert!(shadowed, "the fixture declares {alias}");
            assert!(apply(conn, input).is_err(), "accepted shadow alias {alias}");
        });
    }
}

#[test]
fn physical_location_cursor_rewrites_every_row_while_rows_grow() {
    staged(|conn, input| {
        let preserved = std::path::PathBuf::from(format!(
            "/preserved/{}/{}",
            "p".repeat(128),
            "q".repeat(128)
        ));
        assert!(preserved.as_os_str().len() > input.current_root.as_os_str().len());
        let growing = Quarantine {
            preserved_root: &preserved,
            ..*input
        };
        for n in 0..301 {
            let location = input.current_root.join(format!("backups/generation-{n}"));
            conn.execute("INSERT INTO backup_generations(generation_id,started_at,state,location) VALUES(?1,0,'complete',?2)", params![format!("generation-{n}"), location.to_str().unwrap()]).unwrap();
        }
        apply(conn, &growing).unwrap();
        let pattern = format!("{}/backups/generation-%", preserved.display());
        let count: i64 = conn
            .query_row(
                "SELECT count(*) FROM backup_generations NOT INDEXED WHERE location LIKE ?1",
                [pattern],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 301);
    });
}

#[test]
fn incomplete_primary_index_cannot_hide_backup_locations() {
    staged(|conn, input| {
        let old = input.current_root.join("backups/hidden");
        conn.execute("INSERT INTO backup_generations(generation_id,started_at,state,location) VALUES('hidden',0,'complete',?1)", [old.to_str().unwrap()]).unwrap();
        conn.execute_batch("CREATE INDEX partial_locations ON backup_generations(generation_id) WHERE generation_id<>'hidden'").unwrap();
        redirect_index(
            conn,
            "sqlite_autoindex_backup_generations_1",
            "partial_locations",
        );
        let physical: i64 = conn
            .query_row(
                "SELECT count(*) FROM backup_generations NOT INDEXED WHERE generation_id='hidden'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let indexed: i64 = conn.query_row("SELECT count(*) FROM backup_generations INDEXED BY sqlite_autoindex_backup_generations_1 WHERE generation_id='hidden'", [], |r| r.get(0)).unwrap();
        assert_eq!((physical, indexed), (1, 0));
        if apply(conn, input).is_ok() {
            let location: String = conn.query_row("SELECT location FROM backup_generations NOT INDEXED WHERE generation_id='hidden'", [], |r| r.get(0)).unwrap();
            assert_eq!(
                Path::new(&location),
                input.preserved_root.join("backups/hidden")
            );
        }
    });
}
