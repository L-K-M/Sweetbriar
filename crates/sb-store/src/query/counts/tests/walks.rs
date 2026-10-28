//! Date walks (spec/06, M1.6r3c): walked pages equal the sorted order for
//! every anchor, a driven read walks only when its kept set is dense, and
//! walked reads read their scope's date index without a sort.

use super::driven::{Order, addressed, both, counted, page, store};
use super::*;
use crate::lists::{self, PageRead};
use crate::query::{CoverageCounts, Kept, SearchAnchor, SearchPage, search_date_ids};

/// The rows of a page read with `kept`, as occurrence ids.
fn rows(page: &SearchPage) -> Vec<i64> {
    page.rows()
        .iter()
        .map(|row| row.row().occurrence_id)
        .collect()
}

fn ready(read: PageRead<SearchPage>) -> SearchPage {
    match read {
        PageRead::Ready(page) => page,
        other => panic!("not ready: {other:?}"),
    }
}

/// Every anchor's date page of `node` equals the slice of the sorted id
/// index it names, with and without the proof.
#[track_caller]
fn walks_like_sorting(
    tx: &Transaction,
    node: &Json,
    scope: &SearchScope,
    coverage: &CoverageCounts,
) {
    const LIMIT: usize = 3;
    let PageRead::Ready((_, ids)) =
        search_date_ids(tx, &query(node.clone()), scope.clone(), None, None).unwrap()
    else {
        panic!("ids not ready");
    };
    let proven = Kept {
        coverage: Some(coverage),
        ..Kept::default()
    };
    for kept in [Kept::default(), proven] {
        let context = format!("{node} {scope:?} proven={}", kept.coverage.is_some());
        let read = |anchor| ready(page(tx, node, scope, Order::Date, anchor, kept).unwrap());
        let top = read(SearchAnchor::Top);
        let first = &ids[..ids.len().min(LIMIT)];
        assert_eq!(rows(&top), first, "{context} top");
        assert_eq!(top.has_more_after(), ids.len() > LIMIT, "{context} top");
        let Some(last) = top.rows().last() else {
            continue;
        };

        // After the first page, then back before the second page's first row.
        let after = read(SearchAnchor::After(last.cursor().to_owned()));
        let second = &ids[first.len()..ids.len().min(2 * LIMIT)];
        assert_eq!(rows(&after), second, "{context} after");
        assert_eq!(
            after.has_more_after(),
            ids.len() > 2 * LIMIT,
            "{context} after"
        );
        if let Some(head) = after.rows().first() {
            let before = read(SearchAnchor::Before(head.cursor().to_owned()));
            assert_eq!(rows(&before), first, "{context} before");
            assert!(!before.has_more_before(), "{context} before");
        }

        // Around every hit: each side up to the limit, then centred.
        for (at, id) in ids.iter().enumerate() {
            let around = read(SearchAnchor::Around(*id));
            let newer = at.min(LIMIT);
            let older = (ids.len() - at - 1).min(LIMIT);
            let (before, after) = lists::center_counts(LIMIT, newer, older);
            assert_eq!(
                rows(&around),
                &ids[at - before..=at + after],
                "{context} around {id}"
            );
            assert_eq!(
                around.has_more_before(),
                newer > before,
                "{context} around {id}"
            );
            assert_eq!(
                around.has_more_after(),
                older > after,
                "{context} around {id}"
            );
        }
    }
}

#[test]
fn walked_date_pages_equal_the_sorted_order_for_every_anchor_and_scope() {
    let is = |field, value| super::field(field, "is", value);
    let cases = [
        json!({"text":"alpha"}),
        json!({"and":[{"text":"alpha"},{"flag":"seen"}]}),
        json!({"or":[{"text":"alpha"},{"flag":"seen"}]}),
        json!({"not":{"text":"alpha"}}),
        json!({"flag":"seen"}),
        json!({"and":[]}),
        json!({"text":"nomatch"}),
        is("from", "ada@example.test"),
        json!({"and":[{"text":"alpha"}, is("from", "ada@example.test")]}),
        json!({"has":"attachment"}),
    ];
    let (conn, archive) = store();
    let account = SearchScope::Account(archive_account(&conn).parse().unwrap());
    let tx = conn.unchecked_transaction().unwrap();
    // A second copy of 1 in the Archive: one hit, on the lowest copy.
    let SearchScope::Mailbox(id) = archive else {
        unreachable!()
    };
    occurrence(&tx, id, 1);
    for scope in [SearchScope::All, account, archive] {
        let coverage = counted(&tx, &scope);
        for node in &cases {
            walks_like_sorting(&tx, node, &scope, &coverage);
        }
    }
    drop(tx);

    let (conn, archive) = addressed();
    let tx = conn.unchecked_transaction().unwrap();
    for scope in [SearchScope::All, archive] {
        let coverage = counted(&tx, &scope);
        for node in &cases {
            walks_like_sorting(&tx, node, &scope, &coverage);
        }
    }
    drop(tx);

    let conn = both();
    let tx = conn.unchecked_transaction().unwrap();
    let coverage = counted(&tx, &SearchScope::All);
    for node in &cases {
        walks_like_sorting(&tx, node, &SearchScope::All, &coverage);
    }
}

/// `rare` matches one message in `FILLER`, `common` every fourth, `tied`
/// every one; every even message shares one date.
const FILLER: i64 = 64;

fn dated() -> Connection {
    let conn = fixture(FILLER as usize);
    for id in 1..=FILLER {
        let subject = match id {
            1 => "rare common tied",
            _ if id % 4 == 0 => "common tied",
            _ => "tied",
        };
        doc(&conn, id, "indexed", "[]", &[("subject", subject)]);
    }
    edit(
        &conn,
        "UPDATE d_list_rows SET sort_date = 1000 WHERE message_id % 2 = 0",
    );
    conn
}

/// Whether a date page of `node` walks under `coverage`'s proof.
fn walks(tx: &Transaction, node: &Json, scope: &SearchScope, coverage: &CoverageCounts) -> bool {
    compile_driven(tx, &query(node.clone()), scope.clone(), Some(coverage))
        .unwrap()
        .date_walk
        .is_some()
}

#[test]
fn a_driven_read_walks_only_when_its_kept_set_is_a_sixteenth_of_the_scope() {
    let conn = dated();
    let tx = conn.unchecked_transaction().unwrap();
    let all = counted(&tx, &SearchScope::All);
    // A full read always walks.
    assert!(walks(&tx, &json!({"flag":"seen"}), &SearchScope::All, &all));
    // 1 of 64 sorts its one kept candidate; 16 of 64 walks.
    assert!(!walks(
        &tx,
        &json!({"text":"rare"}),
        &SearchScope::All,
        &all
    ));
    assert!(walks(
        &tx,
        &json!({"text":"common"}),
        &SearchScope::All,
        &all
    ));
    // Without the proof the read is full, so it walks.
    let plan = compile(&tx, &query(json!({"text":"rare"})), SearchScope::All).unwrap();
    assert!(plan.date_walk.is_some());
    for node in [
        json!({"text":"rare"}),
        json!({"text":"common"}),
        json!({"text":"tied"}),
        json!({"and":[{"text":"tied"},{"not":{"text":"common"}}]}),
    ] {
        walks_like_sorting(&tx, &node, &SearchScope::All, &all);
    }
}

#[test]
fn walked_reads_read_their_scopes_date_index_without_a_sort() {
    use crate::testing::fixtures::mailbox;
    let conn = dated();
    let account = archive_account(&conn);
    let archive = mailbox(&conn, &account, "Archive", "folder", None);
    let tx = conn.unchecked_transaction().unwrap();
    for id in 1..=FILLER {
        occurrence(&tx, archive, id);
    }
    register(&tx).unwrap();
    for (scope, index) in [
        (SearchScope::All, "d_list_rows_by_date"),
        (
            SearchScope::Account(account.parse().unwrap()),
            "d_list_rows_account_by_date",
        ),
        (SearchScope::Mailbox(archive), "d_list_rows_window"),
    ] {
        let coverage = counted(&tx, &scope);
        for node in [json!({"flag":"seen"}), json!({"text":"common"})] {
            let plan =
                compile_driven(&tx, &query(node.clone()), scope.clone(), Some(&coverage)).unwrap();
            let seeks = super::super::paging::date_walk_seeks(&plan)
                .unwrap_or_else(|| panic!("{node} {scope:?} does not walk"));
            for sql in seeks {
                let detail: Vec<String> = tx
                    .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                    .unwrap()
                    .query_map(rusqlite::params_from_iter(&plan.values), |row| row.get(3))
                    .unwrap()
                    .collect::<Result<_, _>>()
                    .unwrap();
                let detail = detail.join("\n");
                assert!(detail.contains(index), "{node} {scope:?}: {detail}");
                assert!(
                    !detail.contains("TEMP B-TREE FOR ORDER BY"),
                    "{node} {scope:?}: {detail}"
                );
            }
        }
    }
}
