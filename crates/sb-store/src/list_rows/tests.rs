use proptest::prelude::*;
use rusqlite::Connection;
use sb_test_support::real_fs;

use super::*;
use crate::schema::{MIGRATIONS, run_migrations};
use crate::testing::fixtures::{
    archive_account as archive, mailbox as add_mailbox, message as add_message, migrated,
};
use crate::testing::{FIXED_NOW_MS, test_seams};

/// The mailboxes every synthetic store holds: (name, kind, role).
const MAILBOXES: [(&str, &str, Option<&str>); 5] = [
    ("Inbox", "folder", Some("inbox")),
    ("Projects", "folder", None),
    ("Work", "label", None),
    ("Starred", "label", Some("flagged")),
    ("All Mail", "virtual", Some("all")),
];

/// Logical messages per synthetic store.
const MESSAGES: usize = 4;

const STATES: [&str; 5] = [
    "present",
    "vanish_pending",
    "vanished",
    "epoch_stale",
    "superseded",
];

const KINDS: [&str; 3] = ["folder", "label", "virtual"];

fn add_occurrence(
    conn: &Connection,
    account: &str,
    mailbox: i64,
    message: i64,
    state: &str,
    seen: bool,
    flagged: bool,
) -> i64 {
    conn.execute(
        "INSERT INTO occurrences (account_id, mailbox_id, message_id, f_seen, f_flagged, state,
         first_seen_at, last_seen_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
        params![
            account,
            mailbox,
            message,
            seen,
            flagged,
            state,
            FIXED_NOW_MS
        ],
    )
    .unwrap();
    conn.last_insert_rowid()
}

/// One list row: (occurrence, mailbox, role, message, unread, flagged,
/// representative).
type Row = (i64, i64, Option<String>, i64, bool, bool, bool);

fn rows(conn: &Connection) -> Vec<Row> {
    let mut statement = conn
        .prepare(
            "SELECT occurrence_id, mailbox_id, mailbox_role, message_id, unread, flagged,
             representative FROM d_list_rows ORDER BY occurrence_id",
        )
        .unwrap();
    statement
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// Every column of every row, to tell a real change from a no-op.
fn snapshot(conn: &Connection) -> Vec<Vec<rusqlite::types::Value>> {
    let mut statement = conn
        .prepare(&format!(
            "SELECT {COLUMNS} FROM d_list_rows ORDER BY occurrence_id"
        ))
        .unwrap();
    let width = statement.column_count();
    statement
        .query_map([], |row| (0..width).map(|i| row.get(i)).collect())
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// A store at migration 2 with imported-era rows, then migration 3:
/// rows for exactly the present occurrences, fields from headers over
/// the summary, the folder occurrence as representative even when a
/// label's is older, and the first generation.
#[test]
fn migration_3_fills_rows_for_existing_occurrences() {
    let mut conn = migrated(&MIGRATIONS[..2]);
    let account = archive(&conn);
    let folder = add_mailbox(&conn, &account, "Inbox", "folder", Some("inbox"));
    let label = add_mailbox(&conn, &account, "Work", "label", None);
    conn.execute(
        "INSERT INTO acquisitions (content_sha256, byte_length, object_state, first_acquired_at)
         VALUES (?1, 10, 'published', ?2)",
        params!["a".repeat(64), FIXED_NOW_MS],
    )
    .unwrap();
    let acquisition = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO d_message_summary (acquisition_id, parser_version, parse_state, subject,
         from_addr, from_name, date_header, has_attachments, snippet)
         VALUES (?1, 1, 'ok', 'Summary subject', 'ada@example.test', '', 1000, 1, 'Hello')",
        [acquisition],
    )
    .unwrap();
    let summarized = add_message(&conn, &account, Some(acquisition));
    let labelled = add_occurrence(&conn, &account, label, summarized, "present", true, false);
    let filed = add_occurrence(&conn, &account, folder, summarized, "present", false, true);
    add_occurrence(&conn, &account, folder, summarized, "vanished", true, false);
    let headed = add_message(&conn, &account, Some(acquisition));
    conn.execute(
        "INSERT INTO message_headers (message_id, raw_fields, subject, from_json, date_header,
         fetched_at) VALUES (?1, x'00', 'Header subject',
         '[{\"address\":\"bea@example.test\",\"name\":\"Bea\"}]', 2000, ?2)",
        params![headed, FIXED_NOW_MS],
    )
    .unwrap();
    let header_row = add_occurrence(&conn, &account, folder, headed, "present", true, false);
    let bare = add_message(&conn, &account, None);
    let bare_row = add_occurrence(&conn, &account, label, bare, "present", true, false);

    run_migrations(&mut conn, &test_seams(real_fs()), MIGRATIONS).unwrap();

    assert_eq!(
        rows(&conn),
        vec![
            (labelled, label, None, summarized, false, false, false),
            (
                filed,
                folder,
                Some("inbox".into()),
                summarized,
                true,
                true,
                true
            ),
            (
                header_row,
                folder,
                Some("inbox".into()),
                headed,
                false,
                false,
                true
            ),
            (bare_row, label, None, bare, false, false, true),
        ]
    );
    let shown = |occurrence: i64| -> (i64, Option<String>, Option<String>, Option<String>, bool) {
        conn.query_row(
            "SELECT sort_date, sender, subject, snippet, has_attachments FROM d_list_rows
             WHERE occurrence_id = ?1",
            [occurrence],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap()
    };
    let text = |value: &str| Some(value.to_owned());
    // An empty display name falls back to the address.
    assert_eq!(
        shown(filed),
        (
            1000,
            text("ada@example.test"),
            text("Summary subject"),
            text("Hello"),
            true
        )
    );
    assert_eq!(
        shown(header_row),
        (
            2000,
            text("Bea"),
            text("Header subject"),
            text("Hello"),
            true
        )
    );
    // No summary and no headers: the first-seen time, nothing to show.
    assert_eq!(shown(bare_row), (FIXED_NOW_MS, None, None, None, false));
    assert_eq!(generation(&conn).unwrap(), FIRST_GENERATION);
}

/// A generation that isn't a decimal count is `Corrupt`, and advancing
/// adds exactly one.
#[test]
fn the_generation_is_a_checked_decimal_count() {
    let mut conn = migrated(MIGRATIONS);
    let tx = conn.transaction().unwrap();
    assert_eq!(advance_generation(&tx).unwrap(), FIRST_GENERATION + 1);
    assert_eq!(generation(&tx).unwrap(), FIRST_GENERATION + 1);
    tx.execute(
        "UPDATE store_meta SET value = 'many' WHERE key = ?1",
        [GENERATION_KEY],
    )
    .unwrap();
    assert!(matches!(generation(&tx), Err(StoreError::Corrupt { .. })));
    tx.execute(
        "UPDATE store_meta SET value = ?2 WHERE key = ?1",
        params![GENERATION_KEY, u64::MAX.to_string()],
    )
    .unwrap();
    assert!(matches!(
        advance_generation(&tx),
        Err(StoreError::Corrupt { .. })
    ));
}

/// One authoritative change, as a later sync or command would make it.
#[derive(Debug, Clone)]
enum Change {
    Insert {
        message: usize,
        mailbox: usize,
        seen: bool,
        flagged: bool,
    },
    State {
        occurrence: usize,
        state: usize,
    },
    Move {
        occurrence: usize,
        mailbox: usize,
    },
    Flags {
        occurrence: usize,
        seen: bool,
        flagged: bool,
    },
    Kind {
        mailbox: usize,
        kind: usize,
    },
    Rebind {
        occurrence: usize,
        message: usize,
    },
}

fn change() -> impl Strategy<Value = Change> {
    prop_oneof![
        3 => (0..MESSAGES, 0..MAILBOXES.len(), any::<bool>(), any::<bool>()).prop_map(
            |(message, mailbox, seen, flagged)| Change::Insert { message, mailbox, seen, flagged }
        ),
        2 => (any::<usize>(), 0..STATES.len())
            .prop_map(|(occurrence, state)| Change::State { occurrence, state }),
        2 => (any::<usize>(), 0..MAILBOXES.len())
            .prop_map(|(occurrence, mailbox)| Change::Move { occurrence, mailbox }),
        2 => (any::<usize>(), any::<bool>(), any::<bool>())
            .prop_map(|(occurrence, seen, flagged)| Change::Flags { occurrence, seen, flagged }),
        1 => (0..MAILBOXES.len(), 0..KINDS.len())
            .prop_map(|(mailbox, kind)| Change::Kind { mailbox, kind }),
        1 => (any::<usize>(), 0..MESSAGES)
            .prop_map(|(occurrence, message)| Change::Rebind { occurrence, message }),
    ]
}

/// The oracle's occurrence.
#[derive(Debug, Clone)]
struct Modelled {
    id: i64,
    message: usize,
    mailbox: usize,
    state: usize,
    seen: bool,
    flagged: bool,
}

/// What the rows must be, from the model alone (spec/02): one per
/// present occurrence, and per message with any, exactly one
/// representative — folder before label before virtual, then the
/// lowest occurrence id.
fn expected(
    occurrences: &[Modelled],
    kinds: &[usize],
    mailboxes: &[i64],
    messages: &[i64],
) -> Vec<Row> {
    let present: Vec<&Modelled> = occurrences.iter().filter(|o| o.state == 0).collect();
    let representative = |o: &Modelled| {
        present
            .iter()
            .filter(|other| other.message == o.message)
            .min_by_key(|other| (kinds[other.mailbox], other.id))
            .is_some_and(|winner| winner.id == o.id)
    };
    let mut rows: Vec<Row> = present
        .iter()
        .map(|o| {
            (
                o.id,
                mailboxes[o.mailbox],
                MAILBOXES[o.mailbox].2.map(str::to_owned),
                messages[o.message],
                !o.seen,
                o.flagged,
                representative(o),
            )
        })
        .collect();
    rows.sort();
    rows
}

proptest! {
    /// Every logical message with a present occurrence has exactly one
    /// representative, chosen by spec/02's rule, before and after
    /// moves, vanishes and flag changes; rows exist for exactly the
    /// present occurrences, also after an occurrence moves to another
    /// message; and `refresh` reports a change exactly
    /// when the rows changed (plan M1.3 acceptance).
    #[test]
    fn representatives_follow_every_occurrence_change(
        changes in proptest::collection::vec(change(), 1..40)
    ) {
        let mut conn = migrated(MIGRATIONS);
        let account = archive(&conn);
        let mailboxes: Vec<i64> = MAILBOXES
            .iter()
            .map(|(name, kind, role)| add_mailbox(&conn, &account, name, kind, *role))
            .collect();
        let messages: Vec<i64> = (0..MESSAGES).map(|_| add_message(&conn, &account, None)).collect();
        let mut kinds: Vec<usize> = MAILBOXES
            .iter()
            .map(|(_, kind, _)| KINDS.iter().position(|k| k == kind).unwrap())
            .collect();
        let mut occurrences: Vec<Modelled> = Vec::new();

        for change in changes {
            let tx = conn.transaction().unwrap();
            let pick = |index: usize| (!occurrences.is_empty()).then(|| index % occurrences.len());
            let touched: BTreeSet<i64> = match change {
                Change::Insert { message, mailbox, seen, flagged } => {
                    let id = add_occurrence(
                        &tx, &account, mailboxes[mailbox], messages[message], "present", seen, flagged,
                    );
                    occurrences.push(Modelled { id, message, mailbox, state: 0, seen, flagged });
                    [id].into()
                }
                Change::State { occurrence, state } => {
                    let Some(i) = pick(occurrence) else { continue };
                    tx.execute(
                        "UPDATE occurrences SET state = ?2 WHERE occurrence_id = ?1",
                        params![occurrences[i].id, STATES[state]],
                    ).unwrap();
                    occurrences[i].state = state;
                    [occurrences[i].id].into()
                }
                Change::Move { occurrence, mailbox } => {
                    let Some(i) = pick(occurrence) else { continue };
                    tx.execute(
                        "UPDATE occurrences SET mailbox_id = ?2 WHERE occurrence_id = ?1",
                        params![occurrences[i].id, mailboxes[mailbox]],
                    ).unwrap();
                    occurrences[i].mailbox = mailbox;
                    [occurrences[i].id].into()
                }
                Change::Flags { occurrence, seen, flagged } => {
                    let Some(i) = pick(occurrence) else { continue };
                    tx.execute(
                        "UPDATE occurrences SET f_seen = ?2, f_flagged = ?3 WHERE occurrence_id = ?1",
                        params![occurrences[i].id, seen, flagged],
                    ).unwrap();
                    occurrences[i].seen = seen;
                    occurrences[i].flagged = flagged;
                    [occurrences[i].id].into()
                }
                Change::Kind { mailbox, kind } => {
                    tx.execute(
                        "UPDATE mailboxes SET kind = ?2 WHERE mailbox_id = ?1",
                        params![mailboxes[mailbox], KINDS[kind]],
                    ).unwrap();
                    kinds[mailbox] = kind;
                    occurrences.iter().filter(|o| o.mailbox == mailbox).map(|o| o.id).collect()
                }
                // The message it left must elect a new representative too.
                Change::Rebind { occurrence, message } => {
                    let Some(i) = pick(occurrence) else { continue };
                    tx.execute(
                        "UPDATE occurrences SET message_id = ?2 WHERE occurrence_id = ?1",
                        params![occurrences[i].id, messages[message]],
                    ).unwrap();
                    occurrences[i].message = message;
                    [occurrences[i].id].into()
                }
            };
            let before = snapshot(&tx);
            let changed = refresh(&tx, &touched).unwrap();
            prop_assert_eq!(changed, snapshot(&tx) != before);
            tx.commit().unwrap();
            prop_assert_eq!(rows(&conn), expected(&occurrences, &kinds, &mailboxes, &messages));
        }
    }
}

/// The writer announces a list generation after the job that made it
/// commits, once; a rolled-back job and a write that leaves the
/// generation announce nothing (spec/07 "Subscriptions").
#[test]
fn committed_generations_are_announced_once() {
    use std::sync::{Arc, Mutex};

    let (_scratch, paths) = crate::testing::scratch_paths("list-observer");
    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    let heard = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&heard);
    store.observe_list_generation(Arc::new(move |generation| {
        sink.lock().unwrap().push(generation);
    }));
    let write = |advance: bool, fail: bool| {
        sb_test_support::block_on(store.write(crate::WritePriority::User, move |tx| {
            if advance {
                advance_generation(tx)?;
            }
            if fail {
                return Err(StoreError::internal("rolled back"));
            }
            Ok(())
        }))
    };
    write(true, false).unwrap();
    assert!(write(true, true).is_err());
    write(false, false).unwrap();
    write(true, false).unwrap();
    assert_eq!(
        *heard.lock().unwrap(),
        vec![FIRST_GENERATION + 1, FIRST_GENERATION + 2]
    );
    store.close().unwrap();
}
