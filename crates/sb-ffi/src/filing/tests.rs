//! Filing across the FFI boundary: refusals keep their argument
//! names, a move answers an opaque token whose undo crosses back,
//! labels answer none, and the local archive acts with no intent.

use std::path::Path;

use rusqlite::{Connection, OpenFlags};
use sb_test_support::{ScratchDir, block_on};

use super::*;
use crate::{AccountKind, CoreConfig, open_core};

const PROTON: &str = "acct-ffi";
const INBOX: i64 = 1;
const ARCHIVE: i64 = 2;
const SENT: i64 = 3;
const WORK: i64 = 4;
const IMPORTED: i64 = 5;
const KEPT: i64 = 6;
const TRASH: i64 = 7;
const PROTON_MESSAGE: i64 = 1;
const LOCAL_MESSAGE: i64 = 2;
const PROTON_OCCURRENCE: i64 = 11;
const LOCAL_OCCURRENCE: i64 = 21;

fn config(scratch: &ScratchDir) -> CoreConfig {
    CoreConfig {
        data_root: scratch.path().join("store").to_string_lossy().into_owned(),
        cache_root: scratch.path().join("cache").to_string_lossy().into_owned(),
    }
}

fn open(scratch: &ScratchDir) -> Arc<Core> {
    block_on(open_core(CancelToken::new(), config(scratch))).expect("a fresh store opens")
}

fn state_db(scratch: &ScratchDir) -> std::path::PathBuf {
    scratch.path().join("store/state.sqlite")
}

fn one_i64(state_db: &Path, sql: &str) -> i64 {
    let connection = Connection::open_with_flags(state_db, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .unwrap_or_else(|error| panic!("cannot open {} read-only: {error}", state_db.display()));
    connection
        .query_row(sql, [], |row| row.get(0))
        .unwrap_or_else(|error| panic!("cannot read `{sql}`: {error}"))
}

fn one_text(state_db: &Path, sql: &str) -> String {
    let connection = Connection::open_with_flags(state_db, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .unwrap_or_else(|error| panic!("cannot open {} read-only: {error}", state_db.display()));
    connection
        .query_row(sql, [], |row| row.get(0))
        .unwrap_or_else(|error| panic!("cannot read `{sql}`: {error}"))
}

fn count(state_db: &Path, what: &str) -> i64 {
    one_i64(state_db, &format!("SELECT COUNT(*) FROM {what}"))
}

fn field(error: CoreError) -> String {
    match error {
        CoreError::InvalidInput { field, .. } => field,
        other => panic!("expected InvalidInput, got {other:?}"),
    }
}

fn key(n: u8) -> String {
    format!("60000000-0000-4000-8000-0000000000{n:02x}")
}

/// A Proton account with one message in Inbox (plus Archive, Sent, a
/// label and Trash), and the local archive with one imported message.
fn seed(store: &Path, local_archive: &str) {
    sb_test_support::execute_sql(
        &store.join("state.sqlite"),
        &format!(
            r#"INSERT INTO accounts (account_id, kind, provider, display_name, state,
               config_generation, sort_order, created_at)
             VALUES ('{PROTON}', 'proton', 'proton_api', 'Proton', 'paused', 1, 1, 0);
             INSERT INTO proton_accounts (account_id, api_base_url, username, proton_user_id,
               credential_ref, password_mode, listing_state)
             VALUES ('{PROTON}', 'https://127.0.0.1:1', 'synthetic', 'user-1',
               'cred-1', 1, 'none');
             INSERT INTO mailboxes (mailbox_id, account_id, remote_name, role, kind,
               selectable, sync_state) VALUES
               ({INBOX}, '{PROTON}', 'Inbox', 'inbox', 'folder', 1, 'synced'),
               ({ARCHIVE}, '{PROTON}', 'Archive', 'archive', 'folder', 1, 'synced'),
               ({SENT}, '{PROTON}', 'Sent', 'sent', 'folder', 1, 'synced'),
               ({WORK}, '{PROTON}', 'Work', NULL, 'label', 1, 'synced'),
               ({TRASH}, '{PROTON}', 'Trash', 'trash', 'folder', 1, 'synced'),
               ({IMPORTED}, '{local_archive}', 'Imported', NULL, 'folder', 1, 'synced'),
               ({KEPT}, '{local_archive}', 'Kept', NULL, 'folder', 1, 'synced');
             INSERT INTO logical_messages (message_id, account_id, provider_key, created_at)
             VALUES ({PROTON_MESSAGE}, '{PROTON}', 'msg-1', 0),
                    ({LOCAL_MESSAGE}, '{local_archive}', NULL, 0);
             INSERT INTO occurrences (occurrence_id, account_id, mailbox_id, message_id,
               state, first_seen_at, last_seen_at) VALUES
               ({PROTON_OCCURRENCE}, '{PROTON}', {INBOX}, {PROTON_MESSAGE}, 'present', 0, 0),
               ({LOCAL_OCCURRENCE}, '{local_archive}', {IMPORTED}, {LOCAL_MESSAGE},
                'present', 0, 0);"#
        ),
    );
}

/// Open, capture the local archive's id, close, seed, reopen: the way a
/// fixture store is prepared without a running core.
fn seeded(label: &str) -> (ScratchDir, Arc<Core>) {
    let scratch = ScratchDir::new(label);
    let core = open(&scratch);
    let local_archive = block_on(core.accounts(CancelToken::new()))
        .expect("accounts")
        .into_iter()
        .find(|account| account.kind == AccountKind::Local)
        .expect("the local archive always exists")
        .account_id;
    block_on(core.shutdown());
    seed(&scratch.path().join("store"), &local_archive);
    let core = open(&scratch);
    (scratch, core)
}

/// The refusal fields cross unchanged: the destination for a move to
/// Sent, the label for adding a folder as a label, and the selection
/// when it is empty. Nothing was planned.
#[test]
fn filing_refusals_keep_their_field_names() {
    let (scratch, core) = seeded("ffi-filing-refusals");
    let state = state_db(&scratch);
    let selection = MessageSelection::OccurrenceIds(vec![PROTON_OCCURRENCE]);

    let sent = block_on(core.move_messages(CancelToken::new(), key(1), selection.clone(), SENT));
    assert_eq!(field(sent.unwrap_err()), "to_mailbox");
    let folder_as_label =
        block_on(core.add_label(CancelToken::new(), key(2), selection.clone(), INBOX));
    assert_eq!(field(folder_as_label.unwrap_err()), "label_mailbox");
    let empty = block_on(core.delete_to_trash(
        CancelToken::new(),
        key(3),
        MessageSelection::OccurrenceIds(vec![]),
    ));
    assert_eq!(field(empty.unwrap_err()), "selection");

    assert_eq!(count(&state, "intents"), 0, "a refusal planned nothing");
    assert_eq!(
        one_i64(
            &state,
            &format!(
                "SELECT COUNT(*) FROM occurrences WHERE message_id = {PROTON_MESSAGE}
                 AND state = 'present'"
            )
        ),
        1,
        "the message stayed put"
    );
    block_on(core.shutdown());
}

/// A Proton move plans one intent, answers a token a replay repeats,
/// and its undo over the same boundary moves the message back.
#[test]
fn a_proton_move_answers_a_token_and_its_undo_crosses_back() {
    let (scratch, core) = seeded("ffi-filing-move");
    let state = state_db(&scratch);
    let selection = MessageSelection::OccurrenceIds(vec![PROTON_OCCURRENCE]);

    let token =
        block_on(core.move_messages(CancelToken::new(), key(1), selection.clone(), ARCHIVE))
            .expect("a move commits")
            .expect("a Proton move answers a token");
    assert_eq!(
        one_i64(
            &state,
            &format!(
                "SELECT COUNT(*) FROM occurrences WHERE message_id = {PROTON_MESSAGE}
                 AND mailbox_id = {ARCHIVE} AND state = 'present'"
            )
        ),
        1
    );
    assert_eq!(
        one_i64(
            &state,
            &format!(
                "SELECT COUNT(*) FROM occurrences WHERE message_id = {PROTON_MESSAGE}
                 AND mailbox_id = {INBOX} AND state = 'present'"
            )
        ),
        0
    );
    assert_eq!(count(&state, "intents"), 1);
    assert_eq!(one_text(&state, "SELECT op FROM intents"), "move");

    let replay = block_on(core.move_messages(CancelToken::new(), key(1), selection, ARCHIVE))
        .expect("a replay answers");
    assert_eq!(replay, Some(token.clone()), "and repeats the first token");
    assert_eq!(count(&state, "intents"), 1, "planning nothing again");

    block_on(core.undo(CancelToken::new(), key(2), token)).expect("the undo commits");
    assert_eq!(
        one_i64(
            &state,
            &format!(
                "SELECT COUNT(*) FROM occurrences WHERE message_id = {PROTON_MESSAGE}
                 AND mailbox_id = {INBOX} AND state = 'present'"
            )
        ),
        1
    );
    assert_eq!(count(&state, "intents"), 2);
    assert_eq!(
        one_text(
            &state,
            "SELECT origin FROM intents ORDER BY rowid DESC LIMIT 1"
        ),
        "undo"
    );
    block_on(core.shutdown());
}

/// Trash moves to the Trash folder, labels return no token while their
/// intents plan, and each keeps the message readable where it landed.
#[test]
fn trash_and_labels_answer_as_specified() {
    let (scratch, core) = seeded("ffi-filing-trash-labels");
    let state = state_db(&scratch);

    let token = block_on(core.delete_to_trash(
        CancelToken::new(),
        key(1),
        MessageSelection::OccurrenceIds(vec![PROTON_OCCURRENCE]),
    ))
    .expect("a trash move commits")
    .expect("delete_to_trash answers a token");
    assert_eq!(
        one_i64(
            &state,
            &format!(
                "SELECT COUNT(*) FROM occurrences WHERE message_id = {PROTON_MESSAGE}
                 AND mailbox_id = {TRASH} AND state = 'present'"
            )
        ),
        1
    );
    assert_eq!(
        one_text(&state, "SELECT op FROM intents"),
        "delete_to_trash"
    );
    let trashed = one_i64(
        &state,
        &format!(
            "SELECT occurrence_id FROM occurrences WHERE message_id = {PROTON_MESSAGE}
             AND mailbox_id = {TRASH} AND state = 'present' LIMIT 1"
        ),
    );

    let added = block_on(core.add_label(
        CancelToken::new(),
        key(2),
        MessageSelection::OccurrenceIds(vec![trashed]),
        WORK,
    ));
    assert!(added.is_ok(), "add_label answers no token");
    assert_eq!(
        one_i64(
            &state,
            &format!(
                "SELECT COUNT(*) FROM occurrences WHERE message_id = {PROTON_MESSAGE}
                 AND mailbox_id = {WORK} AND state = 'present'"
            )
        ),
        1
    );
    assert_eq!(
        one_text(&state, "SELECT op FROM intents WHERE op = 'copy' LIMIT 1"),
        "copy"
    );

    let removed = block_on(core.remove_label(
        CancelToken::new(),
        key(3),
        MessageSelection::OccurrenceIds(vec![trashed]),
        WORK,
    ));
    assert!(removed.is_ok(), "remove_label answers no token");
    assert_eq!(
        one_i64(
            &state,
            &format!(
                "SELECT COUNT(*) FROM occurrences WHERE message_id = {PROTON_MESSAGE}
                 AND mailbox_id = {WORK} AND state = 'present'"
            )
        ),
        0,
        "the label came off"
    );
    assert_eq!(
        one_text(
            &state,
            "SELECT op FROM intents WHERE op = 'remove_label' LIMIT 1"
        ),
        "remove_label"
    );

    block_on(core.undo(CancelToken::new(), key(4), token)).expect("the trash undo commits");
    assert_eq!(
        one_i64(
            &state,
            &format!(
                "SELECT COUNT(*) FROM occurrences WHERE message_id = {PROTON_MESSAGE}
                 AND mailbox_id = {INBOX} AND state = 'present'"
            )
        ),
        1
    );
    block_on(core.shutdown());
}

/// The local archive moves at once with no intent and undoes locally.
#[test]
fn the_local_archive_moves_without_an_intent() {
    let (scratch, core) = seeded("ffi-filing-local");
    let state = state_db(&scratch);

    let token = block_on(core.move_messages(
        CancelToken::new(),
        key(1),
        MessageSelection::OccurrenceIds(vec![LOCAL_OCCURRENCE]),
        KEPT,
    ))
    .expect("a local move commits")
    .expect("a local move answers a token");
    assert_eq!(
        one_i64(
            &state,
            &format!(
                "SELECT COUNT(*) FROM occurrences WHERE message_id = {LOCAL_MESSAGE}
                 AND mailbox_id = {KEPT} AND state = 'present'"
            )
        ),
        1
    );
    assert_eq!(
        count(&state, "intents"),
        0,
        "no intent for the local archive"
    );

    block_on(core.undo(CancelToken::new(), key(2), token)).expect("the local undo commits");
    assert_eq!(
        one_i64(
            &state,
            &format!(
                "SELECT COUNT(*) FROM occurrences WHERE message_id = {LOCAL_MESSAGE}
                 AND mailbox_id = {IMPORTED} AND state = 'present'"
            )
        ),
        1
    );
    block_on(core.shutdown());
}

/// A command cancelled before it begins answers `Cancelled` and writes
/// nothing, like every command at this boundary (spec/07).
#[test]
fn a_cancelled_filing_call_writes_nothing() {
    let (scratch, core) = seeded("ffi-filing-cancelled");
    let state = state_db(&scratch);

    let token = CancelToken::new();
    token.cancel();
    let error = block_on(core.move_messages(
        token,
        key(1),
        MessageSelection::OccurrenceIds(vec![PROTON_OCCURRENCE]),
        ARCHIVE,
    ))
    .expect_err("the command was cancelled before it began");
    assert!(matches!(error, CoreError::Cancelled));

    assert_eq!(count(&state, "intents"), 0);
    assert_eq!(
        one_i64(
            &state,
            &format!(
                "SELECT COUNT(*) FROM occurrences WHERE message_id = {PROTON_MESSAGE}
                 AND mailbox_id = {INBOX} AND state = 'present'"
            )
        ),
        1,
        "the cancelled command moved nothing"
    );
    block_on(core.shutdown());
}
