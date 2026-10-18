//! Store-backed windows (plan M1.3e, spec/07 "Store windows"): imports
//! listed by `Date`, paging and generations over real rows, Flagged
//! through the representative, Unified by role and Thread windows.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sb_resources::{ImportJobState, StoreError, Transaction, WritePriority, refresh_list_rows};
use sb_test_support::{ScratchDir, block_on};

use crate::{Anchor, Count, Engine, EngineError, MailboxRole, WindowSpec};

const PATIENCE: Duration = Duration::from_secs(300);

/// A mbox of `(subject, date header)` messages, in file order.
fn mbox(dir: &Path, name: &str, messages: &[(&str, Option<&str>)]) -> PathBuf {
    let text: String = messages
        .iter()
        .enumerate()
        .map(|(n, (subject, date))| {
            let date = date.map(|d| format!("Date: {d}\n")).unwrap_or_default();
            format!(
                "From sender@example.test Sat Oct 17 12:00:00 2026\n\
                 Message-ID: <{name}-{n}@x>\nFrom: Ada <ada@example.test>\n{date}\
                 Subject: {subject}\n\nbody {n}\n"
            )
        })
        .collect();
    let path = dir.join(name);
    fs::write(&path, text).unwrap();
    path
}

fn import(engine: &Engine, path: PathBuf) {
    let id = block_on(engine.start_import(&engine.new_idempotency_key(), vec![path]))
        .unwrap()
        .job_id()
        .to_owned();
    settle(engine, &id, ImportJobState::AwaitingConfirmation);
    block_on(engine.confirm_import(&engine.new_idempotency_key(), &id, Vec::new())).unwrap();
    settle(engine, &id, ImportJobState::Completed);
}

fn settle(engine: &Engine, id: &str, state: ImportJobState) {
    let deadline = Instant::now() + PATIENCE;
    while block_on(engine.import_job(id)).unwrap().state() != state {
        assert!(
            Instant::now() < deadline,
            "job {id} never reached {state:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn sql_error(e: impl std::fmt::Display) -> StoreError {
    StoreError::internal(e.to_string())
}

/// One value `sql` reads.
fn value(engine: &Engine, sql: &'static str) -> i64 {
    engine
        .resources()
        .read(|tx| tx.query_row(sql, [], |row| row.get(0)).map_err(sql_error))
        .unwrap()
}

/// Run `f` as a user write, then refresh the list rows of every
/// occurrence: what a later writer that changes occurrences does.
fn rewrite(
    engine: &Engine,
    f: impl FnOnce(&Transaction) -> Result<(), StoreError> + Send + 'static,
) {
    block_on(engine.resources().write(WritePriority::User, move |tx| {
        f(tx)?;
        let mut statement = tx
            .prepare("SELECT occurrence_id FROM occurrences")
            .map_err(sql_error)?;
        let all: BTreeSet<i64> = statement
            .query_map([], |row| row.get(0))
            .map_err(sql_error)?
            .collect::<Result<_, _>>()
            .map_err(sql_error)?;
        refresh_list_rows(tx, &all)?;
        Ok(())
    }))
    .unwrap();
}

fn subjects(window: &crate::Window) -> Vec<&str> {
    window.rows().iter().map(|row| row.subject()).collect()
}

/// An mbox written newest first lists by `Date`, not by import order;
/// an undated message sorts by when it was first seen; display fields
/// come from the summary (plan M1.3 acceptance).
#[test]
fn an_mbox_in_reverse_chronological_order_lists_by_date() {
    let scratch = ScratchDir::new("windows-by-date");
    let engine = block_on(Engine::open(scratch.path().join("store"))).unwrap();
    let path = mbox(
        scratch.path(),
        "Inbox.mbox",
        &[
            ("Newest", Some("Thu, 15 Oct 2026 10:00:00 +0000")),
            ("Middle", Some("Wed, 14 Oct 2026 10:00:00 +0000")),
            ("Oldest", Some("Tue, 13 Oct 2026 10:00:00 +0000")),
            ("Undated", None),
        ],
    );
    import(&engine, path);
    let mailbox = value(
        &engine,
        "SELECT mailbox_id FROM mailboxes WHERE remote_name = 'Inbox'",
    );
    let window =
        block_on(engine.open_window(WindowSpec::Mailbox(mailbox), Anchor::Top, 10)).unwrap();
    // Undated messages sort by first-seen time: the import, after 2026-10-15.
    assert_eq!(
        subjects(&window),
        vec!["Undated", "Newest", "Middle", "Oldest"]
    );
    assert_eq!(window.total(), Count::Exact(4));
    let ids: Vec<i64> = window
        .rows()
        .iter()
        .map(|row| row.occurrence_id())
        .collect();
    assert_ne!(ids, {
        let mut sorted = ids.clone();
        sorted.sort_unstable_by(|a, b| b.cmp(a));
        sorted
    });
    assert_eq!(window.rows()[1].sender(), "Ada");
    // The parser keeps the body's trailing line break in its snippet.
    assert_eq!(window.rows()[1].snippet().trim_end(), "body 0");
    block_on(engine.shutdown()).unwrap();
}

/// Store windows page with cursors both ways, report `AtLeast` until a
/// page holds the whole window, answer `ResetRequired` once an import
/// moves the generation, and refuse foreign cursors and unknown ids.
#[test]
fn store_windows_page_and_reset() {
    let scratch = ScratchDir::new("windows-store-paging");
    let engine = block_on(Engine::open(scratch.path().join("store"))).unwrap();
    let dates = [
        "Mon, 05 Oct 2026 10:00:00 +0000",
        "Sun, 04 Oct 2026 10:00:00 +0000",
        "Sat, 03 Oct 2026 10:00:00 +0000",
        "Fri, 02 Oct 2026 10:00:00 +0000",
        "Thu, 01 Oct 2026 10:00:00 +0000",
    ];
    let messages: Vec<(&str, Option<&str>)> = ["A", "B", "C", "D", "E"]
        .iter()
        .zip(dates)
        .map(|(subject, date)| (*subject, Some(date)))
        .collect();
    import(&engine, mbox(scratch.path(), "Inbox.mbox", &messages));
    let mailbox = value(
        &engine,
        "SELECT mailbox_id FROM mailboxes WHERE remote_name = 'Inbox'",
    );
    let spec = WindowSpec::Mailbox(mailbox);

    let first = block_on(engine.open_window(spec, Anchor::Top, 2)).unwrap();
    assert_eq!(subjects(&first), vec!["A", "B"]);
    assert_eq!(first.total(), Count::AtLeast(3));
    let (id, generation) = (first.window_id(), first.generation());
    let next = block_on(engine.read_window(
        id,
        generation,
        Anchor::After(first.after().unwrap().clone()),
        2,
    ))
    .unwrap();
    assert_eq!(subjects(&next), vec!["C", "D"]);
    assert_eq!(next.total(), Count::AtLeast(4));
    let back = block_on(engine.read_window(
        id,
        generation,
        Anchor::Before(next.before().unwrap().clone()),
        2,
    ))
    .unwrap();
    assert_eq!(subjects(&back), vec!["A", "B"]);
    let ids = block_on(engine.window_ids(id)).unwrap();
    assert_eq!(ids.generation(), generation);
    assert_eq!(ids.ids().len(), 5);
    let around =
        block_on(engine.read_window(id, generation, Anchor::Around(ids.ids()[4]), 10)).unwrap();
    assert_eq!(around.total(), Count::Exact(5));

    // A cursor carried to another spec, or an id outside the window.
    let flagged = block_on(engine.open_window(WindowSpec::Flagged, Anchor::Top, 2)).unwrap();
    assert!(matches!(
        block_on(engine.read_window(
            flagged.window_id(),
            flagged.generation(),
            Anchor::After(first.after().unwrap().clone()),
            2
        )),
        Err(EngineError::InvalidInput { .. })
    ));
    assert!(matches!(
        block_on(engine.read_window(id, generation, Anchor::Around(i64::MAX), 2)),
        Err(EngineError::NotFound { .. })
    ));

    // Another import moves the shared generation.
    import(&engine, mbox(scratch.path(), "Later.mbox", &[("F", None)]));
    let current = match block_on(engine.read_window(id, generation, Anchor::Top, 2)) {
        Err(EngineError::ResetRequired { generation }) => generation,
        other => panic!("expected ResetRequired, got {other:?}"),
    };
    assert!(current > generation);
    assert!(matches!(
        block_on(engine.open_window(spec, Anchor::After(first.after().unwrap().clone()), 2)),
        Err(EngineError::ResetRequired { .. })
    ));
    assert!(matches!(
        block_on(engine.read_window(
            id,
            current,
            Anchor::After(first.after().unwrap().clone()),
            2
        )),
        Err(EngineError::ResetRequired { .. })
    ));
    block_on(engine.close_window(id));
    assert!(matches!(
        block_on(engine.read_window(id, current, Anchor::Top, 2)),
        Err(EngineError::NotFound { .. })
    ));
    block_on(engine.shutdown()).unwrap();
}

/// A flagged message with occurrences in a folder and two labels
/// shows once in `Flagged`, through its folder occurrence; `Unified`
/// lists occurrences by role (plan M1.3 acceptance).
#[test]
fn a_flagged_message_shows_once_through_its_folder_occurrence() {
    let scratch = ScratchDir::new("windows-flagged-once");
    let engine = block_on(Engine::open(scratch.path().join("store"))).unwrap();
    import(
        &engine,
        mbox(
            scratch.path(),
            "Inbox.mbox",
            &[("Starred", Some("Thu, 15 Oct 2026 10:00:00 +0000"))],
        ),
    );
    let filed = value(&engine, "SELECT occurrence_id FROM occurrences");
    rewrite(&engine, |tx| {
        tx.execute_batch(
            "UPDATE mailboxes SET role = 'inbox';
             INSERT INTO mailboxes (account_id, remote_name, kind, selectable, sync_state)
               SELECT account_id, 'Work', 'label', 1, 'synced' FROM accounts
               WHERE provider = 'local_archive';
             INSERT INTO mailboxes (account_id, remote_name, kind, selectable, sync_state)
               SELECT account_id, 'Later', 'label', 1, 'synced' FROM accounts
               WHERE provider = 'local_archive';
             INSERT INTO occurrences (account_id, mailbox_id, message_id, f_flagged, state,
               first_seen_at, last_seen_at)
               SELECT o.account_id, m.mailbox_id, o.message_id, 1, 'present', 0, 0
               FROM occurrences o, mailboxes m WHERE m.kind = 'label';
             UPDATE occurrences SET f_flagged = 1;",
        )
        .map_err(sql_error)
    });
    let flagged = block_on(engine.open_window(WindowSpec::Flagged, Anchor::Top, 10)).unwrap();
    let ids: Vec<i64> = flagged
        .rows()
        .iter()
        .map(|row| row.occurrence_id())
        .collect();
    assert_eq!(ids, vec![filed]);
    assert_eq!(flagged.total(), Count::Exact(1));
    let unified =
        block_on(engine.open_window(WindowSpec::Unified(MailboxRole::Inbox), Anchor::Top, 10))
            .unwrap();
    assert_eq!(unified.rows().len(), 1);
    block_on(engine.shutdown()).unwrap();
}

/// A `Thread` window lists a thread's messages once each.
#[test]
fn a_thread_window_lists_its_messages() {
    let scratch = ScratchDir::new("windows-thread");
    let engine = block_on(Engine::open(scratch.path().join("store"))).unwrap();
    let path = scratch.path().join("Inbox.mbox");
    fs::write(
        &path,
        "From sender@example.test Sat Oct 17 12:00:00 2026\n\
         Message-ID: <a@x>\nSubject: Plans\nDate: Thu, 15 Oct 2026 10:00:00 +0000\n\nfirst\n\
         From sender@example.test Sat Oct 17 12:00:00 2026\n\
         Message-ID: <b@x>\nReferences: <a@x>\nSubject: Re: Plans\n\
         Date: Fri, 16 Oct 2026 10:00:00 +0000\n\nsecond\n\
         From sender@example.test Sat Oct 17 12:00:00 2026\n\
         Message-ID: <c@x>\nSubject: Other\n\nthird\n",
    )
    .unwrap();
    import(&engine, path);
    let deadline = Instant::now() + PATIENCE;
    while value(&engine, "SELECT count(*) FROM d_thread_members") < 3 {
        assert!(Instant::now() < deadline, "threads were never built");
        std::thread::sleep(Duration::from_millis(10));
    }
    let thread = value(
        &engine,
        "SELECT m.thread_id FROM d_thread_members m JOIN d_threads t USING (thread_id)
         WHERE t.subject_norm = 'plans'",
    );
    let window = block_on(engine.open_window(WindowSpec::Thread(thread), Anchor::Top, 10)).unwrap();
    assert_eq!(subjects(&window), vec!["Re: Plans", "Plans"]);
    block_on(engine.shutdown()).unwrap();
}
