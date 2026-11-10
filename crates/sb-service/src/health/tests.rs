//! `health()` and its subscription through the service (plan
//! M1.8h5b3): a finding at runtime reaches a subscribed shell, the
//! answer names the finding, its time and its copy, and the next open
//! reports the same; a latch that can't be read is still reported.

use std::pin::pin;
use std::sync::mpsc;
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use sb_test_support::{ScratchDir, block_on, damage_an_index};

use crate::{Core, CoreConfig, CoreError, Health, HealthSubscription, Integrity, open_core};

/// How long a genuinely asynchronous condition may take.
const PATIENCE: Duration = Duration::from_secs(60);

fn open(scratch: &ScratchDir) -> Core {
    block_on(open_core(CoreConfig {
        data_root: scratch.path().join("store"),
        cache_root: scratch.path().join("cache"),
    }))
    .unwrap()
}

/// Whether `next` is still waiting after one poll.
fn waits(subscription: &HealthSubscription) -> bool {
    pin!(subscription.next())
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
        .is_pending()
}

/// Poll `next` until it answers or `PATIENCE` runs out.
fn answer(subscription: &HealthSubscription) -> Result<(), CoreError> {
    let mut next = pin!(subscription.next());
    let mut cx = Context::from_waker(Waker::noop());
    let deadline = Instant::now() + PATIENCE;
    loop {
        if let Poll::Ready(answered) = next.as_mut().poll(&mut cx) {
            return answered;
        }
        assert!(Instant::now() < deadline, "next never answered");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_finding_reaches_a_subscribed_shell_and_the_next_open() {
    let scratch = ScratchDir::new("service-health-finding");
    block_on(open(&scratch).shutdown());
    damage_an_index(&scratch.path().join("store/state.sqlite"));
    let core = open(&scratch);
    let changes = core.subscribe_health();
    assert_eq!(
        core.health(),
        Health {
            integrity: Integrity::Sound
        }
    );
    assert!(waits(&changes));

    // The engine's seam directly: this crate's own tests don't build
    // with its `test-seams` feature.
    assert!(block_on(core.engine().check_integrity_now()).unwrap());

    answer(&changes).expect("the finding is one change");
    assert!(waits(&changes));
    let found = core.health();
    let Integrity::ReadOnly {
        detail,
        detected_at_ms,
        copy,
    } = &found.integrity
    else {
        panic!("health reports no finding");
    };
    assert!(detail.contains("integrity_probe_pad"), "{detail}");
    assert!(detected_at_ms.is_some());
    let copy = copy.as_ref().expect("the copy is published");
    assert!(copy.join("manifest.json").is_file(), "{}", copy.display());
    // Queries go on, coverage among them; a command that writes names
    // the finding.
    block_on(core.settings()).unwrap();
    let coverage = block_on(core.coverage(crate::SearchScope::All)).unwrap();
    assert_eq!(coverage.index_state, crate::SearchIndexState::ReadOnly);
    match block_on(core.set_setting("after-the-finding", "appearance.compose_cue", "strong")) {
        Err(CoreError::Corrupt { detail }) => {
            assert!(detail.contains("integrity_probe_pad"), "{detail}");
        }
        other => panic!("expected Corrupt, got {other:?}"),
    }
    block_on(core.shutdown());

    // The latch outlives the process: the next open answers the same.
    let core = open(&scratch);
    assert_eq!(core.health(), found);
    block_on(core.settings()).unwrap();
    let coverage = block_on(core.coverage(crate::SearchScope::All)).unwrap();
    assert_eq!(coverage.index_state, crate::SearchIndexState::ReadOnly);
    block_on(core.shutdown());
}

#[test]
fn a_latch_that_cannot_be_read_is_reported_without_time_or_copy() {
    let scratch = ScratchDir::new("service-health-unreadable");
    block_on(open(&scratch).shutdown());
    std::fs::write(
        scratch.path().join("store/integrity-latch.json"),
        b"not json",
    )
    .unwrap();

    let core = open(&scratch);

    let Integrity::ReadOnly {
        detail,
        detected_at_ms,
        copy,
    } = core.health().integrity
    else {
        panic!("health reports no finding");
    };
    assert!(detail.contains("can't be read"), "{detail}");
    assert_eq!((detected_at_ms, copy), (None, None));
    block_on(core.shutdown());
}

/// Shutdown ends a waiting `next` with `Unavailable`.
#[test]
fn shutdown_ends_a_waiting_next() {
    let scratch = ScratchDir::new("service-health-shutdown");
    let core = open(&scratch);
    let changes = core.subscribe_health();
    let (sent, received) = mpsc::channel();
    let waiter = std::thread::spawn(move || {
        sent.send(block_on(changes.next())).unwrap();
    });
    block_on(core.shutdown());
    let ended = received.recv_timeout(PATIENCE).expect("next never ended");
    assert!(
        matches!(ended, Err(CoreError::Unavailable { .. })),
        "{ended:?}"
    );
    waiter.join().unwrap();
}
