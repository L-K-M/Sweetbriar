use std::collections::BTreeSet;

use rusqlite::{Connection, params};
use sb_test_support::real_fs;

use super::*;
use crate::schema::{MIGRATIONS, run_migrations};
use crate::testing::{FIXED_NOW_MS, test_seams};

fn store() -> (Connection, String) {
    let mut conn = Connection::open_in_memory().unwrap();
    run_migrations(&mut conn, &test_seams(real_fs()), MIGRATIONS).unwrap();
    let archive = conn
        .query_row(
            "SELECT account_id FROM accounts WHERE provider = 'local_archive'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    (conn, archive)
}

fn mailbox(conn: &Connection, account: &str, name: &str, kind: &str, role: Option<&str>) -> i64 {
    conn.execute(
        "INSERT INTO mailboxes (account_id, remote_name, role, kind, selectable, sync_state)
         VALUES (?1, ?2, ?3, ?4, ?5, 'synced')",
        params![account, name, role, kind, kind != "virtual"],
    )
    .unwrap();
    conn.last_insert_rowid()
}

/// A mailbox as (id, name, role, kind, selectable, total, unread).
type Summary<'a> = (
    i64,
    &'a str,
    Option<MailboxRole>,
    MailboxKind,
    bool,
    u64,
    u64,
);

/// A listed occurrence in `mailbox`, read or not.
fn occurrence(conn: &mut Connection, account: &str, mailbox: i64, seen: bool) {
    let tx = conn.transaction().unwrap();
    tx.execute(
        "INSERT INTO logical_messages (account_id, created_at) VALUES (?1, ?2)",
        params![account, FIXED_NOW_MS],
    )
    .unwrap();
    let message = tx.last_insert_rowid();
    tx.execute(
        "INSERT INTO occurrences (account_id, mailbox_id, message_id, f_seen, state,
         first_seen_at, last_seen_at) VALUES (?1, ?2, ?3, ?4, 'present', ?5, ?5)",
        params![account, mailbox, message, seen, FIXED_NOW_MS],
    )
    .unwrap();
    let id = tx.last_insert_rowid();
    list_rows::refresh(&tx, &BTreeSet::from([id])).unwrap();
    tx.commit().unwrap();
}

/// Accounts list by sort order then id, with their kind, state and
/// reason.
#[test]
fn accounts_list_in_sort_order() {
    let (mut conn, archive) = store();
    conn.execute(
        "INSERT INTO accounts (account_id, kind, provider, display_name, state, state_reason,
         sort_order, created_at)
         VALUES ('00000000-0000-4000-8000-0000000000b0', 'imap', 'proton_bridge', 'Work',
                 'needs_attention', 'Bridge is not running', -1, 0)",
        [],
    )
    .unwrap();
    let tx = conn.transaction().unwrap();
    let listed = accounts(&tx).unwrap();
    assert_eq!(
        listed,
        vec![
            StoredAccount {
                account_id: "00000000-0000-4000-8000-0000000000b0".to_owned(),
                kind: AccountKind::Imap,
                display_name: "Work".to_owned(),
                state: AccountState::NeedsAttention,
                state_reason: Some("Bridge is not running".to_owned()),
                sort_order: -1,
            },
            StoredAccount {
                account_id: archive,
                kind: AccountKind::Local,
                display_name: "Local Archive".to_owned(),
                state: AccountState::Active,
                state_reason: None,
                sort_order: 0,
            },
        ]
    );
}

/// Mailboxes list folders, then labels, then virtual ones, each by
/// name, with their present and unread occurrences; an empty mailbox is
/// listed with zeros, and the list names its generation (spec/07).
#[test]
fn mailboxes_carry_counts_and_their_generation() {
    let (mut conn, archive) = store();
    let work = mailbox(&conn, &archive, "Work", "label", None);
    let inbox = mailbox(&conn, &archive, "Inbox", "folder", Some("inbox"));
    let all = mailbox(&conn, &archive, "All Mail", "virtual", Some("all"));
    let archived = mailbox(&conn, &archive, "Archive", "folder", None);
    occurrence(&mut conn, &archive, inbox, false);
    occurrence(&mut conn, &archive, inbox, true);
    occurrence(&mut conn, &archive, work, false);
    let tx = conn.transaction().unwrap();
    let list = mailboxes(&tx, &archive).unwrap().unwrap();
    assert_eq!(list.generation, list_rows::generation(&tx).unwrap());
    let summary: Vec<Summary> = list
        .mailboxes
        .iter()
        .map(|m| {
            (
                m.mailbox_id,
                m.name.as_str(),
                m.role,
                m.kind,
                m.selectable,
                m.total,
                m.unread,
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            (archived, "Archive", None, MailboxKind::Folder, true, 0, 0),
            (
                inbox,
                "Inbox",
                Some(MailboxRole::Inbox),
                MailboxKind::Folder,
                true,
                2,
                1
            ),
            (work, "Work", None, MailboxKind::Label, true, 1, 1),
            (
                all,
                "All Mail",
                Some(MailboxRole::All),
                MailboxKind::Virtual,
                false,
                0,
                0
            ),
        ]
    );
}

/// An unknown account has no mailbox list; a known one without
/// mailboxes has an empty one.
#[test]
fn unknown_accounts_have_no_mailbox_list() {
    let (mut conn, archive) = store();
    let tx = conn.transaction().unwrap();
    assert_eq!(mailboxes(&tx, "no-such-account").unwrap(), None);
    assert!(
        mailboxes(&tx, &archive)
            .unwrap()
            .unwrap()
            .mailboxes
            .is_empty()
    );
}

/// Every role `mailboxes.role` allows parses back.
#[test]
fn every_role_parses() {
    for role in [
        MailboxRole::Inbox,
        MailboxRole::Sent,
        MailboxRole::Drafts,
        MailboxRole::Trash,
        MailboxRole::Junk,
        MailboxRole::Archive,
        MailboxRole::All,
        MailboxRole::Flagged,
    ] {
        assert_eq!(MailboxRole::parse(role.as_str()), Some(role));
    }
    assert_eq!(MailboxRole::parse("spam"), None);
}
