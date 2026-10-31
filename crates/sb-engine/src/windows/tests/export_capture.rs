//! Real handle capture through the native Engine-to-Resources adapter.

use std::collections::BTreeSet;
use std::ops::ControlFlow;

use sb_resources::{
    ExportCaptureRequest, ExportFormat, ExportJobState, ExportScope, MAX_EXPORT_CHUNK_BYTES,
    MAX_EXPORT_CHUNKS, Scoped, WritePriority, export_chunks, refresh_list_rows,
};
use sb_test_support::{ScratchDir, block_on};
use serde_json::Value;

use crate::{Anchor, Engine, EngineError, SelectionId, SelectionInput, WindowSpec};

const MEMBERS: usize = 6_001;
const BITS_PER_BYTE: usize = u8::BITS as usize;

fn seeded(label: &str, count: usize) -> (ScratchDir, Engine) {
    let scratch = ScratchDir::new(label);
    let engine = block_on(Engine::open(scratch.path().join("store"))).unwrap();
    let account: String = engine
        .resources()
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
    block_on(engine.resources().write(WritePriority::User, move |tx| {
        tx.execute("INSERT INTO mailboxes (mailbox_id,account_id,remote_name,kind,selectable,sync_state) VALUES (1,?1,'Inbox','folder',1,'synced')", [&account]).unwrap();
        for id in 1..=count as i64 {
            tx.execute("INSERT INTO logical_messages (message_id,account_id,created_at) VALUES (?1,?2,1)", (id,&account)).unwrap();
            tx.execute("INSERT INTO occurrences (occurrence_id,account_id,mailbox_id,message_id,internal_date,keywords,state,first_seen_at,last_seen_at) VALUES (?1,?2,1,?1,?1,'[]','present',1,1)", (id,&account)).unwrap();
        }
        refresh_list_rows(tx, &(1..=count as i64).collect::<BTreeSet<_>>())?;
        Ok(())
    })).unwrap();
    (scratch, engine)
}

fn all(count: usize) -> SelectionInput {
    let mut bits = vec![u8::MAX; count.div_ceil(BITS_PER_BYTE)];
    if !count.is_multiple_of(BITS_PER_BYTE) {
        *bits.last_mut().unwrap() = (1 << (count % BITS_PER_BYTE)) - 1;
    }
    SelectionInput::RankBitmap(bits)
}

#[test]
fn large_real_selection_leases_exact_members_after_window_change_and_close() {
    let (_scratch, engine) = seeded("real-export-selection", MEMBERS);
    let window = block_on(engine.open_window(WindowSpec::Mailbox(1), Anchor::Top, 1)).unwrap();
    let index = block_on(engine.window_ids(window.window_id())).unwrap();
    let expected: Vec<i64> = (1..=MEMBERS as i64).rev().collect();
    assert_eq!(index.ids(), expected);
    let snapshot =
        block_on(engine.open_selection(window.window_id(), index.generation(), all(MEMBERS)))
            .unwrap();
    let lease = block_on(engine.selection_for_export(snapshot.selection_id().clone())).unwrap();
    assert_eq!(lease.count(), MEMBERS as u64);
    assert_eq!(lease.window_id(), window.window_id().get());
    assert_eq!(lease.generation(), index.generation());
    assert_eq!(lease.selection_id(), snapshot.selection_id().as_str());
    block_on(engine.close_window(window.window_id()));
    assert_eq!(
        block_on(engine.selection_for_export(snapshot.selection_id().clone()))
            .unwrap()
            .count(),
        MEMBERS as u64
    );

    let vanished = expected[0];
    block_on(engine.resources().write(WritePriority::User, move |tx| {
        tx.execute(
            "UPDATE occurrences SET state='vanished' WHERE occurrence_id=?1",
            [vanished],
        )
        .unwrap();
        refresh_list_rows(tx, &BTreeSet::from([vanished]))?;
        Ok(())
    }))
    .unwrap();
    let request = ExportCaptureRequest::new(
        ExportFormat::Eml,
        "/synthetic/selection-export".into(),
        ExportScope::Selection(lease),
    )
    .unwrap();
    let Scoped::Done(job) = engine
        .resources()
        .capture_export("large-real-selection", &request, |_| {
            ControlFlow::Continue(())
        })
        .unwrap()
    else {
        panic!("real selection stopped")
    };
    assert_eq!(job.state(), ExportJobState::Ready);
    let mut bytes = Vec::new();
    for from in (0..job.captured_chunks()).step_by(MAX_EXPORT_CHUNKS) {
        bytes.extend(
            engine
                .resources()
                .read(|tx| export_chunks(tx, job.id(), from, MAX_EXPORT_CHUNKS as u32))
                .unwrap()
                .into_iter()
                .flatten(),
        );
    }
    assert!(bytes.len() > MAX_EXPORT_CHUNK_BYTES * MAX_EXPORT_CHUNKS);
    let records: Vec<Value> = bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect();
    assert_eq!(records[0]["entries"], MEMBERS);
    assert_eq!(records[1]["occurrence_state"], "vanished");
    assert_eq!(
        records[1..]
            .iter()
            .map(|entry| entry["occurrence_id"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        expected
    );
    block_on(engine.close_selection(snapshot.selection_id().clone()));
    assert!(matches!(
        block_on(engine.selection_for_export(snapshot.selection_id().clone())),
        Err(EngineError::NotFound { .. })
    ));
    assert!(matches!(
        block_on(engine.selection_for_export(SelectionId::from("malformed".to_owned()))),
        Err(EngineError::NotFound { .. })
    ));

    let (_foreign_scratch, foreign) = seeded("foreign-export-selection", 1);
    let foreign_window =
        block_on(foreign.open_window(WindowSpec::Mailbox(1), Anchor::Top, 1)).unwrap();
    let foreign_index = block_on(foreign.window_ids(foreign_window.window_id())).unwrap();
    let foreign_snapshot = block_on(foreign.open_selection(
        foreign_window.window_id(),
        foreign_index.generation(),
        all(1),
    ))
    .unwrap();
    assert!(matches!(
        block_on(engine.selection_for_export(foreign_snapshot.selection_id().clone())),
        Err(EngineError::NotFound { .. })
    ));
    block_on(foreign.close_selection(foreign_snapshot.selection_id().clone()));
    block_on(foreign.shutdown()).unwrap();
    block_on(engine.shutdown()).unwrap();
}
