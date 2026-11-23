//! Crash cuts during migrations (spec/11 "Crash cuts"): a store left at
//! version 27 opens in a child that the test SQLite VFS kills before
//! its Nth write, truncation or sync, through migration 28's and 35's
//! snapshots and rebuilds. The parent reopens what each kill left.

use std::env;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags, types::Value};
use sb_test_support::{ScratchDir, fault_points, real_fs, run_crash_cut_points};

use super::{one_int, other_seams, snapshots, store_at_27, table_rows};
use crate::power_vfs::PowerSupply;
use crate::schema::LATEST_SCHEMA_VERSION;
use crate::testing::{copy_tree, reading, test_seams};
use crate::{StorePaths, open};

const CUT_ENV: &str = "SB_MIGRATION_CUT_N";
const ROOT_ENV: &str = "SB_MIGRATION_CUT_ROOT";
const CHILD: &str = "schema::tests::cuts::migration_cut_child";

/// The cut points a pull request samples; nightly cuts every one.
const CUT_POINTS: usize = 40;

/// The rows migration 28 and 35 rebuild, as `store_at_27` seeded them.
type Rebuilt = (Vec<Vec<Value>>, Vec<Vec<Value>>, Vec<Vec<Value>>);

fn rebuilt(conn: &Connection) -> Rebuilt {
    (
        table_rows(conn, "accounts", 10, "account_id"),
        table_rows(conn, "acquisition_sources", 7, "source_id"),
        table_rows(conn, "intents", 18, "intent_id"),
    )
}

fn store_paths(root: &Path) -> StorePaths {
    StorePaths::new(root.join("store")).unwrap()
}

/// The child: open the store under `ROOT_ENV`, which runs migrations 28
/// to the latest, and die before operation `CUT_ENV`. A run that
/// finishes first exits cleanly.
#[test]
#[ignore = "the child of migrations_survive_a_kill_at_any_write, which runs it"]
fn migration_cut_child() {
    let nth: u64 = env::var(CUT_ENV).unwrap().parse().unwrap();
    let root = PathBuf::from(env::var(ROOT_ENV).unwrap());
    let supply = PowerSupply::arm(&root).unwrap();
    supply.kill_before(nth);
    let store = open(&store_paths(&root), &test_seams(real_fs())).unwrap();
    store.close().unwrap();
}

/// What a kill during the upgrade left, reopened: the migrations finish
/// with every rebuilt row and id kept, the checks are clean, and at
/// most two snapshots remain, the newest a whole database. The reopen's
/// clock is later than the child's, so its snapshots sort last.
fn check(root: &Path, expected: &Rebuilt) {
    let paths = store_paths(root);
    let store = open(&paths, &other_seams())
        .unwrap_or_else(|e| panic!("{}: the store reopens: {e}", root.display()));
    let (version, rows, integrity, foreign) = reading(&store, |tx| {
        let integrity: String = tx
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        Ok((
            one_int(tx, "PRAGMA user_version"),
            rebuilt(tx),
            integrity,
            one_int(tx, "SELECT count(*) FROM pragma_foreign_key_check"),
        ))
    });
    store.close().unwrap();
    assert_eq!(
        version,
        i64::from(LATEST_SCHEMA_VERSION),
        "{}",
        root.display()
    );
    assert_eq!(
        &rows,
        expected,
        "{}: a rebuilt row or id changed",
        root.display()
    );
    assert_eq!(
        (integrity.as_str(), foreign),
        ("ok", 0),
        "{}",
        root.display()
    );

    let found = snapshots(&paths);
    assert!(found.len() <= 2, "{}: {found:?}", root.display());
    if let Some(newest) = found.last() {
        let snapshot =
            Connection::open_with_flags(newest, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let integrity: String = snapshot
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(integrity, "ok", "{}", newest.display());
    }
}

/// A kill at any write, truncation or sync of the upgrade from version
/// 27 leaves a store that reopens to the latest version intact.
#[test]
fn migrations_survive_a_kill_at_any_write() {
    let (template, template_paths, _) = store_at_27("migration-cut-template");
    let expected = rebuilt(&Connection::open(template_paths.state_db()).unwrap());

    // The uncut upgrade, through the same VFS, measures the span.
    let span = {
        let scratch = ScratchDir::new("migration-cut-uncut");
        copy_tree(template.path(), scratch.path());
        let supply = PowerSupply::arm(scratch.path()).unwrap();
        let store = open(&store_paths(scratch.path()), &test_seams(real_fs())).unwrap();
        store.close().unwrap();
        supply.operations()
    };
    assert!(span > 0, "the upgrade wrote through the test VFS");

    let count = fault_points(CUT_POINTS, span);
    let step = (span / count as u64).max(1) as usize;
    let points: Vec<u64> = (1..=span).step_by(step).take(count).collect();
    let mut roots = Vec::new();
    let cuts = run_crash_cut_points(
        CUT_ENV,
        ROOT_ENV,
        CHILD,
        &points,
        &mut |n| {
            let scratch = ScratchDir::new(&format!("migration-cut-{n}"));
            copy_tree(template.path(), scratch.path());
            let root = scratch.path().to_path_buf();
            roots.push(scratch);
            root
        },
        &mut |root| check(root, &expected),
    );
    // The child repeats the measured run, so every point lands in it.
    assert_eq!(cuts.killed, points, "survived: {:?}", cuts.survived);
}
