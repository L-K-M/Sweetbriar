//! All four capture scopes through Resources; no body or file output.

use std::collections::BTreeSet;
use std::ops::ControlFlow;
use std::sync::Arc;

use sb_resources::{
    ExportCaptureRequest, ExportFormat, ExportJob, ExportJobState, ExportScope, ExportSearchSort,
    ExportSelection, MAX_EXPORT_CHANGED_BYTES, MAX_EXPORT_CHUNK_BYTES, MAX_EXPORT_CHUNKS,
    MAX_EXPORT_JSON_BYTES, Query, Resources, Scoped, SearchScope, StoreError, Transaction,
    WritePriority, advance_export_job, export_chunks, export_job, initialize_account_appearance,
    list_generation, note_membership_change, search_generation,
};
use sb_test_support::{ScratchDir, block_on};
use serde_json::{Value, json};

const OTHER_ACCOUNT: &str = "00000000-0000-4000-8000-000000000002";
const EMPTY_HASH: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const DESTINATION: &str = "/synthetic/export";
// Independent SHA-256 oracles over the fixture's signed big-endian i64 IDs.
const ACCOUNT_IDS_SHA256: &str = "b8f8f4d62734108de03cb1fb97d2663e2252263ed868d26dd794932c0015e250";
const SELECTION_IDS_SHA256: &str =
    "0e2959e7a8fb183e252ae5ca781b02659171cae91697610faf80513b7852c728";

fn write<T: Send + 'static>(
    res: &Resources,
    f: impl FnOnce(&Transaction) -> Result<T, StoreError> + Send + 'static,
) -> T {
    block_on(res.write(WritePriority::User, f)).unwrap()
}

fn fixture() -> (ScratchDir, Resources, String) {
    let scratch = ScratchDir::new("export-capture");
    let res = Resources::open(&scratch.path().join("store")).unwrap();
    let account = res
        .read(|tx| {
            Ok(tx
                .query_row(
                    "SELECT account_id FROM accounts WHERE provider='local_archive'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap())
        })
        .unwrap();
    let id = account.clone();
    write(&res, move |tx| {
        tx.execute("INSERT INTO accounts (account_id,kind,provider,display_name,state,sort_order,created_at) VALUES (?1,'imap','generic_imap','Other account','disconnected',1,1)", [OTHER_ACCOUNT]).unwrap();
        initialize_account_appearance(tx, OTHER_ACCOUNT)?;
        tx.execute("INSERT INTO mailboxes (mailbox_id,account_id,remote_name,kind,selectable,sync_state) VALUES (1,?1,'Inbox','folder',1,'synced'),(2,?1,'Other','folder',1,'synced')", [&id]).unwrap();
        tx.execute("INSERT INTO mailboxes (mailbox_id,account_id,remote_name,kind,selectable,sync_state) VALUES (3,?1,'Foreign','folder',1,'synced')", [OTHER_ACCOUNT]).unwrap();
        tx.execute("INSERT INTO acquisitions (acquisition_id,content_sha256,byte_length,object_state,first_acquired_at) VALUES (1,?1,0,'published',1)", [EMPTY_HASH]).unwrap();
        tx.execute("INSERT INTO logical_messages (message_id,account_id,acquisition_id,created_at) VALUES (1,?1,1,1),(2,?1,1,1)", [&id]).unwrap();
        tx.execute(
            "INSERT INTO logical_messages (message_id,account_id,created_at) VALUES (3,?1,1)",
            [OTHER_ACCOUNT],
        )
        .unwrap();
        for (occurrence, mailbox, message, date, state) in [
            (1, 1, 1, 40, "present"),
            (2, 1, 2, 10, "present"),
            (3, 2, 1, 50, "present"),
            (5, 1, 1, 20, "present"),
            (6, 2, 2, 5, "present"),
            (7, 2, 1, 100, "present"),
            (8, 2, 1, 80, "vanished"),
        ] {
            tx.execute("INSERT INTO occurrences (occurrence_id,account_id,mailbox_id,message_id,internal_date,f_flagged,keywords,state,first_seen_at,last_seen_at) VALUES (?1,?2,?3,?4,?5,1,'[]',?6,1,1)", (occurrence,&id,mailbox,message,date,state)).unwrap();
        }
        tx.execute("INSERT INTO occurrences (occurrence_id,account_id,mailbox_id,message_id,internal_date,f_flagged,keywords,state,first_seen_at,last_seen_at) VALUES (4,?1,3,3,70,1,'[]','present',1,1)", [OTHER_ACCOUNT]).unwrap();
        tx.execute_batch("INSERT INTO message_headers (message_id,raw_fields,from_json,fetched_at) VALUES (1,x'00','[]',1); INSERT INTO notes (note_id,message_id,body_source,created_at,updated_at) VALUES ('note',1,'frozen note',1,1);").unwrap();
        sb_store::refresh_list_rows(tx, &(1..=8).collect::<BTreeSet<_>>())?;
        tx.execute("UPDATE message_headers SET from_json='not-json'", [])
            .unwrap();
        Ok(())
    });
    (scratch, res, account)
}

fn list_gen(res: &Resources) -> u64 {
    res.read(list_generation).unwrap()
}
fn request(scope: ExportScope) -> ExportCaptureRequest {
    ExportCaptureRequest::new(ExportFormat::Eml, DESTINATION.to_owned(), scope).unwrap()
}
fn mailbox(res: &Resources) -> ExportScope {
    ExportScope::Mailbox {
        mailbox_id: 1,
        expected_generation: list_gen(res),
    }
}
fn capture(res: &Resources, id: &str, request: &ExportCaptureRequest) -> ExportJob {
    match res
        .capture_export(id, request, |_| ControlFlow::Continue(()))
        .unwrap()
    {
        Scoped::Done(job) => job,
        Scoped::Stopped => panic!("capture stopped without a stop request"),
    }
}
fn bytes(res: &Resources, id: &str) -> Vec<u8> {
    let job = res.read(|tx| export_job(tx, id)).unwrap();
    let mut bytes = Vec::new();
    for from in (0..job.captured_chunks()).step_by(MAX_EXPORT_CHUNKS) {
        bytes.extend(
            res.read(|tx| export_chunks(tx, id, from, MAX_EXPORT_CHUNKS as u32))
                .unwrap()
                .into_iter()
                .flatten(),
        );
    }
    bytes
}
fn records(res: &Resources, id: &str) -> Vec<Value> {
    bytes(res, id)
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect()
}
fn ids(records: &[Value]) -> Vec<i64> {
    records[1..]
        .iter()
        .map(|entry| entry["occurrence_id"].as_i64().unwrap())
        .collect()
}
fn flagged() -> Query {
    Query::parse_json(r#"{"v":1,"q":{"flag":"flagged"}}"#).unwrap()
}

#[test]
fn all_scopes_preserve_order_copies_and_search_chosen_occurrences() {
    let (_scratch, res, account) = fixture();
    let generation = list_gen(&res);
    let search = res.read(|tx| search_generation(tx)).unwrap() as u64;
    let cases = [
        (
            ExportScope::Account {
                account_id: account,
                expected_generation: generation,
            },
            vec![1, 5, 2, 7, 3, 6],
        ),
        (
            ExportScope::Mailbox {
                mailbox_id: 2,
                expected_generation: generation,
            },
            vec![7, 3, 6],
        ),
        (
            ExportScope::Search {
                query: flagged(),
                scope: SearchScope::Mailbox(2),
                sort: ExportSearchSort::Date,
                expected_generation: search,
            },
            vec![3, 6],
        ),
        (
            ExportScope::Search {
                query: flagged(),
                scope: SearchScope::Mailbox(2),
                sort: ExportSearchSort::Relevance,
                expected_generation: search,
            },
            vec![6, 3],
        ),
        (
            ExportScope::Selection(
                ExportSelection::new(
                    "native-selection".into(),
                    1,
                    generation,
                    Arc::from([8, 3, 2]),
                )
                .unwrap(),
            ),
            vec![8, 3, 2],
        ),
    ];
    for (index, (scope, expected)) in cases.into_iter().enumerate() {
        let id = format!("scope-{index}");
        let job = capture(&res, &id, &request(scope));
        assert_eq!(job.state(), ExportJobState::Ready);
        assert!(job.request_json().len() <= MAX_EXPORT_JSON_BYTES);
        let kept = records(&res, &id);
        assert_eq!(kept[0]["v"], 1);
        assert_eq!(kept[0]["kind"], "capture");
        if matches!(index, 2 | 3) {
            assert_eq!(
                kept[0]["intent"]["scope"]["query"],
                json!({"v":1,"q":{"flag":"flagged"}})
            );
        }
        assert_eq!(kept[0]["entries"], expected.len());
        assert_eq!(ids(&kept), expected);
        assert!(
            kept[1..]
                .iter()
                .all(|entry| entry["acquisition"]["id"] == 1)
        );
        assert!(
            kept[1..]
                .iter()
                .all(|entry| entry["account"]["id"] != OTHER_ACCOUNT)
        );
        if index == 0 {
            assert_eq!(kept[0]["ids_sha256"], ACCOUNT_IDS_SHA256);
        }
        if index == 4 {
            assert_eq!(kept[0]["ids_sha256"], SELECTION_IDS_SHA256);
            assert_eq!(
                kept[0]["intent"]["scope"]["ids_sha256"],
                SELECTION_IDS_SHA256
            );
            assert_eq!(kept[1]["occurrence_state"], "vanished");
        }
    }
    let foreign = capture(
        &res,
        "other-account",
        &ExportCaptureRequest::new(
            ExportFormat::Mboxrd,
            DESTINATION.into(),
            ExportScope::Account {
                account_id: OTHER_ACCOUNT.into(),
                expected_generation: generation,
            },
        )
        .unwrap(),
    );
    assert_eq!(foreign.state(), ExportJobState::Ready);
    let kept = records(&res, "other-account");
    assert_eq!(kept[0]["intent"]["format"], "mboxrd");
    assert_eq!(ids(&kept), [4]);
    assert_eq!(kept[1]["body_availability"], "not_acquired");
}

#[test]
fn sealed_replay_reopens_without_source_reads_progress_or_reinterpretation() {
    let (scratch, res, _) = fixture();
    let request = request(mailbox(&res));
    let job = capture(&res, "replay", &request);
    let before = bytes(&res, "replay");
    write(&res, |tx| {
        tx.execute_batch("PRAGMA ignore_check_constraints=ON; UPDATE occurrences SET f_seen=2 WHERE occurrence_id=1; UPDATE notes SET body_source=CAST(x'ff' AS TEXT); PRAGMA ignore_check_constraints=OFF;").unwrap();
        note_membership_change(tx)
    });
    res.close().unwrap();
    let res = Resources::open(&scratch.path().join("store")).unwrap();
    let Scoped::Done(replay) = res
        .capture_export("replay", &request, |_| {
            panic!("sealed replay re-read source")
        })
        .unwrap()
    else {
        panic!("sealed replay stopped")
    };
    assert_eq!(replay.revision(), job.revision());
    assert_eq!(bytes(&res, "replay"), before);
    let other =
        ExportCaptureRequest::new(ExportFormat::Mboxrd, DESTINATION.into(), mailbox(&res)).unwrap();
    assert!(matches!(
        res.capture_export("replay", &other, |_| ControlFlow::Continue(())),
        Err(StoreError::Conflict { .. })
    ));
    let revision = replay.revision();
    let now = res.now();
    write(&res, move |tx| {
        advance_export_job(
            tx,
            "replay",
            revision,
            ExportJobState::Cancelled,
            "{\"v\":1}",
            now,
        )
    });
    assert!(matches!(
        res.capture_export("replay", &request, |_| ControlFlow::Continue(())),
        Err(StoreError::Conflict { .. })
    ));
    assert_eq!(bytes(&res, "replay"), before);
}

fn large_note() -> String {
    const NOTE: &str = "large✓";
    NOTE.repeat((MAX_EXPORT_CHANGED_BYTES + MAX_EXPORT_CHUNK_BYTES) / NOTE.len() + 1)
}

#[test]
fn preack_stop_replays_exact_prefix_and_changed_metadata_conflicts() {
    let (_scratch, res, _) = fixture();
    let note = large_note();
    write(&res, move |tx| {
        tx.execute("UPDATE notes SET body_source=?1", [note])
            .unwrap();
        Ok(())
    });
    let target = request(mailbox(&res));
    assert!(matches!(
        res.capture_export("resume", &target, |completed| if completed == 1 {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        })
        .unwrap(),
        Scoped::Stopped
    ));
    let partial = res.read(|tx| export_job(tx, "resume")).unwrap();
    assert_eq!(partial.state(), ExportJobState::Capturing);
    assert!(partial.captured_chunks() >= MAX_EXPORT_CHUNKS as u64);
    assert_eq!(
        capture(&res, "resume", &target).state(),
        ExportJobState::Ready
    );
    assert_eq!(ids(&records(&res, "resume")), [1, 5, 2]);
    assert!(matches!(
        res.capture_export("changed", &target, |completed| if completed == 1 {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        })
        .unwrap(),
        Scoped::Stopped
    ));
    write(&res, |tx| {
        tx.execute("UPDATE notes SET body_source='changed prefix'", [])
            .unwrap();
        Ok(())
    });
    assert!(matches!(
        res.capture_export("changed", &target, |_| ControlFlow::Continue(())),
        Err(StoreError::Conflict { .. })
    ));
    assert_eq!(
        res.read(|tx| export_job(tx, "changed")).unwrap().state(),
        ExportJobState::Capturing
    );
}

#[test]
fn stale_unknown_empty_cancelled_and_writer_thread_paths_never_claim_false_ready() {
    enum Expected {
        Conflict,
        NotFound,
    }
    let (_scratch, res, _) = fixture();
    let bad = [
        (
            "stale",
            Expected::Conflict,
            ExportScope::Mailbox {
                mailbox_id: 1,
                expected_generation: list_gen(&res) + 1,
            },
        ),
        (
            "unknown-mailbox",
            Expected::NotFound,
            ExportScope::Mailbox {
                mailbox_id: 999,
                expected_generation: list_gen(&res),
            },
        ),
        (
            "unknown-account",
            Expected::NotFound,
            ExportScope::Account {
                account_id: "missing".into(),
                expected_generation: list_gen(&res),
            },
        ),
        (
            "unknown-member",
            Expected::NotFound,
            ExportScope::Selection(
                ExportSelection::new("unknown-member".into(), 1, 1, Arc::from([999])).unwrap(),
            ),
        ),
    ];
    for (id, expected, scope) in bad {
        let error = res
            .capture_export(id, &request(scope), |_| ControlFlow::Continue(()))
            .unwrap_err();
        match expected {
            Expected::Conflict => assert!(
                matches!(error, StoreError::Conflict { .. }),
                "{id}: {error:?}"
            ),
            Expected::NotFound => assert!(
                matches!(error, StoreError::NotFound { .. }),
                "{id}: {error:?}"
            ),
        }
        assert_eq!(
            res.read(|tx| export_job(tx, id)).unwrap().state(),
            ExportJobState::Capturing
        );
    }
    let empty = request(ExportScope::Selection(
        ExportSelection::new("empty".into(), 1, 1, Arc::from([])).unwrap(),
    ));
    assert_eq!(
        capture(&res, "empty", &empty).state(),
        ExportJobState::Ready
    );
    let kept = records(&res, "empty");
    assert!(ids(&kept).is_empty());
    assert_eq!(kept[0]["ids_sha256"], EMPTY_HASH);
    assert!(ExportSelection::new("duplicate".into(), 1, 1, Arc::from([1, 1])).is_err());
    assert!(ExportSelection::new(String::new(), 1, 1, Arc::from([])).is_err());
    assert!(ExportSelection::new("zero-window".into(), 0, 1, Arc::from([])).is_err());
    let target = request(mailbox(&res));
    let now = res.now();
    let result = res.capture_export("cancel", &target, |completed| {
        if completed == 1 {
            let job = res.read(|tx| export_job(tx, "cancel")).unwrap();
            write(&res, move |tx| {
                advance_export_job(
                    tx,
                    "cancel",
                    job.revision(),
                    ExportJobState::Cancelled,
                    "{\"v\":1}",
                    now,
                )
            });
        }
        ControlFlow::Continue(())
    });
    assert!(matches!(result, Err(StoreError::Conflict { .. })));
    assert_eq!(
        res.read(|tx| export_job(tx, "cancel")).unwrap().state(),
        ExportJobState::Cancelled
    );
    let res = Arc::new(res);
    let inside = Arc::clone(&res);
    write(&res, move |_tx| {
        assert!(matches!(
            inside.capture_export("writer-thread", &target, |_| ControlFlow::Continue(())),
            Err(StoreError::Internal { .. })
        ));
        Ok(())
    });
}

#[test]
fn capture_preserves_real_sqlite_full_through_serde_and_rolls_back() {
    let (_scratch, res, _) = fixture();
    let note = large_note();
    write(&res, move |tx| {
        tx.execute("UPDATE notes SET body_source=?1", [note])
            .unwrap();
        Ok(())
    });
    let req = request(mailbox(&res));
    assert!(matches!(
        res.capture_export("full", &req, |_| ControlFlow::Break(()))
            .unwrap(),
        Scoped::Stopped
    ));
    write(&res, |tx| {
        let pages: i64 = tx
            .query_row("PRAGMA page_count", [], |row| row.get(0))
            .unwrap();
        tx.pragma_update(None, "max_page_count", pages).unwrap();
        Ok(())
    });
    let mut progress = Vec::new();
    let error = res
        .capture_export("full", &req, |count| {
            progress.push(count);
            ControlFlow::Continue(())
        })
        .unwrap_err();
    assert!(matches!(error, StoreError::Unavailable { .. }), "{error:?}");
    assert_eq!(error.io_kind(), Some(std::io::ErrorKind::StorageFull));
    assert_eq!(progress, [0]);
    let kept = res.read(|tx| export_job(tx, "full")).unwrap();
    assert_eq!(kept.state(), ExportJobState::Capturing);
    assert_eq!((kept.revision(), kept.captured_chunks()), (1, 0));
}

#[test]
fn long_intent_stays_out_of_the_bounded_request() {
    let (_scratch, res, _) = fixture();
    let destination = format!("/{}", "d".repeat(MAX_EXPORT_JSON_BYTES + 1));
    let request =
        ExportCaptureRequest::new(ExportFormat::Eml, destination.clone(), mailbox(&res)).unwrap();
    let job = capture(&res, "long-intent", &request);
    assert!(job.request_json().len() < MAX_EXPORT_JSON_BYTES);
    assert_eq!(
        records(&res, "long-intent")[0]["intent"]["destination"],
        destination
    );
    assert!(
        ExportCaptureRequest::new(ExportFormat::Eml, "relative".into(), mailbox(&res)).is_err()
    );
}
