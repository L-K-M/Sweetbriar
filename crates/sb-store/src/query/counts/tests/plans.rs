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
