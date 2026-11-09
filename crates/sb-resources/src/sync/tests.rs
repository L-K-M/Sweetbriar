//! The header mapping (spec/04 step 3, spec/02 `message_headers`): a
//! fetched header-fields literal through the MIME leaf onto the
//! columns, in the import path's shapes, then one batch through the
//! store so the lists show the header-only row.

use sb_store::accounts::{
    EndpointSecurity, IdentitySendable, NewBridgeAccount, NewIdentity, NewImapEndpoint,
    insert_bridge_account,
};
use sb_store::sidebar::MailboxKind;
use sb_test_support::{ScratchDir, block_on};

use super::*;
use crate::{MailboxRole, Resources, Timestamp, WritePriority};

/// Spec/04 step 3's fetched fields, as Bridge might answer them.
const LITERAL: &[u8] = b"Date: Mon, 02 Nov 2026 09:15:00 +0000\r\n\
From: Owner Name <owner@example.org>\r\n\
Reply-To: replies@example.org\r\n\
To: Alice <alice@example.org>, bob@example.org\r\n\
Cc: carol@example.org\r\n\
Subject: Quarterly\r\n numbers\r\n\
Message-ID: <q4@example.org>\r\n\
In-Reply-To: <q3@example.org>\r\n\
References: <q2@example.org> <q3@example.org>\r\n\
List-Id: Team <team.example.org>\r\n\
Delivered-To: owner@example.org\r\n\
X-Original-To: alias@example.org\r\n\
X-Pm-Internal-Id: pm-42\r\n\
\r\n";

#[test]
fn a_header_literal_maps_onto_the_columns() {
    let columns = header_columns(LITERAL);

    assert_eq!(columns.header_message_id.as_deref(), Some("q4@example.org"));
    assert_eq!(
        columns.subject.as_deref(),
        Some("Quarterly numbers"),
        "unfolded"
    );
    assert_eq!(
        columns.from_json.as_deref(),
        Some(r#"[{"address":"owner@example.org","name":"Owner Name"}]"#)
    );
    assert_eq!(
        columns.to_json.as_deref(),
        Some(
            r#"[{"address":"alice@example.org","name":"Alice"},{"address":"bob@example.org","name":null}]"#
        )
    );
    assert_eq!(
        columns.delivered_to_json.as_deref(),
        Some(r#"["owner@example.org","alias@example.org"]"#),
        "Delivered-To and X-Original-To, in header order"
    );
    assert!(columns.references_json.unwrap().contains("q2@example.org"));
    assert!(columns.date_header.is_some());
    assert!(columns.list_id.is_some());
}

#[test]
fn a_literal_without_its_blank_line_maps_the_same() {
    let trimmed = LITERAL.strip_suffix(b"\r\n").unwrap();
    assert_eq!(header_columns(trimmed), header_columns(LITERAL));
    let bare = LITERAL.strip_suffix(b"\r\n\r\n").unwrap();
    assert_eq!(header_columns(bare), header_columns(LITERAL));
}

#[test]
fn an_empty_literal_maps_to_no_columns() {
    let columns = header_columns(b"");
    assert_eq!(columns.subject, None);
    assert_eq!(columns.from_json, None);
    assert_eq!(columns.delivered_to_json, None);
}

#[test]
fn a_mapped_batch_shows_in_the_lists() {
    let scratch = ScratchDir::new("sync-header-columns");
    let res = Resources::open(&scratch.path().join("store")).unwrap();
    let now = Timestamp::from_millis(1_735_689_600_000);
    block_on(res.write(WritePriority::User, move |tx| {
        insert_bridge_account(tx, &bridge_account(), now)
    }))
    .unwrap();
    let inbox = block_on(res.write(WritePriority::User, |tx| {
        reconcile_mailboxes(
            tx,
            "acct",
            &[ObservedMailbox {
                remote_name: "INBOX".to_owned(),
                delimiter: Some('/'),
                kind: MailboxKind::Folder,
                role: Some(MailboxRole::Inbox),
                selectable: true,
            }],
        )
        .map(|reconciled| reconciled.added[0])
    }))
    .unwrap();
    block_on(res.write(WritePriority::User, move |tx| {
        check_epoch(tx, "acct", inbox, 7)
    }))
    .unwrap();

    let header = SyncedHeader {
        uid: 1,
        flags: SyncedFlags::default(),
        internal_date: None,
        rfc822_size: Some(4096),
        provider_key: Some("pm-42".to_owned()),
        raw_fields: LITERAL.to_vec(),
        columns: header_columns(LITERAL),
    };
    block_on(res.write(WritePriority::Sync, move |tx| {
        commit_header_batch(
            tx,
            &HeaderBatch {
                account_id: "acct",
                mailbox_id: inbox,
                uidvalidity: 7,
                expected_uidnext: None,
                next_uidnext: 2,
                caught_up: true,
                headers: &[header],
            },
            now,
        )
    }))
    .unwrap();

    let (sender, subject, unread): (String, String, i64) = res
        .read(|tx| {
            Ok(tx
                .query_row(
                    "SELECT sender, subject, unread FROM d_list_rows WHERE mailbox_id = ?1",
                    [inbox],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap())
        })
        .unwrap();
    assert_eq!(
        (sender.as_str(), subject.as_str(), unread),
        ("Owner Name", "Quarterly numbers", 1)
    );
}

fn bridge_account() -> NewBridgeAccount {
    NewBridgeAccount {
        account_id: "acct".to_owned(),
        display_name: "Bridge".to_owned(),
        endpoint: NewImapEndpoint {
            imap_port: 1143,
            imap_security: EndpointSecurity::StartTls,
            smtp_port: 1025,
            smtp_security: EndpointSecurity::StartTls,
            username: "owner@example.org".to_owned(),
            credential_ref: "account/acct/bridge-password".to_owned(),
            trusted_cert_sha256: "00".repeat(32),
            address_mode: None,
            capabilities_json: "{}".to_owned(),
        },
        identities: vec![NewIdentity {
            identity_id: "identity-acct".to_owned(),
            email: "owner@example.org".to_owned(),
            display_name: "Owner".to_owned(),
            is_default: true,
            sendable: IdentitySendable::Provisional,
        }],
    }
}
