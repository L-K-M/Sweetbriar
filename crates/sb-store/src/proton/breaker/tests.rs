//! Proton's breaker (plan M2.15c2): retention and small deletes apply,
//! a mass delete or an emptied folder opens an event that holds its
//! messages and pauses the account, Keep and Accept resolve it, and
//! messages that reappear clear it.

use rusqlite::params;
use sb_test_support::{ScratchDir, block_on, real_fs};
use sb_types::time::Timestamp;

use super::*;
use crate::lists::MailboxRole;
use crate::proton::{
    AppliedEvents, EventChunk, ListedMessage, NewProtonAccount, ObservedLabel, ProtonChange,
    ProtonPasswordMode, apply_event_chunk, begin_listing, commit_listed_page, finish_listing,
    insert_proton_account, proton_account, reconcile_proton_labels,
};
use crate::sidebar::MailboxKind;
use crate::sync::{HeaderColumns, SyncedFlags};
use crate::testing::{reading, scratch_paths, test_seams};
use crate::{ListedPage, Store, WritePriority, open};

const ACCOUNT: &str = "00000000-0000-4000-8000-0000000000e2";

fn now() -> Timestamp {
    Timestamp::from_millis(1_735_689_600_000)
}

fn write<T: Send + 'static>(
    store: &Store,
    f: impl FnOnce(&Transaction) -> Result<T, StoreError> + Send + 'static,
) -> Result<T, StoreError> {
    block_on(store.write(WritePriority::Sync, f))
}

fn folder(key: &str, name: &str, role: MailboxRole) -> ObservedLabel {
    ObservedLabel {
        provider_key: key.to_owned(),
        remote_name: name.to_owned(),
        delimiter: None,
        kind: MailboxKind::Folder,
        role: Some(role),
        selectable: true,
    }
}

fn labels() -> Vec<ObservedLabel> {
    let mut all_mail = folder("5", "All Mail", MailboxRole::All);
    all_mail.kind = MailboxKind::Virtual;
    vec![
        folder("0", "Inbox", MailboxRole::Inbox),
        folder("6", "Archive", MailboxRole::Archive),
        folder("3", "Trash", MailboxRole::Trash),
        folder("4", "Spam", MailboxRole::Junk),
        all_mail,
    ]
}

fn message(id: &str, location: &str) -> ListedMessage {
    ListedMessage {
        provider_key: id.to_owned(),
        label_ids: vec![location.to_owned(), "5".to_owned()],
        raw_metadata: format!(r#"{{"v":1,"proton_metadata":{{"ID":"{id}"}}}}"#).into_bytes(),
        columns: HeaderColumns::default(),
        internal_date: Some(Timestamp::from_millis(1_700_000_000_000)),
        size: Some(100),
        flags: SyncedFlags::default(),
    }
}

/// A Proton account whose listing completed with `messages` as (id,
/// location label), at cursor `c0` and revision 4.
fn listed(label_name: &str, messages: Vec<(String, &'static str)>) -> (ScratchDir, Store) {
    let (scratch, paths) = scratch_paths(label_name);
    let store = open(&paths, &test_seams(real_fs())).unwrap();
    write(&store, move |tx| {
        tx.execute(
            "INSERT INTO accounts (account_id, kind, provider, display_name, state, sort_order, created_at)
             VALUES (?1, 'proton', 'proton_api', 'Proton', 'active', 1, 0)",
            params![ACCOUNT],
        )
        .unwrap();
        insert_proton_account(
            tx,
            &NewProtonAccount {
                account_id: ACCOUNT.to_owned(),
                api_base_url: "https://mail.proton.me/api".to_owned(),
                username: "owner".to_owned(),
                proton_user_id: "user-2".to_owned(),
                credential_ref: "proton-e2".to_owned(),
                password_mode: ProtonPasswordMode::Login,
            },
        )?;
        begin_listing(tx, ACCOUNT, "c0", 1)?;
        reconcile_proton_labels(tx, ACCOUNT, &labels())?;
        let page: Vec<ListedMessage> = messages
            .iter()
            .map(|(id, location)| message(id, location))
            .collect();
        commit_listed_page(
            tx,
            &ListedPage {
                account_id: ACCOUNT,
                messages: &page,
                checkpoint: "p1",
                expected_revision: 2,
            },
            now(),
        )?;
        finish_listing(tx, ACCOUNT, &[], 3, now()).map(drop)
    })
    .unwrap();
    (scratch, store)
}

fn ids(prefix: &str, count: usize, location: &'static str) -> Vec<(String, &'static str)> {
    (1..=count)
        .map(|n| (format!("{prefix}{n}"), location))
        .collect()
}

/// Apply one chunk at the account's cursor and revision; `last` moves
/// the cursor and runs the breaker.
fn apply(store: &Store, changes: Vec<ProtonChange>, last: bool) -> AppliedEvents {
    write(store, move |tx| {
        let account = proton_account(tx, ACCOUNT)?.unwrap();
        let from = account.event_cursor.unwrap();
        let next = format!("{from}+");
        apply_event_chunk(
            tx,
            &EventChunk {
                account_id: ACCOUNT,
                from_cursor: &from,
                labels: None,
                changes: &changes,
                next_cursor: last.then_some(next.as_str()),
                expected_revision: account.revision,
            },
            now(),
        )
    })
    .unwrap()
}

fn deletes(ids: &[&str]) -> Vec<ProtonChange> {
    ids.iter()
        .map(|id| ProtonChange::Delete((*id).to_owned()))
        .collect()
}

fn resolve(
    store: &Store,
    event_id: &str,
    choice: BreakerResolution,
) -> Result<Classified, StoreError> {
    let event_id = event_id.to_owned();
    write(store, move |tx| {
        resolve_breaker_event(tx, &event_id, choice, now())
    })
}

/// The states of `id`'s occurrences, sorted.
fn states(store: &Store, id: &str) -> Vec<String> {
    let id = id.to_owned();
    reading(store, move |tx| {
        let mut statement = tx
            .prepare(
                "SELECT o.state FROM occurrences o
                 JOIN logical_messages l ON l.message_id = o.message_id
                 WHERE l.provider_key = ?1 ORDER BY o.state",
            )
            .unwrap();
        let rows = statement.query_map(params![id], |row| row.get(0)).unwrap();
        Ok(rows.map(Result::unwrap).collect())
    })
}

/// (kind, state, details) of every breaker event, by id.
fn events(store: &Store) -> Vec<(String, String, String, String)> {
    reading(store, |tx| {
        let mut statement = tx
            .prepare(
                "SELECT event_id, kind, state, details_json FROM breaker_events ORDER BY event_id",
            )
            .unwrap();
        let rows = statement
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .unwrap();
        Ok(rows.map(Result::unwrap).collect())
    })
}

fn recorded(store: &Store) -> i64 {
    reading(store, |tx| {
        Ok(tx
            .query_row("SELECT COUNT(*) FROM proton_disappearances", [], |row| {
                row.get(0)
            })
            .unwrap())
    })
}

fn vanished() -> Vec<String> {
    vec!["vanished".to_owned(); 2]
}

fn pending() -> Vec<String> {
    vec!["vanish_pending".to_owned(); 2]
}

#[test]
fn retention_and_a_few_deletes_apply_at_once() {
    let mut messages = ids("m", 10, "0");
    messages.push(("t1".to_owned(), "3"));
    messages.push(("s1".to_owned(), "4"));
    let (_scratch, store) = listed("proton-breaker-apply", messages);

    // Trash and Spam empty themselves (retention), even entirely; one
    // of ten Inbox messages is under every threshold.
    let applied = apply(&store, deletes(&["t1", "s1", "m1"]), true);
    assert_eq!(
        (applied.withheld, applied.breaker),
        (
            6,
            Classified {
                applied: 3,
                opened: 0
            }
        )
    );
    for id in ["t1", "s1", "m1"] {
        assert_eq!(states(&store, id), vanished(), "{id}");
    }
    assert_eq!(states(&store, "m2"), vec!["present"; 2]);
    assert!(events(&store).is_empty());
    assert_eq!(recorded(&store), 0);
}

#[test]
fn a_mass_delete_over_two_chunks_opens_an_event_that_pauses_the_account() {
    let (_scratch, store) = listed("proton-breaker-mass", ids("m", 10, "0"));

    // Six of ten, across the two chunks of one response: one pass.
    let first = apply(&store, deletes(&["m1", "m2", "m3"]), false);
    assert_eq!(first.breaker, Classified::default());
    let last = apply(&store, deletes(&["m4", "m5", "m6"]), true);
    assert_eq!(last.breaker.opened, 1);
    let event_id = format!("proton-{ACCOUNT}-1");
    assert_eq!(
        events(&store),
        [(
            event_id.clone(),
            "bulk_disappearance".to_owned(),
            "open".to_owned(),
            r#"{"disappeared":6,"mailbox_messages":10}"#.to_owned()
        )]
    );
    assert_eq!(states(&store, "m1"), pending());

    // While the event is open a single delete waits too.
    let paused = apply(&store, deletes(&["m7"]), true);
    assert_eq!(paused.breaker, Classified::default());
    assert_eq!(states(&store, "m7"), pending());
    assert_eq!(recorded(&store), 7);

    // Keep leaves the six withheld and lifts the pause: m7, one of
    // four, applies.
    let kept = resolve(&store, &event_id, BreakerResolution::Keep).unwrap();
    assert_eq!(
        kept,
        Classified {
            applied: 1,
            opened: 0
        }
    );
    assert_eq!(states(&store, "m7"), vanished());
    assert_eq!(states(&store, "m1"), pending());
    assert_eq!(events(&store)[0].2, "kept");

    // A kept event can still be accepted.
    resolve(&store, &event_id, BreakerResolution::Accept).unwrap();
    for n in 1..=6 {
        assert_eq!(states(&store, &format!("m{n}")), vanished());
    }
    assert_eq!(events(&store)[0].2, "accepted");
    assert_eq!(recorded(&store), 0);
}

#[test]
fn more_than_25_deletes_from_a_large_mailbox_open_an_event() {
    let (_scratch, store) = listed("proton-breaker-count", ids("m", 100, "0"));
    let gone: Vec<String> = (1..=26).map(|n| format!("m{n}")).collect();
    let gone: Vec<&str> = gone.iter().map(String::as_str).collect();
    let applied = apply(&store, deletes(&gone), true);
    assert_eq!(applied.breaker.opened, 1);
    assert_eq!(
        events(&store)[0].3,
        r#"{"disappeared":26,"mailbox_messages":100}"#
    );
}

#[test]
fn an_emptied_folder_opens_an_event_that_messages_reappearing_clear() {
    let mut messages = ids("a", 2, "6");
    messages.extend(ids("m", 3, "0"));
    let (_scratch, store) = listed("proton-breaker-emptied", messages);

    let applied = apply(&store, deletes(&["a1", "a2"]), true);
    assert_eq!(applied.breaker.opened, 1);
    assert_eq!(events(&store)[0].1, "folder_emptied");

    // A replayed delete changes nothing; one message back is not all.
    apply(&store, deletes(&["a1"]), true);
    assert_eq!(recorded(&store), 2);
    apply(
        &store,
        vec![ProtonChange::Upsert(Box::new(message("a1", "6")))],
        true,
    );
    assert_eq!(events(&store)[0].2, "open");
    apply(
        &store,
        vec![ProtonChange::Upsert(Box::new(message("a2", "6")))],
        true,
    );
    assert_eq!(events(&store)[0].2, "cleared");
    assert_eq!(states(&store, "a2"), vec!["present"; 2]);
    assert_eq!(recorded(&store), 0);

    // The pause is lifted: a delete applies again.
    let after = apply(&store, deletes(&["m1"]), true);
    assert_eq!(after.breaker.applied, 1);
}

#[test]
fn only_an_open_event_is_kept_and_an_open_or_kept_one_accepted() {
    let (_scratch, store) = listed("proton-breaker-refused", ids("a", 1, "6"));
    apply(&store, deletes(&["a1"]), true);
    let event_id = format!("proton-{ACCOUNT}-1");

    assert!(matches!(
        resolve(&store, "proton-unknown", BreakerResolution::Accept),
        Err(StoreError::NotFound { .. })
    ));
    resolve(&store, &event_id, BreakerResolution::Keep).unwrap();
    assert!(matches!(
        resolve(&store, &event_id, BreakerResolution::Keep),
        Err(StoreError::Conflict { .. })
    ));
    resolve(&store, &event_id, BreakerResolution::Accept).unwrap();
    for choice in [BreakerResolution::Keep, BreakerResolution::Accept] {
        assert!(matches!(
            resolve(&store, &event_id, choice),
            Err(StoreError::Conflict { .. })
        ));
    }
    assert_eq!(states(&store, "a1"), vanished());
}

#[test]
fn resolving_between_a_responses_chunks_leaves_its_pass_whole() {
    let mut messages = ids("a", 1, "6");
    messages.extend(ids("m", 100, "0"));
    let (_scratch, store) = listed("proton-breaker-mid-response", messages);
    apply(&store, deletes(&["a1"]), true);
    let emptied = format!("proton-{ACCOUNT}-1");

    // The user keeps the first event between the two chunks of a
    // response deleting 26 Inbox messages: the 20 already recorded are
    // not a pass of their own.
    let first: Vec<String> = (1..=20).map(|n| format!("m{n}")).collect();
    let first: Vec<&str> = first.iter().map(String::as_str).collect();
    apply(&store, deletes(&first), false);
    let kept = resolve(&store, &emptied, BreakerResolution::Keep).unwrap();
    assert_eq!(kept, Classified::default());
    assert_eq!(states(&store, "m1"), pending());

    let last = apply(
        &store,
        deletes(&["m21", "m22", "m23", "m24", "m25", "m26"]),
        true,
    );
    assert_eq!(last.breaker.opened, 1);
    assert_eq!(
        events(&store)[1].3,
        r#"{"disappeared":26,"mailbox_messages":100}"#
    );
}

mod refresh;
