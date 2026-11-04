//! Draft persistence through the native Resources facade (M1.9a1,
//! spec/02 "Drafts", spec/05, spec/07): the public-API consumer for
//! `sb_store::drafts` and the draft attachment publication path. It
//! proves the acceptance checks at the layer below the engine's
//! commands: the same-base `Conflict`, the publication ordering, the
//! NULL-route draft and the live-attempt refusal wiring.

use std::fs;

use sb_resources::{
    AttachmentRef, AttemptState, ComposeFormat, DraftAttachment, DraftRecipient, DraftSource,
    DraftState, LiveAttempt, PublishedObject, Resources, RevisionInput, StoreError, StoredDraft,
    Timestamp, Transaction, WritePriority, create_draft, discard_draft, draft, drafts, save_draft,
};
use sb_test_support::{ScratchDir, block_on};

/// sha256("attachment one") and sha256("attachment two"), so the
/// revision's references can be asserted exactly.
const ATTACHMENT_ONE: &[u8] = b"attachment one";
const ATTACHMENT_ONE_SHA: &str = "01d55557ecdae250daf0120901373870e2a493b178431392b0c1b8abd3aa4939";
const ATTACHMENT_TWO: &[u8] = b"attachment two";
const ATTACHMENT_TWO_SHA: &str = "08a57b2954f15a43ddc7d37bcc5c866860739ca6f137a62e519144326877bc3d";

fn now() -> Timestamp {
    Timestamp::from_millis(1_735_689_600_000)
}

fn open(label: &str) -> (ScratchDir, Resources) {
    let scratch = ScratchDir::new(label);
    let resources = Resources::open(&scratch.path().join("store")).unwrap();
    (scratch, resources)
}

fn write<T: Send + 'static>(
    res: &Resources,
    f: impl FnOnce(&Transaction) -> Result<T, StoreError> + Send + 'static,
) -> T {
    block_on(res.write(WritePriority::User, f)).unwrap()
}

fn write_err(
    res: &Resources,
    f: impl FnOnce(&Transaction) -> Result<(), StoreError> + Send + 'static,
) -> StoreError {
    block_on(res.write(WritePriority::User, f)).unwrap_err()
}

/// One save's content: plain flowed text with one recipient, the
/// fields each test overrides.
fn input(revision_id: &str) -> RevisionInput {
    RevisionInput {
        revision_id: revision_id.to_owned(),
        identity_id: None,
        format: ComposeFormat::Plain,
        plain_flowed: true,
        subject: "a subject".to_owned(),
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

/// The only way to make a `DraftAttachment`: the spec/03 reservation
/// publish (M1.8a1), whose `PublishedObject` the save's transaction
/// spends. A revision therefore cannot reference an unpublished
/// object.
fn publish(res: &Resources, bytes: &[u8]) -> PublishedObject {
    res.publish_draft_attachment(bytes).unwrap()
}

fn create(res: &Resources, draft_id: &str, source: DraftSource) {
    let id = draft_id.to_owned();
    write(res, move |tx| create_draft(tx, &id, &source, now()));
}

fn save(res: &Resources, draft_id: &str, base: Option<&str>, revision: RevisionInput) {
    let id = draft_id.to_owned();
    let base = base.map(str::to_owned);
    write(res, move |tx| {
        save_draft(tx, &id, base.as_deref(), revision, now())
    });
}

fn one(res: &Resources, draft_id: &str) -> StoredDraft {
    let id = draft_id.to_owned();
    res.read(|tx| draft(tx, &id)).unwrap().unwrap()
}

fn count(res: &Resources, sql: &str) -> i64 {
    let sql = sql.to_owned();
    res.read(move |tx| {
        tx.query_row(&sql, [], |row| row.get(0))
            .map_err(|e| StoreError::internal(e.to_string()))
    })
    .unwrap()
}

/// A sending account's identity and two source rows, so a draft can
/// hold a real route and reply/forward contexts can resolve. The send
/// machinery is M4's; the schema and the refusal wiring are here.
fn seed(res: &Resources) {
    write(res, |tx| {
        tx.execute_batch(
            "INSERT INTO accounts (account_id, kind, provider, display_name, state, sort_order, created_at)
             VALUES ('acct-1', 'imap', 'generic_imap', 'Sender', 'active', 1, 0);
             INSERT INTO identities (identity_id, account_id, email, display_name, is_default, sendable)
             VALUES ('ident-1', 'acct-1', 'me@example.test', 'Me', 1, 'validated');
             INSERT INTO mailboxes (mailbox_id, account_id, remote_name, kind, selectable, sync_state)
             VALUES (7, 'acct-1', 'Inbox', 'folder', 1, 'synced');
             INSERT INTO logical_messages (message_id, account_id, created_at)
             VALUES (9, 'acct-1', 0);
             INSERT INTO occurrences (occurrence_id, account_id, mailbox_id, message_id,
                 first_seen_at, last_seen_at, state)
             VALUES (11, 'acct-1', 7, 9, 0, 0, 'present');",
        )
        .unwrap();
        Ok(())
    });
}

/// One `send_attempts` row on a routed revision (02).
fn plant_attempt(
    res: &Resources,
    attempt_id: &str,
    draft_id: &str,
    revision_id: &str,
    state: &str,
) {
    let sql = format!(
        "INSERT INTO send_attempts (attempt_id, draft_id, revision_id, account_id,
            config_generation, identity_id, mime_sha256, message_id_header,
            envelope_from, state, sent_copy, created_at, updated_at)
         VALUES ('{attempt_id}', '{draft_id}', '{revision_id}', 'acct-1', 1, 'ident-1',
            '401a4499cbe890fe7f85964ac62bb63527c3768036ea79bf32c5e43e7ba24bd2',
            '<{attempt_id}@example.test>', 'me@example.test', '{state}', 'server_saves', 0, 0)"
    );
    write(res, move |tx| {
        tx.execute_batch(&sql).unwrap();
        Ok(())
    });
}

/// Move a planted attempt through its states (05).
fn set_attempt(res: &Resources, attempt_id: &str, state: &str, resolution: Option<&str>) {
    let sql = format!(
        "UPDATE send_attempts SET state = '{state}',
             resolution = {}, resolved_at = {} WHERE attempt_id = '{attempt_id}'",
        resolution.map_or("NULL".to_owned(), |r| format!("'{r}'")),
        resolution.map_or("NULL".to_owned(), |_| "1".to_owned()),
    );
    write(res, move |tx| {
        tx.execute_batch(&sql).unwrap();
        Ok(())
    });
}

#[test]
fn a_draft_saves_and_reopens_with_no_sending_identity() {
    let (scratch, res) = open("draft-null-route");
    let root = scratch.path().join("store");
    create(&res, "d1", DraftSource::New);
    let mut first = input("r1");
    first.subject = "hello there".to_owned();
    first.format = ComposeFormat::Markdown;
    first.plain_flowed = false;
    first.to = vec![
        DraftRecipient {
            address: "one@example.test".to_owned(),
            name: Some("One".to_owned()),
        },
        DraftRecipient {
            address: "two@example.test".to_owned(),
            name: None,
        },
    ];
    first.cc = vec![DraftRecipient {
        address: "cc@example.test".to_owned(),
        name: None,
    }];
    save(&res, "d1", None, first);

    // The whole revision round-trips with its route NULL (spec/02:
    // drafts without an identity are allowed).
    let draft = one(&res, "d1");
    assert_eq!(draft.state, DraftState::Editing);
    assert_eq!(draft.created_at, now());
    assert_eq!(draft.updated_at, now());
    assert!(draft.attempt.is_none());
    let revision = draft.revision.unwrap();
    assert_eq!(revision.revision_id, "r1");
    assert_eq!(revision.seq, 1);
    assert_eq!(revision.base_revision_id, None);
    assert_eq!(revision.account_id, None);
    assert_eq!(revision.identity_id, None);
    assert_eq!(revision.format, ComposeFormat::Markdown);
    assert!(!revision.plain_flowed);
    assert_eq!(revision.subject, "hello there");
    assert_eq!(revision.body_source, "the source text");
    assert_eq!(
        revision.to,
        [
            DraftRecipient {
                address: "one@example.test".to_owned(),
                name: Some("One".to_owned()),
            },
            DraftRecipient {
                address: "two@example.test".to_owned(),
                name: None,
            },
        ]
    );
    assert_eq!(
        revision.cc,
        [DraftRecipient {
            address: "cc@example.test".to_owned(),
            name: None,
        }]
    );
    assert!(revision.bcc.is_empty() && revision.attachments.is_empty());
    assert_eq!(revision.created_at, now());

    // It reopens intact, and the sidebar row shows the same route.
    res.close().unwrap();
    let res = Resources::open(&root).unwrap();
    assert_eq!(one(&res, "d1").revision.unwrap(), revision);
    let listed = res.read(drafts).unwrap();
    assert_eq!(listed.len(), 1);
    let summary = &listed[0];
    assert_eq!(summary.draft_id, "d1");
    assert_eq!(summary.state, DraftState::Editing);
    assert_eq!(
        (
            summary.account_id.as_deref(),
            summary.identity_id.as_deref()
        ),
        (None, None)
    );
    assert_eq!(summary.subject, "hello there");
    assert_eq!(summary.to, revision.to);
    assert_eq!(summary.updated_at, now());
    assert_eq!(summary.attempt, None);
}

#[test]
fn a_second_save_over_the_same_base_is_conflict() {
    let (_scratch, res) = open("draft-conflict");
    create(&res, "d1", DraftSource::New);
    save(&res, "d1", None, input("r1"));
    let mut second = input("r2");
    second.subject = "second".to_owned();
    save(&res, "d1", Some("r1"), second);

    // Two saves over one base: the loser is Conflict whatever it names
    // as its base, and it changed nothing (spec/02, 05).
    for base in [None, Some("r1"), Some("never-saved")] {
        let id = "d1".to_owned();
        let error = write_err(&res, move |tx| {
            save_draft(tx, &id, base, input("r3"), now())
        });
        assert!(
            matches!(error, StoreError::Conflict { .. }),
            "{base:?}: {error:?}"
        );
    }
    let revision = one(&res, "d1").revision.unwrap();
    assert_eq!(revision.revision_id, "r2");
    assert_eq!(revision.seq, 2);
    assert_eq!(revision.base_revision_id.as_deref(), Some("r1"));
    assert_eq!(revision.subject, "second");
    assert_eq!(
        count(&res, "SELECT count(*) FROM draft_revisions"),
        2,
        "the conflicted saves inserted revisions"
    );

    // Unknown drafts and identities name what is missing.
    let error = write_err(&res, |tx| {
        save_draft(tx, "no-such-draft", None, input("r4"), now())
    });
    assert!(
        matches!(&error, StoreError::NotFound { what } if what == "draft no-such-draft"),
        "{error:?}"
    );
    let mut routed = input("r4");
    routed.identity_id = Some("no-such-identity".to_owned());
    let error = write_err(&res, move |tx| {
        save_draft(tx, "d1", Some("r2"), routed, now())
    });
    assert!(
        matches!(&error, StoreError::NotFound { what } if what == "identity no-such-identity"),
        "{error:?}"
    );
}

#[test]
fn attachments_are_published_before_the_revision_references_them() {
    let (scratch, res) = open("draft-attachments");
    let root = scratch.path().join("store");
    create(&res, "d1", DraftSource::New);
    let first = publish(&res, ATTACHMENT_ONE);
    let second = publish(&res, ATTACHMENT_TWO);
    let mut with_files = input("r1");
    with_files.attachments = vec![
        DraftAttachment::New {
            object: first,
            filename: "one.txt".to_owned(),
            media_type: "text/plain".to_owned(),
        },
        DraftAttachment::New {
            object: second,
            filename: "two.txt".to_owned(),
            media_type: "text/plain".to_owned(),
        },
    ];
    save(&res, "d1", None, with_files);

    let revision = one(&res, "d1").revision.unwrap();
    assert_eq!(
        revision.attachments,
        [
            AttachmentRef {
                object_sha256: ATTACHMENT_ONE_SHA.to_owned(),
                filename: "one.txt".to_owned(),
                media_type: "text/plain".to_owned(),
                size: ATTACHMENT_ONE.len() as u64,
            },
            AttachmentRef {
                object_sha256: ATTACHMENT_TWO_SHA.to_owned(),
                filename: "two.txt".to_owned(),
                media_type: "text/plain".to_owned(),
                size: ATTACHMENT_TWO.len() as u64,
            },
        ]
    );
    // Each referenced object is a published file whose bytes round-trip.
    for (sha, bytes) in [
        (ATTACHMENT_ONE_SHA, ATTACHMENT_ONE),
        (ATTACHMENT_TWO_SHA, ATTACHMENT_TWO),
    ] {
        let path = root
            .join("objects/sha256")
            .join(&sha[..2])
            .join(&sha[2..4])
            .join(sha);
        assert!(path.is_file(), "{sha} has no published object");
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
    // The revision's transaction spent both reservations (spec/03).
    assert_eq!(count(&res, "SELECT count(*) FROM object_pins"), 0);
}

#[test]
fn discard_is_refused_while_any_attempt_is_live() {
    let (_scratch, res) = open("draft-live-attempt");
    seed(&res);
    create(&res, "d1", DraftSource::New);
    let mut routed = input("r1");
    routed.identity_id = Some("ident-1".to_owned());
    save(&res, "d1", None, routed);

    // The route snapshot is the identity's account at save (spec/02).
    let revision = one(&res, "d1").revision.unwrap();
    assert_eq!(revision.account_id.as_deref(), Some("acct-1"));
    assert_eq!(revision.identity_id.as_deref(), Some("ident-1"));

    // A live attempt reports on both queries and refuses the discard.
    plant_attempt(&res, "attempt-1", "d1", "r1", "held");
    let held = LiveAttempt {
        attempt_id: "attempt-1".to_owned(),
        state: AttemptState::Held,
    };
    assert_eq!(one(&res, "d1").attempt, Some(held.clone()));
    let id = "d1".to_owned();
    let error = write_err(&res, move |tx| discard_draft(tx, &id, now()));
    assert!(
        matches!(&error, StoreError::Conflict { detail } if detail.contains("held")),
        "{error:?}"
    );

    // A live attempt never blocks a save (spec/02).
    let mut second = input("r2");
    second.identity_id = Some("ident-1".to_owned());
    save(&res, "d1", Some("r1"), second);
    plant_attempt(&res, "attempt-2", "d1", "r2", "queued");

    // The current revision's attempt is the reported one; the older
    // held attempt still refuses the discard.
    let queued = LiveAttempt {
        attempt_id: "attempt-2".to_owned(),
        state: AttemptState::Queued,
    };
    assert_eq!(one(&res, "d1").attempt, Some(queued.clone()));
    assert_eq!(res.read(drafts).unwrap()[0].attempt, Some(queued.clone()));
    let id = "d1".to_owned();
    let error = write_err(&res, move |tx| discard_draft(tx, &id, now()));
    assert!(
        matches!(&error, StoreError::Conflict { detail } if detail.contains("queued")),
        "{error:?}"
    );

    // A cancelled attempt is history: the report falls back to the
    // older live one, which still refuses.
    set_attempt(&res, "attempt-2", "cancelled", None);
    assert_eq!(one(&res, "d1").attempt, Some(held.clone()));
    let id = "d1".to_owned();
    let error = write_err(&res, move |tx| discard_draft(tx, &id, now()));
    assert!(
        matches!(&error, StoreError::Conflict { detail } if detail.contains("held")),
        "{error:?}"
    );

    // `submitting` and unresolved `outcome_unknown` are live (05).
    for state in ["submitting", "outcome_unknown"] {
        set_attempt(&res, "attempt-1", state, None);
        let id = "d1".to_owned();
        let error = write_err(&res, move |tx| discard_draft(tx, &id, now()));
        assert!(
            matches!(&error, StoreError::Conflict { detail } if detail.contains(state)),
            "{state}: {error:?}"
        );
    }
    set_attempt(&res, "attempt-1", "outcome_unknown", Some("marked_sent"));
    let id = "d1".to_owned();
    write(&res, move |tx| discard_draft(tx, &id, now()));
    assert_eq!(one(&res, "d1").state, DraftState::Discarded);
    // The revisions are kept; the draft just leaves the live list.
    assert_eq!(one(&res, "d1").revision.unwrap().revision_id, "r2");
    assert!(res.read(drafts).unwrap().is_empty());

    // Terminal states refuse both directions (spec/05's transitions).
    let id = "d1".to_owned();
    let error = write_err(&res, move |tx| discard_draft(tx, &id, now()));
    assert!(matches!(error, StoreError::Conflict { .. }), "{error:?}");
    let id = "d1".to_owned();
    let error = write_err(&res, move |tx| {
        save_draft(tx, &id, Some("r2"), input("r3"), now())
    });
    assert!(matches!(error, StoreError::Conflict { .. }), "{error:?}");
}

#[test]
fn a_reply_or_forward_resolves_its_source_occurrence() {
    let (_scratch, res) = open("draft-context");
    seed(&res);
    create(&res, "d-reply", DraftSource::Reply { occurrence_id: 11 });
    assert_eq!(one(&res, "d-reply").reply_to_occurrence_id, Some(11));
    create(
        &res,
        "d-reply-all",
        DraftSource::ReplyAll { occurrence_id: 11 },
    );
    assert_eq!(one(&res, "d-reply-all").reply_to_occurrence_id, Some(11));
    create(
        &res,
        "d-forward",
        DraftSource::Forward { occurrence_id: 11 },
    );
    let forward = one(&res, "d-forward");
    assert_eq!(forward.reply_to_occurrence_id, None);
    assert_eq!(forward.forward_of_message_id, Some(9));

    for source in [
        DraftSource::Reply { occurrence_id: 404 },
        DraftSource::ReplyAll { occurrence_id: 404 },
        DraftSource::Forward { occurrence_id: 404 },
    ] {
        let error = write_err(&res, move |tx| create_draft(tx, "d-404", &source, now()));
        assert!(
            matches!(&error, StoreError::NotFound { what } if what == "occurrence 404"),
            "{error:?}"
        );
    }
}
