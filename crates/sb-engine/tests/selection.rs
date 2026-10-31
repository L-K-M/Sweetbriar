//! Selection snapshots (spec/07 "Selection snapshots") through
//! `sb-engine`'s public API only — nothing private is reachable: a
//! real 2,101-message archive imported by `start_import`, its
//! mailbox window's public `window_ids` index, and rank bitmaps
//! selecting past the 1,000-id cap materialized in window order and
//! paged exactly.

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use sb_engine::{
    Anchor, Engine, EngineError, ImportJobState, SelectionInput, SelectionSnapshot, WindowId,
    WindowIds, WindowSpec,
};
use sb_test_support::{ScratchDir, block_on};

const PATIENCE: Duration = Duration::from_secs(300);
const PAGE_SIZE: u32 = 1_000;
const SHUFFLE_MULTIPLIER: usize = 2_654_435_761;

/// A synthetic `n`-message mbox — invented senders and subjects only.
fn mbox(dir: &std::path::Path, name: &str, n: usize) -> PathBuf {
    let text: String = (0..n)
        .map(|rank| {
            format!(
                "From sender@example.test Sat Oct 17 12:00:00 2026\n\
                 Message-ID: <{name}-{rank}@x>\nFrom: Ada <ada@example.test>\n\
                 Subject: message {rank}\n\nbody {rank}\n"
            )
        })
        .collect();
    let path = dir.join(name);
    fs::write(&path, text).unwrap();
    path
}

/// Wait until the job reports `state`; a job ending elsewhere fails.
fn settle(engine: &Engine, id: &str, state: ImportJobState) {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let job = block_on(engine.import_job(id)).unwrap();
        if job.state() == state {
            return;
        }
        assert!(
            !matches!(
                job.state(),
                ImportJobState::Completed | ImportJobState::Cancelled | ImportJobState::Failed
            ),
            "job {id} ended {:?}, failure {:?}",
            job.state(),
            job.checkpoint().failure()
        );
        assert!(
            Instant::now() < deadline,
            "job {id} never reached {state:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Import and confirm `path`, waited out to `Completed`.
fn import(engine: &Engine, path: PathBuf) {
    let id = block_on(engine.start_import(&engine.new_idempotency_key(), vec![path]))
        .unwrap()
        .job_id()
        .to_owned();
    settle(engine, &id, ImportJobState::AwaitingConfirmation);
    block_on(engine.confirm_import(&engine.new_idempotency_key(), &id, Vec::new())).unwrap();
    settle(engine, &id, ImportJobState::Completed);
}

/// The imported "Inbox" mailbox's id, found the public way:
/// `accounts`, then each account's `mailboxes`, until the import has
/// created it.
fn inbox(engine: &Engine) -> i64 {
    let deadline = Instant::now() + PATIENCE;
    loop {
        for account in block_on(engine.accounts()).unwrap() {
            let list = block_on(engine.mailboxes(&account.account_id)).unwrap();
            if let Some(mailbox) = list
                .mailboxes
                .iter()
                .find(|mailbox| mailbox.name == "Inbox")
            {
                return mailbox.mailbox_id;
            }
        }
        assert!(Instant::now() < deadline, "no Inbox mailbox ever appeared");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The window's id index once the import's refresh has published all
/// `n` rows — the refresh lands after the job completes, so the index
/// itself is the public signal that it settled.
fn settled_index(engine: &Engine, window_id: WindowId, n: usize) -> WindowIds {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let index = block_on(engine.window_ids(window_id)).unwrap();
        if index.ids().len() == n {
            return index;
        }
        assert!(Instant::now() < deadline, "the index never held {n} rows");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The rank bitmap selecting `ranks` of a `len`-row window,
/// least-significant bit first (spec/07).
fn bitmap(ranks: impl IntoIterator<Item = usize>, len: usize) -> Vec<u8> {
    let mut bitmap = vec![0u8; len.div_ceil(8)];
    for rank in ranks {
        bitmap[rank / 8] |= 1 << (rank % 8);
    }
    bitmap
}

/// Open a selection at the current index generation; a genuine
/// `ResetRequired` during setup retries with the fresh index.
fn open_at_index(
    engine: &Engine,
    window_id: WindowId,
    input: SelectionInput,
) -> (SelectionSnapshot, WindowIds) {
    for _ in 0..10 {
        let index = block_on(engine.window_ids(window_id)).unwrap();
        match block_on(engine.open_selection(window_id, index.generation(), input.clone())) {
            Ok(snapshot) => return (snapshot, index),
            Err(EngineError::ResetRequired { .. }) => continue,
            Err(error) => panic!("open_selection failed: {error}"),
        }
    }
    panic!("the selection never met a stable generation");
}

/// Read exactly the advertised page count; a broken pager must fail, not loop.
fn paged(engine: &Engine, snapshot: &SelectionSnapshot) -> Vec<i64> {
    let mut all = Vec::new();
    for from in (0..=snapshot.count()).step_by(PAGE_SIZE as usize) {
        let page = block_on(engine.selection_ids(snapshot.selection_id().clone(), from, PAGE_SIZE))
            .unwrap();
        assert_eq!(
            page.len() as u64,
            (snapshot.count() - from).min(u64::from(PAGE_SIZE)),
            "each ordinal page must have its exact remaining length"
        );
        all.extend(page);
    }
    all
}

/// One real archive, all public calls: rank bitmaps past the
/// 1,000-id cap — alternating (worst case) and scattered — paged
/// exactly in window order, then explicit ids at the cap agreeing
/// with their equivalent bitmap.
#[test]
fn a_real_archive_selects_past_a_thousand_in_window_order() {
    const N: usize = 2_101;
    let scratch = ScratchDir::new("public-selection-large");
    let engine = block_on(Engine::open(scratch.path().join("store"))).unwrap();
    import(&engine, mbox(scratch.path(), "Inbox.mbox", N));
    let window =
        block_on(engine.open_window(WindowSpec::Mailbox(inbox(&engine)), Anchor::Top, 10)).unwrap();
    let window_id = window.window_id();
    let index = settled_index(&engine, window_id, N);

    let alternating: Vec<usize> = (0..N).step_by(2).collect();
    let mut shuffled: Vec<usize> = (0..N).collect();
    shuffled.sort_by_key(|rank| rank.wrapping_mul(SHUFFLE_MULTIPLIER) % N);
    let scattered: Vec<usize> = shuffled[..1_100].to_vec();
    for ranks in [&alternating, &scattered] {
        assert!(ranks.len() > 1_000, "the case must clear the id cap");
        let (snapshot, index) = open_at_index(
            &engine,
            window_id,
            SelectionInput::RankBitmap(bitmap(ranks.iter().copied(), N)),
        );
        assert_eq!(index.ids().len(), N);
        assert_eq!(snapshot.count() as usize, ranks.len());
        assert_eq!(snapshot.window_id(), window_id);
        assert_eq!(snapshot.generation(), index.generation());
        // The snapshot keeps window order: ranks ascending, whatever
        // order the set was described in.
        let mut ordered = ranks.clone();
        ordered.sort_unstable();
        let expected: Vec<i64> = ordered.iter().map(|&rank| index.ids()[rank]).collect();
        assert_eq!(paged(&engine, &snapshot), expected);
        // An ordinal page is a prefix: its tail joins seamlessly.
        let head =
            block_on(engine.selection_ids(snapshot.selection_id().clone(), 0, 1_000)).unwrap();
        assert_eq!(head, expected[..1_000]);
        block_on(engine.close_selection(snapshot.selection_id().clone()));
    }

    // At the explicit-id cap: the odd ranks' 1,000 members named in
    // arbitrary input order materialize the same ordered set the
    // equivalent rank bitmap does.
    let ranks: Vec<usize> = (0..1_000).map(|rank| rank * 2 + 1).collect();
    assert_eq!(ranks.len(), 1_000);
    let chosen: Vec<i64> = ranks.iter().map(|&rank| index.ids()[rank]).collect();
    // The input's order is arbitrary; it must not shape the output's.
    let mut scrambled = chosen.clone();
    scrambled.swap(0, 999);
    scrambled.swap(3, 500);

    let (by_ids, _) = open_at_index(&engine, window_id, SelectionInput::Ids(scrambled));
    let (by_bitmap, _) = open_at_index(
        &engine,
        window_id,
        SelectionInput::RankBitmap(bitmap(ranks, N)),
    );
    for snapshot in [&by_ids, &by_bitmap] {
        assert_eq!(snapshot.count(), 1_000);
        assert_eq!(snapshot.window_id(), window_id);
    }
    let from_ids = block_on(engine.selection_ids(by_ids.selection_id().clone(), 0, 1_000)).unwrap();
    let from_bitmap = paged(&engine, &by_bitmap);
    assert_eq!(from_ids, chosen);
    assert_eq!(from_bitmap, chosen);
    block_on(engine.shutdown()).unwrap();
}
