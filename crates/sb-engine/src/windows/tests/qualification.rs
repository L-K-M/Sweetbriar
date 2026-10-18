//! M1.3 qualification (plan M1.3g): selection by occurrence id survives
//! invalidation and follows a message whose representative changes,
//! and the 100k-row window timings CI records (never asserts).

use std::time::{Duration, Instant};

use sb_test_support::{ScratchDir, block_on};

use super::store::{import, mbox, rewrite, sql_error, value};
use crate::{Anchor, Engine, EngineError, MailboxRole, WindowEvent, WindowSpec};

fn engine(label: &str) -> (ScratchDir, Engine) {
    let scratch = ScratchDir::new(label);
    let engine = block_on(Engine::open(scratch.path().join("store"))).unwrap();
    (scratch, engine)
}

fn ids(window: &crate::Window) -> Vec<i64> {
    window
        .rows()
        .iter()
        .map(|row| row.occurrence_id())
        .collect()
}

/// A selected row survives an invalidation that moves its rank: the
/// shell re-reads `Around` it at the new generation and finds the same
/// occurrence; once the occurrence leaves, `Around` is `NotFound` and
/// the selection goes (spec/07 "Selection", plan M1.3 acceptance).
#[test]
fn selection_by_occurrence_survives_invalidation() {
    let (scratch, engine) = engine("selection-survives");
    import(
        &engine,
        mbox(
            scratch.path(),
            "Inbox.mbox",
            &[
                ("Three", Some("Sat, 03 Oct 2001 10:00:00 +0000")),
                ("Two", Some("Fri, 02 Oct 2001 10:00:00 +0000")),
                ("One", Some("Thu, 01 Oct 2001 10:00:00 +0000")),
            ],
        ),
    );
    let mailbox = value(
        &engine,
        "SELECT mailbox_id FROM mailboxes WHERE remote_name = 'Inbox'",
    );
    let window =
        block_on(engine.open_window(WindowSpec::Mailbox(mailbox), Anchor::Top, 10)).unwrap();
    let selected = window.rows()[1].occurrence_id();
    let subscription = engine.subscribe(window.window_id());

    // Newer mail arrives in the same mailbox, from another source of
    // that name: the selection's rank moves.
    let later = scratch.path().join("later");
    std::fs::create_dir(&later).unwrap();
    import(
        &engine,
        mbox(
            &later,
            "Inbox.mbox",
            &[("Four", Some("Sun, 04 Oct 2001 10:00:00 +0000"))],
        ),
    );
    let WindowEvent::Invalidated { generation } = block_on(subscription.next()).unwrap() else {
        panic!("expected an invalidation");
    };
    let page =
        block_on(engine.read_window(window.window_id(), generation, Anchor::Around(selected), 3))
            .unwrap();
    assert!(ids(&page).contains(&selected));
    // The id index shows the move: rank 1 before, rank 2 now.
    let index = block_on(engine.window_ids(window.window_id())).unwrap();
    assert_eq!(index.generation(), generation);
    assert_eq!(index.ids().iter().position(|&id| id == selected), Some(2));

    // The selected occurrence vanishes: the window has no row for it.
    rewrite(&engine, move |tx| {
        tx.execute(
            "UPDATE occurrences SET state = 'vanished' WHERE occurrence_id = ?1",
            [selected],
        )
        .map(|_| ())
        .map_err(sql_error)
    });
    let WindowEvent::Invalidated { generation } = block_on(subscription.next()).unwrap() else {
        panic!("expected an invalidation");
    };
    assert!(matches!(
        block_on(engine.read_window(window.window_id(), generation, Anchor::Around(selected), 3)),
        Err(EngineError::NotFound { .. })
    ));
    block_on(engine.shutdown()).unwrap();
}

/// In a logical-message window the representative can change; `Around`
/// with the old one's id finds the message's new row, so the shell keeps
/// the message selected (spec/07 "Selection").
#[test]
fn selection_follows_a_message_whose_representative_changes() {
    let (scratch, engine) = engine("selection-follows");
    import(
        &engine,
        mbox(
            scratch.path(),
            "Inbox.mbox",
            &[("Starred", Some("Thu, 01 Oct 2001 10:00:00 +0000"))],
        ),
    );
    rewrite(&engine, |tx| {
        tx.execute_batch(
            "INSERT INTO mailboxes (account_id, remote_name, kind, selectable, sync_state)
               SELECT account_id, 'Work', 'label', 1, 'synced' FROM accounts
               WHERE provider = 'local_archive';
             INSERT INTO occurrences (account_id, mailbox_id, message_id, f_flagged, state,
               first_seen_at, last_seen_at)
               SELECT o.account_id, m.mailbox_id, o.message_id, 1, 'present', 0, 0
               FROM occurrences o, mailboxes m WHERE m.kind = 'label';
             UPDATE occurrences SET f_flagged = 1;",
        )
        .map_err(sql_error)
    });
    let filed = value(
        &engine,
        "SELECT o.occurrence_id FROM occurrences o JOIN mailboxes m USING (mailbox_id)
         WHERE m.kind = 'folder'",
    );
    let labelled = value(
        &engine,
        "SELECT o.occurrence_id FROM occurrences o JOIN mailboxes m USING (mailbox_id)
         WHERE m.kind = 'label'",
    );
    let window = block_on(engine.open_window(WindowSpec::Flagged, Anchor::Top, 10)).unwrap();
    assert_eq!(ids(&window), vec![filed]);
    let message = window.rows()[0].message_id();
    let subscription = engine.subscribe(window.window_id());

    // The folder copy vanishes: the label's occurrence now represents it.
    rewrite(&engine, move |tx| {
        tx.execute(
            "UPDATE occurrences SET state = 'vanished' WHERE occurrence_id = ?1",
            [filed],
        )
        .map(|_| ())
        .map_err(sql_error)
    });
    let WindowEvent::Invalidated { generation } = block_on(subscription.next()).unwrap() else {
        panic!("expected an invalidation");
    };
    let page =
        block_on(engine.read_window(window.window_id(), generation, Anchor::Around(filed), 3))
            .unwrap();
    assert_eq!(ids(&page), vec![labelled]);
    assert_eq!(page.rows()[0].message_id(), message);
    block_on(engine.shutdown()).unwrap();
}

/// Runs a timed call `ROUNDS` times; the median, in milliseconds.
fn median_ms(mut call: impl FnMut()) -> f64 {
    const ROUNDS: usize = 25;
    let mut times: Vec<Duration> = (0..ROUNDS)
        .map(|_| {
            let started = Instant::now();
            call();
            started.elapsed()
        })
        .collect();
    times.sort();
    times[ROUNDS / 2].as_secs_f64() * 1_000.0
}

/// Plan M1.3 timing acceptance: over 100k synthetic rows, a first page
/// and a deep keyset page of 200 for each store spec, through the
/// engine. Recorded by CI (`window-timings`), never asserted: the
/// target is under 5 ms. Run with `--release --ignored --nocapture`.
#[test]
#[ignore = "timing record for CI, run with --release --ignored --nocapture"]
fn list_window_timings() {
    const ROWS: i64 = 100_000;
    const PAGE: u32 = 200;
    let (_scratch, engine) = engine("window-timings");
    let built = Instant::now();
    rewrite(&engine, |tx| {
        tx.execute_batch(&format!(
            "INSERT INTO mailboxes (account_id, remote_name, role, kind, selectable, sync_state)
               SELECT account_id, 'Inbox', 'inbox', 'folder', 1, 'synced' FROM accounts
               WHERE provider = 'local_archive';
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < {ROWS})
             INSERT INTO logical_messages (account_id, created_at)
               SELECT (SELECT account_id FROM accounts WHERE provider = 'local_archive'), 0
               FROM n;
             INSERT INTO occurrences (account_id, mailbox_id, message_id, internal_date,
               f_seen, f_flagged, state, first_seen_at, last_seen_at)
               SELECT lm.account_id, (SELECT mailbox_id FROM mailboxes WHERE role = 'inbox'),
                 lm.message_id, lm.message_id * 60000, lm.message_id % 3 = 0,
                 lm.message_id % 10 = 0, 'present', 0, 0
               FROM logical_messages lm;"
        ))
        .map_err(sql_error)
    });
    let setup = built.elapsed();
    let mailbox = value(
        &engine,
        "SELECT mailbox_id FROM mailboxes WHERE role = 'inbox'",
    );

    println!("window timings over {ROWS} rows, pages of {PAGE} (setup {setup:?}):");
    for (name, spec) in [
        ("mailbox", WindowSpec::Mailbox(mailbox)),
        ("unified", WindowSpec::Unified(MailboxRole::Inbox)),
        ("flagged", WindowSpec::Flagged),
    ] {
        let first = median_ms(|| {
            let window = block_on(engine.open_window(spec, Anchor::Top, PAGE)).unwrap();
            block_on(engine.close_window(window.window_id()));
        });
        let window = block_on(engine.open_window(spec, Anchor::Top, PAGE)).unwrap();
        let index = block_on(engine.window_ids(window.window_id())).unwrap();
        // Nine tenths of the way down each window.
        let deep_id = index.ids()[index.ids().len() * 9 / 10];
        let near = block_on(engine.read_window(
            window.window_id(),
            index.generation(),
            Anchor::Around(deep_id),
            PAGE,
        ))
        .unwrap();
        let cursor = near.after().unwrap().clone();
        let deep = median_ms(|| {
            block_on(engine.read_window(
                window.window_id(),
                index.generation(),
                Anchor::After(cursor.clone()),
                PAGE,
            ))
            .unwrap();
        });
        println!(
            "  {name}: {} rows, first page {first:.2} ms, deep page {deep:.2} ms (median of 25)",
            index.ids().len()
        );
    }
    block_on(engine.shutdown()).unwrap();
}
