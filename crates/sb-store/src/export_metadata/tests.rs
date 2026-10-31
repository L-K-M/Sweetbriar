use super::*;
use crate::testing::fixtures::{archive_account, index_connection, mailbox, message};

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
