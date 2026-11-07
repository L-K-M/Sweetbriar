//! Idempotency-key validation at the engine's command boundary
//! (spec/07 "Commands"): an empty or overlong key is `InvalidInput` on
//! field `idempotency_key`, and no command submits anything, so no
//! receipt row or effect can come from it. Every command that spends a
//! key through `run_once` is represented in its family's test.

use sb_resources::StoreError;
use sb_test_support::{ScratchDir, block_on};

use crate::engine::MAX_IDEMPOTENCY_KEY_BYTES;
use crate::{
    AccountAppearanceChange, AccountColor, ComposeFormat, DraftContent, DraftRecipient,
    DraftSource, Engine, EngineError, IdentityAppearanceChange,
};

/// An engine over a fresh scratch data root, with the scratch dir kept
/// alive for the test.
fn scratch_engine(label: &str) -> (ScratchDir, Engine) {
    let scratch = ScratchDir::new(label);
    let engine = block_on(Engine::open(scratch.path().join("store"))).expect("the engine opens");
    (scratch, engine)
}

/// A key one byte past the bound.
fn overlong_key() -> String {
    "x".repeat(MAX_IDEMPOTENCY_KEY_BYTES + 1)
}

/// Assert `result` is `InvalidInput` naming `idempotency_key`.
fn assert_invalid_key<T: std::fmt::Debug>(result: Result<T, EngineError>) {
    match result {
        Err(EngineError::InvalidInput { field, .. }) => assert_eq!(field, "idempotency_key"),
        err => panic!("an invalid idempotency key must be InvalidInput, got {err:?}"),
    }
}

/// One aggregate over the store.
fn count(engine: &Engine, sql: &str) -> i64 {
    engine
        .resources()
        .read(|tx| {
            tx.query_row(sql, [], |row| row.get(0))
                .map_err(|error| StoreError::internal(error.to_string()))
        })
        .unwrap()
}

/// The stored `command_receipts` rows.
fn receipt_count(engine: &Engine) -> i64 {
    count(engine, "SELECT count(*) FROM command_receipts")
}

/// One save's content: plain flowed text with one recipient.
fn content(subject: &str) -> DraftContent {
    DraftContent {
        identity_id: None,
        format: ComposeFormat::Plain,
        plain_flowed: true,
        subject: subject.to_owned(),
        to: vec![DraftRecipient {
            address: "to@example.test".to_owned(),
            name: Some("To".to_owned()),
        }],
        cc: vec![],
        bcc: vec![],
        body_source: "the source text".to_owned(),
        attachments: vec![],
    }
}

/// The seeded local archive's id.
fn archive_id(engine: &Engine) -> String {
    engine
        .resources()
        .read(|tx| {
            tx.query_row(
                "SELECT account_id FROM accounts WHERE provider = 'local_archive'",
                [],
                |row| row.get(0),
            )
            .map_err(|error| StoreError::internal(error.to_string()))
        })
        .unwrap()
}

/// Every draft command rejects an empty and an overlong key before
/// submitting: no receipt, draft or revision is written.
#[test]
fn draft_commands_reject_an_empty_or_overlong_idempotency_key() {
    let (_scratch, engine) = scratch_engine("idempotency-drafts");
    for key in ["", overlong_key().as_str()] {
        assert_invalid_key(block_on(engine.create_draft(key, DraftSource::New)));
        assert_invalid_key(block_on(engine.save_draft(
            key,
            "draft-1",
            None,
            content("x"),
        )));
        assert_invalid_key(block_on(engine.discard_draft(key, "draft-1")));
    }
    assert_eq!(receipt_count(&engine), 0, "no receipt may be written");
    assert!(
        block_on(engine.drafts()).unwrap().is_empty(),
        "no draft may be created"
    );
    assert_eq!(count(&engine, "SELECT count(*) FROM draft_revisions"), 0);
}

/// Every import command rejects an empty and an overlong key before
/// submitting: no receipt or job is written. `confirm_import` and
/// `cancel_job` also name the key field rather than the job lookup's
/// error, because the key is checked first.
#[test]
fn import_commands_reject_an_empty_or_overlong_idempotency_key() {
    let (scratch, engine) = scratch_engine("idempotency-imports");
    let missing = scratch.path().join("missing.mbox");
    for key in ["", overlong_key().as_str()] {
        assert_invalid_key(block_on(engine.start_import(key, vec![missing.clone()])));
        assert_invalid_key(block_on(engine.confirm_import(key, "job-1", vec![])));
        assert_invalid_key(block_on(engine.cancel_job(key, "job-1")));
    }
    assert_eq!(receipt_count(&engine), 0, "no receipt may be written");
    assert_eq!(count(&engine, "SELECT count(*) FROM import_jobs"), 0);
}

/// `set_setting` rejects an empty and an overlong key before
/// submitting: no receipt and no stored value change.
#[test]
fn set_setting_rejects_an_empty_or_overlong_idempotency_key() {
    let (_scratch, engine) = scratch_engine("idempotency-settings");
    for key in ["", overlong_key().as_str()] {
        assert_invalid_key(block_on(engine.set_setting(
            key,
            "appearance.compose_cue",
            "strong",
        )));
    }
    assert_eq!(receipt_count(&engine), 0, "no receipt may be written");
    let settings = block_on(engine.settings()).unwrap();
    assert_eq!(settings[0].value(), "standard", "the setting is unchanged");
}

/// Every search-rebuild command rejects an empty and an overlong key
/// before submitting: no receipt and no new rebuild job. A fresh open
/// may reconcile an extractor upgrade, so the job count is compared
/// with the count the calls started from.
#[test]
fn rebuild_commands_reject_an_empty_or_overlong_idempotency_key() {
    let (_scratch, engine) = scratch_engine("idempotency-rebuild");
    let jobs_before = count(&engine, "SELECT count(*) FROM search_rebuild_jobs");
    for key in ["", overlong_key().as_str()] {
        assert_invalid_key(block_on(engine.start_search_rebuild(key)));
        assert_invalid_key(block_on(engine.resume_search_rebuild(key, "job-1")));
        assert_invalid_key(block_on(engine.retry_search_index(key)));
    }
    assert_eq!(receipt_count(&engine), 0, "no receipt may be written");
    assert_eq!(
        count(&engine, "SELECT count(*) FROM search_rebuild_jobs"),
        jobs_before,
        "no rebuild job may be started"
    );
}

/// Both appearance setters reject an empty and an overlong key before
/// submitting: no receipt and no appearance change.
#[test]
fn appearance_setters_reject_an_empty_or_overlong_idempotency_key() {
    let (_scratch, engine) = scratch_engine("idempotency-appearance");
    let archive = archive_id(&engine);
    let account_change = AccountAppearanceChange::new(
        AccountColor::Custom("#112233".to_owned()),
        Some("A".to_owned()),
    );
    let identity_change =
        IdentityAppearanceChange::new(Some("#445566".to_owned()), Some("I".to_owned()));
    for key in ["", overlong_key().as_str()] {
        assert_invalid_key(block_on(engine.set_account_appearance(
            key,
            &archive,
            1,
            &account_change,
        )));
        assert_invalid_key(block_on(engine.set_identity_appearance(
            key,
            "ada",
            None,
            &identity_change,
        )));
    }
    assert_eq!(receipt_count(&engine), 0, "no receipt may be written");
    let appearance = block_on(engine.appearance()).unwrap();
    assert_eq!(appearance.generation(), 0, "no appearance change");
    assert_eq!(appearance.accounts()[0].revision(), 1);
    assert!(appearance.identities().is_empty());
}

/// A readable non-UUID key and a key of exactly the bound are accepted:
/// the format is not a validation rule and the bound is inclusive.
#[test]
fn a_readable_key_and_a_key_at_the_bound_are_accepted() {
    let (_scratch, engine) = scratch_engine("idempotency-accepted");
    block_on(engine.set_setting("key-f", "appearance.compose_cue", "strong")).unwrap();
    let at_bound = "x".repeat(MAX_IDEMPOTENCY_KEY_BYTES);
    block_on(engine.set_setting(&at_bound, "appearance.compose_cue", "standard")).unwrap();
    assert_eq!(receipt_count(&engine), 2);
    let settings = block_on(engine.settings()).unwrap();
    assert_eq!(settings[0].value(), "standard");
}
