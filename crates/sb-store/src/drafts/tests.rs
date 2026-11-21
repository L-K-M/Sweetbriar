//! Draft store tests: the writes and reads over a scratch store at
//! the layer below the engine's commands: exact rows, conflict
//! boundaries and stored shapes.

use rusqlite::Connection;
use sb_test_support::block_on;

use super::*;
use crate::testing::{reading, scratch_paths, test_seams, writing};
use crate::{ObjectStore, Store, StorePaths, WritePriority, open};

/// A revision save's content, at store level: ids are the caller's.
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

fn scratch() -> (sb_test_support::ScratchDir, StorePaths, Store) {
    let (scratch, paths) = scratch_paths("drafts");
    let store = open(&paths, &test_seams(sb_test_support::real_fs())).unwrap();
    (scratch, paths, store)
}

#[test]
fn create_save_and_read_back() {
    let (_scratch, _paths, store) = scratch();
    let now = crate::Timestamp::from_millis(1_735_689_600_000);
    writing(&store, move |tx| {
        create_draft(tx, "d1", &DraftSource::New, now)
    });
    writing(&store, move |tx| {
        save_draft(tx, "d1", None, input("r1"), now)
    });

    let draft = reading(&store, |tx| draft(tx, "d1")).unwrap();
    assert_eq!(draft.state, DraftState::Editing);
    let revision = draft.revision.unwrap();
    assert_eq!((revision.seq, revision.revision_id.as_str()), (1, "r1"));
    assert_eq!(revision.subject, "a subject");
    assert_eq!(revision.to[0].name.as_deref(), Some("To"));

    // The recipients landed as canonical JSON (spec/02 rule 7).
    let stored = reading(&store, |tx| {
        tx.query_row(
            "SELECT to_json FROM draft_revisions WHERE revision_id = 'r1'",
            [],
            |row| row.get::<_, String>(0),
        )
        .map_err(|e| classify(e, "read the stored recipients"))
    });
    assert_eq!(stored, r#"[{"address":"to@example.test","name":"To"}]"#);
    store.close().unwrap();
}

#[test]
fn a_spent_or_missing_attachment_reservation_conflicts() {
    let (_scratch, _paths, store) = scratch();
    let now = crate::Timestamp::from_millis(1_735_689_600_000);
    writing(&store, move |tx| {
        create_draft(tx, "d1", &DraftSource::New, now)
    });

    // Spent: this store's own reservation, already released, cannot
    // pin a new reference (spec/03). Releasing it the way the
    // referencing commit would leaves the pin exactly as a spent
    // publication leaves it.
    let published = store
        .objects()
        .publish(std::io::Cursor::new(b"spent".as_slice()))
        .unwrap();
    let published = writing(&store, move |tx| {
        release_reservation(tx, &published, ReservationRelease::Required)?;
        Ok(published)
    });
    let mut with_object = input("r1");
    with_object.attachments = vec![DraftAttachment::New {
        object: std::sync::Arc::new(published),
        filename: "one.txt".to_owned(),
        media_type: "text/plain".to_owned(),
        content_id: None,
    }];
    let error = block_on(store.write(WritePriority::User, move |tx| {
        save_draft(tx, "d1", None, with_object, now)
    }))
    .unwrap_err();
    assert!(matches!(error, StoreError::Conflict { .. }), "{error:?}");

    // Missing: a `PublishedObject` minted by another store carries a
    // reservation this store never made — even with the same bytes
    // published and pinned here, the foreign pin cannot be referenced.
    let (_foreign_scratch, _foreign_paths, foreign) = scratch();
    let foreign_object = foreign
        .objects()
        .publish(std::io::Cursor::new(b"missing".as_slice()))
        .unwrap();
    let _local = store
        .objects()
        .publish(std::io::Cursor::new(b"missing".as_slice()))
        .unwrap();
    let mut with_foreign = input("r2");
    with_foreign.attachments = vec![DraftAttachment::New {
        object: std::sync::Arc::new(foreign_object),
        filename: "two.txt".to_owned(),
        media_type: "text/plain".to_owned(),
        content_id: None,
    }];
    let error = block_on(store.write(WritePriority::User, move |tx| {
        save_draft(tx, "d1", None, with_foreign, now)
    }))
    .unwrap_err();
    assert!(matches!(error, StoreError::Conflict { .. }), "{error:?}");
    assert!(
        reading(&store, |tx| draft(tx, "d1"))
            .unwrap()
            .revision
            .is_none()
    );
    foreign.close().unwrap();
    store.close().unwrap();
}

/// The deferred foreign key resolves at commit: the revision insert
/// and the `current_revision_id` update are one transaction (spec/02).
#[test]
fn the_revision_and_the_current_pointer_commit_together() {
    let (_scratch, paths, store) = scratch();
    let now = crate::Timestamp::from_millis(1_735_689_600_000);
    writing(&store, move |tx| {
        create_draft(tx, "d1", &DraftSource::New, now)
    });
    writing(&store, move |tx| {
        save_draft(tx, "d1", None, input("r1"), now)
    });

    // Foreign keys hold at every commit boundary.
    let conn = Connection::open(paths.state_db()).unwrap();
    let dangling: Vec<String> = conn
        .prepare("PRAGMA foreign_key_check")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(dangling.is_empty(), "{dangling:?}");
    store.close().unwrap();
}

/// The M4.4e2 Content-ID is stored only when set: an ordinary entry's
/// canonical JSON is byte-identical to the older shape.
#[test]
fn stored_json_omits_content_id_when_absent() {
    let (_scratch, _paths, store) = scratch();
    let now = crate::Timestamp::from_millis(1_735_689_600_000);
    writing(&store, move |tx| {
        create_draft(tx, "d1", &DraftSource::New, now)
    });
    let published = store
        .objects()
        .publish(std::io::Cursor::new(b"image".as_slice()))
        .unwrap();
    let sha = published.id().sha256_hex();
    let size = published.id().byte_length();
    let mut content = input("r1");
    content.attachments = vec![DraftAttachment::New {
        object: std::sync::Arc::new(published),
        filename: "plain.txt".to_owned(),
        media_type: "text/plain".to_owned(),
        content_id: None,
    }];
    writing(&store, move |tx| save_draft(tx, "d1", None, content, now));

    let stored = reading(&store, |tx| {
        tx.query_row(
            "SELECT attachments_json FROM draft_revisions WHERE revision_id = 'r1'",
            [],
            |row| row.get::<_, String>(0),
        )
        .map_err(|e| classify(e, "read the stored attachments"))
    });
    assert_eq!(
        stored,
        format!(
            r#"[{{"filename":"plain.txt","media_type":"text/plain","object_sha256":"{sha}","size":{size}}}]"#
        )
    );
    let revision = reading(&store, |tx| draft(tx, "d1"))
        .unwrap()
        .revision
        .unwrap();
    assert_eq!(revision.attachments[0].content_id, None);
    store.close().unwrap();
}

/// A stored Content-ID reads back on the reference, bare.
#[test]
fn stored_content_id_round_trips() {
    let (_scratch, _paths, store) = scratch();
    let now = crate::Timestamp::from_millis(1_735_689_600_000);
    writing(&store, move |tx| {
        create_draft(tx, "d1", &DraftSource::New, now)
    });
    let published = store
        .objects()
        .publish(std::io::Cursor::new(b"image".as_slice()))
        .unwrap();
    let content_id = "0f8fad5b-d9cb-469f-a165-70867728950e@sweetbriar.invalid";
    let mut content = input("r1");
    content.attachments = vec![DraftAttachment::New {
        object: std::sync::Arc::new(published),
        filename: "chart.png".to_owned(),
        media_type: "image/png".to_owned(),
        content_id: Some(content_id.to_owned()),
    }];
    writing(&store, move |tx| save_draft(tx, "d1", None, content, now));

    let revision = reading(&store, |tx| draft(tx, "d1"))
        .unwrap()
        .revision
        .unwrap();
    assert_eq!(
        revision.attachments[0].content_id.as_deref(),
        Some(content_id)
    );
    store.close().unwrap();
}

/// A `content_id` that is not a string, null included, is stored
/// damage: the writer omits an absent one.
#[test]
fn a_non_string_content_id_is_corrupt() {
    for value in ["5", "null"] {
        let json = format!(
            r#"[{{"object_sha256":"aa","filename":"f","media_type":"text/plain","size":1,"content_id":{value}}}]"#
        );
        let error = parse_attachments(&json).unwrap_err();
        assert!(
            matches!(error, StoreError::Corrupt { .. }),
            "{value}: {error:?}"
        );
    }
}

/// Republication matches the four identity fields and ignores the
/// Content-ID, so a retained attachment may change it (spec/07).
#[test]
fn republish_ignores_a_changed_content_id() {
    let (_scratch, _paths, store) = scratch();
    let now = crate::Timestamp::from_millis(1_735_689_600_000);
    writing(&store, move |tx| {
        create_draft(tx, "d1", &DraftSource::New, now)
    });
    let published = store
        .objects()
        .publish(std::io::Cursor::new(b"image".as_slice()))
        .unwrap();
    let object_id = *published.id();
    let mut content = input("r1");
    content.attachments = vec![DraftAttachment::New {
        object: std::sync::Arc::new(published),
        filename: "chart.png".to_owned(),
        media_type: "image/png".to_owned(),
        content_id: Some("a@b".to_owned()),
    }];
    writing(&store, move |tx| save_draft(tx, "d1", None, content, now));

    let stored = reading(&store, |tx| draft(tx, "d1"))
        .unwrap()
        .revision
        .unwrap()
        .attachments[0]
        .clone();
    for content_id in [Some("c@d".to_owned()), None] {
        let mut changed = stored.clone();
        changed.content_id = content_id;
        let source = reading(&store, |tx| attachment_source(tx, "d1", "r1", &changed));
        assert_eq!(source, object_id);
    }
    store.close().unwrap();
}
