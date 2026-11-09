//! The pass's pure rules: flag mapping and checkpoint arithmetic
//! (spec/04 step 3). The pass itself runs over the fake Bridge in
//! `account_sync`'s tests.

use super::*;

fn checkpoint(stored: Option<u32>, server_next: Option<u32>) -> Checkpoint<'static> {
    Checkpoint {
        account_id: "a",
        mailbox_id: 1,
        uidvalidity: 4100,
        stored,
        server_next,
    }
}

#[test]
fn system_flags_fill_columns_and_the_rest_are_keywords() {
    let flags = [
        "\\Seen",
        "\\FLAGGED",
        "\\answered",
        "\\Draft",
        "\\Deleted",
        "\\Recent",
        "$Label1",
        "NonJunk",
    ]
    .map(str::to_owned);
    assert_eq!(
        synced_flags(&flags),
        SyncedFlags {
            seen: true,
            flagged: true,
            answered: true,
            draft: true,
            deleted: true,
            keywords: vec!["$Label1".to_owned(), "NonJunk".to_owned()],
        }
    );
    assert_eq!(synced_flags(&[]), SyncedFlags::default());
}

#[test]
fn the_checkpoint_rises_above_each_batch_and_reaches_the_server_last() {
    // A fresh mailbox starts at UID 1.
    assert_eq!(checkpoint(None, Some(10)).target(None, false), 1);
    // Above the batch's highest UID, the server's UIDNEXT only once
    // the pass reached it.
    assert_eq!(checkpoint(None, Some(10)).target(Some(4), false), 5);
    assert_eq!(checkpoint(None, Some(10)).target(Some(4), true), 10);
    // Backfilled UIDs below the stored checkpoint leave it in place.
    assert_eq!(checkpoint(Some(8), Some(8)).target(Some(3), true), 8);
    // A server without UIDNEXT: the highest UID decides.
    assert_eq!(checkpoint(Some(2), None).target(Some(6), true), 7);
    // Never below the stored checkpoint, even if the server reports less.
    assert_eq!(checkpoint(Some(9), Some(5)).target(None, true), 9);
}
