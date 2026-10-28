//! Ordered walks (spec/06, M1.6r3c–d): walked and streamed pages equal
//! the sorted order for every anchor, a driven read walks only when its
//! kept set is dense, and walked reads use their index without a sort.

use super::driven::{Order, addressed, both, counted, page, store};
use super::plans::{lowest_copies_by_message, plan_of};
use super::*;
use crate::lists::{self, PageRead};
use crate::query::{
    CoverageCounts, Kept, SearchAnchor, SearchPage, search_date_ids, search_relevance_ids,
};

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
    reads_like_sorting(tx, node, scope, coverage, Order::Date);
}

/// `walks_like_sorting` in `order`: the id index sorts every hit.
#[track_caller]
fn reads_like_sorting(
    tx: &Transaction,
    node: &Json,
    scope: &SearchScope,
    coverage: &CoverageCounts,
    order: Order,
) {
    const LIMIT: usize = 3;
    let ids = match order {
        Order::Date => search_date_ids,
        Order::Relevance => search_relevance_ids,
    };
    let PageRead::Ready((_, ids)) =
        ids(tx, &query(node.clone()), scope.clone(), None, None).unwrap()
    else {
        panic!("ids not ready");
    };
    let proven = Kept {
        coverage: Some(coverage),
        ..Kept::default()
    };
    for kept in [Kept::default(), proven] {
        let context = format!(
            "{node} {scope:?} {order:?} proven={}",
            kept.coverage.is_some()
        );
        let read = |anchor| ready(page(tx, node, scope, order, anchor, kept).unwrap());
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
    let copy = occurrence(&tx, id, 1);
    for scope in [SearchScope::All, account, archive.clone()] {
        let coverage = counted(&tx, &scope);
        for node in &cases {
            walks_like_sorting(&tx, node, &scope, &coverage);
        }
    }

    // Around the other copy centres on the chosen one.
    let chosen: i64 = tx
        .query_row(
            "SELECT min(occurrence_id) FROM occurrences WHERE message_id=1 AND mailbox_id=?1",
            [id],
            |row| row.get(0),
        )
        .unwrap();
    assert_ne!(chosen, copy);
    let coverage = counted(&tx, &archive);
    let alpha = json!({"text":"alpha"});
    let proven = Kept {
        coverage: Some(&coverage),
        ..Kept::default()
    };
    for (kept, order) in [Kept::default(), proven]
        .into_iter()
        .flat_map(|kept| [(kept, Order::Date), (kept, Order::Relevance)])
    {
        let around = |at| page(&tx, &alpha, &archive, order, SearchAnchor::Around(at), kept);
        assert_eq!(around(copy).unwrap(), around(chosen).unwrap(), "{order:?}");
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

/// `rare` matches one message in `FILLER`, `trio` three, `quad` four (a
/// sixteenth), `common` every fourth and `tied` every one; every even
/// message shares one early date, every odd one the fixture's.
const FILLER: i64 = 64;

fn dated() -> Connection {
    let conn = fixture(FILLER as usize);
    for id in 1..=FILLER {
        let subject = match id {
            1 => "rare common tied",
            16 | 32 | 48 => "trio quad common tied",
            64 => "quad common tied",
            _ if id % 4 == 0 => "common tied",
            _ => "tied",
        };
        doc(&conn, id, "indexed", "[]", &[("subject", subject)]);
    }
    // `sort_date` derives from the occurrence's date.
    edit(
        &conn,
        "UPDATE occurrences SET internal_date = 1000 WHERE message_id % 2 = 0",
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
    let walks = |node: Json| walks(&tx, &node, &SearchScope::All, &all);
    // A full read always walks.
    assert!(walks(json!({"flag":"seen"})));
    // 1 and 3 of 64 sort their kept candidates; 4, a sixteenth, walks.
    assert!(!walks(json!({"text":"rare"})));
    assert!(!walks(json!({"text":"trio"})));
    assert!(walks(json!({"text":"quad"})));
    assert!(walks(json!({"text":"common"})));
    // Without the proof the read is full, so it walks.
    let plan = compile(&tx, &query(json!({"text":"rare"})), SearchScope::All).unwrap();
    assert!(plan.date_walk.is_some());
    let early = json!({"date":{"before":"2000-01-01","tz":"UTC"}});
    for node in [
        json!({"text":"rare"}),
        json!({"text":"trio"}),
        json!({"text":"quad"}),
        json!({"text":"common"}),
        json!({"text":"tied"}),
        json!({"and":[{"text":"tied"},{"not":{"text":"common"}}]}),
        early.clone(),
        json!({"not":early.clone()}),
        json!({"and":[{"text":"common"}, early]}),
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
            for (keyed, sql) in seeks.into_iter().enumerate() {
                let nodes = plan_of(&tx, &sql, &plan.values);
                lowest_copies_by_message(&nodes, &format!("{node} {scope:?}"));
                let detail: Vec<&str> = nodes.iter().map(|node| node.detail.as_str()).collect();
                let detail = detail.join("\n");
                assert!(detail.contains(index), "{node} {scope:?}: {detail}");
                assert!(
                    !detail.contains("TEMP B-TREE FOR ORDER BY"),
                    "{node} {scope:?}: {detail}"
                );
                // A key bounds the index range rather than filtering a scan.
                if keyed > 0 {
                    assert!(
                        detail.contains(&format!("SEARCH r USING INDEX {index}"))
                            || detail.contains(&format!("SEARCH r USING COVERING INDEX {index}")),
                        "{node} {scope:?}: {detail}"
                    );
                }
            }
        }
    }
}

/// Relevance trees for each reader: one AND-reached score leaf streams,
/// none walks occurrences, and the rest sort.
fn relevance_cases() -> Vec<(Json, Reads)> {
    let is = |field, value| super::field(field, "is", value);
    vec![
        (json!({"text":"alpha"}), Reads::Rank),
        (
            json!({"and":[{"text":"alpha"},{"flag":"seen"}]}),
            Reads::Rank,
        ),
        (
            json!({"and":[{"text":"alpha"},{"not":{"field":"body","op":"contains","value":"gamma"}}]}),
            Reads::Rank,
        ),
        (json!({"text":"nomatch"}), Reads::Rank),
        (
            json!({"field":"body","op":"contains","value":"gamma"}),
            Reads::Rank,
        ),
        (
            json!({"and":[{"text":"alpha"},{"text":"alpha"}]}),
            Reads::Rank,
        ),
        (
            json!({"and":[{"text":"alpha"},{"or":[{"text":"alpha"},{"flag":"seen"}]}]}),
            Reads::Rank,
        ),
        (json!({"flag":"seen"}), Reads::Occurrence),
        (json!({"and":[]}), Reads::Occurrence),
        (json!({"not":{"text":"alpha"}}), Reads::Occurrence),
        (is("from", "ada@example.test"), Reads::Occurrence),
        (json!({"has":"attachment"}), Reads::Occurrence),
        (
            json!({"or":[{"text":"alpha"},{"flag":"seen"}]}),
            Reads::Sort,
        ),
        (
            json!({"and":[{"text":"alpha"},{"text":"gamma"}]}),
            Reads::Sort,
        ),
        (
            json!({"and":[{"text":"alpha"}, is("from", "ada@example.test")]}),
            Reads::Sort,
        ),
    ]
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Reads {
    Rank,
    Occurrence,
    Sort,
}

/// How a relevance page of `node` reads, unproven: a full read.
fn reads(tx: &Transaction, node: &Json, scope: &SearchScope) -> Reads {
    let plan = compile(tx, &query(node.clone()), scope.clone()).unwrap();
    match (plan.occurrence_walk.is_some(), plan.rank_stream) {
        (true, _) => Reads::Occurrence,
        (false, true) => Reads::Rank,
        (false, false) => Reads::Sort,
    }
}

#[test]
fn streamed_and_walked_relevance_pages_equal_the_sorted_order() {
    let (conn, archive) = store();
    let account = SearchScope::Account(archive_account(&conn).parse().unwrap());
    let tx = conn.unchecked_transaction().unwrap();
    let SearchScope::Mailbox(id) = archive else {
        unreachable!()
    };
    occurrence(&tx, id, 1);
    for scope in [SearchScope::All, account, archive] {
        let coverage = counted(&tx, &scope);
        for (node, expected) in relevance_cases() {
            assert_eq!(reads(&tx, &node, &scope), expected, "{node}");
            reads_like_sorting(&tx, &node, &scope, &coverage, Order::Relevance);
        }
    }
    drop(tx);

    let (conn, archive) = addressed();
    let tx = conn.unchecked_transaction().unwrap();
    for scope in [SearchScope::All, archive] {
        let coverage = counted(&tx, &scope);
        for (node, _) in relevance_cases() {
            reads_like_sorting(&tx, &node, &scope, &coverage, Order::Relevance);
        }
    }
    drop(tx);

    let conn = both();
    let tx = conn.unchecked_transaction().unwrap();
    let coverage = counted(&tx, &SearchScope::All);
    for (node, _) in relevance_cases() {
        reads_like_sorting(&tx, &node, &SearchScope::All, &coverage, Order::Relevance);
    }
}

#[test]
fn long_rank_ties_complete_in_every_scope() {
    use crate::testing::fixtures::mailbox;
    // Every message, and a second copy of each tenth, in one mailbox: the
    // long "tied" run completes through each scope's occurrence order.
    let conn = dated();
    let account = archive_account(&conn);
    let archive = mailbox(&conn, &account, "Archive", "folder", None);
    let tx = conn.unchecked_transaction().unwrap();
    for id in 1..=FILLER {
        occurrence(&tx, archive, id);
        if id % 10 == 0 {
            occurrence(&tx, archive, id);
        }
    }
    for scope in [
        SearchScope::Account(account.parse().unwrap()),
        SearchScope::Mailbox(archive),
    ] {
        let coverage = counted(&tx, &scope);
        for node in [
            json!({"text":"tied"}),
            json!({"and":[{"text":"tied"},{"not":{"text":"common"}}]}),
        ] {
            reads_like_sorting(&tx, &node, &scope, &coverage, Order::Relevance);
        }
    }
}

#[test]
fn rank_ties_complete_across_page_boundaries() {
    // Every message says "tied" in a one-word subject or with "common":
    // two scores, so occurrence order alone breaks each tie, page after page.
    let conn = dated();
    let tx = conn.unchecked_transaction().unwrap();
    let all = counted(&tx, &SearchScope::All);
    for node in [
        json!({"text":"tied"}),
        json!({"and":[{"text":"tied"},{"not":{"text":"common"}}]}),
        json!({"text":"common"}),
    ] {
        assert_eq!(reads(&tx, &node, &SearchScope::All), Reads::Rank, "{node}");
        reads_like_sorting(&tx, &node, &SearchScope::All, &all, Order::Relevance);
    }
}

#[test]
fn walked_and_streamed_relevance_reads_need_no_sort() {
    use crate::testing::fixtures::mailbox;
    let conn = dated();
    let account = archive_account(&conn);
    let archive = mailbox(&conn, &account, "Archive", "folder", None);
    let tx = conn.unchecked_transaction().unwrap();
    for id in 1..=FILLER {
        occurrence(&tx, archive, id);
    }
    register(&tx).unwrap();
    // All reads the table in rowid order: a scan, or a seek past a key.
    for (scope, index) in [
        (SearchScope::All, " r USING INTEGER PRIMARY KEY|SCAN r"),
        (
            SearchScope::Account(account.parse().unwrap()),
            "d_list_rows_account_order",
        ),
        (SearchScope::Mailbox(archive), "d_list_rows_mailbox_order"),
    ] {
        let coverage = counted(&tx, &scope);
        for (node, expected) in [
            (json!({"flag":"seen"}), index),
            (json!({"text":"common"}), "VIRTUAL TABLE"),
        ] {
            let plan =
                compile_driven(&tx, &query(node.clone()), scope.clone(), Some(&coverage)).unwrap();
            let reads = super::super::paging::relevance_reads(&plan)
                .unwrap_or_else(|| panic!("{node} {scope:?} sorts"));
            for sql in reads {
                let nodes = plan_of(&tx, &sql, &plan.values);
                lowest_copies_by_message(&nodes, &format!("{node} {scope:?}"));
                let detail: Vec<&str> = nodes.iter().map(|node| node.detail.as_str()).collect();
                let detail = detail.join("\n");
                assert!(
                    expected.split('|').any(|marker| detail.contains(marker)),
                    "{node} {scope:?}: {detail}"
                );
                assert!(
                    !detail.contains("TEMP B-TREE FOR ORDER BY"),
                    "{node} {scope:?}: {detail}"
                );
                // Each streamed match finds its messages and scope row by key.
                if expected == "VIRTUAL TABLE" {
                    let ties = super::super::paging::tie_walk_reads(&plan).expect("a tie walk");
                    for sql in ties {
                        let nodes = plan_of(&tx, &sql, &plan.values);
                        lowest_copies_by_message(&nodes, &format!("{node} {scope:?} ties"));
                        let detail: Vec<&str> =
                            nodes.iter().map(|node| node.detail.as_str()).collect();
                        let detail = detail.join("\n");
                        assert!(
                            index.split('|').any(|marker| detail.contains(marker)),
                            "{node} {scope:?} ties: {detail}"
                        );
                        assert!(
                            !detail.contains("TEMP B-TREE FOR ORDER BY"),
                            "{node} {scope:?} ties: {detail}"
                        );
                    }
                    assert!(
                        detail.contains(
                            "SEARCH lmx USING COVERING INDEX logical_messages_by_acquisition"
                        ),
                        "{node} {scope:?}: {detail}"
                    );
                    assert!(
                        nodes
                            .iter()
                            .any(|node| node.detail.starts_with("SEARCH r USING")
                                && node.detail.contains("message_id=?")),
                        "{node} {scope:?}: {detail}"
                    );
                }
            }
        }
    }
}

#[test]
fn the_rank_column_scores_exactly_as_the_relevance_bm25() {
    let conn = dated();
    for term in ["tied", "common", "rare"] {
        let differing: i64 = conn
            .query_row(
                &format!(
                    "SELECT count(*) FROM d_search_fts f
                     WHERE f.d_search_fts MATCH ?1 AND f.rank MATCH {}
                     AND f.rank IS NOT {}",
                    super::super::paging::RANK,
                    super::super::paging::BM25
                ),
                [format!("{{subject}} : \"{term}\"")],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(differing, 0, "{term}");
    }
}
