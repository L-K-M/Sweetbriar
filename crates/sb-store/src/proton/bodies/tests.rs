//! Proton body writes (plan M2.16b): a committed reconstruction becomes
//! its logical message's body once, a later version re-acquires it, the
//! listing keeps the stored header block, and the queue orders what is
//! left.

use rusqlite::{OptionalExtension, params};
use sb_test_support::{ScratchDir, block_on, real_fs};
use sb_types::time::Timestamp;

use super::*;
use crate::imports::{ImportParseState, ImportSummary};
use crate::lists::MailboxRole;
use crate::proton::{
    ListedMessage, ListedPage, ListedPageStats, NewProtonAccount, ObservedLabel,
    ProtonBodyFailureReason, ProtonPasswordMode, begin_listing, bump_key_generation,
    commit_listed_page, insert_proton_account, proton_account, proton_coverage,
    reconcile_proton_labels, record_body_failure,
};
use crate::sidebar::MailboxKind;
use crate::sync::{HeaderColumns, SyncedFlags};
use crate::testing::{reading, scratch_paths, test_seams};
use crate::{ObjectStore, PublishedObject, Store, StoreError, WritePriority, open};

const ACCOUNT: &str = "00000000-0000-4000-8000-0000000000b1";

fn now() -> Timestamp {
    Timestamp::from_millis(1_735_689_600_000)
}

fn write<T: Send + 'static>(
    store: &Store,
    f: impl FnOnce(&Transaction) -> Result<T, StoreError> + Send + 'static,
) -> Result<T, StoreError> {
    block_on(store.write(WritePriority::Sync, f))
}

/// A Proton account with its system labels reconciled, still listing.
fn proton_store(label: &str) -> (ScratchDir, Store) {
    let (scratch, paths) = scratch_paths(label);
    let store = open(&paths, &test_seams(real_fs())).unwrap();
    write(&store, |tx| {
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
                proton_user_id: "user-1".to_owned(),
                credential_ref: "proton-b1".to_owned(),
                password_mode: ProtonPasswordMode::Login,
            },
        )?;
        begin_listing(tx, ACCOUNT, "event-0", 1).map(|_| ())?;
        reconcile_proton_labels(tx, ACCOUNT, &labels())
    })
    .unwrap();
    (scratch, store)
}

fn label(key: &str, name: &str, kind: MailboxKind, role: Option<MailboxRole>) -> ObservedLabel {
    ObservedLabel {
        provider_key: key.to_owned(),
        remote_name: name.to_owned(),
        delimiter: (kind == MailboxKind::Folder).then_some('/'),
        kind,
        role,
        selectable: true,
    }
}

fn labels() -> Vec<ObservedLabel> {
    vec![
        label("0", "Inbox", MailboxKind::Folder, Some(MailboxRole::Inbox)),
        label(
            "5",
            "All Mail",
            MailboxKind::Virtual,
            Some(MailboxRole::All),
        ),
        label(
            "10",
            "Starred",
            MailboxKind::Virtual,
            Some(MailboxRole::Flagged),
        ),
    ]
}

/// One listed message, as the metadata mapping leaves it.
fn listed(key: &str, labels: &[&str], date: i64) -> ListedMessage {
    ListedMessage {
        provider_key: key.to_owned(),
        label_ids: labels.iter().map(|label| (*label).to_owned()).collect(),
        raw_metadata: format!(r#"{{"v":1,"proton_metadata":{{"ID":"{key}"}}}}"#).into_bytes(),
        columns: HeaderColumns {
            subject: Some(format!("Listed {key}")),
            ..HeaderColumns::default()
        },
        internal_date: Some(Timestamp::from_millis(date)),
        size: Some(1234),
        flags: SyncedFlags::default(),
    }
}

/// Commit one listed page under the account's current revision.
fn commit_page(
    store: &Store,
    messages: Vec<ListedMessage>,
    checkpoint: &str,
) -> Result<ListedPageStats, StoreError> {
    let checkpoint = checkpoint.to_owned();
    write(store, move |tx| {
        let account = proton_account(tx, ACCOUNT)?.expect("the account is listing");
        commit_listed_page(
            tx,
            &ListedPage {
                account_id: ACCOUNT,
                messages: &messages,
                checkpoint: &checkpoint,
                expected_revision: account.revision,
            },
            now(),
        )
    })
}

/// A body carrying a published object, with the stored header block
/// and its parsed columns (In-Reply-To and References now filled).
fn body_with(object: PublishedObject, key: &str, version: u32) -> ProtonBody {
    ProtonBody {
        account_id: ACCOUNT.to_owned(),
        provider_key: key.to_owned(),
        object,
        reconstruction_version: version,
        raw_header: b"Subject: Body\nIn-Reply-To: <parent@example.test>\n\
                     References: <parent@example.test>\n"
            .to_vec(),
        columns: HeaderColumns {
            subject: Some("Body".to_owned()),
            in_reply_to: Some("<parent@example.test>".to_owned()),
            references_json: Some("[\"<parent@example.test>\"]".to_owned()),
            ..HeaderColumns::default()
        },
        summary: ImportSummary {
            parser_version: 1,
            parse_state: ImportParseState::Parsed,
            subject: Some("Body".to_owned()),
            snippet: Some("Hello".to_owned()),
            ..ImportSummary::default()
        },
    }
}

fn body(store: &Store, key: &str, version: u32, bytes: &[u8]) -> ProtonBody {
    let object = store.objects().publish(bytes).unwrap();
    body_with(object, key, version)
}

fn commit_body(store: &Store, body: ProtonBody) -> Result<ProtonBodyCommit, StoreError> {
    write(store, move |tx| commit_proton_body(tx, &body, now()))
}

fn message_id(store: &Store, key: &str) -> i64 {
    let key = key.to_owned();
    reading(store, move |tx| {
        Ok(tx
            .query_row(
                "SELECT message_id FROM logical_messages
                 WHERE account_id = ?1 AND provider_key = ?2",
                params![ACCOUNT, key],
                |row| row.get(0),
            )
            .unwrap())
    })
}

fn count(store: &Store, sql: &'static str) -> i64 {
    reading(store, move |tx| {
        Ok(tx.query_row(sql, [], |row| row.get(0)).unwrap())
    })
}

fn reservations(store: &Store) -> i64 {
    count(
        store,
        "SELECT count(*) FROM object_pins WHERE pin_kind = 'reservation'",
    )
}

fn source_refs(store: &Store) -> Vec<String> {
    reading(store, |tx| {
        let mut statement = tx
            .prepare(
                "SELECT source_ref FROM acquisition_sources
                 WHERE account_id = ?1 AND source_kind = 'proton_fetch'
                 ORDER BY source_ref",
            )
            .unwrap();
        let rows = statement
            .query_map(params![ACCOUNT], |row| row.get(0))
            .unwrap();
        Ok(rows.map(Result::unwrap).collect())
    })
}

/// Commit a body, then read back the message's linked acquisition.
fn linked_acquisition(store: &Store, key: &str) -> Option<i64> {
    let key = key.to_owned();
    reading(store, move |tx| {
        Ok(tx
            .query_row(
                "SELECT acquisition_id FROM logical_messages
                 WHERE account_id = ?1 AND provider_key = ?2",
                params![ACCOUNT, key],
                |row| row.get(0),
            )
            .optional()
            .unwrap())
    })
}

#[test]
fn commit_proton_body_stores_the_body_and_its_provenance() {
    let (_scratch, store) = proton_store("proton-body-commit");
    commit_page(&store, vec![listed("m1", &["0"], 1_700_000_000_000)], "a1").unwrap();
    let message = message_id(&store, "m1");
    write(&store, move |tx| {
        record_body_failure(
            tx,
            message,
            ProtonBodyFailureReason::Decrypt,
            None,
            1,
            now(),
        )
    })
    .unwrap();

    let bytes = b"Subject: Body\n\nHello\n";
    let object = store.objects().publish(&bytes[..]).unwrap();
    let (hash, length) = (object.id().sha256_hex(), object.id().byte_length());
    let committed = commit_body(&store, body_with(object, "m1", 1)).unwrap();
    assert!(committed.new_object);
    assert_eq!(committed.message_id, message);
    assert_eq!(
        linked_acquisition(&store, "m1"),
        Some(committed.acquisition_id)
    );

    let (stored_hash, stored_length, state): (String, i64, String) = reading(&store, |tx| {
        Ok(tx
            .query_row(
                "SELECT content_sha256, byte_length, object_state FROM acquisitions
                 WHERE acquisition_id = ?1",
                params![committed.acquisition_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap())
    });
    assert_eq!(
        (stored_hash.as_str(), stored_length, state.as_str()),
        (hash.as_str(), length as i64, "published")
    );
    assert_eq!(
        source_refs(&store),
        vec!["m1#r1".to_owned()],
        "the exact proton_fetch source ref"
    );
    let queued: i64 = reading(&store, |tx| {
        Ok(tx
            .query_row(
                "SELECT count(*) FROM d_index_queue WHERE doc_id = ?1 AND reason = 'new'",
                params![committed.acquisition_id],
                |row| row.get(0),
            )
            .unwrap())
    });
    assert_eq!(queued, 1, "the body is queued for search");

    let (raw, in_reply_to, references, fetched): (Vec<u8>, Option<String>, Option<String>, i64) =
        reading(&store, |tx| {
            Ok(tx
                .query_row(
                    "SELECT raw_fields, in_reply_to, references_json, fetched_at
                     FROM message_headers WHERE message_id = ?1",
                    params![message],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .unwrap())
        });
    assert_eq!(raw, body_header());
    assert_eq!(in_reply_to.as_deref(), Some("<parent@example.test>"));
    assert_eq!(references.as_deref(), Some("[\"<parent@example.test>\"]"));
    assert_eq!(fetched, now().as_millis());

    assert!(
        reading(&store, |tx| crate::proton::body_failure(tx, message)).is_none(),
        "the failure is cleared"
    );
    let (subject, snippet): (Option<String>, Option<String>) = reading(&store, |tx| {
        Ok(tx
            .query_row(
                "SELECT subject, snippet FROM d_list_rows WHERE message_id = ?1",
                params![message],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap())
    });
    assert_eq!(subject.as_deref(), Some("Body"));
    assert_eq!(snippet.as_deref(), Some("Hello"));
    assert_eq!(
        reservations(&store),
        0,
        "the publication's reservation is released"
    );
}

/// The header block `body_with` stores, as a slice.
fn body_header() -> &'static [u8] {
    b"Subject: Body\nIn-Reply-To: <parent@example.test>\nReferences: <parent@example.test>\n"
}

#[test]
fn replaying_a_body_is_a_noop_and_releases_its_reservation() {
    let (_scratch, store) = proton_store("proton-body-replay");
    commit_page(&store, vec![listed("m1", &["0"], 1_700_000_000_000)], "a1").unwrap();

    let bytes = b"Subject: Body\n\nHello\n";
    let first = store.objects().publish(&bytes[..]).unwrap();
    let committed = commit_body(&store, body_with(first, "m1", 1)).unwrap();
    assert_eq!(reservations(&store), 0);

    // A crash before the commit closed replays the same source: no
    // second row, and this publication's reservation is tolerated.
    let replay = store.objects().publish(&bytes[..]).unwrap();
    assert_eq!(reservations(&store), 1);
    let replayed = commit_body(&store, body_with(replay, "m1", 1)).unwrap();
    assert!(!replayed.new_object);
    assert_eq!(replayed.acquisition_id, committed.acquisition_id);
    assert_eq!(source_refs(&store), vec!["m1#r1".to_owned()]);
    assert_eq!(count(&store, "SELECT count(*) FROM acquisitions"), 1);
    assert_eq!(reservations(&store), 0);
}

#[test]
fn the_same_source_ref_with_other_bytes_is_a_conflict() {
    let (_scratch, store) = proton_store("proton-body-conflict");
    commit_page(&store, vec![listed("m1", &["0"], 1_700_000_000_000)], "a1").unwrap();

    let first = store.objects().publish(&b"one"[..]).unwrap();
    let committed = commit_body(&store, body_with(first, "m1", 1)).unwrap();
    let acquisitions = count(&store, "SELECT count(*) FROM acquisitions");

    let other = store.objects().publish(&b"two"[..]).unwrap();
    let refused = commit_body(&store, body_with(other, "m1", 1));
    assert!(
        matches!(refused, Err(StoreError::Conflict { .. })),
        "{refused:?}"
    );
    assert_eq!(
        count(&store, "SELECT count(*) FROM acquisitions"),
        acquisitions
    );
    assert_eq!(source_refs(&store), vec!["m1#r1".to_owned()]);
    assert_eq!(
        linked_acquisition(&store, "m1"),
        Some(committed.acquisition_id)
    );
    // A refused commit leaves its reservation; an abandoned publication's
    // reservation expires at startup (spec/03).
    assert_eq!(reservations(&store), 1);

    // The index refuses a second row for the same reconstruction even
    // past the commit's own check (migration 32).
    let duplicate = write(&store, move |tx| {
        tx.execute(
            "INSERT INTO acquisition_sources
             (acquisition_id, account_id, source_kind, source_ref, completeness, acquired_at)
             VALUES (?1, ?2, 'proton_fetch', 'm1#r1', 'complete', 0)",
            params![committed.acquisition_id, ACCOUNT],
        )
        .map_err(|e| crate::schema::classify(e, "insert a duplicate source"))
    });
    assert!(duplicate.is_err(), "{duplicate:?}");
}

#[test]
fn a_later_reconstruction_adds_a_source_and_relinks() {
    let (_scratch, store) = proton_store("proton-body-versions");
    commit_page(&store, vec![listed("m1", &["0"], 1_700_000_000_000)], "a1").unwrap();

    let first = store.objects().publish(&b"first"[..]).unwrap();
    let committed = commit_body(&store, body_with(first, "m1", 1)).unwrap();
    assert!(committed.new_object);

    let second = store.objects().publish(&b"second"[..]).unwrap();
    let relinked = commit_body(&store, body_with(second, "m1", 2)).unwrap();
    assert!(relinked.new_object);
    assert_ne!(relinked.acquisition_id, committed.acquisition_id);
    assert_eq!(
        linked_acquisition(&store, "m1"),
        Some(relinked.acquisition_id)
    );
    // Both originals and both source rows stay (spec/02).
    assert_eq!(
        source_refs(&store),
        vec!["m1#r1".to_owned(), "m1#r2".to_owned()]
    );
    assert_eq!(count(&store, "SELECT count(*) FROM acquisitions"), 2);
    assert_eq!(
        count(
            &store,
            "SELECT count(*) FROM acquisitions WHERE object_state = 'published'"
        ),
        2
    );

    // Version 2 replayed: no third row, no relink backwards.
    let replay = store.objects().publish(&b"second"[..]).unwrap();
    let replayed = commit_body(&store, body_with(replay, "m1", 2)).unwrap();
    assert!(!replayed.new_object);
    assert_eq!(replayed.acquisition_id, relinked.acquisition_id);
    assert_eq!(source_refs(&store).len(), 2);
}

#[test]
fn a_body_for_an_unknown_key_or_account_writes_nothing() {
    let (_scratch, store) = proton_store("proton-body-refused");
    commit_page(&store, vec![listed("m1", &["0"], 1_700_000_000_000)], "a1").unwrap();
    let acquisitions = count(&store, "SELECT count(*) FROM acquisitions");
    let sources = count(&store, "SELECT count(*) FROM acquisition_sources");

    let refused = commit_body(&store, body(&store, "missing", 1, b"body"));
    assert!(
        matches!(refused, Err(StoreError::NotFound { .. })),
        "{refused:?}"
    );

    // A non-Proton account: even a logical message of that account is
    // not a Proton body's target.
    write(&store, |tx| {
        tx.execute(
            "INSERT INTO accounts (account_id, kind, provider, display_name, state, sort_order, created_at)
             VALUES ('bridge', 'imap', 'proton_bridge', 'Bridge', 'active', 2, 0)",
            [],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO logical_messages (account_id, provider_key, created_at)
             VALUES ('bridge', 'm1', 0)",
            [],
        )
        .unwrap();
        Ok(())
    })
    .unwrap();
    let mut other = body(&store, "m1", 1, b"body");
    other.account_id = "bridge".to_owned();
    let refused = commit_body(&store, other);
    assert!(
        matches!(refused, Err(StoreError::NotFound { .. })),
        "{refused:?}"
    );

    assert_eq!(
        count(&store, "SELECT count(*) FROM acquisitions"),
        acquisitions
    );
    assert_eq!(
        count(&store, "SELECT count(*) FROM acquisition_sources"),
        sources
    );
    assert_eq!(count(&store, "SELECT count(*) FROM d_index_queue"), 0);
    // Both refused publications keep their reservations until startup's
    // expiry (spec/03).
    assert_eq!(reservations(&store), 2);
}

#[test]
fn a_later_listed_page_keeps_the_body_header_block() {
    let (_scratch, store) = proton_store("proton-body-headers");
    commit_page(&store, vec![listed("m1", &["0"], 1_700_000_000_000)], "a1").unwrap();
    let message = message_id(&store, "m1");

    let object = store.objects().publish(&b"body"[..]).unwrap();
    commit_body(&store, body_with(object, "m1", 1)).unwrap();
    // The next listing page brings the metadata again; the stored
    // header block wins (spec/02 "Headers before bodies").
    commit_page(&store, vec![listed("m1", &["0"], 1_700_000_000_000)], "a2").unwrap();
    let (raw, subject): (Vec<u8>, Option<String>) = reading(&store, |tx| {
        Ok(tx
            .query_row(
                "SELECT raw_fields, subject FROM message_headers WHERE message_id = ?1",
                params![message],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap())
    });
    assert_eq!(raw, body_header());
    assert_eq!(subject.as_deref(), Some("Body"));
}

#[test]
fn pending_bodies_order_then_newest_and_skip_unfetchable_messages() {
    let (_scratch, store) = proton_store("proton-body-pending");
    commit_page(
        &store,
        vec![
            listed("m-old", &["0"], 1_600_000_000_000),
            listed("m-new", &["0"], 1_700_000_000_000),
            listed("m-bodied", &["0"], 1_650_000_000_000),
            listed("m-vanished", &["0"], 1_640_000_000_000),
            listed("m-failed", &["0"], 1_630_000_000_000),
        ],
        "a1",
    )
    .unwrap();

    // One message gets a body, one loses its last present occurrence
    // (All Sent has no mailbox), one fails at the current generation.
    let bodied = store.objects().publish(&b"body"[..]).unwrap();
    commit_body(&store, body_with(bodied, "m-bodied", 1)).unwrap();
    commit_page(
        &store,
        vec![listed("m-vanished", &["2"], 1_640_000_000_000)],
        "a2",
    )
    .unwrap();
    let failed = message_id(&store, "m-failed");
    write(&store, move |tx| {
        record_body_failure(tx, failed, ProtonBodyFailureReason::NoKey, None, 1, now())
    })
    .unwrap();

    let keys = |first: &[&str], limit: usize| -> Vec<String> {
        let first: Vec<String> = first.iter().map(|key| (*key).to_owned()).collect();
        reading(&store, |tx| {
            pending_proton_bodies(tx, ACCOUNT, &first, limit)
        })
        .into_iter()
        .map(|body| body.provider_key)
        .collect()
    };
    // Newest first; bodied, vanished-only and failed messages skip.
    assert_eq!(keys(&[], 10), vec!["m-new", "m-old"]);
    // The opened keys lead, in the order given, then newest.
    assert_eq!(keys(&["m-old", "m-new"], 10), vec!["m-old", "m-new"]);
    assert_eq!(keys(&["m-new", "m-old"], 10), vec!["m-new", "m-old"]);
    // A key opened twice is queued once.
    assert_eq!(keys(&["m-old", "m-old"], 10), vec!["m-old", "m-new"]);
    assert_eq!(keys(&[], 1), vec!["m-new"]);
    // An opened key that no longer qualifies is skipped, not a stop.
    assert_eq!(keys(&["m-bodied"], 10), vec!["m-new", "m-old"]);

    // A key change makes the failure eligible again.
    let next = write(&store, |tx| bump_key_generation(tx, ACCOUNT)).unwrap();
    assert_eq!(next, 2);
    let account = reading(&store, |tx| {
        Ok(proton_account(tx, ACCOUNT).unwrap().unwrap())
    });
    assert_eq!(account.key_generation, 2);
    assert_eq!(keys(&[], 10), vec!["m-new", "m-failed", "m-old"]);

    // The bounds are enforced.
    let too_many = store.read(|tx| pending_proton_bodies(tx, ACCOUNT, &[], 501));
    assert!(matches!(too_many, Err(StoreError::InvalidInput { .. })));
    let opened = vec![String::new(); 501];
    let too_many_keys = store.read(|tx| pending_proton_bodies(tx, ACCOUNT, &opened, 10));
    assert!(matches!(
        too_many_keys,
        Err(StoreError::InvalidInput { .. })
    ));
}

#[test]
fn coverage_counts_a_committed_body_and_clears_its_failure() {
    let (_scratch, store) = proton_store("proton-body-coverage");
    commit_page(&store, vec![listed("m1", &["0"], 1_700_000_000_000)], "a1").unwrap();
    let message = message_id(&store, "m1");
    write(&store, move |tx| {
        record_body_failure(tx, message, ProtonBodyFailureReason::NoKey, None, 1, now())
    })
    .unwrap();
    let coverage = reading(&store, |tx| proton_coverage(tx, ACCOUNT));
    assert_eq!(
        (
            coverage.messages,
            coverage.with_body,
            coverage.body_failures
        ),
        (1, 0, 1)
    );

    let object = store.objects().publish(&b"body"[..]).unwrap();
    commit_body(&store, body_with(object, "m1", 1)).unwrap();
    let coverage = reading(&store, |tx| proton_coverage(tx, ACCOUNT));
    assert_eq!(
        (
            coverage.messages,
            coverage.with_body,
            coverage.body_failures
        ),
        (1, 1, 0)
    );
}

#[test]
fn the_fetch_source_lookup_uses_its_index() {
    let (_scratch, store) = proton_store("bodies-plan");
    let plan = write(&store, |tx| {
        let mut statement = tx
            .prepare(&format!("EXPLAIN QUERY PLAN {FIND_PROTON_FETCH_SOURCE}"))
            .unwrap();
        let details = statement
            .query_map(params![ACCOUNT, "m-1#r1"], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        Ok(details.join("\n"))
    })
    .unwrap();
    assert!(
        plan.contains("USING INDEX acquisition_proton_fetch_identity"),
        "{plan}"
    );
    assert!(!plan.contains("SCAN s"), "{plan}");
}
