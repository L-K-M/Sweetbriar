//! Driven reads (spec/06, M1.6r2) against full reads of the same snapshot:
//! identical pages, cursors, unknown counts and id indexes, a restriction
//! only when coverage proves it exact, and the stated error difference.

use super::*;
use crate::lists::PageRead;
use crate::query::{
    CoverageCounts, Kept, SearchAnchor, SearchPage, read_search_coverage_reusing,
    read_search_date_page, read_search_relevance_page, search_date_ids, search_relevance_ids,
};

/// Plain indexed messages that match no case's terms, so kept sets stay
/// smaller than every scope.
const FILLER: i64 = 30;

/// Messages 1–9 cover every FTS state, 10 onwards are filler and one more
/// message shares acquisition 1. The Archive holds 1, 3, 6, the sharer and
/// the filler.
fn store() -> (Connection, SearchScope) {
    let conn = fixture(FILLER as usize);
    doc(
        &conn,
        1,
        "indexed",
        "[]",
        &[
            ("subject", "alpha"),
            ("body_authored", "gamma"),
            ("to_text", "delta"),
        ],
    );
    doc(
        &conn,
        2,
        "indexed",
        "[]",
        &[
            ("subject", "beta"),
            ("body_authored", "gamma"),
            ("cc_text", "delta"),
            ("attachment_names", "delta"),
        ],
    );
    // Subject truncated: unknown for subject and text, usable for body.
    doc(
        &conn,
        3,
        "partial",
        "[\"subject\"]",
        &[("subject", "alp"), ("body_authored", "alpha")],
    );
    doc(
        &conn,
        4,
        "partial",
        "[\"body_authored\"]",
        &[("subject", "alpha zeta")],
    );
    doc(&conn, 5, "failed", "[]", &[]);
    doc(&conn, 8, "indexed", "[]", &[("subject", "alpha")]);
    doc(
        &conn,
        9,
        "indexed",
        "[]",
        &[("subject", "zeta alpha"), ("body_authored", "gamma")],
    );
    for id in 10..=FILLER {
        doc(
            &conn,
            id,
            "indexed",
            "[]",
            &[("subject", "filler"), ("body_authored", "plain")],
        );
    }
    edit(
        &conn,
        "UPDATE occurrences SET f_seen=1 WHERE message_id IN (1,3,9,12);
         UPDATE logical_messages SET acquisition_id=NULL WHERE message_id=7;
         UPDATE acquisitions SET object_state='missing' WHERE acquisition_id=8",
    );
    let account = archive_account(&conn);
    let archive = mailbox(&conn, &account, "Archive", "folder", None);
    committed(&conn, |tx| {
        // 6 waits for its first document; 2's re-extraction keeps it usable.
        index::enqueue(
            tx,
            &[
                (6, index::IndexReason::New),
                (2, index::IndexReason::Reextract),
            ],
            crate::Timestamp::from_millis(FIXED_NOW_MS),
        )?;
        let shared = message(tx, &account, Some(1));
        for id in [1, 3, 6, shared].into_iter().chain(10..=FILLER) {
            occurrence(tx, archive, id);
        }
        Ok(())
    });
    (conn, SearchScope::Mailbox(archive))
}

fn committed<T>(conn: &Connection, body: impl FnOnce(&Transaction) -> Result<T, StoreError>) -> T {
    let tx = conn.unchecked_transaction().unwrap();
    let result = body(&tx).unwrap();
    tx.commit().unwrap();
    result
}

fn counted(tx: &Transaction, scope: &SearchScope) -> CoverageCounts {
    read_search_coverage_reusing(tx, scope.clone(), None)
        .unwrap()
        .1
}

/// Whether a read with `coverage` as proof is restricted.
fn driven(tx: &Transaction, node: &Json, scope: &SearchScope, coverage: &CoverageCounts) -> bool {
    let full = compile(tx, &query(node.clone()), scope.clone()).unwrap();
    let plan = compile_driven(tx, &query(node.clone()), scope.clone(), Some(coverage)).unwrap();
    plan.source != full.source
}

#[derive(Clone, Copy, Debug)]
enum Order {
    Date,
    Relevance,
}

fn page(
    tx: &Transaction,
    node: &Json,
    scope: &SearchScope,
    order: Order,
    anchor: SearchAnchor,
    kept: Kept<'_>,
) -> Result<PageRead<SearchPage>, StoreError> {
    let (query, scope) = (query(node.clone()), scope.clone());
    match order {
        Order::Date => read_search_date_page(tx, &query, scope, anchor, 3, None, kept),
        Order::Relevance => read_search_relevance_page(tx, &query, scope, anchor, 3, None, kept),
    }
}

fn ids(
    tx: &Transaction,
    node: &Json,
    scope: &SearchScope,
    order: Order,
    coverage: Option<&CoverageCounts>,
) -> Result<PageRead<(u64, Vec<i64>)>, StoreError> {
    let (query, scope) = (query(node.clone()), scope.clone());
    match order {
        Order::Date => search_date_ids(tx, &query, scope, None, coverage),
        Order::Relevance => search_relevance_ids(tx, &query, scope, None, coverage),
    }
}

/// Every read of `node` gives the same result with and without the proof.
#[track_caller]
fn same(tx: &Transaction, node: &Json, scope: &SearchScope, coverage: &CoverageCounts) {
    let proven = Kept {
        coverage: Some(coverage),
        ..Kept::default()
    };
    for order in [Order::Date, Order::Relevance] {
        let context = format!("{node} {scope:?} {order:?}");
        let full = page(tx, node, scope, order, SearchAnchor::Top, Kept::default()).unwrap();
        assert_eq!(
            page(tx, node, scope, order, SearchAnchor::Top, proven).unwrap(),
            full,
            "{context}"
        );
        let full_ids = ids(tx, node, scope, order, None).unwrap();
        assert_eq!(
            ids(tx, node, scope, order, Some(coverage)).unwrap(),
            full_ids,
            "{context}"
        );
        let PageRead::Ready(first) = full else {
            continue;
        };
        let Some(last) = first.rows().last() else {
            continue;
        };
        let after = SearchAnchor::After(last.cursor().to_owned());
        assert_eq!(
            page(tx, node, scope, order, after.clone(), proven).unwrap(),
            page(tx, node, scope, order, after, Kept::default()).unwrap(),
            "{context} after"
        );
        let before = SearchAnchor::Before(last.cursor().to_owned());
        assert_eq!(
            page(tx, node, scope, order, before.clone(), proven).unwrap(),
            page(tx, node, scope, order, before, Kept::default()).unwrap(),
            "{context} before"
        );
        let around = SearchAnchor::Around(first.rows()[0].row().occurrence_id);
        assert_eq!(
            page(tx, node, scope, order, around.clone(), proven).unwrap(),
            page(tx, node, scope, order, around, Kept::default()).unwrap(),
            "{context} around"
        );
    }
}

#[test]
fn driven_reads_equal_full_reads_over_leaves_compositions_classes_and_scopes() {
    let (conn, archive) = store();
    let subject = |value| field("subject", "contains", value);
    let body = |value| field("body", "contains", value);
    // (tree, whether it has a driver)
    let cases = [
        (json!({"text":"alpha"}), true),
        (subject("alpha"), true),
        (body("gamma"), true),
        (json!({"text":"nomatch"}), true),
        (json!({"and":[{"text":"alpha"},{"flag":"seen"}]}), true),
        (
            json!({"and":[{"text":"alpha"},{"not":body("gamma")}]}),
            true,
        ),
        (
            json!({"and":[{"and":[{"text":"alpha"},{"flag":"seen"}]},{"text":"zeta"}]}),
            true,
        ),
        (
            json!({"and":[{"text":"alpha"},{"text":"alpha"},subject("zeta")]}),
            true,
        ),
        (
            json!({"and":[{"text":"alpha"},field("from","is","x@y.test")]}),
            true,
        ),
        (json!({"or":[{"text":"alpha"},{"flag":"seen"}]}), false),
        (json!({"not":{"text":"alpha"}}), false),
        (json!({"not":{"not":{"text":"alpha"}}}), false),
        (
            json!({"and":[{"or":[{"text":"alpha"},{"text":"beta"}]},{"flag":"seen"}]}),
            false,
        ),
        (json!({"and":[{"flag":"seen"}]}), false),
        (field("to", "contains", "delta"), true),
        (field("cc", "contains", "delta"), true),
        (field("attachment", "contains", "delta"), true),
    ];
    let account = SearchScope::Account(archive_account(&conn).parse().unwrap());
    let tx = conn.unchecked_transaction().unwrap();
    for scope in [SearchScope::All, account, archive] {
        let coverage = counted(&tx, &scope);
        assert_eq!(coverage.forbidden(), 0, "{scope:?}");
        for (node, driver) in &cases {
            assert_eq!(
                driven(&tx, node, &scope, &coverage),
                *driver,
                "{node} {scope:?}"
            );
            same(&tx, node, &scope, &coverage);
        }
    }
}

#[test]
fn each_guard_and_a_stale_proof_fall_back_to_the_full_read() {
    let alpha = json!({"text":"alpha"});
    let (conn, _) = store();
    let account = archive_account(&conn);
    let tiny = mailbox(&conn, &account, "Tiny", "folder", None);
    committed(&conn, |tx| {
        occurrence(tx, tiny, 11);
        occurrence(tx, tiny, 12);
        Ok(())
    });
    let tx = conn.unchecked_transaction().unwrap();
    let all = counted(&tx, &SearchScope::All);
    assert!(driven(&tx, &alpha, &SearchScope::All, &all));

    // Another scope's, generation's or target's counts prove nothing.
    let inbox = SearchScope::Mailbox(1);
    assert!(!driven(&tx, &alpha, &inbox, &all));
    tx.execute_batch("UPDATE store_meta SET value=value+1 WHERE key='search_generation'")
        .unwrap();
    assert!(!driven(&tx, &alpha, &SearchScope::All, &all));
    let all = counted(&tx, &SearchScope::All);
    assert!(driven(&tx, &alpha, &SearchScope::All, &all));

    // A kept set, counted store-wide, as large as the scope.
    let tiny = SearchScope::Mailbox(tiny);
    let small = counted(&tx, &tiny);
    assert!(!driven(&tx, &alpha, &tiny, &small));
    same(&tx, &alpha, &tiny, &small);

    // An unqueued document at another extractor version; queued, as during
    // a re-index, the kept set holds it.
    tx.execute(
        "UPDATE d_search_docs SET extractor_version=2 WHERE doc_id=11",
        [],
    )
    .unwrap();
    assert!(!driven(&tx, &alpha, &SearchScope::All, &all));
    same(&tx, &alpha, &SearchScope::All, &all);
    index::enqueue(
        &tx,
        &[(11, index::IndexReason::Reextract)],
        crate::Timestamp::from_millis(FIXED_NOW_MS),
    )
    .unwrap();
    let all = counted(&tx, &SearchScope::All);
    assert!(driven(&tx, &alpha, &SearchScope::All, &all));
    same(&tx, &alpha, &SearchScope::All, &all);
    tx.commit().unwrap();

    // The forbidden classes: a published object with neither a document nor
    // a queue row, and a dangling acquisition reference.
    for damage in [
        "DELETE FROM d_search_docs WHERE doc_id=12",
        "PRAGMA foreign_keys=OFF; UPDATE logical_messages SET acquisition_id=999 WHERE message_id=12",
    ] {
        let (conn, _) = store();
        conn.execute_batch(damage).unwrap();
        let tx = conn.unchecked_transaction().unwrap();
        let all = counted(&tx, &SearchScope::All);
        assert_eq!(all.forbidden(), 1, "{damage}");
        assert!(!driven(&tx, &alpha, &SearchScope::All, &all), "{damage}");
        same(&tx, &alpha, &SearchScope::All, &all);
    }
}

#[test]
fn malformed_masks_fail_both_reads_and_skipped_decoder_corruption_fails_only_the_full_read() {
    let (conn, _) = store();
    let scope = SearchScope::All;

    // Filler 13 never matches "alpha", yet its malformed mask is kept and
    // rejected inside the predicate, as the full read rejects it.
    conn.execute(
        "UPDATE d_search_docs SET truncated_fields='[\"bogus\"]' WHERE doc_id=13",
        [],
    )
    .unwrap();
    let tx = conn.unchecked_transaction().unwrap();
    let coverage = counted(&tx, &scope);
    let alpha = json!({"text":"alpha"});
    assert!(driven(&tx, &alpha, &scope, &coverage));
    let proven = Kept {
        coverage: Some(&coverage),
        ..Kept::default()
    };
    for kept in [Kept::default(), proven] {
        assert!(matches!(
            page(&tx, &alpha, &scope, Order::Date, SearchAnchor::Top, kept),
            Err(StoreError::Corrupt { .. })
        ));
    }
    tx.commit().unwrap();

    // Filler 14's sender cannot be decoded. A full read that evaluates the
    // address leaf first decodes it (SQLite stops at a settled conjunct, so
    // order matters); a driven read skips 14, which "alpha" proves false.
    conn.execute_batch(
        "UPDATE d_search_docs SET truncated_fields='[]' WHERE doc_id=13;
         INSERT INTO message_headers (message_id,raw_fields,from_json,fetched_at)
         VALUES (14,x'01','not json',1)",
    )
    .unwrap();
    let tx = conn.unchecked_transaction().unwrap();
    let coverage = counted(&tx, &scope);
    let node = json!({"and":[field("from","is","x@y.test"),{"text":"alpha"}]});
    let proven = Kept {
        coverage: Some(&coverage),
        ..Kept::default()
    };
    assert!(matches!(
        page(
            &tx,
            &node,
            &scope,
            Order::Date,
            SearchAnchor::Top,
            Kept::default()
        ),
        Err(StoreError::Corrupt { .. })
    ));
    assert!(page(&tx, &node, &scope, Order::Date, SearchAnchor::Top, proven).is_ok());
}

/// One message's stored summary and its address keys, as an import writes
/// them (02).
fn summarize(
    conn: &Connection,
    id: i64,
    state: &str,
    version: i64,
    from: Option<&str>,
    to: Option<&str>,
) {
    conn.execute(
        "INSERT INTO d_message_summary (acquisition_id, parse_state, parser_version, from_addr,
         to_json, has_attachments) VALUES (?1, ?2, ?3, ?4, ?5, 0)",
        params![id, state, version, from, to],
    )
    .unwrap();
    committed(conn, |tx| {
        let columns = crate::address_keys::Columns {
            from_addr: from,
            from_name: None,
            to_json: to,
            cc_json: None,
        };
        crate::address_keys::write(tx, id, crate::address_keys::SUMMARY_TARGET, &columns)
    });
}

/// Bind `message` to a completed parser-2 assembly of its acquisition.
fn bind(conn: &Connection, message: i64, from: &str, cc: Option<&str>) {
    bind_at(conn, message, 2, from, cc);
}

/// Bind `message` to a completed assembly of its acquisition at `target`.
fn bind_at(conn: &Connection, message: i64, target: u32, from: &str, cc: Option<&str>) {
    committed(conn, |tx| {
        let acquisition: i64 = tx
            .query_row(
                "SELECT acquisition_id FROM logical_messages WHERE message_id=?1",
                [message],
                |row| row.get(0),
            )
            .unwrap();
        let repaired = crate::imports::ImportSummary {
            parser_version: target,
            parse_state: crate::imports::ImportParseState::Parsed,
            from_addr: Some(from.into()),
            cc_json: cc.map(str::to_owned),
            ..crate::imports::ImportSummary::default()
        };
        let capture = crate::repair::begin(tx, acquisition, target)?.unwrap();
        let prepared = crate::repair::prepare(capture, repaired)?;
        crate::repair::publish(tx, prepared, crate::Timestamp::from_millis(1))?;
        tx.execute(
            "INSERT INTO d_message_bindings VALUES (?1, ?2, ?3, 1)",
            params![message, acquisition, target],
        )
        .unwrap();
        Ok(())
    });
}

/// Every address source shape (spec/06 Predicate reads), then filler; the
/// Archive holds 1, 3, 7, 8, 10, the sharer and the filler.
fn addressed() -> (Connection, SearchScope) {
    let conn = fixture(FILLER as usize);
    let ada = Some("ada@example.test");
    let to = |list: &str| Some(list.to_owned());
    summarize(
        &conn,
        1,
        "ok",
        2,
        ada,
        to(r#"[{"address":"bob@other.test","name":null}]"#).as_deref(),
    );
    summarize(
        &conn,
        2,
        "ok",
        2,
        Some("carol@example.test"),
        to(r#"[{"address":"ada@example.test","name":null},{"name":"Name only","address":null}]"#)
            .as_deref(),
    );
    summarize(&conn, 3, "partial", 2, ada, None);
    summarize(&conn, 4, "ok", 1, ada, None);
    summarize(&conn, 5, "failed", 2, None, None);
    summarize(&conn, 6, "ok", 2, Some("dan@example.test"), None);
    summarize(&conn, 8, "ok", 2, Some("zed@else.test"), None);
    summarize(&conn, 9, "ok", 2, ada, None);
    // Legacy summaries that repairs bind to parser-2 assemblies below.
    summarize(&conn, 10, "ok", 1, Some("zed@else.test"), None);
    summarize(&conn, 11, "ok", 1, ada, None);
    for id in 12..=FILLER {
        summarize(
            &conn,
            id,
            "ok",
            2,
            Some(&format!("filler{id}@filler.test")),
            None,
        );
    }
    edit(
        &conn,
        r#"UPDATE occurrences SET f_seen=1 WHERE message_id IN (1,2,8,10);
           UPDATE logical_messages SET acquisition_id=NULL WHERE message_id=7;
           INSERT INTO message_headers (message_id, raw_fields, from_json, fetched_at) VALUES
             (8, x'01', '[{"address":"ada@example.test","name":null}]', 1),
             (9, x'01', '[]', 1)"#,
    );
    bind(&conn, 10, "ada@example.test", None);
    bind(
        &conn,
        11,
        "zed@else.test",
        Some(r#"[{"name":"Name only","address":null}]"#),
    );
    let account = archive_account(&conn);
    let archive = mailbox(&conn, &account, "Archive", "folder", None);
    committed(&conn, |tx| {
        let shared = message(tx, &account, Some(1));
        for id in [1, 3, 7, 8, 10, shared].into_iter().chain(12..=FILLER) {
            occurrence(tx, archive, id);
        }
        Ok(())
    });
    (conn, SearchScope::Mailbox(archive))
}

#[test]
fn address_driven_reads_equal_full_reads_over_every_source() {
    let (conn, archive) = addressed();
    let is = |field, value| super::field(field, "is", value);
    let domain = |field, value| json!({"field":field,"op":"domain","value":value});
    let cases = [
        is("from", "ada@example.test"),
        is("from", "Ada@example.test"),
        domain("from", "example.test"),
        domain("from", "EXAMPLE.test"),
        is("to", "ada@example.test"),
        domain("to", "other.test"),
        is("cc", "ada@example.test"),
        is("from", "nobody@nowhere.test"),
        json!({"and":[is("from", "ada@example.test"), {"flag":"seen"}]}),
        json!({"and":[is("from", "ada@example.test"), domain("to", "other.test")]}),
        json!({"and":[{"and":[domain("from", "example.test")]}, {"not":is("to", "bob@other.test")}]}),
        // No message here has a document, so FTS leaves cannot drive; the
        // address leaf beside one still does.
        json!({"and":[{"text":"anything"}, is("from", "ada@example.test")]}),
    ];
    let undriven = [
        json!({"or":[is("from", "ada@example.test"), {"flag":"seen"}]}),
        json!({"not":is("from", "ada@example.test")}),
    ];
    let tx = conn.unchecked_transaction().unwrap();
    for scope in [SearchScope::All, archive] {
        let coverage = counted(&tx, &scope);
        assert_eq!(coverage.summaryless(), 0, "{scope:?}");
        for node in &cases {
            assert!(driven(&tx, node, &scope, &coverage), "{node} {scope:?}");
            same(&tx, node, &scope, &coverage);
        }
        for node in &undriven {
            assert!(!driven(&tx, node, &scope, &coverage), "{node} {scope:?}");
            same(&tx, node, &scope, &coverage);
        }
    }
}

#[test]
fn a_message_without_any_address_source_stops_address_leaves_driving() {
    let (conn, _) = addressed();
    let ada = super::field("from", "is", "ada@example.test");
    conn.execute("DELETE FROM d_message_summary WHERE acquisition_id=6", [])
        .unwrap();
    let tx = conn.unchecked_transaction().unwrap();
    let all = counted(&tx, &SearchScope::All);
    assert_eq!(all.summaryless(), 1);
    assert!(!driven(&tx, &ada, &SearchScope::All, &all));
    same(&tx, &ada, &SearchScope::All, &all);
}

/// Indexed documents and usable summaries for every message, so FTS and
/// address leaves can drive together: 1–5 say "alpha", 1 and 2 are from
/// Ada, 3 copies cc@example.test, and 12 has no summary but a binding to
/// a parser-1 assembly.
fn both() -> Connection {
    let conn = fixture(FILLER as usize);
    for id in 1..=FILLER {
        let subject = if id <= 5 { "alpha" } else { "plain" };
        doc(&conn, id, "indexed", "[]", &[("subject", subject)]);
        if id == 12 {
            continue;
        }
        let from = if id <= 2 {
            "ada@example.test".to_owned()
        } else {
            format!("filler{id}@filler.test")
        };
        summarize(&conn, id, "ok", 2, Some(&from), None);
    }
    edit(
        &conn,
        r#"UPDATE d_message_summary SET cc_json='[{"address":"cc@example.test","name":null}]'
           WHERE acquisition_id=3;
           INSERT INTO d_address_keys VALUES ('cc','cc@example.test',3,0,'example.test')"#,
    );
    // A store whose repair target was once parser 1 kept such bindings.
    edit(
        &conn,
        "UPDATE store_meta SET value='1' WHERE key='mime_repair_target'",
    );
    bind_at(&conn, 12, 1, "old@example.test", None);
    edit(
        &conn,
        "UPDATE store_meta SET value='2' WHERE key='mime_repair_target'",
    );
    conn
}

#[test]
fn fts_and_address_drivers_intersect_and_each_keeps_its_own_guard() {
    let conn = both();
    let is = |field, value| super::field(field, "is", value);
    let domain = |field, value| json!({"field":field,"op":"domain","value":value});
    let cases = [
        json!({"and":[{"text":"alpha"}, is("from", "ada@example.test")]}),
        json!({"and":[{"text":"alpha"}, domain("cc", "example.test")]}),
        domain("cc", "example.test"),
        is("cc", "cc@example.test"),
        // 12's parser-1 assembly leaves its leaf unknown.
        is("from", "old@example.test"),
        domain("from", "example.test"),
    ];
    let tx = conn.unchecked_transaction().unwrap();
    let all = counted(&tx, &SearchScope::All);
    assert_eq!((all.forbidden(), all.summaryless()), (0, 0));
    for node in &cases {
        assert!(driven(&tx, node, &SearchScope::All, &all), "{node}");
        same(&tx, node, &SearchScope::All, &all);
    }
    tx.commit().unwrap();

    // Without any address source for one message, only the FTS leaf drives.
    conn.execute("DELETE FROM d_message_summary WHERE acquisition_id=20", [])
        .unwrap();
    let tx = conn.unchecked_transaction().unwrap();
    let all = counted(&tx, &SearchScope::All);
    assert_eq!(all.summaryless(), 1);
    let mixed = &cases[0];
    assert!(driven(&tx, mixed, &SearchScope::All, &all));
    same(&tx, mixed, &SearchScope::All, &all);
    let address = is("from", "ada@example.test");
    assert!(!driven(&tx, &address, &SearchScope::All, &all));
}
