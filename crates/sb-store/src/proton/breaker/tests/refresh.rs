//! A Refresh's listing and the breaker (plan M2.15c3, spec/04-sync-
//! proton "Refresh" steps 3 and 4): what one complete listing missed
//! waits, withheld, until a second listing or a delete event confirms
//! it; a listing missing too much opens one `epoch_change`.

use super::*;
use crate::proton::withhold_unlisted;

/// Run a complete listing at `cursor` of `messages`, in pages of
/// `page_size`, each page its own write as after a crash.
fn relist_in_pages(
    store: &Store,
    cursor: &str,
    messages: Vec<(String, &'static str)>,
    page_size: usize,
) {
    let cursor = cursor.to_owned();
    let mut revision = write(store, move |tx| {
        let revision = proton_account(tx, ACCOUNT)?.unwrap().revision;
        let account = begin_listing(tx, ACCOUNT, &cursor, revision)?;
        reconcile_proton_labels(tx, ACCOUNT, &labels())?;
        Ok(account.revision)
    })
    .unwrap();
    for (n, page) in messages.chunks(page_size).enumerate() {
        let page: Vec<ListedMessage> = page.iter().map(|(id, at)| message(id, at)).collect();
        revision = write(store, move |tx| {
            commit_listed_page(
                tx,
                &ListedPage {
                    account_id: ACCOUNT,
                    messages: &page,
                    checkpoint: &format!("p{n}"),
                    expected_revision: revision,
                },
                now(),
            )?;
            Ok(proton_account(tx, ACCOUNT)?.unwrap().revision)
        })
        .unwrap();
    }
    while write(store, move |tx| withhold_unlisted(tx, ACCOUNT, revision)).unwrap() > 0 {}
    write(store, move |tx| {
        finish_listing(tx, ACCOUNT, &[], revision, now()).map(drop)
    })
    .unwrap();
}

fn relist(store: &Store, cursor: &str, messages: Vec<(String, &'static str)>) {
    relist_in_pages(store, cursor, messages, usize::MAX);
}

/// m1 to m10 in Inbox but those `missing`.
fn inbox_without(missing: &[&str]) -> Vec<(String, &'static str)> {
    ids("m", 10, "0")
        .into_iter()
        .filter(|(id, _)| !missing.contains(&id.as_str()))
        .collect()
}

#[test]
fn what_one_listing_missed_waits_until_a_second_listing_confirms_it() {
    let (_scratch, store) = listed("proton-refresh-twice", ids("m", 10, "0"));

    relist(&store, "r1", inbox_without(&["m1", "m2"]));
    assert_eq!(states(&store, "m1"), pending());
    assert_eq!(states(&store, "m2"), pending());
    assert!(events(&store).is_empty());
    // An events response doesn't classify what only a listing missed.
    let applied = apply(&store, Vec::new(), true);
    assert_eq!(applied.breaker, Classified::default());
    assert_eq!(states(&store, "m1"), pending());

    // m2 is listed again; m1 is missed a second time and applies.
    relist(&store, "r2", inbox_without(&["m1"]));
    assert_eq!(states(&store, "m1"), vanished());
    assert_eq!(states(&store, "m2"), vec!["present"; 2]);
    assert_eq!(recorded(&store), 0);
    assert!(events(&store).is_empty());
}

#[test]
fn a_delete_event_confirms_what_a_listing_missed() {
    let (_scratch, store) = listed("proton-refresh-delete", ids("m", 10, "0"));
    relist(&store, "r1", inbox_without(&["m1"]));

    let applied = apply(&store, deletes(&["m1"]), true);
    assert_eq!((applied.withheld, applied.breaker.applied), (0, 1));
    assert_eq!(states(&store, "m1"), vanished());
    assert_eq!(recorded(&store), 0);
}

#[test]
fn a_listing_missing_too_much_opens_one_epoch_change() {
    let mut messages = ids("m", 10, "0");
    messages.extend(ids("a", 2, "6"));
    let (_scratch, store) = listed("proton-refresh-epoch", messages);

    // Six of ten Inbox messages and one of two in Archive: one event
    // holds all seven, and the account pauses.
    let mut kept = inbox_without(&["m1", "m2", "m3", "m4", "m5", "m6"]);
    kept.push(("a2".to_owned(), "6"));
    relist(&store, "r1", kept);
    let event_id = format!("proton-{ACCOUNT}-1");
    assert_eq!(
        events(&store),
        [(
            event_id.clone(),
            "epoch_change".to_owned(),
            "open".to_owned(),
            r#"{"disappeared":7,"mailbox_messages":null}"#.to_owned()
        )]
    );
    assert_eq!(states(&store, "a1"), pending());
    let paused = apply(&store, deletes(&["m7"]), true);
    assert_eq!(paused.breaker, Classified::default());

    // Accept applies the seven, then the delete the pause held.
    let resolved = resolve(&store, &event_id, BreakerResolution::Accept).unwrap();
    assert_eq!(resolved.applied, 1);
    for id in ["m1", "m6", "a1", "m7"] {
        assert_eq!(states(&store, id), vanished(), "{id}");
    }
    assert_eq!(recorded(&store), 0);
}

#[test]
fn a_kept_epoch_change_holds_its_messages_through_later_listings() {
    let (_scratch, store) = listed("proton-refresh-kept", ids("m", 10, "0"));
    relist(
        &store,
        "r1",
        inbox_without(&["m1", "m2", "m3", "m4", "m5", "m6"]),
    );
    let event_id = format!("proton-{ACCOUNT}-1");
    resolve(&store, &event_id, BreakerResolution::Keep).unwrap();

    // Missed again, still held by the kept event; m1 returns.
    relist(&store, "r2", inbox_without(&["m2", "m3", "m4", "m5", "m6"]));
    assert_eq!(states(&store, "m2"), pending());
    assert_eq!(states(&store, "m1"), vec!["present"; 2]);
    assert_eq!(events(&store).len(), 1);
    assert_eq!(recorded(&store), 5);
}

#[test]
fn trash_and_spam_misses_open_no_epoch_change() {
    let mut messages = ids("t", 3, "3");
    messages.push(("m1".to_owned(), "0"));
    let (_scratch, store) = listed("proton-refresh-trash", messages);

    // Trash emptied by Proton: withheld, but no event.
    relist(&store, "r1", ids("m", 1, "0"));
    assert_eq!(states(&store, "t1"), pending());
    assert!(events(&store).is_empty());
    relist(&store, "r2", ids("m", 1, "0"));
    assert_eq!(states(&store, "t3"), vanished());
}

#[test]
fn a_listing_finishes_only_once_its_misses_are_withheld() {
    let (_scratch, store) = listed("proton-refresh-finish", ids("m", 10, "0"));
    let revision = write(&store, |tx| {
        let revision = proton_account(tx, ACCOUNT)?.unwrap().revision;
        Ok(begin_listing(tx, ACCOUNT, "r1", revision)?.revision)
    })
    .unwrap();
    let page: Vec<ListedMessage> = inbox_without(&["m1", "m2"])
        .iter()
        .map(|(id, at)| message(id, at))
        .collect();
    let revision = write(&store, move |tx| {
        commit_listed_page(
            tx,
            &ListedPage {
                account_id: ACCOUNT,
                messages: &page,
                checkpoint: "p0",
                expected_revision: revision,
            },
            now(),
        )?;
        Ok(proton_account(tx, ACCOUNT)?.unwrap().revision)
    })
    .unwrap();

    assert!(matches!(
        write(&store, move |tx| finish_listing(
            tx,
            ACCOUNT,
            &[],
            revision,
            now()
        )),
        Err(StoreError::Conflict { .. })
    ));
    assert_eq!(
        write(&store, move |tx| withhold_unlisted(tx, ACCOUNT, revision)).unwrap(),
        2
    );
    assert_eq!(
        write(&store, move |tx| withhold_unlisted(tx, ACCOUNT, revision)).unwrap(),
        0
    );
    write(&store, move |tx| {
        finish_listing(tx, ACCOUNT, &[], revision, now()).map(drop)
    })
    .unwrap();
    assert_eq!(states(&store, "m1"), pending());
    // Not listing any more.
    assert!(matches!(
        write(&store, move |tx| withhold_unlisted(
            tx,
            ACCOUNT,
            revision + 1
        )),
        Err(StoreError::Conflict { .. })
    ));
}

#[test]
fn a_listing_resumed_page_by_page_misses_nothing_it_listed() {
    let (_scratch, store) = listed("proton-refresh-resume", ids("m", 10, "0"));
    relist_in_pages(&store, "r1", ids("m", 10, "0"), 3);
    assert_eq!(recorded(&store), 0);
    assert_eq!(states(&store, "m10"), vec!["present"; 2]);
}

#[test]
fn misses_past_the_write_budget_are_withheld_over_several_writes() {
    let (_scratch, store) = listed("proton-refresh-budget", ids("m", 250, "0"));
    // Long list rows: a full batch of misses doesn't fit one write.
    write(&store, |tx| {
        tx.execute(
            "UPDATE d_list_rows SET subject = ?1 WHERE message_id IN
             (SELECT message_id FROM logical_messages
              WHERE CAST(substr(provider_key, 2) AS INTEGER) <= 200)",
            params!["s".repeat(8 * 1024)],
        )
        .unwrap();
        Ok(())
    })
    .unwrap();

    let revision = write(&store, |tx| {
        let revision = proton_account(tx, ACCOUNT)?.unwrap().revision;
        let account = begin_listing(tx, ACCOUNT, "r1", revision)?;
        reconcile_proton_labels(tx, ACCOUNT, &labels())?;
        let page: Vec<ListedMessage> = ids("m", 250, "0")[200..]
            .iter()
            .map(|(id, at)| message(id, at))
            .collect();
        commit_listed_page(
            tx,
            &ListedPage {
                account_id: ACCOUNT,
                messages: &page,
                checkpoint: "p0",
                expected_revision: account.revision,
            },
            now(),
        )?;
        Ok(proton_account(tx, ACCOUNT)?.unwrap().revision)
    })
    .unwrap();
    let first = write(&store, move |tx| withhold_unlisted(tx, ACCOUNT, revision)).unwrap();
    assert!(0 < first && first < 200, "withheld {first}");
    let mut withheld = first;
    loop {
        let more = write(&store, move |tx| withhold_unlisted(tx, ACCOUNT, revision)).unwrap();
        if more == 0 {
            break;
        }
        withheld += more;
    }
    assert_eq!(withheld, 200);
    write(&store, move |tx| {
        finish_listing(tx, ACCOUNT, &[], revision, now()).map(drop)
    })
    .unwrap();
    assert_eq!(states(&store, "m1"), pending());
    assert_eq!(states(&store, "m200"), pending());
    assert_eq!(states(&store, "m201"), vec!["present"; 2]);
}
