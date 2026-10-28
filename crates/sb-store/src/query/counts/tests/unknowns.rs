//! Class-restricted unknown counts (spec/06, M1.6r3b) against evaluating
//! counts of the same snapshot: identical pages and counts, a restriction
//! only under each class's guard, and the same corruption failures.

use super::driven::{Order, addressed, both, counted, page, same, store};
use super::*;
use crate::query::{CoverageCounts, Kept, SearchAnchor, search_date_ids, search_relevance_ids};

/// Whether a read of `node` with `coverage` as proof counts unknowns over
/// classes only. For trees without a driver: a driven read also skips.
fn by_class(tx: &Transaction, node: &Json, scope: &SearchScope, coverage: &CoverageCounts) -> bool {
    let full = compile(tx, &query(node.clone()), scope.clone()).unwrap();
    let plan = compile_driven(tx, &query(node.clone()), scope.clone(), Some(coverage)).unwrap();
    assert_eq!(plan.hit_source, full.hit_source, "{node} is driven");
    plan.unknown_source.as_ref() != Some(&plan.source)
}

fn is(field: &str, value: &str) -> Json {
    super::field(field, "is", value)
}

/// Undriven trees over every kind of leaf, and the guards each needs.
fn cases() -> Vec<(Json, Guard)> {
    let seen = json!({"flag":"seen"});
    vec![
        (seen.clone(), Guard::None),
        (json!({"not":{"has":"note"}}), Guard::None),
        (json!({"size":{"op":"lt","bytes":15}}), Guard::References),
        (
            json!({"or":[{"size":{"op":"gt","bytes":0}}, seen]}),
            Guard::References,
        ),
        (json!({"or":[{"text":"alpha"}, seen]}), Guard::Fts),
        (json!({"not":{"text":"alpha"}}), Guard::Fts),
        (
            json!({"not":{"field":"body","op":"contains","value":"gamma"}}),
            Guard::Fts,
        ),
        (json!({"has":"attachment"}), Guard::Address),
        (json!({"not":{"has":"attachment"}}), Guard::Address),
        (
            json!({"or":[is("from", "ada@example.test"), seen]}),
            Guard::Address,
        ),
        (json!({"not":is("to", "bob@other.test")}), Guard::Address),
        (
            json!({"or":[{"text":"alpha"}, is("cc", "cc@example.test")]}),
            Guard::Both,
        ),
        (
            json!({"or":[{"not":{"has":"attachment"}}, {"text":"alpha"}]}),
            Guard::Both,
        ),
    ]
}

#[derive(Clone, Copy, Debug)]
enum Guard {
    None,
    Fts,
    Address,
    Both,
    /// No forbidden class, which a dangling reference would be.
    References,
}

impl Guard {
    fn holds(self, coverage: &CoverageCounts) -> bool {
        let fts = coverage.forbidden() == 0;
        let address = coverage.summaryless() == 0;
        match self {
            Self::None => true,
            Self::Fts => fts,
            Self::Address => address,
            Self::Both => fts && address,
            Self::References => coverage.forbidden() == 0,
        }
    }
}

/// Every case counts by class exactly when its guards hold, and reads the
/// same with and without the proof, in every scope.
#[track_caller]
fn check_cases(conn: &Connection, scopes: &[SearchScope], extra: &[Json]) {
    let tx = conn.unchecked_transaction().unwrap();
    for scope in scopes {
        let coverage = counted(&tx, scope);
        for (node, guard) in cases() {
            assert_eq!(
                by_class(&tx, &node, scope, &coverage),
                guard.holds(&coverage),
                "{node} {scope:?} {guard:?}"
            );
            same(&tx, &node, scope, &coverage);
        }
        // Driven trees count their kept classes only.
        for node in extra {
            same(&tx, node, scope, &coverage);
        }
    }
}

#[test]
fn class_restricted_counts_equal_evaluating_counts_over_every_leaf_kind() {
    let driven = [
        json!({"and":[{"text":"alpha"}, {"has":"attachment"}]}),
        json!({"and":[{"text":"alpha"}, {"not":{"size":{"op":"lt","bytes":15}}}]}),
        json!({"and":[is("from", "ada@example.test"), {"or":[{"text":"alpha"}, {"flag":"seen"}]}]}),
    ];

    // Every FTS state; no summaries, so address classes cannot count.
    let (conn, archive) = store();
    let account = SearchScope::Account(archive_account(&conn).parse().unwrap());
    check_cases(&conn, &[SearchScope::All, account, archive], &driven);

    // Every address source; no documents, so FTS classes cannot count.
    let (conn, archive) = addressed();
    check_cases(&conn, &[SearchScope::All, archive], &driven);

    // A header row's name-only sender is unknown however usable the
    // message's summary is.
    conn.execute(
        "INSERT INTO message_headers (message_id,raw_fields,from_json,fetched_at)
         VALUES (15,x'01','[{\"name\":\"Only\",\"address\":null}]',1)",
        [],
    )
    .unwrap();
    check_cases(&conn, &[SearchScope::All], &driven);

    // Both guards hold.
    check_cases(&both(), &[SearchScope::All], &driven);
}

#[test]
fn another_snapshots_coverage_counts_every_candidate_unless_none_can_be_unknown() {
    let (conn, _) = store();
    let tx = conn.unchecked_transaction().unwrap();
    let coverage = counted(&tx, &SearchScope::All);
    let text = json!({"or":[{"text":"alpha"}, {"flag":"seen"}]});
    let seen = json!({"flag":"seen"});
    assert!(by_class(&tx, &text, &SearchScope::All, &coverage));
    tx.execute_batch("UPDATE store_meta SET value=value+1 WHERE key='search_generation'")
        .unwrap();
    assert!(!by_class(&tx, &text, &SearchScope::All, &coverage));
    same(&tx, &text, &SearchScope::All, &coverage);

    // No leaf of a flag tree can be unknown, so it needs no proof at all.
    assert!(by_class(&tx, &seen, &SearchScope::All, &coverage));
    let plan = compile(&tx, &query(seen.clone()), SearchScope::All).unwrap();
    assert!(plan.unknown_source.is_none());
    same(&tx, &seen, &SearchScope::All, &coverage);

    // An unqueued document at another extractor version: the FTS classes
    // cannot hold it, so the count evaluates every candidate.
    tx.execute(
        "UPDATE d_search_docs SET extractor_version=2 WHERE doc_id=11",
        [],
    )
    .unwrap();
    let coverage = counted(&tx, &SearchScope::All);
    assert_eq!(coverage.forbidden(), 0);
    assert!(!by_class(&tx, &text, &SearchScope::All, &coverage));
    same(&tx, &text, &SearchScope::All, &coverage);
}

#[test]
fn a_header_row_without_a_summary_leaves_attachments_unknown() {
    // 8's header row stays; without its summary nothing says whether it has
    // attachments, and only the header-row class holds it.
    let (conn, archive) = addressed();
    conn.execute("DELETE FROM d_message_summary WHERE acquisition_id=8", [])
        .unwrap();
    let tx = conn.unchecked_transaction().unwrap();
    for scope in [SearchScope::All, archive] {
        let coverage = counted(&tx, &scope);
        assert_eq!(coverage.summaryless(), 0, "{scope:?}");
        for node in [
            json!({"has":"attachment"}),
            json!({"not":{"has":"attachment"}}),
        ] {
            assert!(by_class(&tx, &node, &scope, &coverage), "{node} {scope:?}");
            same(&tx, &node, &scope, &coverage);
        }
    }
}

/// Which part of a class-restricted read reports the corruption.
#[derive(Clone, Copy, PartialEq)]
enum Reporter {
    /// The unknown count, which evaluates the corrupt candidate.
    Count,
    /// The per-read binding check, the count evaluating nothing.
    Bindings,
}

/// `node`'s first date page fails as `Corrupt` with and without the proof
/// once `damage` runs, and `reporter` alone reports it. Damage runs after
/// coverage, in the same transaction, so the proof stays current and
/// coverage's own checks cannot fail first.
#[track_caller]
fn both_fail(conn: &Connection, damage: &str, node: &Json, reporter: Reporter) {
    let tx = conn.unchecked_transaction().unwrap();
    let coverage = counted(&tx, &SearchScope::All);
    tx.execute_batch(damage).unwrap();
    assert!(by_class(&tx, node, &SearchScope::All, &coverage), "{node}");

    // Pages also evaluate hits, so check the count statement on its own.
    let plan =
        compile_driven(&tx, &query(node.clone()), SearchScope::All, Some(&coverage)).unwrap();
    let count = plan.unknown_source.as_deref().map_or(Ok(0), |source| {
        let sql = format!("SELECT coalesce(sum(v IS NULL),0) FROM ({source})");
        plan.read(&tx, &sql, &[], |statement, values| {
            statement.query_row(rusqlite::params_from_iter(values), |row| {
                row.get::<_, i64>(0)
            })
        })
    });
    let checked = plan.check_bindings(&tx);
    match reporter {
        Reporter::Count => {
            assert!(matches!(count, Err(StoreError::Corrupt { .. })), "{node}");
            assert!(checked.is_ok(), "{node}");
        }
        Reporter::Bindings => {
            assert_eq!(count.unwrap(), 0, "{node}");
            assert!(matches!(checked, Err(StoreError::Corrupt { .. })), "{node}");
        }
    }

    let proven = Kept {
        coverage: Some(&coverage),
        ..Kept::default()
    };
    for kept in [Kept::default(), proven] {
        let read = page(
            &tx,
            node,
            &SearchScope::All,
            Order::Date,
            SearchAnchor::Top,
            kept,
        );
        assert!(matches!(read, Err(StoreError::Corrupt { .. })), "{node}");
    }
    for coverage in [None, Some(&coverage)] {
        for read in [search_date_ids, search_relevance_ids] {
            let ids = read(&tx, &query(node.clone()), SearchScope::All, None, coverage);
            assert!(matches!(ids, Err(StoreError::Corrupt { .. })), "{node}");
        }
    }
}

#[test]
fn corruption_fails_class_restricted_counts_as_it_fails_evaluating_counts() {
    // A malformed mask on unseen filler 13: its document is unusable, so
    // the class-restricted count evaluates it and the callback rejects it.
    let (conn, _) = store();
    both_fail(
        &conn,
        "UPDATE d_search_docs SET truncated_fields='[\"bogus\"]' WHERE doc_id=13",
        &json!({"or":[{"text":"alpha"}, {"flag":"seen"}]}),
        Reporter::Count,
    );

    // An undecodable header row on unseen filler 14: header rows are in the
    // address classes.
    let (conn, _) = addressed();
    both_fail(
        &conn,
        "INSERT INTO message_headers (message_id,raw_fields,from_json,fetched_at)
         VALUES (14,x'01','not json',1)",
        &json!({"or":[{"flag":"seen"}, is("from", "x@y.test")]}),
        Reporter::Count,
    );

    // An invalid binding on a tree no leaf of which can be unknown: the
    // count evaluates nothing, and the per-read binding check reports it.
    let (conn, _) = addressed();
    both_fail(
        &conn,
        "UPDATE acquisitions SET content_sha256='00' WHERE acquisition_id=10",
        &json!({"or":[{"flag":"seen"}, {"has":"note"}]}),
        Reporter::Bindings,
    );
}

#[test]
fn an_assemblys_unidentified_entry_leaves_its_field_unknown() {
    // 11 is bound to a parser-2 assembly with a name-only Cc entry. With
    // its summary raised to parser 2, only the class's bindings arm holds
    // it: no summary arm or old binding does.
    let (conn, archive) = addressed();
    conn.execute(
        "UPDATE d_message_summary SET parser_version=2 WHERE acquisition_id=11",
        [],
    )
    .unwrap();
    let tx = conn.unchecked_transaction().unwrap();
    let seen = json!({"flag":"seen"});
    for scope in [SearchScope::All, archive] {
        let coverage = counted(&tx, &scope);
        for node in [
            json!({"or":[is("cc", "x@y.test"), seen.clone()]}),
            json!({"or":[{"field":"cc","op":"domain","value":"example.test"}, seen.clone()]}),
            json!({"not":{"field":"from","op":"domain","value":"example.test"}}),
        ] {
            assert!(by_class(&tx, &node, &scope, &coverage), "{node} {scope:?}");
            same(&tx, &node, &scope, &coverage);
        }
    }
}
