//! The proptest model of Proton sync (plan M2.15c4b; spec/04-sync-proton
//! "Sync loop", "Refresh" and "The breaker"). A server in memory adds,
//! moves and deletes messages, renames a label, keeps an event log and
//! asks for refreshes. A driver calls the store as the engine's worker does
//! (cursor first, labels, pages with checkpoints, withholding what a
//! listing missed, then events in chunks), and is cut after any write,
//! as a crash or a dropped connection would cut the worker; the next
//! sync resumes from what the store holds. Server changes land between
//! syncs, so also in the middle of a listing.
//!
//! Whatever the history, the store converges to the server once the
//! user accepts the breaker's events, and every message has at most one
//! current occurrence per mailbox.

use std::collections::{BTreeMap, BTreeSet};

use proptest::prelude::*;
use proptest::sample::Index;
use rusqlite::params;
use sb_test_support::{ScratchDir, block_on, real_fs};
use sb_types::time::Timestamp;

use super::breaker::{MAX_DISAPPEARANCES_PER_MAILBOX, MIN_MAILBOX_FOR_SHARE};
use super::*;
use crate::intents::{
    FlagChange, IntentFlag, IntentOrigin, QuarantineReason, cancel_intents, claim_next_intent,
    finish_intent_done, plan_flag_intents, quarantine_intent, record_intent_failure,
    recover_in_flight_intents, release_intents,
};
use crate::lists::MailboxRole;
use crate::sidebar::MailboxKind;
use crate::sync::{HeaderColumns, SyncedFlags};
use crate::testing::{reading, scratch_paths, test_seams};
use crate::{Store, StorePaths, WritePriority, open};

const ACCOUNT: &str = "00000000-0000-4000-8000-0000000000e4";
const INBOX: &str = "0";
const ARCHIVE: &str = "6";
const TRASH: &str = "3";
const SPAM: &str = "4";
const ALL_MAIL: &str = "5";
const TRAVEL: &str = "l1";
const LOCATIONS: [&str; 4] = [INBOX, ARCHIVE, TRASH, SPAM];
/// Messages per listed page: small, so listings take several writes.
const PAGE: usize = 2;
/// Changes per write, as the worker's `CHANGES_PER_WRITE` but small.
const CHUNK: usize = 2;

fn now() -> Timestamp {
    Timestamp::from_millis(1_735_689_600_000)
}

fn label(key: &str, name: &str, kind: MailboxKind, role: Option<MailboxRole>) -> ObservedLabel {
    ObservedLabel {
        provider_key: key.to_owned(),
        remote_name: name.to_owned(),
        delimiter: None,
        kind,
        role,
        selectable: true,
    }
}

/// The account's labels, with the Travel label's current name.
fn labels(travel: &str) -> Vec<ObservedLabel> {
    use MailboxKind::{Folder, Label, Virtual};
    vec![
        label(INBOX, "Inbox", Folder, Some(MailboxRole::Inbox)),
        label(ARCHIVE, "Archive", Folder, Some(MailboxRole::Archive)),
        label(TRASH, "Trash", Folder, Some(MailboxRole::Trash)),
        label(SPAM, "Spam", Folder, Some(MailboxRole::Junk)),
        label(ALL_MAIL, "All Mail", Virtual, Some(MailboxRole::All)),
        label(TRAVEL, travel, Label, None),
    ]
}

/// One message as the server holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Held {
    time: i64,
    location: &'static str,
    travel: bool,
    seen: bool,
    starred: bool,
}

impl Held {
    fn label_ids(&self) -> Vec<String> {
        let mut ids = vec![self.location.to_owned(), ALL_MAIL.to_owned()];
        if self.travel {
            ids.push(TRAVEL.to_owned());
        }
        ids
    }

    fn listed(&self, id: &str) -> ListedMessage {
        ListedMessage {
            provider_key: id.to_owned(),
            label_ids: self.label_ids(),
            raw_metadata: format!(
                r#"{{"v":1,"proton_metadata":{{"ID":"{id}","Time":{}}}}}"#,
                self.time
            )
            .into_bytes(),
            columns: HeaderColumns::default(),
            internal_date: Some(Timestamp::from_millis(self.time * 1000)),
            size: Some(100),
            flags: SyncedFlags {
                seen: self.seen,
                flagged: self.starred,
                ..SyncedFlags::default()
            },
        }
    }
}

/// One entry of the server's event log. An upsert carries the message
/// as it was then, as Proton's events do.
#[derive(Debug, Clone)]
enum Logged {
    Upsert(String, Held),
    Delete(String),
    /// A label changed: the worker lists the labels again.
    Labels,
    /// Proton's `Refresh`: the response asks for a listing again.
    Refresh,
}

/// The server: its messages, newest first by (time, id), and its log,
/// whose cursor is a position in it.
struct Server {
    messages: BTreeMap<String, Held>,
    /// The Travel label's name.
    travel: String,
    log: Vec<Logged>,
    next_id: usize,
    responses: Responses,
    /// The intents the model's executor ran, in order. A claim runs
    /// once, so no id repeats.
    applied_intents: Vec<String>,
}

/// How many events one response carries.
#[derive(Clone, Copy)]
enum Responses {
    /// One to four, by where the response starts, so response and
    /// chunk boundaries fall on refreshes and deletes in different
    /// arrangements.
    Varying,
    /// Everything pending, so one pass sees every delete at once.
    All,
}

/// One events response.
struct Response {
    changes: Vec<ProtonChange>,
    labels: bool,
    refresh: bool,
    next: usize,
    more: bool,
}

fn cursor(position: usize) -> String {
    format!("e{position}")
}

fn position(cursor: &str) -> usize {
    cursor[1..].parse().expect("the driver's cursors")
}

impl Server {
    fn new(responses: Responses) -> Self {
        Self {
            messages: BTreeMap::new(),
            travel: "Travel".to_owned(),
            log: Vec::new(),
            next_id: 0,
            responses,
            applied_intents: Vec::new(),
        }
    }

    fn put(&mut self, id: String, held: Held) {
        self.log.push(Logged::Upsert(id.clone(), held.clone()));
        self.messages.insert(id, held);
    }

    fn add(&mut self, held: Held) -> String {
        self.next_id += 1;
        let id = format!("m{:03}", self.next_id);
        self.put(id.clone(), held);
        id
    }

    fn rename_travel(&mut self, name: String) {
        self.travel = name;
        self.log.push(Logged::Labels);
    }

    fn delete(&mut self, id: &str) {
        if self.messages.remove(id).is_some() {
            self.log.push(Logged::Delete(id.to_owned()));
        }
    }

    fn pick(&self, index: &Index) -> Option<String> {
        if self.messages.is_empty() {
            return None;
        }
        self.messages
            .keys()
            .nth(index.index(self.messages.len()))
            .cloned()
    }

    /// A page of up to `PAGE` messages older than `after`, newest
    /// first, as `/mail/v4/messages` with `End` and `EndID`.
    fn page(&self, after: Option<&(i64, String)>) -> Vec<(String, Held)> {
        let mut older: Vec<_> = self
            .messages
            .iter()
            .filter(|(id, held)| {
                after.is_none_or(|(time, last)| (held.time, id.as_str()) < (*time, last.as_str()))
            })
            .collect();
        older.sort_by(|(a, x), (b, y)| (y.time, b.as_str()).cmp(&(x.time, a.as_str())));
        older
            .into_iter()
            .take(PAGE)
            .map(|(id, held)| (id.clone(), held.clone()))
            .collect()
    }

    fn events_since(&self, from: usize) -> Response {
        let next = match self.responses {
            Responses::Varying => (from + 1 + from % 4).min(self.log.len()),
            Responses::All => self.log.len(),
        };
        let mut response = Response {
            changes: Vec::new(),
            labels: false,
            refresh: false,
            next,
            more: next < self.log.len(),
        };
        for logged in &self.log[from..next] {
            match logged {
                Logged::Upsert(id, held) => response
                    .changes
                    .push(ProtonChange::Upsert(Box::new(held.listed(id)))),
                Logged::Delete(id) => response.changes.push(ProtonChange::Delete(id.clone())),
                Logged::Labels => response.labels = true,
                Logged::Refresh => response.refresh = true,
            }
        }
        response
    }
}

/// The driver ran out of writes: the worker stopped there.
struct Cut;

/// The engine's worker (sb-engine `proton_sync`), over the store and
/// the model server, with a budget of writes.
struct Driver<'a> {
    store: &'a Store,
    server: &'a Server,
    writes_left: Option<usize>,
}

impl Driver<'_> {
    fn write<T: Send + 'static>(
        &mut self,
        f: impl FnOnce(&Transaction) -> Result<T, StoreError> + Send + 'static,
    ) -> Result<T, Cut> {
        if let Some(left) = self.writes_left.as_mut() {
            let Some(rest) = left.checked_sub(1) else {
                return Err(Cut);
            };
            *left = rest;
        }
        // A refused write is a failure of the model, never a cut.
        Ok(block_on(self.store.write(WritePriority::Sync, f)).expect("the worker's write"))
    }

    fn account(&self) -> ProtonAccount {
        reading(self.store, |tx| proton_account(tx, ACCOUNT)).expect("the account")
    }

    /// List when the store says a listing is due, then follow the
    /// events until nothing more is pending.
    fn sync(&mut self) -> Result<(), Cut> {
        // Each pass but the last starts over for a refresh in the log,
        // so the log's length bounds a sync that works; a broken one
        // fails here instead of hanging.
        for _ in 0..=self.server.log.len() + 1 {
            self.bootstrap()?;
            if self.follow()? {
                return Ok(());
            }
        }
        panic!("the sync never settled");
    }

    /// "Sync loop" step 1, resumed from where the store says the
    /// listing stands.
    fn bootstrap(&mut self) -> Result<(), Cut> {
        let account = self.account();
        if account.listing_state == ProtonListingState::Complete {
            return Ok(());
        }
        let mut revision = account.revision;
        let mut after = None;
        if let (ProtonListingState::Listing, Some(_)) =
            (account.listing_state, &account.pending_cursor)
        {
            after = account.listing_checkpoint.as_deref().map(|checkpoint| {
                let (time, id) = checkpoint
                    .split_once(':')
                    .expect("the driver's checkpoints");
                (time.parse().expect("a time"), id.to_owned())
            });
        } else {
            // 1.1: the cursor before the first page.
            let cursor = cursor(self.server.log.len());
            revision = self.write(move |tx| {
                begin_listing(tx, ACCOUNT, &cursor, revision).map(|account| account.revision)
            })?;
        }
        let observed = labels(&self.server.travel);
        self.write(move |tx| reconcile_proton_labels(tx, ACCOUNT, &observed).map(drop))?;

        loop {
            let page = self.server.page(after.as_ref());
            let Some((last_id, last)) = page.last() else {
                break;
            };
            let next = (last.time, last_id.clone());
            let checkpoint = format!("{}:{}", next.0, next.1);
            let messages: Vec<_> = page.iter().map(|(id, held)| held.listed(id)).collect();
            revision = self.write(move |tx| {
                commit_listed_page(
                    tx,
                    &ListedPage {
                        account_id: ACCOUNT,
                        messages: &messages,
                        checkpoint: &checkpoint,
                        expected_revision: revision,
                    },
                    now(),
                )?;
                Ok(proton_account(tx, ACCOUNT)?.expect("the account").revision)
            })?;
            after = Some(next);
            if page.len() < PAGE {
                break;
            }
        }

        while self.write(move |tx| withhold_unlisted(tx, ACCOUNT, revision))? > 0 {}
        self.write(move |tx| finish_listing(tx, ACCOUNT, &[], revision, now()).map(drop))
    }

    /// "Sync loop" step 2 until a response leaves nothing more: true,
    /// or false when a response asks for a listing again.
    fn follow(&mut self) -> Result<bool, Cut> {
        loop {
            let account = self.account();
            let (ProtonListingState::Complete, Some(from)) =
                (account.listing_state, account.event_cursor)
            else {
                return Ok(false);
            };
            let response = self.server.events_since(position(&from));
            let next = cursor(response.next);
            // Only a response's last chunk moves the cursor and the
            // revision (`apply_event_chunk`), so every chunk is written
            // under the revision read here, as the worker does.
            let revision = account.revision;
            if response.refresh {
                self.write(move |tx| begin_listing(tx, ACCOUNT, &next, revision).map(drop))?;
                return Ok(false);
            }
            if response.changes.is_empty() && next == from {
                return Ok(true);
            }
            let mut chunks: Vec<Vec<ProtonChange>> =
                response.changes.chunks(CHUNK).map(<[_]>::to_vec).collect();
            // A response that only renamed a label still moves the
            // cursor, in one empty chunk.
            if chunks.is_empty() {
                chunks.push(Vec::new());
            }
            // The labels, listed again, go with the first chunk.
            let mut observed = response.labels.then(|| labels(&self.server.travel));
            let last = chunks.len() - 1;
            for (index, changes) in chunks.into_iter().enumerate() {
                let from = from.clone();
                let next = (index == last).then(|| next.clone());
                let observed = observed.take();
                self.write(move |tx| {
                    apply_event_chunk(
                        tx,
                        &EventChunk {
                            account_id: ACCOUNT,
                            from_cursor: &from,
                            labels: observed.as_deref(),
                            changes: &changes,
                            next_cursor: next.as_deref(),
                            expected_revision: revision,
                        },
                        now(),
                    )
                    .map(drop)
                })?;
            }
            if !response.more {
                return Ok(true);
            }
        }
    }
}

fn open_store(paths: &StorePaths) -> Store {
    open(paths, &test_seams(real_fs())).expect("the store opens")
}

/// A store with a Proton account that hasn't listed yet.
fn signed_in(label: &str) -> (ScratchDir, StorePaths, Store) {
    let (scratch, paths) = scratch_paths(label);
    let store = open_store(&paths);
    block_on(store.write(WritePriority::User, |tx| {
        tx.execute(
            "INSERT INTO accounts (account_id, kind, provider, display_name, state, sort_order, created_at)
             VALUES (?1, 'proton', 'proton_api', 'Proton', 'active', 1, 0)",
            params![ACCOUNT],
        )
        .map_err(|e| crate::schema::classify(e, "insert the test account"))?;
        insert_proton_account(
            tx,
            &NewProtonAccount {
                account_id: ACCOUNT.to_owned(),
                api_base_url: "https://mail.proton.me/api".to_owned(),
                username: "owner".to_owned(),
                proton_user_id: "user-4".to_owned(),
                credential_ref: "proton-e4".to_owned(),
                password_mode: ProtonPasswordMode::Login,
            },
        )
        .map(drop)
    }))
    .expect("the account");
    (scratch, paths, store)
}

/// Sync without a cut.
fn sync(store: &Store, server: &Server) {
    let mut driver = Driver {
        store,
        server,
        writes_left: None,
    };
    assert!(driver.sync().is_ok(), "an uncut sync ends");
}

/// The user accepts every open or kept breaker event, until none is
/// left: classifying what a pause held back can open another. Each
/// round applies at least one recorded delete, so the deletes recorded
/// now bound the rounds; a broken store fails here instead of hanging.
fn accept_all(store: &Store) {
    let recorded = count(store, "SELECT COUNT(*) FROM proton_disappearances");
    for _ in 0..=recorded {
        if resolve(store, BreakerResolution::Accept) == 0 {
            return;
        }
    }
    panic!("accepting the breaker's events never settled");
}

/// Resolve the events `choice` applies to now (Accept: open and kept;
/// Keep: open); how many.
fn resolve(store: &Store, choice: BreakerResolution) -> usize {
    let states = match choice {
        BreakerResolution::Accept => "'open', 'kept'",
        BreakerResolution::Keep => "'open'",
    };
    let sql =
        format!("SELECT event_id FROM breaker_events WHERE state IN ({states}) ORDER BY event_id");
    let events: Vec<String> = reading(store, move |tx| {
        let read = |e| crate::schema::classify(e, "read breaker events");
        let mut statement = tx.prepare(&sql).map_err(read)?;
        let rows = statement.query_map([], |row| row.get(0)).map_err(read)?;
        rows.collect::<Result<_, _>>().map_err(read)
    });
    let resolved = events.len();
    for event_id in events {
        block_on(store.write(WritePriority::User, move |tx| {
            resolve_breaker_event(tx, &event_id, choice, now()).map(drop)
        }))
        .expect("the user's choice");
    }
    resolved
}

fn count(store: &Store, sql: &str) -> i64 {
    let sql = sql.to_owned();
    reading(store, move |tx| {
        tx.query_row(&sql, [], |row| row.get(0))
            .map_err(|e| crate::schema::classify(e, "count"))
    })
}

/// What holds after every step: no message twice, at most one current
/// occurrence per message and mailbox, and no breaker event over
/// retention (Trash and Spam).
fn assert_invariants(store: &Store) {
    assert_eq!(
        count(
            store,
            "SELECT COUNT(*) - COUNT(DISTINCT provider_key) FROM logical_messages"
        ),
        0,
        "a Proton message stored twice"
    );
    assert_eq!(
        count(
            store,
            "SELECT COUNT(*) FROM (SELECT 1 FROM occurrences
             WHERE state IN ('present', 'vanish_pending')
             GROUP BY message_id, mailbox_id HAVING COUNT(*) > 1)"
        ),
        0,
        "two current occurrences of a message in one mailbox"
    );
    assert_eq!(
        count(
            store,
            "SELECT COUNT(*) FROM breaker_events b JOIN mailboxes m USING (mailbox_id)
             WHERE m.role IN ('trash', 'junk')"
        ),
        0,
        "a retention delete opened the breaker"
    );
}

/// Each message's current occurrences, as (label, state, seen,
/// starred).
fn stored(store: &Store) -> BTreeMap<String, BTreeSet<(String, String, bool, bool)>> {
    let rows: Vec<(String, String, String, bool, bool)> = reading(store, |tx| {
        let read = |e| crate::schema::classify(e, "read the stored occurrences");
        let mut statement = tx
            .prepare(
                "SELECT l.provider_key, m.provider_key, o.state, o.f_seen, o.f_flagged
                 FROM occurrences o
                 JOIN logical_messages l ON l.message_id = o.message_id
                 JOIN mailboxes m ON m.mailbox_id = o.mailbox_id
                 WHERE o.state IN ('present', 'vanish_pending')",
            )
            .map_err(read)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })
            .map_err(read)?;
        rows.collect::<Result<_, _>>().map_err(read)
    });
    let mut stored: BTreeMap<_, BTreeSet<_>> = BTreeMap::new();
    for (message, label, state, seen, starred) in rows {
        stored
            .entry(message)
            .or_default()
            .insert((label, state, seen, starred));
    }
    stored
}

/// The server's messages as the store holds them once converged.
fn expected(server: &Server) -> BTreeMap<String, BTreeSet<(String, String, bool, bool)>> {
    server
        .messages
        .iter()
        .map(|(id, held)| {
            let occurrences = held
                .label_ids()
                .into_iter()
                .map(|label| (label, "present".to_owned(), held.seen, held.starred))
                .collect();
            (id.clone(), occurrences)
        })
        .collect()
}

/// The messages the store holds, by Proton id, ascending.
fn held_keys(store: &Store) -> Vec<String> {
    reading(store, |tx| {
        let read = |e| crate::schema::classify(e, "read the stored messages");
        let mut statement = tx
            .prepare(
                "SELECT DISTINCT l.provider_key FROM logical_messages l
                 JOIN occurrences o ON o.message_id = l.message_id
                 WHERE o.state IN ('present', 'vanish_pending')
                 ORDER BY l.provider_key",
            )
            .map_err(read)?;
        let rows = statement.query_map([], |row| row.get(0)).map_err(read)?;
        rows.collect::<Result<_, _>>().map_err(read)
    })
}

/// A held message's logical id and current occurrence ids.
fn held_message(store: &Store, provider_key: &str) -> Option<(i64, Vec<i64>)> {
    let provider_key = provider_key.to_owned();
    reading(store, move |tx| {
        let read = |e| crate::schema::classify(e, "read a held message");
        let message: Option<i64> = tx
            .query_row(
                "SELECT message_id FROM logical_messages
                 WHERE account_id = ?1 AND provider_key = ?2",
                params![ACCOUNT, provider_key],
                |row| row.get(0),
            )
            .optional()
            .map_err(read)?;
        let Some(message) = message else {
            return Ok(None);
        };
        let mut statement = tx
            .prepare(
                "SELECT occurrence_id FROM occurrences
                 WHERE message_id = ?1 AND state IN ('present', 'vanish_pending')
                 ORDER BY occurrence_id",
            )
            .map_err(read)?;
        let rows = statement
            .query_map(params![message], |row| row.get(0))
            .map_err(read)?;
        Ok(Some((
            message,
            rows.collect::<Result<_, _>>().map_err(read)?,
        )))
    })
}

/// The messages only a listing missed so far: withheld, unconfirmed,
/// waiting for a second listing or a delete event.
fn waiting(store: &Store) -> BTreeSet<String> {
    reading(store, |tx| {
        let read = |e| crate::schema::classify(e, "read what a listing missed");
        let mut statement = tx
            .prepare(
                "SELECT l.provider_key FROM proton_disappearances d
                 JOIN logical_messages l ON l.message_id = d.message_id
                 WHERE d.confirmed = 0",
            )
            .map_err(read)?;
        let rows = statement.query_map([], |row| row.get(0)).map_err(read)?;
        rows.collect::<Result<_, _>>().map_err(read)
    })
}

fn seen_change() -> FlagChange {
    FlagChange {
        flag: IntentFlag::Seen,
        set: true,
    }
}

fn unseen_change() -> FlagChange {
    FlagChange {
        flag: IntentFlag::Seen,
        set: false,
    }
}

fn star_change() -> FlagChange {
    FlagChange {
        flag: IntentFlag::Flagged,
        set: true,
    }
}

fn unstar_change() -> FlagChange {
    FlagChange {
        flag: IntentFlag::Flagged,
        set: false,
    }
}

/// Apply one flag delta to the server's message, as Proton's mutation
/// call would (spec/04-sync-proton "Running an intent": Act).
fn apply_flag(held: &mut Held, change: FlagChange) {
    match change.flag {
        IntentFlag::Seen => held.seen = change.set,
        IntentFlag::Flagged => held.starred = change.set,
    }
}

/// The user changes a flag on a message the store holds: the local
/// effect and the pending intent commit together (M3.1b1).
fn plan_flag(store: &Store, provider_key: &str, change: FlagChange) {
    let Some((message, _)) = held_message(store, provider_key) else {
        return;
    };
    block_on(store.write(WritePriority::User, move |tx| {
        let count: i64 = tx
            .query_row("SELECT COUNT(*) FROM intents", [], |row| row.get(0))
            .map_err(|e| crate::schema::classify(e, "count the intents"))?;
        let mut next = count;
        plan_flag_intents(
            tx,
            ACCOUNT,
            &[message],
            change,
            IntentOrigin::User,
            now().as_millis(),
            &mut || {
                next += 1;
                format!("model-{next}")
            },
        )
        .map(drop)
    }))
    .expect("the flag plan");
}

/// The client deletes a message it holds with a `done`
/// `delete_to_trash` intent, as M5's planner will; the server deletes
/// it and reports the delete event. The intent explains the
/// disappearance, so it never opens a breaker event.
fn delete_with_intent(store: &Store, server: &mut Server, provider_key: &str) {
    let Some((_, occurrences)) = held_message(store, provider_key) else {
        return;
    };
    let target = serde_json::json!({"v": 1, "occurrence_ids": occurrences}).to_string();
    let count = count(store, "SELECT COUNT(*) FROM intents");
    let intent_id = format!("model-delete-{provider_key}-{count}");
    block_on(store.write(WritePriority::User, move |tx| {
        tx.execute(
            "INSERT INTO intents (intent_id, account_id, config_generation, op, target_json,
             args_json, preconditions_json, state, attempts, idempotency_key, origin,
             created_at, updated_at)
             VALUES (?1, ?2, (SELECT config_generation FROM accounts WHERE account_id = ?2),
                     'delete_to_trash', ?3, '{\"v\":1}', '{\"v\":1}', 'done', 0, ?1,
                     'user', 0, 0)",
            params![intent_id, ACCOUNT, target],
        )
        .map_err(|e| crate::schema::classify(e, "insert the delete intent"))?;
        Ok(())
    }))
    .expect("the delete intent");
    server.delete(provider_key);
}

/// The model's executor (there is no worker yet, M3.1c): claim the
/// account's runnable intents in order, apply each change to the
/// server, which reports it as an update event as Proton does, and
/// finish the intent `done`. A message the server no longer holds
/// quarantines the intent (`ambiguous_match`); `fail_first` counts
/// one failure instead, leaving that intent to retry later.
fn run_intents(store: &Store, server: &mut Server, fail_first: bool, now: i64) {
    let mut fail = fail_first;
    loop {
        let claimed = block_on(store.write(WritePriority::Sync, move |tx| {
            claim_next_intent(tx, ACCOUNT, now)
        }))
        .expect("the claim");
        let Some(claimed) = claimed else {
            return;
        };
        let intent_id = claimed.intent_id.clone();
        if fail {
            fail = false;
            block_on(store.write(WritePriority::Sync, move |tx| {
                record_intent_failure(tx, &intent_id, "the model's refusal", now).map(drop)
            }))
            .expect("the failure");
            continue;
        }
        let Some(mut held) = server.messages.get(&claimed.provider_key).cloned() else {
            block_on(store.write(WritePriority::Sync, move |tx| {
                quarantine_intent(tx, &intent_id, QuarantineReason::AmbiguousMatch, now).map(drop)
            }))
            .expect("the quarantine");
            continue;
        };
        apply_flag(&mut held, claimed.change);
        server.put(claimed.provider_key.clone(), held);
        server.applied_intents.push(intent_id.clone());
        block_on(store.write(WritePriority::Sync, move |tx| {
            finish_intent_done(tx, &intent_id, now).map(drop)
        }))
        .expect("the finish");
    }
}

/// Run every remaining runnable intent once the history is over, as
/// the worker would: startup recovers `in_flight`, the health action
/// releases `parked`, and each claim applies and finishes. `later`
/// makes every retry due.
fn run_all_intents(store: &Store, server: &mut Server) {
    let later = now().as_millis() + 24 * 60 * 60 * 1000;
    block_on(store.write(WritePriority::Sync, move |tx| {
        recover_in_flight_intents(tx, ACCOUNT, later).map(drop)
    }))
    .expect("the recovery");
    let parked: Vec<String> = reading(store, |tx| {
        let read = |e| crate::schema::classify(e, "read parked intents");
        let mut statement = tx
            .prepare("SELECT intent_id FROM intents WHERE account_id = ?1 AND state = 'parked'")
            .map_err(read)?;
        let rows = statement
            .query_map(params![ACCOUNT], |row| row.get(0))
            .map_err(read)?;
        rows.collect::<Result<_, _>>().map_err(read)
    });
    if !parked.is_empty() {
        block_on(store.write(WritePriority::User, move |tx| {
            release_intents(tx, &parked, later).map(drop)
        }))
        .expect("the release");
    }
    // A message the server no longer holds quarantines its intent, and
    // a later intent of the same message waits on it. The user's
    // `cancel_intents` ends the quarantine so the later one can run;
    // the model runs until nothing is left to claim.
    loop {
        run_intents(store, server, false, later);
        let quarantined: Vec<String> = reading(store, |tx| {
            let read = |e| crate::schema::classify(e, "read quarantined intents");
            let mut statement = tx
                .prepare(
                    "SELECT intent_id FROM intents
                     WHERE account_id = ?1 AND state = 'quarantined'",
                )
                .map_err(read)?;
            let rows = statement
                .query_map(params![ACCOUNT], |row| row.get(0))
                .map_err(read)?;
            rows.collect::<Result<_, _>>().map_err(read)
        });
        if quarantined.is_empty() {
            return;
        }
        block_on(store.write(WritePriority::User, move |tx| {
            cancel_intents(tx, &quarantined, later).map(drop)
        }))
        .expect("the cancellation");
    }
}

/// No intent is left pending or in flight once the history's intents
/// have run.
fn assert_no_live_intents(store: &Store) {
    assert_eq!(
        count(
            store,
            "SELECT COUNT(*) FROM intents WHERE state IN ('pending', 'in_flight')"
        ),
        0,
        "an intent never ran"
    );
}

/// A claim applies once, so no intent id runs twice.
fn assert_no_double_run(server: &Server) {
    let unique: BTreeSet<_> = server.applied_intents.iter().collect();
    assert_eq!(
        unique.len(),
        server.applied_intents.len(),
        "an intent ran twice for one claim"
    );
}

/// Listed, at the server's last cursor, with the server's labels, and
/// nothing paused.
fn assert_settled(store: &Store, server: &Server) {
    let account = reading(store, |tx| proton_account(tx, ACCOUNT)).expect("the account");
    assert_eq!(account.listing_state, ProtonListingState::Complete);
    assert_eq!(account.event_cursor, Some(cursor(server.log.len())));
    assert_eq!(account.pending_cursor, None);
    assert_eq!(account.listing_checkpoint, None);
    let travel = reading(store, |tx| {
        tx.query_row(
            "SELECT group_concat(remote_name) FROM mailboxes WHERE provider_key = ?1",
            params![TRAVEL],
            |row| row.get::<_, String>(0),
        )
        .map_err(|e| crate::schema::classify(e, "read the label's name"))
    });
    assert_eq!(
        travel, server.travel,
        "one Travel mailbox, as the server names it"
    );
    assert_eq!(
        count(
            store,
            "SELECT COUNT(*) FROM breaker_events WHERE state = 'open'"
        ),
        0
    );
}

/// Bring the store to the server: sync and accept what the breaker
/// holds. The store then holds exactly the server's messages, except
/// one deleted where only a listing saw it go, which waits withheld for
/// a second listing; one refresh settles that.
fn converge(store: &Store, server: &mut Server) {
    sync(store, server);
    accept_all(store);
    run_all_intents(store, server);
    sync(store, server);
    accept_all(store);
    assert_no_live_intents(store);
    assert_no_double_run(server);
    assert_invariants(store);
    assert_settled(store, server);
    let waiting = waiting(store);
    let mut held = stored(store);
    for id in &waiting {
        assert!(
            !server.messages.contains_key(id),
            "{id} is on the server but waits as a listing's miss"
        );
        let occurrences = held
            .remove(id)
            .expect("a withheld message keeps its occurrences");
        assert!(
            occurrences
                .iter()
                .all(|(_, state, _, _)| state == "vanish_pending"),
            "{id}: {occurrences:?}"
        );
    }
    assert_eq!(
        held,
        expected(server),
        "the store holds the server's messages"
    );
    if waiting.is_empty() {
        return;
    }

    server.log.push(Logged::Refresh);
    sync(store, server);
    accept_all(store);
    assert_invariants(store);
    assert_settled(store, server);
    assert_eq!(
        stored(store),
        expected(server),
        "a second listing confirms what the first missed"
    );
}

/// Cases for one property: `per_pr` in every pull request's run, times
/// `SB_PROPTEST_SCALE` in the nightly run (spec/11 "Model").
fn cases(per_pr: u32) -> u32 {
    let scale = std::env::var("SB_PROPTEST_SCALE").map_or(1, |scale| {
        scale
            .parse::<u32>()
            .expect("SB_PROPTEST_SCALE is a whole number")
    });
    per_pr * scale
}

fn small(n: usize) -> i64 {
    i64::try_from(n).expect("a small count")
}

#[derive(Debug, Clone)]
enum Op {
    Add {
        time: i64,
        location: usize,
        travel: bool,
    },
    Change {
        pick: Index,
        location: usize,
        travel: bool,
        seen: bool,
    },
    Delete(Index),
    /// Rename the Travel label to one of a few names.
    Rename(u8),
    /// Delete everything in one location: a mass delete or an emptied
    /// folder, or retention in Trash and Spam.
    Empty(usize),
    Refresh,
    /// Sync with a budget of writes, or until nothing is pending; a
    /// cut sync may also reopen the store, as a relaunch would.
    Sync {
        writes: Option<usize>,
        reopen: bool,
    },
    Accept,
    /// The user keeps what the open events hold, for now.
    Keep,
    /// The user changes a flag on a message the store holds, planning
    /// the intent with its local effect.
    Flag {
        pick: Index,
        change: FlagChange,
    },
    /// Run the account's runnable intents; `fail_first` counts one
    /// failure instead of applying the first claim.
    RunIntents {
        fail_first: bool,
    },
    /// Delete a held message with a done `delete_to_trash` intent; the
    /// server deletes it and reports the delete.
    DeleteWithIntent(Index),
}

fn op() -> impl Strategy<Value = Op> {
    let location = 0..LOCATIONS.len();
    prop_oneof![
        4 => (0..30i64, location.clone(), any::<bool>())
            .prop_map(|(time, location, travel)| Op::Add { time, location, travel }),
        2 => (any::<Index>(), location.clone(), any::<bool>(), any::<bool>())
            .prop_map(|(pick, location, travel, seen)| Op::Change { pick, location, travel, seen }),
        2 => any::<Index>().prop_map(Op::Delete),
        1 => (0..3u8).prop_map(Op::Rename),
        1 => location.prop_map(Op::Empty),
        1 => Just(Op::Refresh),
        4 => (proptest::option::of(1..12usize), any::<bool>())
            .prop_map(|(writes, reopen)| Op::Sync { writes, reopen }),
        1 => Just(Op::Accept),
        1 => Just(Op::Keep),
        2 => (
            any::<Index>(),
            prop_oneof![
                Just(seen_change()),
                Just(unseen_change()),
                Just(star_change()),
                Just(unstar_change())
            ],
        )
            .prop_map(|(pick, change)| Op::Flag { pick, change }),
        1 => any::<bool>().prop_map(|fail_first| Op::RunIntents { fail_first }),
        1 => any::<Index>().prop_map(Op::DeleteWithIntent),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases(24)))]

    /// Acceptance: server changes, refreshes and cuts converge, with
    /// no message lost or stored twice.
    #[test]
    fn sync_converges_to_the_server_whatever_the_cuts(
        ops in proptest::collection::vec(op(), 1..48)
    ) {
        let (_scratch, paths, mut store) = signed_in("proton-model");
        let mut server = Server::new(Responses::Varying);
        for op in ops {
            match op {
                Op::Add { time, location, travel } => {
                    let location = LOCATIONS[location];
                    server.add(Held {
                        time,
                        location,
                        travel,
                        seen: false,
                        starred: false,
                    });
                }
                Op::Change {
                    pick,
                    location,
                    travel,
                    seen,
                } => {
                    if let Some(id) = server.pick(&pick) {
                        let held = &server.messages[&id];
                        let (time, starred) = (held.time, held.starred);
                        let location = LOCATIONS[location];
                        server.put(id, Held {
                            time,
                            location,
                            travel,
                            seen,
                            starred,
                        });
                    }
                }
                Op::Delete(pick) => {
                    if let Some(id) = server.pick(&pick) {
                        server.delete(&id);
                    }
                }
                Op::Rename(name) => server.rename_travel(format!("Travel {name}")),
                Op::Empty(location) => {
                    let ids: Vec<String> = server
                        .messages
                        .iter()
                        .filter(|(_, held)| held.location == LOCATIONS[location])
                        .map(|(id, _)| id.clone())
                        .collect();
                    for id in ids {
                        server.delete(&id);
                    }
                }
                Op::Refresh => server.log.push(Logged::Refresh),
                Op::Sync { writes, reopen } => {
                    let mut driver = Driver { store: &store, server: &server, writes_left: writes };
                    let cut = driver.sync().is_err();
                    if cut && reopen {
                        drop(store);
                        store = open_store(&paths);
                    }
                }
                Op::Accept => accept_all(&store),
                Op::Keep => drop(resolve(&store, BreakerResolution::Keep)),
                Op::Flag { pick, change } => {
                    let keys = held_keys(&store);
                    if !keys.is_empty() {
                        plan_flag(&store, &keys[pick.index(keys.len())], change);
                    }
                }
                Op::RunIntents { fail_first } => {
                    run_intents(&store, &mut server, fail_first, now().as_millis());
                }
                Op::DeleteWithIntent(pick) => {
                    let keys = held_keys(&store);
                    if !keys.is_empty() {
                        let key = keys[pick.index(keys.len())].clone();
                        delete_with_intent(&store, &mut server, &key);
                    }
                }
            }
            assert_invariants(&store);
        }
        converge(&store, &mut server);
    }
}

/// One breaker case: `deleted` of `location`'s `held` messages go, by
/// events or, with `by_listing`, as one listing's misses.
fn breaker_case(
    location: &'static str,
    held: usize,
    deleted: usize,
    elsewhere: usize,
    by_listing: bool,
) -> Result<(), TestCaseError> {
    let (_scratch, _paths, store) = signed_in("proton-breaker-model");
    let mut server = Server::new(Responses::All);
    let other = if location == INBOX { ARCHIVE } else { INBOX };
    let ids: Vec<String> = (0..held)
        .map(|time| {
            server.add(Held {
                time: small(time),
                location,
                travel: false,
                seen: false,
                starred: false,
            })
        })
        .collect();
    for time in 0..elsewhere {
        server.add(Held {
            time: small(time),
            location: other,
            travel: false,
            seen: false,
            starred: false,
        });
    }
    sync(&store, &server);
    if by_listing {
        // The response asks for a listing before its deletes: the
        // listing, not the events, finds them gone.
        server.log.push(Logged::Refresh);
    }
    for id in &ids[..deleted] {
        server.delete(id);
    }
    sync(&store, &server);

    let retention = location == TRASH || location == SPAM;
    // spec/04's rule, with the thresholds the breaker names.
    let opens = !retention
        && (deleted == held
            || deleted > MAX_DISAPPEARANCES_PER_MAILBOX
            || (held >= MIN_MAILBOX_FOR_SHARE && deleted * 2 > held));
    let kind = match (by_listing, deleted == held) {
        (true, _) => "epoch_change",
        (false, true) => "folder_emptied",
        (false, false) => "bulk_disappearance",
    };
    prop_assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) FROM breaker_events WHERE state = 'open'"
        ),
        i64::from(opens)
    );
    prop_assert_eq!(
        count(
            &store,
            &format!("SELECT COUNT(*) FROM breaker_events WHERE kind = '{kind}'")
        ),
        i64::from(opens)
    );
    prop_assert_eq!(
        count(&store, "SELECT COUNT(*) FROM breaker_events"),
        i64::from(opens)
    );
    // Each deleted message was in its location and All Mail; a
    // listing's misses wait withheld even when nothing opens.
    let pending = count(
        &store,
        "SELECT COUNT(*) FROM occurrences WHERE state = 'vanish_pending'",
    );
    let withheld = opens || by_listing;
    prop_assert_eq!(pending, if withheld { 2 * small(deleted) } else { 0 });
    assert_invariants(&store);
    accept_all(&store);
    if by_listing && !opens {
        // A second listing confirms what the first missed.
        server.log.push(Logged::Refresh);
        sync(&store, &server);
        prop_assert_eq!(
            count(
                &store,
                "SELECT COUNT(*) FROM breaker_events WHERE state = 'open'"
            ),
            0
        );
        prop_assert_eq!(
            count(
                &store,
                "SELECT COUNT(*) FROM occurrences WHERE state = 'vanish_pending'"
            ),
            0
        );
    }
    assert_invariants(&store);
    prop_assert_eq!(stored(&store), expected(&server));
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases(12)))]

    /// Acceptance: `deleted` of a location's `held` messages gone in
    /// one events response, or missed by one listing, open the breaker
    /// exactly when they empty the folder, are more than
    /// `MAX_DISAPPEARANCES_PER_MAILBOX`, or more than half of at least
    /// `MIN_MAILBOX_FOR_SHARE`, and never in Trash or Spam. Deletes
    /// open their own kind of event; a listing's misses open one
    /// `epoch_change`. Each case runs a folder and a retention location,
    /// by events and by a listing.
    #[test]
    fn a_mass_delete_opens_the_breaker_and_retention_never_does(
        folder in prop_oneof![Just(INBOX), Just(ARCHIVE)],
        retention in prop_oneof![Just(TRASH), Just(SPAM)],
        (held, deleted) in (1..40usize).prop_flat_map(|held| (Just(held), 1..=held)),
        elsewhere in 0..4usize,
    ) {
        for location in [folder, retention] {
            for by_listing in [false, true] {
                breaker_case(location, held, deleted, elsewhere, by_listing)?;
            }
        }
    }
}

/// Either side of each threshold, which random sizes rarely hit: half
/// of a mailbox of `MIN_MAILBOX_FOR_SHARE`, and
/// `MAX_DISAPPEARANCES_PER_MAILBOX` of a mailbox too large for the
/// share rule; folders emptied, also past the cap; and Trash, which
/// never opens.
#[test]
fn the_breaker_opens_just_past_each_threshold() {
    let share = MIN_MAILBOX_FOR_SHARE;
    let cap = MAX_DISAPPEARANCES_PER_MAILBOX;
    let large = 2 * cap + 10;
    let cases = [
        (INBOX, share - 1, share / 2),
        (INBOX, share, share / 2),
        (INBOX, share, share / 2 + 1),
        (INBOX, large, cap),
        (INBOX, large, cap + 1),
        (INBOX, 1, 1),
        (INBOX, cap + 15, cap + 15),
        (TRASH, 1, 1),
        (TRASH, share, share / 2 + 1),
        (TRASH, large, cap + 1),
    ];
    for (location, held, deleted) in cases {
        for by_listing in [false, true] {
            breaker_case(location, held, deleted, 1, by_listing).expect("the breaker's rule holds");
        }
    }
}

/// A client delete with a `done` `delete_to_trash` intent explains
/// the disappearance: 30 in Inbox in one pass apply and open nothing.
#[test]
fn explained_client_deletes_in_bulk_open_nothing() {
    let (_scratch, _paths, store) = signed_in("proton-model-explained");
    let mut server = Server::new(Responses::All);
    let ids: Vec<String> = (0..100)
        .map(|time| {
            server.add(Held {
                time: small(time),
                location: INBOX,
                travel: false,
                seen: false,
                starred: false,
            })
        })
        .collect();
    sync(&store, &server);
    for id in &ids[..30] {
        delete_with_intent(&store, &mut server, id);
    }
    sync(&store, &server);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM breaker_events"), 0);
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) FROM occurrences WHERE state = 'vanished'"
        ),
        60
    );
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) FROM occurrences WHERE state = 'vanish_pending'"
        ),
        0
    );
    assert_eq!(stored(&store), expected(&server));
}

/// One message's two intents run in planned order, each once, and the
/// server ends with the second change.
#[test]
fn an_intent_runs_once_and_the_last_change_wins() {
    let (_scratch, _paths, store) = signed_in("proton-model-intent-order");
    let mut server = Server::new(Responses::All);
    server.add(Held {
        time: 1,
        location: INBOX,
        travel: false,
        seen: false,
        starred: false,
    });
    sync(&store, &server);
    let key = held_keys(&store)[0].clone();
    plan_flag(&store, &key, star_change());
    plan_flag(&store, &key, unstar_change());
    run_intents(&store, &mut server, false, now().as_millis());
    assert_eq!(server.applied_intents.len(), 2);
    let unique: BTreeSet<_> = server.applied_intents.iter().collect();
    assert_eq!(unique.len(), 2, "an intent ran twice for one claim");
    assert_no_live_intents(&store);
    assert!(!server.messages[&key].starred, "the second change won");
    assert_eq!(stored(&store), expected(&server));
}

/// A failed attempt leaves the intent to retry; a later run applies
/// it once and the store converges to the server's flag.
#[test]
fn a_failed_attempt_retries_and_converges() {
    let (_scratch, _paths, store) = signed_in("proton-model-intent-retry");
    let mut server = Server::new(Responses::All);
    server.add(Held {
        time: 1,
        location: INBOX,
        travel: false,
        seen: false,
        starred: false,
    });
    sync(&store, &server);
    let key = held_keys(&store)[0].clone();
    plan_flag(&store, &key, seen_change());
    run_intents(&store, &mut server, true, now().as_millis());
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) FROM intents WHERE state = 'pending'"
        ),
        1,
        "the failure left the intent pending"
    );
    assert!(
        !server.messages[&key].seen,
        "the failed attempt applied nothing"
    );
    run_all_intents(&store, &mut server);
    sync(&store, &server);
    accept_all(&store);
    assert_no_live_intents(&store);
    assert!(server.messages[&key].seen, "the retry reached the server");
    assert_eq!(stored(&store), expected(&server));
}
