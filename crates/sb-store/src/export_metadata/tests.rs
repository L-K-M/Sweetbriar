use super::*;
use crate::testing::fixtures::{archive_account, index_connection, mailbox, message};

#[test]
fn shared_acquisition_decoders_preserve_corruption_classification() {
    for mutation in [
        "UPDATE acquisitions SET content_sha256=CAST(x'ff' AS TEXT) WHERE acquisition_id=1",
        "UPDATE acquisitions SET object_state=CAST(x'ff' AS TEXT) WHERE acquisition_id=1",
    ] {
        let mut conn = index_connection();
        crate::testing::fixtures::acquisitions(&conn, 1);
        let account = archive_account(&conn);
        let id = message(&conn, &account, Some(1));
        conn.execute_batch("PRAGMA ignore_check_constraints=ON")
            .unwrap();
        conn.execute_batch(mutation).unwrap();
        conn.execute_batch("PRAGMA ignore_check_constraints=OFF")
            .unwrap();
        let tx = conn.transaction().unwrap();
        assert!(
            matches!(
                messages::best_acquisition(&tx, id),
                Err(StoreError::Corrupt { .. })
            ),
            "{mutation}"
        );
        assert!(
            matches!(
                messages::index_acquisition(&tx, 1),
                Err(StoreError::Corrupt { .. })
            ),
            "{mutation}"
        );
    }

    let mut conn = index_connection();
    let account = archive_account(&conn);
    let id = message(&conn, &account, None);
    conn.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
    conn.execute(
        "UPDATE logical_messages SET acquisition_id=99 WHERE message_id=?1",
        [id],
    )
    .unwrap();
    conn.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    let tx = conn.transaction().unwrap();
    assert!(matches!(
        messages::best_acquisition(&tx, id),
        Err(StoreError::Corrupt { .. })
    ));
}

#[test]
fn broken_references_cannot_mix_accounts_or_silently_drop_metadata() {
    for mutation in [
        "UPDATE occurrences SET mailbox_id=2 WHERE occurrence_id=1",
        "UPDATE occurrences SET message_id=2 WHERE occurrence_id=1",
        "UPDATE occurrences SET message_id=99 WHERE occurrence_id=1",
        "UPDATE logical_messages SET acquisition_id=99 WHERE message_id=1",
        "INSERT INTO message_tags (message_id,tag_id,added_at) VALUES (1,'missing',1)",
    ] {
        let mut conn = index_connection();
        let account = archive_account(&conn);
        conn.execute("INSERT INTO accounts (account_id,kind,provider,display_name,state,sort_order,created_at)
                      VALUES ('other','imap','generic_imap','Synthetic','active',1,1)", []).unwrap();
        assert_eq!(mailbox(&conn, &account, "Inbox", "folder", None), 1);
        assert_eq!(mailbox(&conn, "other", "Other", "folder", None), 2);
        assert_eq!(message(&conn, &account, None), 1);
        assert_eq!(message(&conn, "other", None), 2);
        conn.execute("INSERT INTO occurrences
                      (occurrence_id,account_id,mailbox_id,message_id,state,first_seen_at,last_seen_at)
                      VALUES (1,?1,1,1,'present',1,1)", [&account]).unwrap();
        conn.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
        conn.execute_batch(mutation).unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON").unwrap();
        let tx = conn.transaction().unwrap();
        let Err(error) = export_occurrence_metadata(&tx, 1) else {
            panic!("accepted {mutation}");
        };
        assert!(
            matches!(error, StoreError::Corrupt { .. }),
            "{mutation}: {error}"
        );
    }
}
