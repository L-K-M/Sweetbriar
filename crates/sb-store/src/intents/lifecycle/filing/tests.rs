//! Filing's planning, policy, local effect, undo reads and cancel
//! (spec/04-sync-proton "Intents and the mutation matrix", spec/07
//! "Local archive").

use rusqlite::{Connection, params};

use super::*;
use crate::intents::lifecycle::cancel_intents;
use crate::schema::MIGRATIONS;
use crate::testing::fixtures::{mailbox, migrated};

const PROTON: &str = "proton-1";
const OTHER: &str = "proton-2";
const NOW: i64 = 1_700_000_000_000;

struct Proton {
    inbox: i64,
    archive: i64,
    trash: i64,
    sent: i64,
    work: i64,
    message: i64,
    in_inbox: i64,
    in_work: i64,
}

fn account(conn: &Connection, id: &str, kind: &str, provider: &str) {
    conn.execute(
        "INSERT INTO accounts (account_id, kind, provider, display_name, state,
         config_generation, sort_order, created_at)
         VALUES (?1, ?2, ?3, ?1, 'active', 1, 1, 0)",
        params![id, kind, provider],
    )
    .unwrap();
}

/// The local archive account every migrated store holds.
fn local_archive(conn: &Connection) -> String {
    conn.query_row(
        "SELECT account_id FROM accounts WHERE provider = 'local_archive'",
        [],
        |row| row.get(0),
    )
    .unwrap()
}

fn message(conn: &Connection, account: &str, provider_key: Option<&str>) -> i64 {
    conn.execute(
        "INSERT INTO logical_messages (account_id, provider_key, created_at) VALUES (?1, ?2, 0)",
        params![account, provider_key],
    )
    .unwrap();
    conn.last_insert_rowid()
}

fn occurrence(conn: &Connection, account: &str, mailbox_id: i64, message: i64) -> i64 {
    conn.execute(
        "INSERT INTO occurrences (account_id, mailbox_id, message_id, internal_date, f_seen,
         f_flagged, state, first_seen_at, last_seen_at)
         VALUES (?1, ?2, ?3, 42, 1, 1, 'present', 0, 0)",
        params![account, mailbox_id, message],
    )
    .unwrap();
    conn.last_insert_rowid()
}

/// A Proton account with Inbox, Archive, Trash, Sent and the label
/// Work, and one starred, read message in Inbox labelled Work.
fn proton(conn: &Connection) -> Proton {
    account(conn, PROTON, "proton", "proton_api");
    let inbox = mailbox(conn, PROTON, "Inbox", "folder", Some("inbox"));
    let archive = mailbox(conn, PROTON, "Archive", "folder", Some("archive"));
    let trash = mailbox(conn, PROTON, "Trash", "folder", Some("trash"));
    let sent = mailbox(conn, PROTON, "Sent", "folder", Some("sent"));
    let work = mailbox(conn, PROTON, "Work", "label", None);
    let message = message(conn, PROTON, Some("msg-1"));
    let in_inbox = occurrence(conn, PROTON, inbox, message);
    let in_work = occurrence(conn, PROTON, work, message);
    Proton {
        inbox,
        archive,
        trash,
        sent,
        work,
        message,
        in_inbox,
        in_work,
    }
}

/// Plan and commit; ids run `intent-1`, `intent-2`, … across a test.
fn file(
    conn: &Connection,
    account: &str,
    messages: &[i64],
    filing: Filing,
) -> Result<Filed, StoreError> {
    let tx = conn.unchecked_transaction().unwrap();
    let mut next: i64 = tx
        .query_row("SELECT COUNT(*) FROM intents", [], |row| row.get(0))
        .unwrap();
    let filed = plan_filing(
        &tx,
        account,
        messages,
        filing,
        IntentOrigin::User,
        NOW,
        &mut || {
            next += 1;
            format!("intent-{next}")
        },
    )?;
    tx.commit().unwrap();
    Ok(filed)
}

fn intents(ids: &[&str]) -> Filed {
    Filed::Intents(ids.iter().map(|id| (*id).to_owned()).collect())
}

/// `(op, args_json, target_json, origin)` of a stored intent.
fn stored(
    conn: &Connection,
    intent_id: &str,
) -> (String, serde_json::Value, serde_json::Value, String) {
    conn.query_row(
        "SELECT op, args_json, target_json, origin FROM intents WHERE intent_id = ?1",
        [intent_id],
        |row| {
            let args: String = row.get(1)?;
            let target: String = row.get(2)?;
            Ok((
                row.get(0)?,
                serde_json::from_str(&args).unwrap(),
                serde_json::from_str(&target).unwrap(),
                row.get(3)?,
            ))
        },
    )
    .unwrap()
}

/// The mailboxes the message is currently in, ascending.
fn mailboxes_of(conn: &Connection, message: i64) -> Vec<i64> {
    let mut statement = conn
        .prepare(
            "SELECT mailbox_id FROM occurrences
             WHERE message_id = ?1 AND state IN ('present', 'vanish_pending') ORDER BY mailbox_id",
        )
        .unwrap();
    statement
        .query_map([message], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn invalid_message(result: Result<Filed, StoreError>) -> String {
    match result {
        Err(StoreError::InvalidInput { message }) => message,
        other => panic!("expected InvalidInput, got {other:?}"),
    }
}

#[test]
fn a_proton_move_plans_one_intent_with_its_local_effect() {
    let conn = migrated(MIGRATIONS);
    let seed = proton(&conn);

    let filed = file(
        &conn,
        PROTON,
        &[seed.message],
        Filing::Move {
            to_mailbox_id: seed.archive,
        },
    )
    .unwrap();

    assert_eq!(filed, intents(&["intent-1"]));
    let (op, args, target, origin) = stored(&conn, "intent-1");
    assert_eq!(op, "move");
    assert_eq!(
        args,
        serde_json::json!({"v": 1, "to_mailbox_id": seed.archive, "from_mailbox_id": seed.inbox})
    );
    assert_eq!(
        target,
        serde_json::json!({"v": 1, "occurrence_ids": [seed.in_inbox, seed.in_work], "provider_key": "msg-1"})
    );
    assert_eq!(origin, "user");
    assert_eq!(
        mailboxes_of(&conn, seed.message),
        vec![seed.archive, seed.work],
        "the label stays"
    );
    let (seen, flagged, date): (i64, i64, i64) = conn
        .query_row(
            "SELECT f_seen, f_flagged, internal_date FROM occurrences
             WHERE mailbox_id = ?1 AND state = 'present'",
            [seed.archive],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        (seen, flagged, date),
        (1, 1, 42),
        "the new occurrence keeps the flags"
    );
}

#[test]
fn delete_to_trash_records_the_folder_it_leaves() {
    let conn = migrated(MIGRATIONS);
    let seed = proton(&conn);

    let filed = file(&conn, PROTON, &[seed.message], Filing::DeleteToTrash).unwrap();

    assert_eq!(filed, intents(&["intent-1"]));
    let (op, args, _, _) = stored(&conn, "intent-1");
    assert_eq!(op, "delete_to_trash");
    assert_eq!(
        args,
        serde_json::json!({"v": 1, "from_mailbox_id": seed.inbox})
    );
    assert_eq!(
        mailboxes_of(&conn, seed.message),
        vec![seed.trash, seed.work]
    );
}

#[test]
fn a_message_without_a_folder_moves_from_none() {
    let conn = migrated(MIGRATIONS);
    let seed = proton(&conn);
    let loose = message(&conn, PROTON, Some("msg-2"));
    occurrence(&conn, PROTON, seed.work, loose);

    file(
        &conn,
        PROTON,
        &[loose],
        Filing::Move {
            to_mailbox_id: seed.archive,
        },
    )
    .unwrap();

    let (_, args, _, _) = stored(&conn, "intent-1");
    assert_eq!(
        args.get("from_mailbox_id"),
        Some(&serde_json::Value::Null),
        "a move from no folder records null, not an absent key"
    );
    assert_eq!(mailboxes_of(&conn, loose), vec![seed.archive, seed.work]);
}

#[test]
fn messages_already_in_the_destination_are_skipped_and_all_of_them_refuse() {
    let conn = migrated(MIGRATIONS);
    let seed = proton(&conn);
    let archived = message(&conn, PROTON, Some("msg-2"));
    occurrence(&conn, PROTON, seed.archive, archived);

    let filed = file(
        &conn,
        PROTON,
        &[archived, seed.message],
        Filing::Move {
            to_mailbox_id: seed.archive,
        },
    );
    assert_eq!(
        filed.unwrap(),
        intents(&["intent-1"]),
        "only the message in Inbox moves"
    );

    let again = file(
        &conn,
        PROTON,
        &[archived, seed.message],
        Filing::Move {
            to_mailbox_id: seed.archive,
        },
    );
    assert_eq!(
        invalid_message(again),
        "the messages are already in Archive"
    );
}

#[test]
fn a_move_refuses_destinations_outside_the_matrix() {
    let conn = migrated(MIGRATIONS);
    let seed = proton(&conn);
    account(&conn, OTHER, "proton", "proton_api");
    let elsewhere = mailbox(&conn, OTHER, "Inbox", "folder", Some("inbox"));
    let starred = mailbox(&conn, PROTON, "Starred", "virtual", Some("flagged"));
    let drafts = mailbox(&conn, PROTON, "Drafts", "folder", Some("drafts"));

    for (to, expected) in [
        (
            seed.sent,
            "messages can't be moved to Sent or Drafts".to_owned(),
        ),
        (
            drafts,
            "messages can't be moved to Sent or Drafts".to_owned(),
        ),
        (
            seed.work,
            format!("messages can't be moved to mailbox {}", seed.work),
        ),
        (
            starred,
            format!("messages can't be moved to mailbox {starred}"),
        ),
        (
            elsewhere,
            format!("mailbox {elsewhere} belongs to another account"),
        ),
    ] {
        let refused = file(
            &conn,
            PROTON,
            &[seed.message],
            Filing::Move { to_mailbox_id: to },
        );
        assert_eq!(invalid_message(refused), expected);
    }
    assert!(matches!(
        file(
            &conn,
            PROTON,
            &[seed.message],
            Filing::Move { to_mailbox_id: 999 }
        ),
        Err(StoreError::NotFound { .. })
    ));
    assert_eq!(
        mailboxes_of(&conn, seed.message),
        vec![seed.inbox, seed.work],
        "nothing moved"
    );
}

#[test]
fn scheduled_messages_refuse_the_whole_command() {
    let conn = migrated(MIGRATIONS);
    let seed = proton(&conn);
    let scheduled = mailbox(&conn, PROTON, "Scheduled", "virtual", None);
    conn.execute(
        "UPDATE mailboxes SET provider_key = '12' WHERE mailbox_id = ?1",
        [scheduled],
    )
    .unwrap();
    let later = message(&conn, PROTON, Some("msg-2"));
    occurrence(&conn, PROTON, scheduled, later);

    let refused = file(&conn, PROTON, &[seed.message, later], Filing::DeleteToTrash);

    assert_eq!(
        invalid_message(refused),
        "messages in Scheduled can't be filed"
    );
    assert_eq!(
        mailboxes_of(&conn, seed.message),
        vec![seed.inbox, seed.work]
    );
}

#[test]
fn labels_are_added_and_removed_once() {
    let conn = migrated(MIGRATIONS);
    let seed = proton(&conn);
    let home = mailbox(&conn, PROTON, "Home", "label", None);

    let added = file(
        &conn,
        PROTON,
        &[seed.message],
        Filing::AddLabel {
            label_mailbox_id: home,
        },
    )
    .unwrap();
    assert_eq!(added, intents(&["intent-1"]));
    let (op, args, _, _) = stored(&conn, "intent-1");
    assert_eq!(
        (op.as_str(), args),
        ("copy", serde_json::json!({"v": 1, "to_mailbox_id": home}))
    );
    assert_eq!(
        mailboxes_of(&conn, seed.message),
        vec![seed.inbox, seed.work, home]
    );
    let again = file(
        &conn,
        PROTON,
        &[seed.message],
        Filing::AddLabel {
            label_mailbox_id: home,
        },
    );
    assert_eq!(
        again.unwrap(),
        intents(&[]),
        "a labelled message plans nothing"
    );

    let removed = file(
        &conn,
        PROTON,
        &[seed.message],
        Filing::RemoveLabel {
            label_mailbox_id: seed.work,
        },
    );
    assert_eq!(removed.unwrap(), intents(&["intent-2"]));
    let (op, args, _, _) = stored(&conn, "intent-2");
    assert_eq!(
        (op.as_str(), args),
        (
            "remove_label",
            serde_json::json!({"v": 1, "from_mailbox_id": seed.work})
        )
    );
    assert_eq!(mailboxes_of(&conn, seed.message), vec![seed.inbox, home]);

    let refused = file(
        &conn,
        PROTON,
        &[seed.message],
        Filing::AddLabel {
            label_mailbox_id: seed.archive,
        },
    );
    assert_eq!(
        invalid_message(refused),
        format!("mailbox {} isn't a label", seed.archive)
    );
}

#[test]
fn the_local_archive_moves_at_once_and_creates_its_trash() {
    let conn = migrated(MIGRATIONS);
    let local = local_archive(&conn);
    let imported = mailbox(&conn, &local, "Imported", "folder", None);
    let kept = mailbox(&conn, &local, "Kept", "folder", None);
    let message = message(&conn, &local, None);
    occurrence(&conn, &local, imported, message);

    let moved = file(
        &conn,
        &local,
        &[message],
        Filing::Move {
            to_mailbox_id: kept,
        },
    )
    .unwrap();
    assert_eq!(
        moved,
        Filed::Local(vec![LocalMove {
            message_id: message,
            from_mailbox_id: imported,
            to_mailbox_id: kept
        }])
    );
    let trashed = file(&conn, &local, &[message], Filing::DeleteToTrash).unwrap();
    let trash: i64 = conn
        .query_row(
            "SELECT mailbox_id FROM mailboxes WHERE account_id = ?1 AND role = 'trash'",
            [&local],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        trashed,
        Filed::Local(vec![LocalMove {
            message_id: message,
            from_mailbox_id: kept,
            to_mailbox_id: trash
        }])
    );
    assert_eq!(mailboxes_of(&conn, message), vec![trash]);
    let intents: i64 = conn
        .query_row("SELECT COUNT(*) FROM intents", [], |row| row.get(0))
        .unwrap();
    assert_eq!(intents, 0, "the local archive plans no intent");

    let other = self::message(&conn, &local, None);
    occurrence(&conn, &local, imported, other);
    file(&conn, &local, &[other], Filing::DeleteToTrash).unwrap();
    let trashes: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM mailboxes WHERE account_id = ?1 AND role = 'trash'",
            [&local],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(trashes, 1, "the Trash is created once");

    let refused = file(
        &conn,
        &local,
        &[message],
        Filing::AddLabel {
            label_mailbox_id: kept,
        },
    );
    assert_eq!(invalid_message(refused), "the local archive has no labels");
}

#[test]
fn an_imported_trash_folder_becomes_the_local_trash() {
    let conn = migrated(MIGRATIONS);
    let local = local_archive(&conn);
    let imported = mailbox(&conn, &local, "Imported", "folder", None);
    let trash = mailbox(&conn, &local, "Trash", "folder", None);
    let message = message(&conn, &local, None);
    occurrence(&conn, &local, imported, message);

    file(&conn, &local, &[message], Filing::DeleteToTrash).unwrap();

    assert_eq!(mailboxes_of(&conn, message), vec![trash]);
    let role: Option<String> = conn
        .query_row(
            "SELECT role FROM mailboxes WHERE mailbox_id = ?1",
            [trash],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(role.as_deref(), Some("trash"));
}

#[test]
fn a_selection_must_belong_to_one_account() {
    let conn = migrated(MIGRATIONS);
    let seed = proton(&conn);
    account(&conn, OTHER, "proton", "proton_api");
    let theirs = message(&conn, OTHER, Some("msg-9"));
    let tx = conn.unchecked_transaction().unwrap();

    assert_eq!(
        filing_account(&tx, &[seed.message]).unwrap().as_deref(),
        Some(PROTON)
    );
    assert_eq!(filing_account(&tx, &[]).unwrap(), None);
    assert!(matches!(
        filing_account(&tx, &[seed.message, theirs]),
        Err(StoreError::InvalidInput { .. })
    ));
    assert!(matches!(
        filing_account(&tx, &[999]),
        Err(StoreError::NotFound { .. })
    ));
    drop(tx);
    let refused = file(&conn, PROTON, &[theirs], Filing::DeleteToTrash);
    assert_eq!(
        invalid_message(refused),
        format!("message {theirs} belongs to another account")
    );
}

#[test]
fn a_stored_move_reads_back_for_undo() {
    let conn = migrated(MIGRATIONS);
    let seed = proton(&conn);
    file(
        &conn,
        PROTON,
        &[seed.message],
        Filing::Move {
            to_mailbox_id: seed.archive,
        },
    )
    .unwrap();
    let trashed = message(&conn, PROTON, Some("msg-2"));
    occurrence(&conn, PROTON, seed.archive, trashed);
    file(&conn, PROTON, &[trashed], Filing::DeleteToTrash).unwrap();

    let tx = conn.unchecked_transaction().unwrap();
    let moved = move_for_undo(&tx, "intent-1").unwrap().unwrap();
    assert_eq!(
        moved,
        UndoableMove {
            account_id: PROTON.to_owned(),
            message_id: seed.message,
            to_mailbox_id: seed.archive,
            from_mailbox_id: Some(seed.inbox),
            state: "pending".to_owned(),
        }
    );
    let deleted = move_for_undo(&tx, "intent-2").unwrap().unwrap();
    assert_eq!(
        (deleted.message_id, deleted.to_mailbox_id),
        (trashed, seed.trash)
    );
    assert_eq!(deleted.from_mailbox_id, Some(seed.archive));
    assert!(move_for_undo(&tx, "unknown").unwrap().is_none());
}

fn undo(conn: &Connection, intent_id: &str) -> Result<Vec<String>, StoreError> {
    let tx = conn.unchecked_transaction().unwrap();
    let moved = move_for_undo(&tx, intent_id).unwrap().unwrap();
    let mut next: i64 = tx
        .query_row("SELECT COUNT(*) FROM intents", [], |row| row.get(0))
        .unwrap();
    let planned = plan_move_undo(&tx, &moved, NOW, &mut || {
        next += 1;
        format!("intent-{next}")
    })?;
    tx.commit().unwrap();
    Ok(planned)
}

#[test]
fn undo_moves_back_to_the_folder_the_move_left() {
    let conn = migrated(MIGRATIONS);
    let seed = proton(&conn);
    file(
        &conn,
        PROTON,
        &[seed.message],
        Filing::Move {
            to_mailbox_id: seed.archive,
        },
    )
    .unwrap();

    assert_eq!(
        undo(&conn, "intent-1").unwrap(),
        vec!["intent-2".to_owned()]
    );

    let (op, args, _, origin) = stored(&conn, "intent-2");
    assert_eq!((op.as_str(), origin.as_str()), ("move", "undo"));
    assert_eq!(
        args,
        serde_json::json!({"v": 1, "to_mailbox_id": seed.inbox, "from_mailbox_id": seed.archive})
    );
    assert_eq!(
        mailboxes_of(&conn, seed.message),
        vec![seed.inbox, seed.work]
    );
}

#[test]
fn undo_refuses_a_message_moved_elsewhere_since() {
    let conn = migrated(MIGRATIONS);
    let seed = proton(&conn);
    file(
        &conn,
        PROTON,
        &[seed.message],
        Filing::Move {
            to_mailbox_id: seed.archive,
        },
    )
    .unwrap();
    file(&conn, PROTON, &[seed.message], Filing::DeleteToTrash).unwrap();

    match undo(&conn, "intent-1") {
        Err(StoreError::Conflict { detail }) => assert_eq!(detail, "Moved elsewhere since"),
        other => panic!("expected Conflict, got {other:?}"),
    }
    assert_eq!(
        mailboxes_of(&conn, seed.message),
        vec![seed.trash, seed.work]
    );
}

#[test]
fn undo_plans_nothing_for_a_cancelled_move_or_one_from_no_folder() {
    let conn = migrated(MIGRATIONS);
    let seed = proton(&conn);
    let loose = message(&conn, PROTON, Some("msg-2"));
    occurrence(&conn, PROTON, seed.work, loose);
    file(
        &conn,
        PROTON,
        &[seed.message],
        Filing::Move {
            to_mailbox_id: seed.archive,
        },
    )
    .unwrap();
    file(
        &conn,
        PROTON,
        &[loose],
        Filing::Move {
            to_mailbox_id: seed.archive,
        },
    )
    .unwrap();
    let tx = conn.unchecked_transaction().unwrap();
    cancel_intents(&tx, &["intent-1".to_owned()], NOW).unwrap();
    tx.commit().unwrap();

    assert_eq!(undo(&conn, "intent-1").unwrap(), Vec::<String>::new());
    assert_eq!(undo(&conn, "intent-2").unwrap(), Vec::<String>::new());
}

#[test]
fn undo_may_move_a_message_back_into_sent() {
    let conn = migrated(MIGRATIONS);
    let seed = proton(&conn);
    let sent = message(&conn, PROTON, Some("msg-2"));
    occurrence(&conn, PROTON, seed.sent, sent);
    file(
        &conn,
        PROTON,
        &[sent],
        Filing::Move {
            to_mailbox_id: seed.archive,
        },
    )
    .unwrap();

    undo(&conn, "intent-1").unwrap();

    assert_eq!(mailboxes_of(&conn, sent), vec![seed.sent]);
}

#[test]
fn a_local_move_undoes_until_the_message_moves_on() {
    let conn = migrated(MIGRATIONS);
    let local = local_archive(&conn);
    let imported = mailbox(&conn, &local, "Imported", "folder", None);
    let kept = mailbox(&conn, &local, "Kept", "folder", None);
    let message = message(&conn, &local, None);
    occurrence(&conn, &local, imported, message);
    let Filed::Local(moves) = file(
        &conn,
        &local,
        &[message],
        Filing::Move {
            to_mailbox_id: kept,
        },
    )
    .unwrap() else {
        panic!("the local archive moves locally");
    };
    let Filed::Local(trashed) = file(&conn, &local, &[message], Filing::DeleteToTrash).unwrap()
    else {
        panic!("the local archive moves locally");
    };

    let tx = conn.unchecked_transaction().unwrap();
    match undo_local_move(&tx, &local, &moves[0], NOW) {
        Err(StoreError::Conflict { detail }) => assert_eq!(detail, "Moved elsewhere since"),
        other => panic!("expected Conflict, got {other:?}"),
    }
    undo_local_move(&tx, &local, &trashed[0], NOW).unwrap();
    undo_local_move(&tx, &local, &moves[0], NOW).unwrap();
    tx.commit().unwrap();

    assert_eq!(mailboxes_of(&conn, message), vec![imported]);
}

#[test]
fn the_target_check_names_the_destination_before_planning() {
    let conn = migrated(MIGRATIONS);
    let seed = proton(&conn);
    let tx = conn.unchecked_transaction().unwrap();

    assert_eq!(
        filing_target(&tx, PROTON, Filing::DeleteToTrash).unwrap(),
        seed.trash
    );
    assert_eq!(
        filing_target(
            &tx,
            PROTON,
            Filing::AddLabel {
                label_mailbox_id: seed.work
            }
        )
        .unwrap(),
        seed.work
    );
    assert!(matches!(
        filing_target(
            &tx,
            PROTON,
            Filing::Move {
                to_mailbox_id: seed.sent
            }
        ),
        Err(StoreError::InvalidInput { .. })
    ));
}

#[test]
fn cancelling_a_filing_intent_reverts_its_local_effect() {
    let conn = migrated(MIGRATIONS);
    let seed = proton(&conn);
    let home = mailbox(&conn, PROTON, "Home", "label", None);
    file(
        &conn,
        PROTON,
        &[seed.message],
        Filing::Move {
            to_mailbox_id: seed.archive,
        },
    )
    .unwrap();
    file(
        &conn,
        PROTON,
        &[seed.message],
        Filing::AddLabel {
            label_mailbox_id: home,
        },
    )
    .unwrap();
    file(
        &conn,
        PROTON,
        &[seed.message],
        Filing::RemoveLabel {
            label_mailbox_id: seed.work,
        },
    )
    .unwrap();
    assert_eq!(mailboxes_of(&conn, seed.message), vec![seed.archive, home]);

    let tx = conn.unchecked_transaction().unwrap();
    let all = ["intent-2", "intent-3", "intent-1"].map(str::to_owned);
    cancel_intents(&tx, &all, NOW).unwrap();
    tx.commit().unwrap();

    assert_eq!(
        mailboxes_of(&conn, seed.message),
        vec![seed.inbox, seed.work]
    );
    let (flagged, inbox_rows): (i64, i64) = conn
        .query_row(
            "SELECT MAX(f_flagged), COUNT(*) FROM occurrences WHERE mailbox_id = ?1 AND state = 'present'",
            [seed.inbox],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((flagged, inbox_rows), (1, 1));
}

#[test]
fn cancelling_a_move_since_overtaken_leaves_the_message() {
    let conn = migrated(MIGRATIONS);
    let seed = proton(&conn);
    file(
        &conn,
        PROTON,
        &[seed.message],
        Filing::Move {
            to_mailbox_id: seed.archive,
        },
    )
    .unwrap();
    file(&conn, PROTON, &[seed.message], Filing::DeleteToTrash).unwrap();

    let tx = conn.unchecked_transaction().unwrap();
    cancel_intents(&tx, &["intent-1".to_owned()], NOW).unwrap();
    tx.commit().unwrap();

    assert_eq!(
        mailboxes_of(&conn, seed.message),
        vec![seed.trash, seed.work]
    );
}
