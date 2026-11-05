//! Atomic database transformation for a future isolated restore stage.
//! This does not establish that the database or supplied metadata is verified.
use crate::{StoreError, schema::classify};
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use rusqlite::{Connection, OptionalExtension, Params, Transaction, params};
use std::path::{Component, Path};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

const FIRST_LOCATION: &str =
    "SELECT generation_id, location FROM backup_generations ORDER BY generation_id LIMIT 1";
const NEXT_LOCATION: &str = "SELECT generation_id, location FROM backup_generations WHERE generation_id > ?1 ORDER BY generation_id LIMIT 1";

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
    pub cancel: &'a Arc<AtomicBool>,
}

// The caller supplies a fresh, exclusively owned connection without hooks or
// cached statements or progress handler. Both hooks are removed after commit or
// rollback. Trigger and view programs cannot participate. Work is proportional
// to inherited rows, with cancellation checked inside SQLite and between pages;
// the future handoff must also check cancellation before qualifying the stage.
pub(super) fn apply(conn: &mut Connection, input: &Quarantine<'_>) -> Result<(), StoreError> {
    conn.authorizer(Some(authorize))
        .map_err(|e| classify(e, "install restore authorizer"))?;
    let cancel = Arc::clone(input.cancel);
    if let Err(error) = conn.progress_handler(1000, Some(move || cancel.load(Ordering::Acquire))) {
        conn.authorizer(None::<fn(AuthContext<'_>) -> Authorization>)
            .map_err(|e| classify(e, "remove restore authorizer"))?;
        return Err(classify(error, "install restore cancellation"));
    }
    let result = apply_inner(conn, input);
    let progress = conn.progress_handler(0, None::<fn() -> bool>);
    let authorizer = conn.authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
    progress.map_err(|e| classify(e, "remove restore cancellation"))?;
    authorizer.map_err(|e| classify(e, "remove restore authorizer"))?;
    result
}

fn authorize(ctx: AuthContext<'_>) -> Authorization {
    use AuthAction::*;
    if ctx.accessor.is_some() {
        return Authorization::Deny;
    }
    let allowed = match ctx.action {
        Select | Transaction { .. } => true,
        Read { .. } => ctx.database_name == Some("main"),
        Update {
            table_name,
            column_name,
        } if ctx.database_name == Some("main") => matches!(
            (table_name, column_name),
            ("store_meta", "value")
                | ("accounts", "state")
                | ("import_jobs", "state")
                | ("intents", "state" | "quarantine_reason")
                | ("send_attempts", "state")
                | (
                    "backup_generations",
                    "state"
                        | "location"
                        | "manifest_sha256"
                        | "object_count"
                        | "total_bytes"
                        | "completed_at"
                )
        ),
        Delete { table_name } if ctx.database_name == Some("main") => matches!(
            table_name,
            "object_pins"
                | "backup_cleanup_pending"
                | "backup_ownership"
                | "verifier_objects"
                | "verifier_scratch"
        ),
        _ => false,
    };
    if allowed {
        Authorization::Allow
    } else {
        Authorization::Deny
    }
}

// SQLite's changes excludes foreign-key actions, while total_changes includes
// them. Reject even an indirect action on an otherwise permitted column.
fn execute(tx: &Transaction<'_>, sql: &str, args: impl Params) -> Result<usize, StoreError> {
    let before = tx.total_changes();
    let changed = tx
        .execute(sql, args)
        .map_err(|e| classify(e, "quarantine restored rows"))?;
    if tx.total_changes().checked_sub(before) != Some(changed as u64) {
        return Err(StoreError::corrupt("indirect restore quarantine write"));
    }
    Ok(changed)
}

fn apply_inner(conn: &mut Connection, input: &Quarantine<'_>) -> Result<(), StoreError> {
    if input.cancel.load(Ordering::Acquire) {
        return Err(StoreError::unavailable("restore quarantine cancelled"));
    }
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
    // A snapshot can define REPLACE or IGNORE conflict policies. Override
    // them on every update so no row is silently deleted or left unchanged.
    let changed = execute(
        &tx,
        "UPDATE OR ABORT store_meta SET value=?1 WHERE key='incarnation' AND value<>?1",
        [input.new_incarnation],
    )?;
    if changed != 1 {
        return Err(StoreError::corrupt("missing or reused restore incarnation"));
    }
    execute(
        &tx,
        "DELETE FROM backup_ownership WHERE generation_id IN (SELECT generation_id FROM backup_generations WHERE state='in_progress')",
        [],
    )?;
    let changed = execute(
        &tx,
        "UPDATE OR ABORT backup_generations SET state='complete', location=?2, manifest_sha256=?3, object_count=?4, total_bytes=?5, completed_at=?6 WHERE generation_id=?1 AND state='in_progress'",
        params![
            input.generation,
            input.source_location,
            input.manifest_sha256,
            input.object_count,
            input.total_bytes,
            input.completed_at
        ],
    )?;
    if changed != 1 {
        return Err(StoreError::corrupt(
            "restored generation missing or not in progress",
        ));
    }
    // Rewrite with Path components, never string prefixes. The selected
    // source is supplied by the future capability-validating caller.
    let mut after: Option<String> = None;
    loop {
        if input.cancel.load(Ordering::Acquire) {
            return Err(StoreError::unavailable("restore quarantine cancelled"));
        }
        // Finish each read before changing the table; memory stays bounded.
        let next: Option<(String, String)> = match &after {
            None => tx.query_row(FIRST_LOCATION, [], |row| Ok((row.get(0)?, row.get(1)?))),
            Some(id) => tx.query_row(NEXT_LOCATION, [id], |row| Ok((row.get(0)?, row.get(1)?))),
        }
        .optional()
        .map_err(|e| classify(e, "read backup location"))?;
        let Some((id, location)) = next else { break };
        if !valid(Path::new(&location)) {
            return Err(StoreError::corrupt("invalid inherited backup location"));
        }
        if let Ok(relative) = Path::new(&location).strip_prefix(input.current_root) {
            let rewritten = input.preserved_root.join(relative);
            let text = rewritten
                .to_str()
                .ok_or_else(|| StoreError::corrupt("non-UTF-8 preserved backup location"))?;
            execute(
                &tx,
                "UPDATE OR ABORT backup_generations SET location=?1 WHERE generation_id=?2",
                params![text, id],
            )?;
        }
        after = Some(id);
    }
    for sql in [
        "UPDATE OR ABORT accounts SET state='paused'",
        "UPDATE OR ABORT import_jobs SET state='failed' WHERE state IN ('inventory','awaiting_confirmation','running')",
        "UPDATE OR ABORT intents SET state='quarantined', quarantine_reason='restore' WHERE state IN ('pending','in_flight','parked')",
        "UPDATE OR ABORT send_attempts SET state='outcome_unknown' WHERE state IN ('held','queued','submitting')",
        "UPDATE OR ABORT backup_generations SET state='failed' WHERE state='in_progress'",
        "DELETE FROM object_pins WHERE pin_kind='backup_in_progress'",
        "DELETE FROM backup_cleanup_pending",
        "DELETE FROM verifier_objects",
        "DELETE FROM verifier_scratch",
    ] {
        execute(&tx, sql, [])?;
    }
    if input.cancel.load(Ordering::Acquire) {
        return Err(StoreError::unavailable("restore quarantine cancelled"));
    }
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
        conn.execute(
            "INSERT INTO backup_ownership VALUES ('inside','retained-token')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO object_pins VALUES ('other-sha','quarantine','keep',0)",
            [],
        )
        .unwrap();
        let prefix = format!("{}-elsewhere/backup", paths.data_root().display());
        conn.execute("INSERT INTO backup_generations (generation_id,started_at,state,location) VALUES ('prefix',0,'complete',?1)", [&prefix]).unwrap();
        let current = paths.data_root();
        let preserved = current.with_extension("preserved");
        let selected = "/external/new-source";
        let cancel = Arc::new(AtomicBool::new(false));
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
            cancel: &cancel,
        };
        cancel.store(true, Ordering::Release);
        assert!(apply(&mut conn, &input).is_err());
        cancel.store(false, Ordering::Release);
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
        conn.execute_batch("CREATE TRIGGER fail_quarantine BEFORE DELETE ON verifier_scratch BEGIN SELECT RAISE(ABORT, 'injected'); END;").unwrap();
        assert!(apply(&mut conn, &input).is_err());
        let state: String = conn
            .query_row("SELECT state FROM import_jobs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            state, "running",
            "SQL failure rolls back the whole quarantine"
        );
        let state: String = conn
            .query_row(
                "SELECT state FROM backup_generations WHERE generation_id=?1",
                [snapshot.generation_id()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(state, "in_progress");
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
            "backup_cleanup_pending",
            "verifier_objects",
            "verifier_scratch",
        ] {
            let count: i64 = conn
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 0, "{table}");
        }
        let owners: Vec<(String, String)> = conn
            .prepare("SELECT generation_id, token FROM backup_ownership")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(owners, vec![("inside".into(), "retained-token".into())]);
        let pin: String = conn
            .query_row("SELECT pin_kind FROM object_pins", [], |r| r.get(0))
            .unwrap();
        assert_eq!(pin, "quarantine");
    }

    #[test]
    fn rejects_trigger_that_reactivates_account() {
        let (_scratch, paths, store) = open("restore-hostile-trigger");
        let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
        let stage = paths.data_root().with_extension("hostile-trigger.sqlite");
        std::fs::copy(snapshot.state_db(), &stage).unwrap();
        let mut conn = Connection::open(stage).unwrap();
        conn.execute_batch("CREATE TRIGGER hostile AFTER UPDATE OF state ON accounts BEGIN UPDATE accounts SET state='active' WHERE account_id=NEW.account_id; END;").unwrap();
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
        };
        assert!(apply(&mut conn, &input).is_err());
        let state: String = conn
            .query_row(
                "SELECT state FROM backup_generations WHERE generation_id=?1",
                [snapshot.generation_id()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(state, "in_progress");
        conn.execute_batch("DROP TRIGGER hostile").unwrap();
        apply(&mut conn, &input).unwrap();
    }

    #[test]
    fn rejects_foreign_key_cascade_and_rolls_back() {
        let (_scratch, paths, store) = open("restore-hostile-cascade");
        let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
        let stage = paths.data_root().with_extension("hostile-cascade.sqlite");
        std::fs::copy(snapshot.state_db(), &stage).unwrap();
        let mut conn = Connection::open(stage).unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON; CREATE UNIQUE INDEX hostile_location ON backup_generations(location); CREATE TABLE hostile_reference(location TEXT REFERENCES backup_generations(location) ON UPDATE CASCADE); INSERT INTO hostile_reference SELECT location FROM backup_generations;").unwrap();
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
        };
        assert!(apply(&mut conn, &input).is_err());
        let (state, location): (String, String) = conn
            .query_row(
                "SELECT state,location FROM backup_generations WHERE generation_id=?1",
                [snapshot.generation_id()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, "in_progress");
        assert_eq!(location, snapshot.location().to_str().unwrap());
        let child: String = conn
            .query_row("SELECT location FROM hostile_reference", [], |r| r.get(0))
            .unwrap();
        assert_eq!(child, location);
    }

    fn rejects_schema_conflict_policy(policy: &str) {
        let (_scratch, paths, store) = open("restore-conflict-policy");
        let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
        let stage = paths.data_root().with_extension("conflict.sqlite");
        std::fs::copy(snapshot.state_db(), &stage).unwrap();
        let mut conn = Connection::open(stage).unwrap();
        conn.execute_batch(&format!("PRAGMA foreign_keys=OFF;
                CREATE TABLE new_accounts (account_id TEXT PRIMARY KEY, kind TEXT NOT NULL, provider TEXT NOT NULL, display_name TEXT NOT NULL, state TEXT NOT NULL UNIQUE ON CONFLICT {policy}, state_reason TEXT, config_generation INTEGER NOT NULL DEFAULT 1, sort_order INTEGER NOT NULL, created_at INTEGER NOT NULL, revision INTEGER NOT NULL DEFAULT 1) STRICT;
                INSERT INTO new_accounts SELECT * FROM accounts;
                INSERT INTO new_accounts (account_id,kind,provider,display_name,state,sort_order,created_at) VALUES ('other','imap','generic_imap','other','paused',0,0);
                DROP TABLE accounts; ALTER TABLE new_accounts RENAME TO accounts;")).unwrap();
        let before: Vec<(String, String)> = conn
            .prepare("SELECT account_id,state FROM accounts ORDER BY account_id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(before.len(), 2);
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
        };
        assert!(apply(&mut conn, &input).is_err(), "{policy} must abort");
        let after: Vec<(String, String)> = conn
            .prepare("SELECT account_id,state FROM accounts ORDER BY account_id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(after, before);
        let state: String = conn
            .query_row(
                "SELECT state FROM backup_generations WHERE generation_id=?1",
                [snapshot.generation_id()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(state, "in_progress");
    }
    #[test]
    fn rejects_replace_conflict_policy() {
        rejects_schema_conflict_policy("REPLACE");
    }
    #[test]
    fn rejects_ignore_conflict_policy() {
        rejects_schema_conflict_policy("IGNORE");
    }

    #[test]
    fn location_paging_uses_generation_index_range() {
        let (_scratch, _paths, store) = open("restore-location-plan");
        let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
        let conn = Connection::open(snapshot.state_db()).unwrap();
        let plan: String = conn
            .query_row(
                &format!("EXPLAIN QUERY PLAN {NEXT_LOCATION}"),
                ["cursor"],
                |r| r.get(3),
            )
            .unwrap();
        assert!(
            plan.contains("SEARCH backup_generations") && plan.contains("generation_id>?"),
            "{plan}"
        );
    }

    #[test]
    fn all_job_intent_and_attempt_states_are_quarantined_selectively() {
        let (_scratch, paths, store) = open("restore-all-states");
        draft_rows(&store);
        let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
        let stage = paths.data_root().with_extension("all-states.sqlite");
        std::fs::copy(snapshot.state_db(), &stage).unwrap();
        let mut conn = Connection::open(stage).unwrap();
        for state in [
            "inventory",
            "awaiting_confirmation",
            "running",
            "completed",
            "cancelled",
            "failed",
        ] {
            conn.execute(
                "INSERT INTO import_jobs VALUES (?1,?1,'request','checkpoint',1,0,0)",
                [state],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO import_job_sources VALUES (?1,0,'source','position')",
                [state],
            )
            .unwrap();
        }
        for state in [
            "pending",
            "in_flight",
            "parked",
            "done",
            "quarantined",
            "cancelled",
        ] {
            conn.execute("INSERT INTO intents (intent_id,account_id,config_generation,op,target_json,args_json,preconditions_json,state,idempotency_key,origin,created_at,updated_at) SELECT ?1,account_id,1,'move','{}','{}','{}',?1,?1,'user',0,0 FROM accounts WHERE provider='generic_imap'", [state]).unwrap();
        }
        for (i, state) in [
            "held",
            "queued",
            "submitting",
            "accepted",
            "failed",
            "outcome_unknown",
            "cancelled",
        ]
        .iter()
        .enumerate()
        {
            let id = format!("revision-{i}");
            conn.execute("INSERT INTO draft_revisions (revision_id,draft_id,seq,account_id,identity_id,compose_format,subject,to_json,cc_json,bcc_json,body_source,attachments_json,created_at) SELECT ?1,draft_id,?2,account_id,identity_id,'plain','','[]','[]','[]','','[]',0 FROM draft_revisions LIMIT 1", params![id, i as i64 + 2]).unwrap();
            conn.execute("INSERT INTO send_attempts (attempt_id,draft_id,revision_id,account_id,config_generation,identity_id,mime_sha256,message_id_header,envelope_from,state,sent_copy,created_at,updated_at) SELECT ?1,draft_id,revision_id,account_id,1,identity_id,'sha','<m@x>','a@b',?1,'pending',0,0 FROM draft_revisions WHERE revision_id=?2", params![state, id]).unwrap();
        }
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
        };
        apply(&mut conn, &input).unwrap();
        for state in [
            "inventory",
            "awaiting_confirmation",
            "running",
            "completed",
            "cancelled",
            "failed",
        ] {
            let row: (String, String, String, String, String) = conn.query_row("SELECT j.state,j.request_json,j.checkpoint_json,s.source_json,s.checkpoint_json FROM import_jobs j JOIN import_job_sources s USING(job_id) WHERE j.job_id=?1", [state], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).unwrap();
            let expected = if ["inventory", "awaiting_confirmation", "running"].contains(&state) {
                "failed"
            } else {
                state
            };
            assert_eq!(
                row,
                (
                    expected.into(),
                    "request".into(),
                    "checkpoint".into(),
                    "source".into(),
                    "position".into()
                )
            );
        }
        for state in [
            "pending",
            "in_flight",
            "parked",
            "done",
            "quarantined",
            "cancelled",
        ] {
            let row: (String, Option<String>) = conn
                .query_row(
                    "SELECT state,quarantine_reason FROM intents WHERE intent_id=?1",
                    [state],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            let affected = ["pending", "in_flight", "parked"].contains(&state);
            assert_eq!(
                row,
                (
                    (if affected { "quarantined" } else { state }).into(),
                    affected.then(|| "restore".into())
                )
            );
        }
        for state in [
            "held",
            "queued",
            "submitting",
            "accepted",
            "failed",
            "outcome_unknown",
            "cancelled",
        ] {
            let actual: String = conn
                .query_row(
                    "SELECT state FROM send_attempts WHERE attempt_id=?1",
                    [state],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                actual,
                if ["held", "queued", "submitting"].contains(&state) {
                    "outcome_unknown"
                } else {
                    state
                }
            );
        }
    }
}
