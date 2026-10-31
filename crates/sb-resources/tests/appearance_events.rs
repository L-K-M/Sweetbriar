//! Committed appearance revisions through the native Resources facade.

use std::future::Future;
use std::sync::{Arc, Mutex, mpsc};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use sb_resources::{
    AccountColor, Resources, StoreError, Transaction, WritePriority, account_appearance,
    appearance_generation, identity_appearance, initialize_account_appearance, list_generation,
    note_membership_change, run_once, search_generation, set_account_appearance,
    set_identity_appearance,
};
use sb_test_support::{ScratchDir, block_on};

const IDENTITY: &str = "appearance-identity";
const WAIT: Duration = Duration::from_secs(10);
const RESULT: &str = "{\"v\":1}";

fn resources(label: &str) -> (ScratchDir, Resources, String) {
    let scratch = ScratchDir::new(label);
    let res = Resources::open(&scratch.path().join("store")).unwrap();
    let account = res
        .read(|tx| {
            Ok(tx
                .query_row(
                    "SELECT account_id FROM accounts WHERE provider='local_archive'",
                    [],
                    |row| row.get(0),
                )
                .unwrap())
        })
        .unwrap();
    (scratch, res, account)
}

fn write<T: Send + 'static>(
    res: &Resources,
    f: impl FnOnce(&Transaction) -> Result<T, StoreError> + Send + 'static,
) -> Result<T, StoreError> {
    block_on(res.write(WritePriority::User, f))
}

fn generation(res: &Resources) -> u64 {
    res.read(appearance_generation).unwrap()
}

fn observe(res: &Resources) -> mpsc::Receiver<u64> {
    let (send, receive) = mpsc::channel();
    res.observe_appearance_generation(Arc::new(move |value| {
        let _ = send.send(value);
    }));
    receive
}

fn quiet(receiver: &mpsc::Receiver<u64>) {
    assert!(receiver.try_recv().is_err(), "unexpected appearance event");
}

fn identity(res: &Resources, account: &str) {
    let account = account.to_owned();
    write(res, move |tx| {
        tx.execute("INSERT INTO identities (identity_id,account_id,email,display_name,is_default,sendable) VALUES (?1,?2,'synthetic@example.test','Synthetic',0,'validated')", (IDENTITY, account)).unwrap();
        Ok(())
    }).unwrap();
}

fn change(res: &Resources, account: &str, revision: i64, color: &str) -> Result<u64, StoreError> {
    let (account, color) = (account.to_owned(), color.to_owned());
    write(res, move |tx| {
        set_account_appearance(tx, &account, revision, &AccountColor::Custom(color), None)?;
        appearance_generation(tx)
    })
}

#[test]
fn changed_records_advance_but_noops_stale_revisions_and_other_generations_do_not() {
    let (_scratch, res, account) = resources("appearance-events-values");
    identity(&res, &account);
    let other = res
        .read(|tx| Ok((list_generation(tx)?, search_generation(tx)?)))
        .unwrap();
    let receiver = observe(&res);
    assert_eq!(generation(&res), 0);
    let id = account.clone();
    write(&res, move |tx| initialize_account_appearance(tx, &id)).unwrap();
    assert_eq!(generation(&res), 0);
    quiet(&receiver);
    assert_eq!(change(&res, &account, 1, "#112233").unwrap(), 1);
    assert_eq!(receiver.recv_timeout(WAIT).unwrap(), 1);
    assert_eq!(change(&res, &account, 2, "#112233").unwrap(), 1);
    assert!(matches!(
        change(&res, &account, 1, "#112233"),
        Err(StoreError::Conflict { .. })
    ));
    quiet(&receiver);
    write(&res, |tx| {
        set_identity_appearance(tx, IDENTITY, None, None, None)
    })
    .unwrap();
    assert_eq!(receiver.recv_timeout(WAIT).unwrap(), 2);
    write(&res, |tx| {
        set_identity_appearance(tx, IDENTITY, Some(1), None, None)
    })
    .unwrap();
    quiet(&receiver);
    write(&res, |tx| {
        set_identity_appearance(tx, IDENTITY, Some(1), Some("#445566"), None)
    })
    .unwrap();
    assert_eq!(receiver.recv_timeout(WAIT).unwrap(), 3);
    assert_eq!(
        res.read(|tx| Ok((list_generation(tx)?, search_generation(tx)?)))
            .unwrap(),
        other
    );
    assert_eq!(generation(&res), 3);
}

#[test]
fn new_account_initialization_is_atomic_and_notifies_once() {
    let (_scratch, res, _) = resources("appearance-events-initialize");
    let receiver = observe(&res);
    write(&res, |tx| {
        tx.execute("INSERT INTO accounts (account_id,kind,provider,display_name,state,sort_order,created_at) VALUES ('new','imap','generic_imap','Synthetic','active',0,1)", []).unwrap();
        initialize_account_appearance(tx, "new")?;
        initialize_account_appearance(tx, "new")?;
        Ok(())
    }).unwrap();
    assert_eq!(receiver.recv_timeout(WAIT).unwrap(), 1);
    assert_eq!(generation(&res), 1);
    quiet(&receiver);
}

#[test]
fn multiple_changes_announce_after_commit_but_before_future_resolution() {
    let (_scratch, res, account) = resources("appearance-events-order");
    identity(&res, &account);
    let (entered, observed) = mpsc::channel();
    let (release, permit) = mpsc::channel();
    let permit = Mutex::new(permit);
    // A bounded test barrier proves ordering; production observers never wait.
    res.observe_appearance_generation(Arc::new(move |value| {
        let _ = entered.send(value);
        let _ = permit.lock().unwrap().recv_timeout(WAIT);
    }));
    let id = account.clone();
    let mut future = Box::pin(res.write(WritePriority::User, move |tx| {
        set_account_appearance(tx, &id, 1, &AccountColor::Custom("#112233".into()), None)?;
        set_identity_appearance(tx, IDENTITY, None, Some("#445566"), None)?;
        appearance_generation(tx)
    }));
    assert_eq!(observed.recv_timeout(WAIT).unwrap(), 2);
    assert_eq!(generation(&res), 2);
    assert_eq!(
        res.read(|tx| account_appearance(tx, &account))
            .unwrap()
            .color_srgb(),
        "#112233"
    );
    assert_eq!(
        res.read(|tx| identity_appearance(tx, IDENTITY))
            .unwrap()
            .color_override_srgb(),
        Some("#445566")
    );
    let mut context = Context::from_waker(Waker::noop());
    assert!(matches!(future.as_mut().poll(&mut context), Poll::Pending));
    release.send(()).unwrap();
    assert_eq!(block_on(future).unwrap(), 2);
    quiet(&observed);
}

#[test]
fn a_dropped_write_future_still_commits_and_announces() {
    let (_scratch, res, account) = resources("appearance-events-drop");
    let receiver = observe(&res);
    let id = account.clone();
    drop(res.write(WritePriority::User, move |tx| {
        set_account_appearance(tx, &id, 1, &AccountColor::Custom("#112233".into()), None)
    }));
    assert_eq!(receiver.recv_timeout(WAIT).unwrap(), 1);
    assert_eq!(generation(&res), 1);
    assert_eq!(
        res.read(|tx| account_appearance(tx, &account))
            .unwrap()
            .revision(),
        2
    );
}

#[test]
fn errors_panics_and_real_commit_failure_roll_back_without_events() {
    enum Failure {
        Return,
        Panic,
        Commit,
    }
    for failure in [Failure::Return, Failure::Panic, Failure::Commit] {
        let (_scratch, res, account) = resources("appearance-events-failure");
        let receiver = observe(&res);
        let before = res.read(|tx| account_appearance(tx, &account)).unwrap();
        let id = account.clone();
        let result = write(&res, move |tx| {
            set_account_appearance(tx, &id, 1, &AccountColor::Custom("#112233".into()), None)?;
            match failure {
                Failure::Return => Err(StoreError::internal("injected failure")),
                Failure::Panic => panic!("injected appearance panic"),
                Failure::Commit => {
                    tx.execute_batch("PRAGMA defer_foreign_keys=ON").unwrap();
                    tx.execute("INSERT INTO identities (identity_id,account_id,email,display_name,is_default,sendable) VALUES ('dangling','missing','synthetic@example.test','Synthetic',0,'validated')", []).unwrap();
                    Ok(())
                }
            }
        });
        assert!(result.is_err());
        assert_eq!(generation(&res), 0);
        assert_eq!(
            res.read(|tx| account_appearance(tx, &account)).unwrap(),
            before
        );
        quiet(&receiver);
    }
}

#[test]
fn command_replay_preserves_receipt_and_noop_commands_announce_nothing() {
    let (_scratch, res, account) = resources("appearance-events-receipt");
    let receiver = observe(&res);
    let now = res.now();
    let id = account.clone();
    let first = write(&res, move |tx| {
        run_once(tx, "appearance-key", "set_appearance", now, |tx| {
            set_account_appearance(tx, &id, 1, &AccountColor::Custom("#112233".into()), None)?;
            Ok(RESULT.to_owned())
        })
    })
    .unwrap();
    assert_eq!(receiver.recv_timeout(WAIT).unwrap(), 1);
    let replay = write(&res, move |tx| {
        run_once(tx, "appearance-key", "set_appearance", now, |_| {
            panic!("replay executed")
        })
    })
    .unwrap();
    assert_eq!(replay.result_json(), first.result_json());
    assert_eq!(replay.created_at(), first.created_at());
    let id = account.clone();
    write(&res, move |tx| {
        run_once(tx, "noop-key", "set_appearance", now, |tx| {
            set_account_appearance(tx, &id, 2, &AccountColor::Custom("#112233".into()), None)?;
            Ok(RESULT.to_owned())
        })
    })
    .unwrap();
    assert_eq!(generation(&res), 1);
    quiet(&receiver);
}

#[test]
fn replacement_and_reopen_preserve_the_latest_generation() {
    let (scratch, res, account) = resources("appearance-events-reopen");
    let first = observe(&res);
    change(&res, &account, 1, "#112233").unwrap();
    assert_eq!(first.recv_timeout(WAIT).unwrap(), 1);
    let replacement = observe(&res);
    change(&res, &account, 2, "#445566").unwrap();
    quiet(&first);
    assert_eq!(replacement.recv_timeout(WAIT).unwrap(), 2);
    res.close().unwrap();
    let res = Resources::open(&scratch.path().join("store")).unwrap();
    let next = observe(&res);
    assert_eq!(generation(&res), 2);
    quiet(&next);
    assert_eq!(change(&res, &account, 3, "#778899").unwrap(), 3);
    assert_eq!(next.recv_timeout(WAIT).unwrap(), 3);
}

#[test]
fn unreadable_counters_do_not_suppress_independent_announcements() {
    let (_scratch, res, _) = resources("appearance-events-isolation");
    let before = res
        .read(|tx| Ok((list_generation(tx)?, search_generation(tx)?)))
        .unwrap();
    let (list_send, lists) = mpsc::channel();
    let (search_send, searches) = mpsc::channel();
    res.observe_list_generation(Arc::new(move |value| {
        let _ = list_send.send(value);
    }));
    res.observe_search_generation(Arc::new(move |value| {
        let _ = search_send.send(value);
    }));
    let appearances = observe(&res);
    write(&res, |tx| {
        tx.execute(
            "UPDATE store_meta SET value='broken' WHERE key='appearance_generation'",
            [],
        )
        .unwrap();
        note_membership_change(tx)
    })
    .unwrap();
    assert_eq!(lists.recv_timeout(WAIT).unwrap(), before.0 + 1);
    assert_eq!(searches.recv_timeout(WAIT).unwrap(), (before.1 + 1) as u64);
    quiet(&appearances);

    let (_scratch, res, account) = resources("appearance-events-inverse-isolation");
    let appearances = observe(&res);
    write(&res, |tx| {
        tx.execute("UPDATE store_meta SET value='broken' WHERE key IN ('list_generation','search_generation')", []).unwrap();
        Ok(())
    }).unwrap();
    assert_eq!(change(&res, &account, 1, "#112233").unwrap(), 1);
    assert_eq!(appearances.recv_timeout(WAIT).unwrap(), 1);
}

#[test]
fn damaged_and_exhausted_counters_are_not_repaired_or_mutated() {
    for mutation in [
        "UPDATE store_meta SET value='bad' WHERE key='appearance_generation'",
        "UPDATE store_meta SET value=CAST(x'ff' AS TEXT) WHERE key='appearance_generation'",
        "UPDATE store_meta SET value='-1' WHERE key='appearance_generation'",
        "DELETE FROM store_meta WHERE key='appearance_generation'",
    ] {
        let (_scratch, res, account) = resources("appearance-events-corrupt");
        let before = res.read(|tx| account_appearance(tx, &account)).unwrap();
        write(&res, move |tx| {
            tx.execute_batch(mutation).unwrap();
            Ok(())
        })
        .unwrap();
        assert!(
            matches!(
                res.read(appearance_generation),
                Err(StoreError::Corrupt { .. })
            ),
            "{mutation}"
        );
        assert!(
            matches!(
                change(&res, &account, 1, "#112233"),
                Err(StoreError::Corrupt { .. })
            ),
            "{mutation}"
        );
        assert_eq!(
            res.read(|tx| account_appearance(tx, &account)).unwrap(),
            before
        );
    }
    let (_scratch, res, account) = resources("appearance-events-overflow");
    let before = res.read(|tx| account_appearance(tx, &account)).unwrap();
    write(&res, |tx| {
        tx.execute(
            "UPDATE store_meta SET value=?1 WHERE key='appearance_generation'",
            [i64::MAX.to_string()],
        )
        .unwrap();
        Ok(())
    })
    .unwrap();
    assert_eq!(generation(&res), i64::MAX as u64);
    assert!(matches!(
        change(&res, &account, 1, "#112233"),
        Err(StoreError::Corrupt { .. })
    ));
    assert_eq!(
        res.read(|tx| account_appearance(tx, &account)).unwrap(),
        before
    );
}
