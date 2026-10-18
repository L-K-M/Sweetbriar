//! M1.2's end-to-end acceptance (plan M1.2c4): one fixture import
//! through the engine checked against every original M1.2 check, and
//! the crash-cut suite (spec/11): 50 seeded cut points, each followed
//! by reopening, finishing the job and comparing with an uncut run.

use std::env;
use std::sync::Arc;

use sb_test_support::{CutFs, real_fs, run_crash_cut_points};

use super::*;

/// The expected SHA-256 of each acquisition the fixture set yields: the
/// Apple Mail package's message (twice there, twice in `Archive`), the
/// mboxrd decoding of `quoting.mbox`, the loose EML and the empty one.
const INBOX_MESSAGE: &str = "1e8e84fb9b7140540911f04c47ae0556581c6f095511f6675f32a9b224bc3589";
const QUOTING_MBOXO: &str = "69afa20c11a485abc5a04cace01ff8b167658ea89ac7d3e3a31f0fc709cca413";
const QUOTING_MBOXRD: &str = "1786e24a2decd0fa5fa2b4f3d22faecdc36357d8b117bb6e083a2fe68a329873";
const LOOSE_NOTE: &str = "cfd13edd9ae5f734a931db095169e3e01fbb153dce906f811dc7875be2a99dca";
const EMPTY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const NOTE: &[u8] =
    b"From: friend@example.test\nSubject: Loose note\nStatus: O\n\nUnread on arrival.\n";

const CUT_N_ENV: &str = "SB_IMPORT_CUT_N";
const CUT_ROOT_ENV: &str = "SB_IMPORT_CUT_ROOT";
const CUT_POINTS: usize = 50;
const CUT_SEED: u64 = 0x1_2c4;

/// The fixture set under `dir`: the Apple Mail package, a byte-identical
/// `Archive.mbox` beside it, the ambiguous `quoting.mbox`, and a folder
/// of a note (unread by its status header) and an unparseable empty EML.
fn fixture_set(dir: &Path) -> Vec<PathBuf> {
    let package = dir.join("Inbox.mbox");
    fs::create_dir_all(&package).unwrap();
    let inbox = fs::read(fixtures().join("Inbox.mbox/mbox")).unwrap();
    fs::write(package.join("mbox"), &inbox).unwrap();
    fs::copy(
        fixtures().join("Inbox.mbox/Info.plist"),
        package.join("Info.plist"),
    )
    .unwrap();
    fs::write(dir.join("Archive.mbox"), &inbox).unwrap();
    fs::copy(fixtures().join("quoting.mbox"), dir.join("quoting.mbox")).unwrap();
    let loose = dir.join("Loose");
    fs::create_dir_all(&loose).unwrap();
    fs::write(loose.join("note.eml"), NOTE).unwrap();
    fs::write(loose.join("empty.eml"), b"").unwrap();
    ["Inbox.mbox", "Archive.mbox", "quoting.mbox", "Loose"]
        .iter()
        .map(|name| dir.join(name))
        .collect()
}

/// Inventory, confirm with mboxrd for every ambiguous source, finish.
fn drive(engine: &Engine, paths: &[PathBuf]) -> ImportJobStatus {
    let id = start(engine, paths);
    finish_waiting(engine, &id)
}

fn finish_waiting(engine: &Engine, id: &str) -> ImportJobStatus {
    let waiting = settle(engine, id, ImportJobState::AwaitingConfirmation);
    let variants = waiting
        .checkpoint()
        .ambiguous()
        .iter()
        .map(|&ordinal| (ordinal, MboxVariant::Mboxrd))
        .collect();
    confirm(engine, id, variants).unwrap();
    settle(engine, id, ImportJobState::Completed)
}

/// What an import left in the store, for comparing runs.
#[derive(Debug, PartialEq, Eq)]
struct Counts {
    hashes: Vec<String>,
    per_mailbox: Vec<(String, i64)>,
    occurrences: i64,
    provenance: i64,
    distinct_provenance: i64,
    failed_parses: i64,
    unread: i64,
}

/// One text column per row.
fn strings(engine: &Engine, query: &'static str) -> Vec<String> {
    engine
        .resources()
        .read(|tx| {
            let mut statement = tx.prepare(query).map_err(sql)?;
            statement
                .query_map([], |row| row.get(0))
                .map_err(sql)?
                .collect::<Result<Vec<String>, _>>()
                .map_err(sql)
        })
        .unwrap()
}

/// A name and a count per row.
fn named_counts(engine: &Engine, query: &'static str) -> Vec<(String, i64)> {
    engine
        .resources()
        .read(|tx| {
            let mut statement = tx.prepare(query).map_err(sql)?;
            statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .map_err(sql)?
                .collect::<Result<Vec<(String, i64)>, _>>()
                .map_err(sql)
        })
        .unwrap()
}

fn sql(error: impl std::fmt::Display) -> StoreError {
    StoreError::internal(error.to_string())
}

fn counts(engine: &Engine) -> Counts {
    Counts {
        hashes: strings(engine, "SELECT content_sha256 FROM acquisitions ORDER BY 1"),
        per_mailbox: named_counts(
            engine,
            "SELECT mb.remote_name, count(*) FROM occurrences o
             JOIN mailboxes mb ON mb.mailbox_id = o.mailbox_id GROUP BY 1 ORDER BY 1",
        ),
        occurrences: count(engine, "SELECT count(*) FROM occurrences"),
        provenance: count(engine, "SELECT count(*) FROM acquisition_sources"),
        distinct_provenance: count(
            engine,
            "SELECT count(DISTINCT source_ref) FROM acquisition_sources",
        ),
        failed_parses: count(
            engine,
            "SELECT count(*) FROM d_message_summary WHERE parse_state = 'failed'",
        ),
        unread: count(engine, "SELECT count(*) FROM occurrences WHERE f_seen = 0"),
    }
}

/// spec/11 invariants: SQLite is intact and every acquisition's object
/// is published (spec/03 layout `objects/sha256/ab/cd/<hash>`).
fn assert_intact(engine: &Engine, root: &Path) {
    assert_eq!(strings(engine, "PRAGMA integrity_check"), ["ok"]);
    let dangling = strings(engine, "PRAGMA foreign_key_check");
    assert!(dangling.is_empty(), "{dangling:?}");
    for hash in counts(engine).hashes {
        let object = root
            .join("objects/sha256")
            .join(&hash[..2])
            .join(&hash[2..4])
            .join(&hash);
        assert!(object.is_file(), "acquisition {hash} has no object");
    }
}

#[test]
fn the_fixture_import_meets_the_m1_2_acceptance_checks() {
    let scratch = ScratchDir::new("import-acceptance");
    let paths = fixture_set(&scratch.path().join("fixtures"));
    let root = scratch.path().join("store");
    let engine = open(&root);
    let done = drive(&engine, &paths);

    // Exact bytes: one acquisition per distinct message, by hash.
    let mut expected = vec![INBOX_MESSAGE, QUOTING_MBOXRD, LOOSE_NOTE, EMPTY];
    expected.sort_unstable();
    let found = counts(&engine);
    assert_eq!(found.hashes, expected);
    // Byte-identical files under different names are two sources in
    // their own mailboxes; identical messages keep their multiplicity.
    assert_eq!(
        found.per_mailbox,
        [
            ("Archive".to_owned(), 2),
            ("Inbox".to_owned(), 2),
            ("Loose".to_owned(), 2),
            ("quoting".to_owned(), 1),
        ]
    );
    assert_eq!((found.occurrences, found.provenance), (7, 7));
    // Read state follows status headers; without them, read.
    assert_eq!(found.unread, 1);
    // Unparseable bytes are kept and marked failed.
    assert_eq!(found.failed_parses, 1);

    let totals = done.checkpoint().totals();
    assert_eq!((totals.processed, totals.imported), (7, 7));
    assert_eq!((totals.duplicates, totals.rejected), (3, 1));
    let report = fs::read_to_string(done.report_path().unwrap()).unwrap();
    for needle in [
        "\"found\": 7",
        "\"imported\": 7",
        "\"duplicates\": 3",
        "\"rejected\": 1",
    ] {
        assert!(report.contains(needle), "{needle} missing from {report}");
    }
    for source in done.sources() {
        assert!(report.contains(&source.source().snapshot().sha256_hex()));
    }

    // Re-import is idempotent by source identity: nothing new.
    let again = drive(&engine, &paths);
    assert_eq!(again.checkpoint().totals().imported, 0);
    assert_eq!(counts(&engine), found);
    assert_intact(&engine, &root);
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn the_same_file_decodes_by_its_chosen_variant_and_the_job_asks_first() {
    let scratch = ScratchDir::new("import-variants");
    let quoting = fixtures().join("quoting.mbox");
    for (variant, hash) in [
        (MboxVariant::Mboxo, QUOTING_MBOXO),
        (MboxVariant::Mboxrd, QUOTING_MBOXRD),
    ] {
        let engine = open(&scratch.path().join(format!("{variant:?}")));
        let id = start(&engine, std::slice::from_ref(&quoting));
        let waiting = settle(&engine, &id, ImportJobState::AwaitingConfirmation);
        assert_eq!(waiting.checkpoint().ambiguous(), [0]);
        // Unknown variant: the job asks rather than guessing.
        match confirm(&engine, &id, vec![]) {
            Err(EngineError::InvalidInput { field, .. }) => assert_eq!(field, "variants"),
            other => panic!("an unchosen variant must be asked for: {other:?}"),
        }
        confirm(&engine, &id, vec![(0, variant)]).unwrap();
        settle(&engine, &id, ImportJobState::Completed);
        assert_eq!(counts(&engine).hashes, [hash]);
        block_on(engine.shutdown()).unwrap();
    }
}

/// 50 distinct cut points in `1..=total` (all of them if fewer), drawn
/// by splitmix64 from a fixed seed so a failure replays exactly.
fn cut_points(total: u64) -> Vec<u64> {
    let wanted = CUT_POINTS.min(usize::try_from(total).unwrap_or(usize::MAX));
    let mut state = CUT_SEED;
    let mut points = std::collections::BTreeSet::new();
    while points.len() < wanted {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        points.insert(1 + (z ^ (z >> 31)) % total);
    }
    points.into_iter().collect()
}

/// After a cut: reopen on the real filesystem, let recovery run, then
/// finish whatever the cut left (spec/09). An interrupted inventory
/// failed at open, and a cut before the start left no job: either way
/// the user starts the import again.
fn finish_after_cut(engine: &Engine, paths: &[PathBuf]) {
    let ids = strings(engine, "SELECT job_id FROM import_jobs ORDER BY created_at");
    let mut completed = false;
    for id in ids {
        match status(engine, &id).state() {
            ImportJobState::AwaitingConfirmation => {
                finish_waiting(engine, &id);
                completed = true;
            }
            ImportJobState::Running => {
                settle(engine, &id, ImportJobState::Completed);
                completed = true;
            }
            ImportJobState::Completed => completed = true,
            state => assert_eq!(state, ImportJobState::Failed, "job {id}"),
        }
    }
    if !completed {
        drive(engine, paths);
    }
}

#[test]
fn imports_reach_the_uncut_counts_after_crash_cuts_at_seeded_points() {
    let scratch = ScratchDir::new("import-crash-cuts");
    let paths = fixture_set(&scratch.path().join("fixtures"));

    // An uncut run measures the operations and the final counts.
    let uncut = Arc::new(CutFs::new(real_fs(), u64::MAX));
    let root = scratch.path().join("uncut");
    let engine = block_on(Engine::open_with_fs(&root, uncut.clone())).unwrap();
    drive(&engine, &paths);
    let expected = counts(&engine);
    block_on(engine.shutdown()).unwrap();
    let points = cut_points(uncut.operations());

    let killed = run_crash_cut_points(
        CUT_N_ENV,
        CUT_ROOT_ENV,
        "import::tests::acceptance::import_crash_child",
        &points,
        &mut |n| scratch.path().join(format!("cut-{n}")).join("store"),
        &mut |root| {
            let engine = open(root);
            finish_after_cut(&engine, &paths);
            assert_eq!(
                counts(&engine),
                expected,
                "after a cut in {}",
                root.display()
            );
            assert_intact(&engine, root);
            block_on(engine.shutdown()).unwrap();
        },
    );
    assert!(
        killed * 2 >= points.len() as u64,
        "only {killed} of {} cuts landed",
        points.len()
    );
}

/// The crash-cut child: the fixture import on `CutFs`, killed at the
/// cut point. Its fixtures sit beside its store's run directory.
#[test]
#[ignore = "run by the crash-cut suite as its child"]
fn import_crash_child() {
    let n: u64 = env::var(CUT_N_ENV).unwrap().parse().unwrap();
    let root = PathBuf::from(env::var(CUT_ROOT_ENV).unwrap());
    let fixtures = root.parent().unwrap().parent().unwrap().join("fixtures");
    let paths: Vec<PathBuf> = ["Inbox.mbox", "Archive.mbox", "quoting.mbox", "Loose"]
        .iter()
        .map(|name| fixtures.join(name))
        .collect();
    let engine = block_on(Engine::open_with_fs(
        &root,
        Arc::new(CutFs::new(real_fs(), n)),
    ))
    .unwrap();
    drive(&engine, &paths);
    block_on(engine.shutdown()).unwrap();
}
