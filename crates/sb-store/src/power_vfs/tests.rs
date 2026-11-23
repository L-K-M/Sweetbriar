//! The power-loss VFS on its own (spec/11 "Power loss"), then a store's
//! draft saves cut at each of their operations.

use super::*;
use crate::drafts::{
    ComposeFormat, DraftRecipient, DraftSource, RevisionInput, create_draft, draft, save_draft,
};
use crate::schema::classify;
use crate::testing::{FIXED_NOW_MS, copy_tree, scratch_paths, test_seams};
use crate::{Store, StorePaths, Timestamp, WritePriority, open};
use rusqlite::{Connection, OpenFlags};
use sb_test_support::{ScratchDir, block_on};

fn connect(path: &Path, synchronous: &str) -> rusqlite::Result<Connection> {
    let conn = Connection::open_with_flags_and_vfs(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        NAME,
    )?;
    conn.execute_batch(&format!(
        "PRAGMA synchronous = {synchronous}; PRAGMA journal_mode = WAL; \
         PRAGMA wal_autocheckpoint = 4;"
    ))?;
    Ok(conn)
}

fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |row| row.get(0)).unwrap()
}

fn integrity(conn: &Connection) -> String {
    conn.query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap()
}

/// Commits SQLite never synced are gone after a cut, though every
/// connection saw them before it.
#[test]
fn a_cut_drops_what_no_sync_applied() {
    let scratch = ScratchDir::new("power-unsynced");
    let supply = PowerSupply::arm(scratch.path()).unwrap();
    let db = scratch.path().join("state.sqlite");
    let conn = connect(&db, "OFF").unwrap();
    conn.execute_batch("CREATE TABLE t (x); INSERT INTO t VALUES (1);")
        .unwrap();
    let other = connect(&db, "OFF").unwrap();
    assert_eq!(count(&other, "SELECT count(*) FROM t"), 1);
    assert!(supply.unsynced() > 0);
    assert_eq!(std::fs::metadata(&db).unwrap().len(), 0);

    supply.cut();
    assert!(conn.execute("INSERT INTO t VALUES (2)", []).is_err());
    drop((conn, other));
    supply.restore();
    let reopened = connect(&db, "OFF").unwrap();
    assert_eq!(count(&reopened, "SELECT count(*) FROM sqlite_master"), 0);
    assert_eq!(integrity(&reopened), "ok");
}

/// What a sync applied survives the cut.
#[test]
fn synced_commits_survive_a_cut() {
    let scratch = ScratchDir::new("power-synced");
    let supply = PowerSupply::arm(scratch.path()).unwrap();
    let db = scratch.path().join("state.sqlite");
    let conn = connect(&db, "FULL").unwrap();
    conn.execute_batch("CREATE TABLE t (x);").unwrap();
    for x in 0..20 {
        conn.execute("INSERT INTO t VALUES (?1)", [x]).unwrap();
    }
    assert!(supply.operations() > 0);
    supply.cut();
    drop(conn);
    supply.restore();
    let reopened = connect(&db, "FULL").unwrap();
    assert_eq!(count(&reopened, "SELECT count(*) FROM t"), 20);
    assert_eq!(integrity(&reopened), "ok");
}

/// Files outside every armed root pass straight to the default VFS.
#[test]
fn unarmed_files_pass_through() {
    let scratch = ScratchDir::new("power-unarmed");
    let armed = ScratchDir::new("power-armed-elsewhere");
    let supply = PowerSupply::arm(armed.path()).unwrap();
    let db = scratch.path().join("state.sqlite");
    let conn = connect(&db, "OFF").unwrap();
    conn.execute_batch("CREATE TABLE t (x); INSERT INTO t VALUES (1);")
        .unwrap();
    assert!(std::fs::metadata(&db).unwrap().len() > 0);
    assert_eq!(supply.unsynced(), 0);
    assert_eq!(name_for(&db), None);
    assert_eq!(name_for(&armed.path().join("state.sqlite")), Some(NAME));
    assert!(PowerSupply::arm(&armed.path().join("nested")).is_err());
}

/// A cut at every write, truncation or sync of a run of commits keeps
/// every acknowledged one, with checkpoints among them.
#[test]
fn a_cut_at_every_operation_keeps_each_acknowledged_commit() {
    const COMMITS: i64 = 12;
    fn run(dir: &Path, cut: Option<u64>) -> (Vec<i64>, u64) {
        let supply = PowerSupply::arm(dir).unwrap();
        if let Some(n) = cut {
            supply.cut_after(n);
        }
        let mut acknowledged = Vec::new();
        let db = dir.join("state.sqlite");
        if let Ok(conn) = connect(&db, "FULL")
            && conn
                .execute_batch("CREATE TABLE IF NOT EXISTS t (x)")
                .is_ok()
        {
            for x in 0..COMMITS {
                if conn.execute("INSERT INTO t VALUES (?1)", [x]).is_err() {
                    break;
                }
                acknowledged.push(x);
            }
        }
        let operations = supply.operations();
        supply.restore();
        (acknowledged, operations)
    }

    let whole = ScratchDir::new("power-sweep-whole");
    let (all, total) = run(whole.path(), None);
    assert_eq!(all.len() as i64, COMMITS);
    for n in 0..total {
        let scratch = ScratchDir::new("power-sweep");
        let (acknowledged, _) = run(scratch.path(), Some(n));
        let conn = Connection::open(scratch.path().join("state.sqlite")).unwrap();
        assert_eq!(integrity(&conn), "ok", "cut after {n}");
        let tables = count(&conn, "SELECT count(*) FROM sqlite_master WHERE name = 't'");
        let kept: Vec<i64> = if tables == 0 {
            Vec::new()
        } else {
            let mut statement = conn.prepare("SELECT x FROM t ORDER BY x").unwrap();
            statement
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        // Every acknowledged commit is kept; the one in flight may be.
        assert!(
            kept.starts_with(&acknowledged) && kept.len() <= acknowledged.len() + 1,
            "cut after {n}: acknowledged {acknowledged:?}, kept {kept:?}"
        );
    }
}

fn revision(id: &str) -> RevisionInput {
    RevisionInput {
        revision_id: id.to_owned(),
        identity_id: None,
        format: ComposeFormat::Plain,
        plain_flowed: true,
        subject: format!("subject {id}"),
        to: vec![DraftRecipient {
            address: "to@example.test".to_owned(),
            name: None,
        }],
        cc: vec![],
        bcc: vec![],
        body_source: format!("body {id}"),
        attachments: vec![],
    }
}

const SAVES: usize = 8;

fn now() -> Timestamp {
    Timestamp::from_millis(FIXED_NOW_MS)
}

/// Save the draft's revisions in turn until one fails: the ids saved.
fn save_revisions(store: &Store) -> Vec<String> {
    let mut acknowledged = Vec::new();
    let mut base: Option<String> = None;
    for i in 0..SAVES {
        let id = format!("r{i}");
        let (revision_id, previous) = (id.clone(), base.clone());
        let saved = block_on(store.write(WritePriority::User, move |tx| {
            save_draft(tx, "d", previous.as_deref(), revision(&revision_id), now())
        }));
        if saved.is_err() {
            break;
        }
        base = Some(id.clone());
        acknowledged.push(id);
    }
    acknowledged
}

/// A store opened over `paths` under its root's supply; a failed open
/// (the cut landed in it) acknowledged nothing.
fn open_store(paths: &StorePaths) -> Option<Store> {
    open(paths, &test_seams(sb_test_support::real_fs())).ok()
}

/// Every revision the store acknowledged, and the rows spec/11's
/// recovery invariants check, survive a cut at each operation of a run
/// of draft saves.
#[test]
fn a_cut_during_draft_saves_keeps_every_acknowledged_revision() {
    let (template, template_paths) = scratch_paths("power-drafts-template");
    {
        let store = open_store(&template_paths).unwrap();
        block_on(store.write(WritePriority::User, |tx| {
            create_draft(tx, "d", &DraftSource::New, now())
        }))
        .unwrap();
        store.close().unwrap();
    }

    let mut total = None;
    let mut n = 0;
    let mut interrupted = 0;
    loop {
        let (scratch, paths) = scratch_paths("power-drafts");
        copy_tree(template.path(), scratch.path());
        let supply = PowerSupply::arm(scratch.path()).unwrap();
        let store = open_store(&paths).expect("the store opens before any cut");
        let cut = total.is_some();
        if cut {
            supply.cut_after(n);
        }
        let acknowledged = save_revisions(&store);
        let operations = supply.operations();
        // The machine died: whatever close does, it can't write.
        supply.cut();
        let _ = store.close();
        supply.restore();
        if !cut {
            assert_eq!(acknowledged.len(), SAVES);
            assert!(operations > 0, "the store wrote through the power-loss VFS");
            total = Some(operations);
            continue;
        }
        if acknowledged.len() < SAVES {
            interrupted += 1;
        }

        let reopened =
            open_store(&paths).unwrap_or_else(|| panic!("cut after {n}: the store reopens"));
        let current = reopened
            .read(|tx| draft(tx, "d"))
            .unwrap()
            .expect("the draft survives");
        let kept: Vec<String> = reopened
            .read(|tx| {
                let mut statement = tx
                    .prepare("SELECT revision_id FROM draft_revisions ORDER BY seq")
                    .map_err(|e| classify(e, "read the revisions"))?;
                let ids = statement
                    .query_map([], |row| row.get(0))
                    .and_then(Iterator::collect)
                    .map_err(|e| classify(e, "read the revisions"))?;
                Ok(ids)
            })
            .unwrap();
        assert!(
            kept.starts_with(&acknowledged) && kept.len() <= acknowledged.len() + 1,
            "cut after {n}: acknowledged {acknowledged:?}, kept {kept:?}"
        );
        assert_eq!(
            current.revision.map(|revision| revision.revision_id),
            kept.last().cloned(),
            "cut after {n}: the current revision is the last kept"
        );
        let checks = reopened
            .read(|tx| {
                let integrity: String = tx
                    .query_row("PRAGMA integrity_check", [], |row| row.get(0))
                    .map_err(|e| classify(e, "check integrity"))?;
                let foreign: i64 = tx
                    .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
                        row.get(0)
                    })
                    .map_err(|e| classify(e, "check foreign keys"))?;
                Ok((integrity, foreign))
            })
            .unwrap();
        assert_eq!(checks, ("ok".to_owned(), 0), "cut after {n}");
        reopened.close().unwrap();
        drop(supply);

        n += 1;
        if Some(n) == total {
            break;
        }
    }
    assert!(interrupted > 0, "some cut landed among the saves");
}
