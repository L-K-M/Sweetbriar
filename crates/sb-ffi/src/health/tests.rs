//! Health over the exported surface (plan M1.8h5b3): the record's
//! mapping on a sound and a latched store, a finding at runtime
//! reaching a subscription, and the subscription's cancellation and
//! shutdown.

use std::pin::pin;
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use sb_test_support::{ScratchDir, block_on, damage_an_index};

use super::*;
use crate::api::{CoreConfig, open_core};

/// How long a genuinely asynchronous condition may take.
const PATIENCE: Duration = Duration::from_secs(60);

fn token() -> Arc<CancelToken> {
    CancelToken::new()
}

fn open(scratch: &ScratchDir) -> Arc<Core> {
    let config = CoreConfig {
        data_root: scratch.path().join("store").to_string_lossy().into_owned(),
        cache_root: scratch.path().join("cache").to_string_lossy().into_owned(),
    };
    block_on(open_core(token(), config)).unwrap()
}

fn health(core: &Core) -> Health {
    block_on(core.health(token())).unwrap()
}

/// Poll `future` until it answers or `PATIENCE` runs out.
fn finish<F: std::future::Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    let mut cx = Context::from_waker(Waker::noop());
    let deadline = Instant::now() + PATIENCE;
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut cx) {
            return output;
        }
        assert!(Instant::now() < deadline, "the future never finished");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Run the full check through the service's seam; it finds the damage.
fn find_the_damage(core: &Core) {
    let service = block_on(core.service.read());
    let found = block_on(service.as_ref().unwrap().check_integrity_now()).unwrap();
    assert!(found, "the check found nothing");
}

#[test]
fn a_finding_at_runtime_reaches_the_subscription_and_the_record() {
    let scratch = ScratchDir::new("ffi-health-finding");
    block_on(open(&scratch).shutdown());
    damage_an_index(&scratch.path().join("store/state.sqlite"));
    let core = open(&scratch);
    let changes = block_on(core.subscribe_health(token())).unwrap();
    assert_eq!(
        health(&core),
        Health {
            integrity: Integrity::Sound
        }
    );
    // A cancelled wait answers `Cancelled` and moves nothing.
    let waiting = token();
    let mut next = pin!(changes.next(waiting.clone()));
    assert!(
        next.as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending()
    );
    waiting.cancel();
    assert!(matches!(finish(next), Err(CoreError::Cancelled)));

    find_the_damage(&core);

    finish(changes.next(token())).expect("the finding is one change");
    let Integrity::ReadOnly {
        detail,
        detected_at_ms,
        copy_path,
    } = health(&core).integrity
    else {
        panic!("health reports no finding");
    };
    assert!(detail.contains("integrity_probe_pad"), "{detail}");
    assert!(detected_at_ms.is_some());
    let copy = copy_path.expect("the copy is published");
    assert!(
        std::path::Path::new(&copy).join("manifest.json").is_file(),
        "{copy}"
    );
    // A command that writes carries the finding to the shell.
    let refused = block_on(core.set_setting(
        token(),
        "after-the-finding".into(),
        "appearance.compose_cue".into(),
        "strong".into(),
    ));
    match refused {
        Err(CoreError::Corrupt { detail }) => {
            assert!(detail.contains("integrity_probe_pad"), "{detail}");
        }
        other => panic!("expected Corrupt, got {other:?}"),
    }
    block_on(core.shutdown());
}

#[test]
fn a_latch_that_cannot_be_read_maps_to_no_time_and_no_copy() {
    let scratch = ScratchDir::new("ffi-health-unreadable");
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
        copy_path,
    } = health(&core).integrity
    else {
        panic!("health reports no finding");
    };
    assert!(detail.contains("can't be read"), "{detail}");
    assert_eq!((detected_at_ms, copy_path), (None, None));
    block_on(core.shutdown());
}

#[test]
fn shutdown_ends_a_waiting_next_and_later_calls() {
    let scratch = ScratchDir::new("ffi-health-shutdown");
    let core = open(&scratch);
    let changes = block_on(core.subscribe_health(token())).unwrap();
    let mut next = pin!(changes.next(token()));
    assert!(
        next.as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending()
    );

    block_on(core.shutdown());

    assert!(matches!(finish(next), Err(CoreError::Unavailable { .. })));
    assert!(matches!(
        block_on(core.health(token())),
        Err(CoreError::Unavailable { .. })
    ));
    assert!(matches!(
        block_on(core.subscribe_health(token())),
        Err(CoreError::Unavailable { .. })
    ));
}
