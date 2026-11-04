//! Atomic database transformation for a future isolated restore stage.
//! This does not establish that the database or supplied metadata is verified.
use crate::{StoreError, schema::classify};
use rusqlite::{Connection, params};
use std::path::{Component, Path};

#[derive(Clone, Copy)]
pub(super) struct Quarantine<'a> {
    pub new_incarnation: &'a str,
    pub generation: &'a str,
    pub source_location: &'a str,
    pub current_root: &'a Path,
    pub preserved_root: &'a Path,
    pub manifest_sha256: &'a str,
    pub object_count: i64,
    pub total_bytes: i64,
    pub completed_at: i64,
}

pub(super) fn apply(conn: &mut Connection, input: &Quarantine<'_>) -> Result<(), StoreError> {
    fn valid(path: &Path) -> bool {
        path.is_absolute()
            && path
                .components()
                .all(|part| matches!(part, Component::RootDir | Component::Normal(_)))
    }
    if !valid(input.current_root)
        || !valid(input.preserved_root)
        || !valid(Path::new(input.source_location))
    {
        return Err(StoreError::corrupt("invalid restore location"));
    }
    let incarnation: crate::Uuid = input
        .new_incarnation
        .parse()
        .map_err(|_| StoreError::corrupt("invalid restore incarnation"))?;
    if incarnation.to_string() != input.new_incarnation
        || input.object_count < 0
        || input.total_bytes < 0
    {
        return Err(StoreError::corrupt("invalid restore metadata"));
    }
    let tx = conn
        .transaction()
        .map_err(|e| classify(e, "begin restore quarantine"))?;
    let changed = tx
        .execute(
            "UPDATE store_meta SET value=?1 WHERE key='incarnation' AND value<>?1",
            [input.new_incarnation],
        )
        .map_err(|e| classify(e, "set restore incarnation"))?;
    if changed != 1 {
        return Err(StoreError::corrupt("missing or reused restore incarnation"));
    }
    let changed = tx.execute("UPDATE backup_generations SET state='complete', location=?2, manifest_sha256=?3, object_count=?4, total_bytes=?5, completed_at=?6 WHERE generation_id=?1 AND state='in_progress'", params![input.generation, input.source_location, input.manifest_sha256, input.object_count, input.total_bytes, input.completed_at]).map_err(|e| classify(e, "complete restored generation"))?;
    if changed != 1 {
        return Err(StoreError::corrupt(
            "restored generation missing or not in progress",
        ));
    }
    // Rewrite with Path components, never string prefixes. The selected
    // source is supplied by the future capability-validating caller.
    let mut stmt = tx
        .prepare("SELECT generation_id, location FROM backup_generations")
        .map_err(|e| classify(e, "read backup locations"))?;
    let mut rows = stmt
        .query([])
        .map_err(|e| classify(e, "read backup locations"))?;
    while let Some(row) = rows
        .next()
        .map_err(|e| classify(e, "read backup location"))?
    {
        let id: String = row.get(0).map_err(|e| classify(e, "read backup id"))?;
        let location: String = row
            .get(1)
            .map_err(|e| classify(e, "read backup location"))?;
        if !valid(Path::new(&location)) {
            return Err(StoreError::corrupt("invalid inherited backup location"));
        }
        if let Ok(relative) = Path::new(&location).strip_prefix(input.current_root) {
            let rewritten = input.preserved_root.join(relative);
            let text = rewritten
                .to_str()
                .ok_or_else(|| StoreError::corrupt("non-UTF-8 preserved backup location"))?;
            tx.execute(
                "UPDATE backup_generations SET location=?1 WHERE generation_id=?2",
                params![text, id],
            )
            .map_err(|e| classify(e, "rewrite backup location"))?;
        }
    }
    drop(rows);
    drop(stmt);
    tx.execute_batch("UPDATE accounts SET state='paused';
        UPDATE import_jobs SET state='failed' WHERE state IN ('inventory','awaiting_confirmation','running');
        UPDATE intents SET state='quarantined', quarantine_reason='restore' WHERE state IN ('pending','in_flight','parked');
        UPDATE send_attempts SET state='outcome_unknown' WHERE state IN ('held','queued','submitting');
        UPDATE backup_generations SET state='failed' WHERE state='in_progress';
        DELETE FROM object_pins WHERE pin_kind='backup_in_progress';
        DELETE FROM backup_cleanup_pending;
        DELETE FROM backup_ownership;
        DELETE FROM verifier_objects;
        DELETE FROM verifier_scratch;").map_err(|e| classify(e, "quarantine restored rows"))?;
    tx.commit()
        .map_err(|e| classify(e, "commit restore quarantine"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backups::BackupDestination;
    use crate::backups::tests::{draft_rows, open, sql};
    use rusqlite::Connection;

    #[test]
    fn quarantine_snapshot_preserves_requests_and_rewrites_locations() {
        let (_scratch, paths, store) = open("restore-quarantine");
        draft_rows(&store);
        sql(
            &store,
            "INSERT INTO import_jobs VALUES ('job','running','request','checkpoint',1,0,0);".into(),
        );
        sql(&store, "INSERT INTO intents (intent_id,account_id,config_generation,op,target_json,args_json,preconditions_json,state,idempotency_key,origin,created_at,updated_at) SELECT 'intent',account_id,1,'move','{}','{}','{}','pending','key','user',0,0 FROM accounts WHERE provider='generic_imap'; INSERT INTO send_attempts (attempt_id,draft_id,revision_id,account_id,config_generation,identity_id,mime_sha256,message_id_header,envelope_from,state,sent_copy,created_at,updated_at) SELECT 'attempt',draft_id,revision_id,account_id,1,identity_id,'sha','<m@x>','a@b','queued','pending',0,0 FROM draft_revisions;".into());
        let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
        let stage = paths.data_root().with_extension("quarantine-test.sqlite");
        std::fs::copy(snapshot.state_db(), &stage).unwrap();
        let mut conn = Connection::open(stage).unwrap();
        conn.execute_batch("INSERT INTO backup_generations (generation_id,started_at,state,location) VALUES ('other',0,'in_progress','/external/other'); INSERT INTO object_pins VALUES ('sha','backup_in_progress','other',0); INSERT INTO backup_cleanup_pending VALUES ('other'); INSERT INTO backup_ownership VALUES ('other','token');").unwrap();
        let inside = paths.data_root().join("backups/inside");
        conn.execute("INSERT INTO backup_generations (generation_id,started_at,state,location) VALUES ('inside',0,'complete',?1)", [inside.to_str().unwrap()]).unwrap();
        let prefix = format!("{}-elsewhere/backup", paths.data_root().display());
        conn.execute("INSERT INTO backup_generations (generation_id,started_at,state,location) VALUES ('prefix',0,'complete',?1)", [&prefix]).unwrap();
        let current = paths.data_root();
        let preserved = current.with_extension("preserved");
        let selected = "/external/new-source";
        let input = Quarantine {
            new_incarnation: "00000000-0000-4000-8000-000000000001",
            generation: snapshot.generation_id(),
            source_location: selected,
            current_root: current,
            preserved_root: &preserved,
            manifest_sha256: "manifest",
            object_count: 0,
            total_bytes: 12,
            completed_at: 1,
        };
        let old: String = conn
            .query_row(
                "SELECT value FROM store_meta WHERE key='incarnation'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            apply(
                &mut conn,
                &Quarantine {
                    generation: "wrong",
                    ..input
                }
            )
            .is_err()
        );
        assert!(
            apply(
                &mut conn,
                &Quarantine {
                    new_incarnation: &old,
                    ..input
                }
            )
            .is_err()
        );
        let after: String = conn
            .query_row(
                "SELECT value FROM store_meta WHERE key='incarnation'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(after, old);
        conn.execute_batch("CREATE TRIGGER fail_quarantine BEFORE UPDATE ON intents BEGIN SELECT RAISE(ABORT, 'injected'); END;").unwrap();
        assert!(apply(&mut conn, &input).is_err());
        let state: String = conn
            .query_row("SELECT state FROM import_jobs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            state, "running",
            "SQL failure rolls back the whole quarantine"
        );
        conn.execute_batch("DROP TRIGGER fail_quarantine").unwrap();
        apply(&mut conn, &input).unwrap();
        let job: (String, String, String) = conn
            .query_row(
                "SELECT state, request_json, checkpoint_json FROM import_jobs",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            job,
            ("failed".into(), "request".into(), "checkpoint".into())
        );
        let intent: (String, String) = conn
            .query_row("SELECT state,quarantine_reason FROM intents", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(intent, ("quarantined".into(), "restore".into()));
        let attempt: String = conn
            .query_row("SELECT state FROM send_attempts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(attempt, "outcome_unknown");
        let active: i64 = conn
            .query_row(
                "SELECT count(*) FROM accounts WHERE state<>'paused'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(active, 0);
        let generation: (String, String) = conn
            .query_row(
                "SELECT state, location FROM backup_generations WHERE generation_id=?1",
                [snapshot.generation_id()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(generation, ("complete".into(), selected.into()));
        let inherited: (String, String) = conn
            .query_row(
                "SELECT state, location FROM backup_generations WHERE generation_id='other'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(inherited, ("failed".into(), "/external/other".into()));
        let location = |id: &str| -> String {
            conn.query_row(
                "SELECT location FROM backup_generations WHERE generation_id=?1",
                [id],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(
            location("inside"),
            preserved.join("backups/inside").to_str().unwrap()
        );
        assert_eq!(location("prefix"), prefix);
        assert!(
            apply(&mut conn, &input).is_err(),
            "a retry cannot qualify this database"
        );
        for table in [
            "object_pins",
            "backup_cleanup_pending",
            "backup_ownership",
            "verifier_objects",
            "verifier_scratch",
        ] {
            let count: i64 = conn
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 0, "{table}");
        }
    }
}
