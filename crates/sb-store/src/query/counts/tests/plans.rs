use super::*;

/// One `EXPLAIN QUERY PLAN` row: its id, its parent's id and its text.
struct PlanNode {
    id: i64,
    parent: i64,
    detail: String,
}

fn plan_of(tx: &Transaction, sql: &str, values: &[Value]) -> Vec<PlanNode> {
    tx.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .unwrap()
        .query_map(rusqlite::params_from_iter(values), |row| {
            Ok(PlanNode {
                id: row.get(0)?,
                parent: row.get(1)?,
                detail: row.get(3)?,
            })
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// Whether `node` runs once per statement: inside an uncorrelated list
/// subquery or a materialized CTE, with no correlated subquery between.
/// The plan nests a CTE under its first use, but an uncorrelated one is
/// materialized once per statement (SQLite's `OP_Once`) wherever it is
/// used. A scan joined into the candidates' loop has neither ancestor.
fn once_per_statement(nodes: &[PlanNode], node: &PlanNode) -> bool {
    let mut parent = node.parent;
    while let Some(up) = nodes.iter().find(|candidate| candidate.id == parent) {
        if up.detail.starts_with("MATERIALIZE") || up.detail.starts_with("LIST SUBQUERY") {
            return true;
        }
        if up.detail.starts_with("CORRELATED") {
            return false;
        }
        parent = up.parent;
    }
    false
}

/// spec/06: no FTS leaf's MATCH runs once per scoped candidate, for
/// membership or relevance scores, in counts, pages or id indexes.
#[test]
fn fts_leaves_match_once_per_statement_not_per_candidate() {
    let conn = fixture(1);
    let tx = conn.unchecked_transaction().unwrap();
    let query = query(json!({"and":[
        {"text":"alpha"},
        {"field":"subject","op":"contains","value":"beta"},
        {"not":{"field":"body","op":"contains","value":"gamma"}}
    ]}));
    let plan = compile(&tx, &query, SearchScope::All).unwrap();
    // The predicate's callbacks must exist for the statements to prepare.
    register(&tx).unwrap();
    let mut statements = vec![format!(
        "SELECT coalesce(sum(v IS NULL),0) FROM ({})",
        plan.source
    )];
    // The shapes pages and id indexes wrap the hits relation in.
    for hits in super::super::paging::hit_sources(&plan) {
        statements.push(format!(
            "SELECT * FROM ({hits}) ORDER BY occurrence_id DESC LIMIT 51"
        ));
        statements.push(format!(
            "SELECT occurrence_id FROM ({hits}) ORDER BY occurrence_id DESC"
        ));
    }
    for sql in statements {
        let nodes = plan_of(&tx, &sql, &plan.values);
        let scans: Vec<_> = nodes
            .iter()
            .filter(|node| node.detail.contains("d_search_fts"))
            .collect();
        assert!(!scans.is_empty(), "no FTS scan in the plan of {sql}");
        for scan in scans {
            assert!(
                once_per_statement(&nodes, scan),
                "FTS runs per candidate: {}",
                scan.detail
            );
        }
    }
}

/// spec/06 (M1.6r2): a driven read enumerates its kept messages and seeks
/// their scope rows by message, in counts, pages and id indexes, for every
/// scope. Only FTS, JSON, the queue and the two partial indexes (each an
/// unusable class) are read whole, besides the kept sets themselves.
#[test]
fn driven_reads_seek_kept_messages_and_scan_no_table() {
    use crate::testing::fixtures::{archive_account, mailbox};
    let conn = fixture(40);
    for id in 1..=40 {
        let subject = if id == 1 { "alpha" } else { "plain" };
        doc(&conn, id, "indexed", "[]", &[("subject", subject)]);
        let from = format!("sender{id}@example.test");
        summary(&conn, id, "ok", 2, Summary::sender(&from));
        let tx = conn.unchecked_transaction().unwrap();
        let columns = crate::address_keys::Columns {
            from_addr: Some(&from),
            from_name: None,
            to_json: None,
            cc_json: None,
        };
        crate::address_keys::write(&tx, id, crate::address_keys::SUMMARY_TARGET, &columns).unwrap();
        tx.commit().unwrap();
    }
    let account = archive_account(&conn);
    let archive = mailbox(&conn, &account, "Archive", "folder", None);
    let tx = conn.unchecked_transaction().unwrap();
    for id in 1..=40 {
        occurrence(&tx, archive, id);
    }
    register(&tx).unwrap();
    let queries = [
        query(json!({"and":[{"text":"alpha"},{"flag":"seen"}]})),
        query(
            json!({"and":[{"field":"from","op":"is","value":"sender3@example.test"},{"flag":"seen"}]}),
        ),
        query(
            json!({"and":[{"field":"from","op":"domain","value":"example.test"},{"text":"alpha"}]}),
        ),
    ];
    let scopes = [
        SearchScope::All,
        SearchScope::Account(account.parse().unwrap()),
        SearchScope::Mailbox(archive),
    ];
    for (scope, query) in scopes
        .iter()
        .flat_map(|scope| queries.iter().map(move |query| (scope.clone(), query)))
    {
        let coverage = crate::query::read_search_coverage_reusing(&tx, scope.clone(), None)
            .unwrap()
            .1;
        let plan = compile_driven(&tx, query, scope.clone(), Some(&coverage)).unwrap();
        assert_ne!(
            plan.source,
            compile(&tx, query, scope.clone()).unwrap().source
        );
        let mut statements = vec![format!(
            "SELECT coalesce(sum(v IS NULL),0) FROM ({})",
            plan.unknown_source
        )];
        for hits in super::super::paging::hit_sources(&plan) {
            statements.push(format!(
                "SELECT * FROM ({hits}) ORDER BY occurrence_id DESC LIMIT 51"
            ));
        }
        for sql in statements {
            seeks_by_message(&tx, &sql, &plan.values, &format!("{scope:?}"));
        }
    }
}

/// Assert that `sql` reads no table whole and seeks every scope row it
/// reads by message. Only FTS, JSON, the queue, header rows and the
/// partial indexes (each an unusable class) are read whole, besides the
/// kept and class sets themselves.
#[track_caller]
fn seeks_by_message(tx: &Transaction, sql: &str, values: &[Value], context: &str) {
    let nodes = plan_of(tx, sql, values);
    for node in &nodes {
        let whole = node.detail.starts_with("SCAN")
            && ![
                "d_search_fts",
                "json_each",
                "d_index_queue",
                "INDEX acquisitions_unpublished",
                "INDEX d_search_docs_unusable",
                "CONSTANT ROW",
                "(subquery-",
                "SCAN kh",
                "INDEX d_message_summary_unusable",
                "INDEX d_message_bindings_old_target",
            ]
            .iter()
            .any(|allowed| node.detail.contains(allowed));
        assert!(!whole, "{context} scans {}", node.detail);
    }
    let candidates: Vec<_> = nodes
        .iter()
        .filter(|node| node.detail.starts_with("SEARCH r USING"))
        .collect();
    assert!(
        !candidates.is_empty(),
        "{context}: no candidate seek in {sql}"
    );
    for node in candidates {
        assert!(
            node.detail.contains("message_id=?") || node.detail.contains("rowid=?"),
            "{context} walks the scope: {}",
            node.detail
        );
    }
}

/// spec/06 (M1.6r3b): an undriven read whose leaves' unknown classes are
/// proven counts unknowns by seeking those classes' messages, in every
/// scope, and one with no leaf that can be unknown counts none.
#[test]
fn class_restricted_counts_seek_unknown_classes_and_scan_no_table() {
    use crate::testing::fixtures::{archive_account, mailbox};
    let conn = fixture(40);
    for id in 1..=40 {
        doc(&conn, id, "indexed", "[]", &[("subject", "plain")]);
        summary(&conn, id, "ok", 2, Summary::sender("sender@example.test"));
    }
    let account = archive_account(&conn);
    let archive = mailbox(&conn, &account, "Archive", "folder", None);
    let tx = conn.unchecked_transaction().unwrap();
    for id in 1..=40 {
        occurrence(&tx, archive, id);
    }
    register(&tx).unwrap();
    let classes = [
        json!({"or":[{"text":"alpha"},{"flag":"seen"}]}),
        json!({"or":[{"field":"from","op":"is","value":"a@example.test"},{"flag":"seen"}]}),
        json!({"not":{"has":"attachment"}}),
        json!({"size":{"op":"gt","bytes":0}}),
    ];
    let scopes = [
        SearchScope::All,
        SearchScope::Account(account.parse().unwrap()),
        SearchScope::Mailbox(archive),
    ];
    for scope in scopes {
        let coverage = crate::query::read_search_coverage_reusing(&tx, scope.clone(), None)
            .unwrap()
            .1;
        let plan = |node: &Json| {
            let plan =
                compile_driven(&tx, &query(node.clone()), scope.clone(), Some(&coverage)).unwrap();
            assert!(plan.skipped.is_some(), "{node} {scope:?}");
            plan
        };
        for node in &classes {
            let plan = plan(node);
            let sql = format!(
                "SELECT coalesce(sum(v IS NULL),0) FROM ({})",
                plan.unknown_source
            );
            seeks_by_message(&tx, &sql, &plan.values, &format!("{node} {scope:?}"));
        }
        let flag = plan(&json!({"flag":"seen"}));
        assert!(
            flag.unknown_source.ends_with(" AND 0"),
            "{scope:?}: {}",
            flag.unknown_source
        );
    }
}
