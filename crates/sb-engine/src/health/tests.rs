//! The engine read-only on an integrity finding (plan M1.8h5b2; spec/03
//! "Integrity"). A finding at runtime stops the background workers and
//! wakes health subscriptions; commands that write are `Corrupt`,
//! Bridge isn't asked for its certificate, and queries go on. A latched
//! store opens the same way.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::sync::atomic::AtomicBool;
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use sb_resources::IntegrityCheck;
use sb_test_support::{ScratchDir, block_on, cert_der, damage_an_index};

use crate::{
    AddressMode, BridgeAccountParams, BridgeIdentity, CertificatePin, EndpointSecurity, Engine,
    EngineError, Health, IdentitySendable, Integrity, Secret,
};

const PATIENCE: Duration = Duration::from_secs(60);
const CUE: &str = "appearance.compose_cue";

/// Poll `future` until it finishes or `PATIENCE` runs out.
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

fn eventually(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting: {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn open(root: &Path) -> Engine {
    block_on(Engine::open(root)).unwrap()
}

fn data_root(scratch: &ScratchDir) -> PathBuf {
    std::fs::canonicalize(scratch.path()).unwrap().join("store")
}

fn set_cue(engine: &Engine, value: &str) -> Result<(), EngineError> {
    block_on(engine.set_setting(&engine.new_idempotency_key(), CUE, value)).map(drop)
}

/// The compose cue, read through the engine's query.
fn cue(engine: &Engine) -> String {
    block_on(engine.settings())
        .unwrap()
        .iter()
        .find(|setting| setting.key() == CUE)
        .expect("the registry holds the cue")
        .value()
        .to_owned()
}

/// An engine on a store that holds a strong compose cue and damage
/// only the full check finds.
fn damaged(root: &Path) -> Engine {
    let engine = open(root);
    set_cue(&engine, "strong").unwrap();
    block_on(engine.shutdown()).unwrap();
    damage_an_index(&root.join("state.sqlite"));
    open(root)
}

/// Run the full check, which finds the damage and latches the store.
fn find_the_damage(engine: &Engine) {
    let found = engine
        .resources()
        .check_integrity(&AtomicBool::new(false))
        .unwrap();
    let IntegrityCheck::Finding(finding) = found else {
        panic!("the check found nothing: {found:?}");
    };
    finding.latch.expect("the latch reached the disk");
    finding.copy.expect("the copy was published");
}

/// A complete Bridge setup request for the server on `port`.
fn bridge_account(port: u16) -> BridgeAccountParams {
    BridgeAccountParams {
        display_name: "Owner Proton".to_owned(),
        username: "owner@example.org".to_owned(),
        password: Secret::new(b"bridge-secret-41C7".to_vec()),
        imap_port: port,
        imap_security: EndpointSecurity::Tls,
        smtp_port: 1025,
        smtp_security: EndpointSecurity::StartTls,
        pin: CertificatePin::of_der(&cert_der()),
        address_mode: Some(AddressMode::Combined),
        identities: vec![BridgeIdentity {
            email: "owner@example.org".to_owned(),
            display_name: "Owner".to_owned(),
            is_default: true,
            sendable: IdentitySendable::Provisional,
        }],
    }
}

fn workers_running(engine: &Engine) -> [bool; 3] {
    [
        engine.indexer().running(),
        engine.received().running(),
        engine.maintenance().running(),
    ]
}

/// What a read-only engine answers and refuses, whether the finding
/// came at runtime or with the open.
fn assert_read_only(engine: &Engine) {
    assert!(engine.read_only());
    let Integrity::ReadOnly {
        detail,
        detected_at,
        copy,
    } = engine.health().integrity
    else {
        panic!("health reports no finding");
    };
    assert!(detail.contains("integrity_probe_pad"), "{detail}");
    assert!(detected_at.is_some());
    let copy = copy.expect("the copy is named once it is published");
    assert!(copy.join("manifest.json").is_file(), "{}", copy.display());

    // Queries go on; a command that writes names the finding.
    assert_eq!(cue(engine), "strong");
    match set_cue(engine, "standard") {
        Err(EngineError::Corrupt { detail }) => {
            assert!(detail.contains("integrity_probe_pad"), "{detail}");
        }
        other => panic!("expected Corrupt, got {other:?}"),
    }
    assert_eq!(cue(engine), "strong");

    // No worker runs, and none starts.
    eventually("the workers end", || workers_running(engine) == [false; 3]);
    engine.schedule_index();
    engine.schedule_received();
    engine.schedule_maintenance();
    assert_eq!(workers_running(engine), [false; 3]);

    // Bridge is asked for neither its certificate nor a login: both
    // are refused before anything connects.
    let bridge = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    bridge.set_nonblocking(true).unwrap();
    let port = bridge.local_addr().unwrap().port();
    let certificate = block_on(engine.bridge_certificate(port, EndpointSecurity::Tls)).map(drop);
    let setup =
        block_on(engine.add_bridge_account("after-the-finding", bridge_account(port))).map(drop);
    for refused in [certificate, setup] {
        match refused {
            Err(EngineError::Corrupt { detail }) => {
                assert!(detail.contains("integrity_probe_pad"), "{detail}");
            }
            other => panic!("expected Corrupt, got {other:?}"),
        }
    }
    match bridge.accept() {
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
        other => panic!("the engine connected to Bridge: {other:?}"),
    }
}

#[test]
fn a_finding_turns_a_running_engine_read_only() {
    let scratch = ScratchDir::new("engine-health-finding");
    let engine = damaged(&data_root(&scratch));
    engine.schedule_maintenance();
    assert_eq!(workers_running(&engine), [true; 3]);
    assert_eq!(
        engine.health(),
        Health {
            integrity: Integrity::Sound
        }
    );
    assert!(!engine.read_only());
    let health = engine.subscribe_health();
    // Nothing changed yet: the subscription waits.
    assert!(
        pin!(health.next())
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending()
    );

    find_the_damage(&engine);

    finish(health.next()).expect("the finding wakes the subscription");
    assert_read_only(&engine);
    // One finding is one change.
    assert!(
        pin!(health.next())
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending()
    );
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn a_latched_store_opens_read_only() {
    let scratch = ScratchDir::new("engine-health-latched-open");
    let root = data_root(&scratch);
    let engine = damaged(&root);
    find_the_damage(&engine);
    block_on(engine.shutdown()).unwrap();

    let engine = open(&root);

    // The open started no worker to stop, the thread rebuild included.
    assert_eq!(workers_running(&engine), [false; 3]);
    assert_eq!(engine.imports().threads().requests(), 0);
    assert_read_only(&engine);
    // A subscription made now starts after the open's finding.
    let health = engine.subscribe_health();
    assert!(
        pin!(health.next())
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending()
    );
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn shutdown_ends_a_health_subscription_s_wait() {
    let scratch = ScratchDir::new("engine-health-shutdown");
    let engine = open(&data_root(&scratch));
    let health = engine.subscribe_health();
    block_on(engine.shutdown()).unwrap();
    assert!(matches!(
        finish(health.next()),
        Err(EngineError::Unavailable { .. })
    ));
}
