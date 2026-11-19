//! M2.17: the read-only sync suite against Proton's own development
//! server (spec/11 "The `go-proton-api` lane").
//!
//! Gated on `SB_GPA_LANE=<path to the driver's JSON line>`. Without it
//! the test prints a note and returns, so `cargo test --workspace`
//! stays green everywhere; no test contacts the live Proton API.

use super::*;

use std::collections::BTreeMap;
use std::fs;

use proton_srp::RPGPVerifier;
use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject;
use serde::Deserialize;

const LANE_ENV: &str = "SB_GPA_LANE";
const SUMMARY_ENV: &str = "SB_GPA_LANE_SUMMARY";

/// Production's SRP salt length (`proton-srp`'s `SALT_LEN_BYTES`,
/// research `auth-session.md` §3). The lane builds the pinned server
/// with `ci/go-proton-api/patches/srp-salt-10.patch` so its SRP salt is
/// production's 10 bytes, not the upstream 16.
const PRODUCTION_SRP_SALT_BYTES: i64 = 10;

#[derive(Debug, Deserialize)]
struct LaneLine {
    base_url: String,
    cert_pem: String,
    username: String,
    password: String,
    /// The key the development server signs its SRP modulus with. It is
    /// Proton's own modulus key, so the lane verifies with the
    /// production `RPGPVerifier` rather than a lane-only key.
    #[allow(dead_code)]
    modulus_pubkey: String,
    srp_salt_bytes: i64,
    label_counts: BTreeMap<String, i64>,
    messages: Vec<LaneMessage>,
    request_log: String,
    /// The send suite's synthetic sender and internal recipient
    /// (M4.3g), and the driver's send log the suite reads back.
    #[serde(default)]
    send_username: String,
    #[serde(default)]
    send_email: String,
    #[serde(default)]
    peer_email: String,
    #[serde(default)]
    send_log: String,
}

#[derive(Debug, Deserialize)]
struct LaneMessage {
    provider_key: String,
    subject: String,
    labels: Vec<String>,
    #[allow(dead_code)]
    unread: bool,
}

#[tokio::test]
async fn the_development_server_lists_the_seeded_account_read_only() {
    let Some(path) = std::env::var_os(LANE_ENV) else {
        println!(
            "skipped: {LANE_ENV} is not set; run ci/go-proton-api and point it at its JSON line"
        );
        return;
    };
    let lane = read_lane(&path);

    // A driver built without the SRP salt patch reports the upstream
    // 16 bytes; signing in would fail anyway, so say so directly.
    assert_eq!(
        lane.srp_salt_bytes, PRODUCTION_SRP_SALT_BYTES,
        "the lane driver was not built against the patched server"
    );

    let scratch = ScratchDir::new("service-go-proton-api-lane");
    let root = scratch.path().join("store");
    let secrets = Arc::new(FakeSecrets::new());
    let clock = Arc::new(MovableClock::new(1_000_000));
    let cert = CertificateDer::from_pem_slice(lane.cert_pem.as_bytes())
        .expect("the driver's certificate parses");
    let engine = Engine::open_with_proton(
        &root,
        secrets,
        lane.base_url.clone(),
        vec![cert],
        Arc::new(RPGPVerifier::default()),
        clock,
    )
    .await
    .expect("the engine opens against the development server");
    let core = Core::around(engine);

    let state = match core
        .begin_proton_sign_in(
            "lane",
            ProtonSignInParams {
                display_name: "Lane".to_owned(),
                username: lane.username.clone(),
                password: lane.password.clone().into_bytes(),
                account_id: None,
            },
        )
        .await
        .expect("the sign-in begins")
    {
        BeginProtonSignIn::Open { state, .. } => state,
        BeginProtonSignIn::Complete { receipt } => {
            panic!("an unused key replayed a receipt: {receipt:?}")
        }
    };
    let id = account_id(&state);

    let coverage = listed(&core, &id).await;
    assert_eq!(
        coverage.messages,
        lane.messages.len() as u64,
        "every seeded message is listed: {coverage:?}"
    );

    // All Mail is deliberately not asserted: the driver's seeded counts
    // don't include the label the server lists on every message, so
    // there is no server total here to compare the local count with.
    let mut labels = BTreeMap::new();
    for (name, expected) in &lane.label_counts {
        let mailbox = coverage
            .mailboxes
            .iter()
            .find(|mailbox| mailbox.remote_name == *name)
            .unwrap_or_else(|| panic!("{name} is missing from {coverage:?}"));
        let expected = u64::try_from(*expected).expect("a small count");
        assert_eq!(mailbox.local, expected, "{name}: local occurrences");
        assert_eq!(
            mailbox.remote_total,
            Some(expected),
            "{name}: the server's total"
        );
        labels.insert(
            name.clone(),
            serde_json::json!({
                "expected": expected,
                "local": mailbox.local,
                "remote_total": mailbox.remote_total,
            }),
        );
    }

    // The message in a folder, with two labels and Starred: one logical
    // message, one occurrence per mailbox.
    let multi = lane
        .messages
        .iter()
        .find(|message| message.labels.len() > 2)
        .expect("a multi-label message was seeded");
    let db = root.join("state.sqlite");
    let of_message = format!(
        "SELECT COUNT(*) FROM occurrences o
         JOIN logical_messages l ON l.message_id = o.message_id
         JOIN mailboxes m ON m.mailbox_id = o.mailbox_id
         WHERE l.provider_key = {}",
        quoted(&multi.provider_key)
    );
    assert_eq!(
        count_rows(
            &db,
            &format!(
                "SELECT COUNT(*) FROM logical_messages WHERE provider_key = {}",
                quoted(&multi.provider_key)
            )
        ),
        1,
        "{} is one logical message",
        multi.subject
    );
    for name in &multi.labels {
        assert_eq!(
            count_rows(
                &db,
                &format!(
                    "{of_message} AND m.remote_name = {} AND o.state = 'present'",
                    quoted(name)
                )
            ),
            1,
            "{} has one present occurrence in {name}",
            multi.subject
        );
    }
    // The development server puts every listed message in All Mail
    // (`"5"`), so the message has one more occurrence than its
    // asserted labels. All Mail itself is not compared against the
    // driver's counts: they are seeded without it.
    assert_eq!(
        count_rows(&db, &of_message),
        multi.labels.len() as i64 + 1,
        "{} has one occurrence per label plus the server's All Mail",
        multi.subject
    );

    // The driver logged every call after it printed the line: the
    // read-only suite sent no mutation. The send suite runs only
    // after this audit, so its mutations are not mistaken for the
    // read-only suite's.
    let mutations = audit_requests(&lane.request_log);
    assert!(
        mutations.is_empty(),
        "the read-only suite mutated the server: {mutations:?}"
    );

    let send = lane_send::run_send_suite(
        &core,
        &lane_send::SendLaneParams {
            send_username: lane.send_username.clone(),
            password: lane.password.clone(),
            send_email: lane.send_email.clone(),
            peer_email: lane.peer_email.clone(),
            send_log: lane.send_log.clone(),
        },
    )
    .await;

    write_summary(
        SUMMARY_ENV,
        serde_json::json!({
            "result": "pass",
            "date": utc_date(),
            "base_url": lane.base_url,
            "account_id": id,
            "messages": coverage.messages,
            "labels": labels,
            "multi_label": multi.subject,
            // The asserted labels plus the All Mail occurrence the
            // server adds to every listed message.
            "occurrences": multi.labels.len() + 1,
            "mutations": mutations,
            "send": send,
        }),
    );

    core.shutdown().await;
}

/// The UTC date, for the artifact's run date.
fn utc_date() -> String {
    let days = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_secs()
        / 86_400;
    let (year, month, day) = civil_from_days(i64::try_from(days).expect("a small day count"));
    format!("{year:04}-{month:02}-{day:02}")
}

/// Days since the epoch to a civil date (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// Writes the lane's summary JSON when `env` names a path; the nightly
/// job sets it, so a run always leaves an artifact.
fn write_summary(env: &str, value: serde_json::Value) {
    let Some(path) = std::env::var_os(env) else {
        return;
    };
    fs::write(
        path,
        serde_json::to_string_pretty(&value).expect("the summary serializes"),
    )
    .expect("the summary writes");
}

fn read_lane(path: impl AsRef<std::path::Path>) -> LaneLine {
    let text = fs::read_to_string(path).expect("the driver's JSON line reads");
    let line = text
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .expect("the driver printed a JSON line");
    serde_json::from_str(line).expect("the driver's line is one JSON object")
}

/// Every call in the log that is not a read or a sign-in: the driver
/// logs `METHOD<TAB>path` for each lane-phase request. A missing log,
/// or one without reads, fails: an unaudited run must not pass.
fn audit_requests(path: &str) -> Vec<String> {
    let log = fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("the driver's request log {path} is unreadable: {error}"));
    assert!(
        log.lines().any(|line| line.starts_with("GET\t")),
        "the request log at {path} holds no reads, so it audited nothing"
    );
    log.lines()
        .filter(|line| {
            let Some((method, path)) = line.split_once('\t') else {
                return false;
            };
            let sign_in = method == "POST"
                && matches!(path, "/auth/v4" | "/auth/v4/info" | "/auth/v4/refresh");
            method != "GET" && !sign_in
        })
        .map(str::to_owned)
        .collect()
}

/// `value` as an SQL string literal: label names are free text.
fn quoted(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}
