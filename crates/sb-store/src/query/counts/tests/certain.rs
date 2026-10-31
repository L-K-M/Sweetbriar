//! Certain unknowns (M1.6r4c3), compared with evaluating the same snapshot.

use super::driven::{Order, both, counted, page, same, store};
use super::*;
use crate::lists::PageRead;
use crate::query::{Kept, SearchAnchor, search_date_ids, search_relevance_ids};

#[test]
fn certain_counts_equal_evaluating_counts_across_fts_trees_classes_and_scopes() {
    let (conn, archive) = store();
    let SearchScope::Mailbox(archive_id) = archive else {
        unreachable!();
    };
    let account = archive_account(&conn);
    let tx = conn.unchecked_transaction().unwrap();
    // A queued failed document is still certain, and one acquisition can
    // belong to two logical messages, each with several mailbox copies.
    index::enqueue(
        &tx,
        &[(5, index::IndexReason::Reextract)],
        crate::Timestamp::from_millis(FIXED_NOW_MS),
    )
    .unwrap();
    let shared = message(&tx, &account, Some(5));
    for id in [5, 7, 8, shared, shared] {
        occurrence(&tx, archive_id, id);
    }
    tx.execute(
        "INSERT INTO accounts(account_id,kind,provider,display_name,state,sort_order,created_at)
         VALUES(?1,'imap','generic_imap','Other','active',1,1)",
        [OTHER_ACCOUNT],
    )
    .unwrap();
    let other_box = mailbox(&tx, OTHER_ACCOUNT, "Other", "folder", None);
    let other_message = message(&tx, OTHER_ACCOUNT, Some(5));
    occurrence(&tx, other_box, other_message);
    let empty_box = mailbox(&tx, &account, "Empty", "folder", None);
    // Failed documents never decode a mask, even one that is malformed.
    tx.execute(
        "UPDATE d_search_docs SET truncated_fields='not json' WHERE doc_id=5",
        [],
    )
    .unwrap();
    tx.commit().unwrap();
    let cases = [
        json!({"text":"alpha"}),
        json!({"text":"nomatch"}),
        field("subject", "contains", "alpha"),
        field("body", "contains", "gamma"),
        field("from", "contains", "delta"),
        field("to", "contains", "delta"),
        field("cc", "contains", "delta"),
        field("attachment", "contains", "delta"),
        json!({"and":[{"text":"alpha"},{"field":"body","op":"contains","value":"gamma"}]}),
        json!({"or":[{"text":"alpha"},{"text":"beta"}]}),
        json!({"not":{"text":"alpha"}}),
        json!({"not":{"not":{"text":"alpha"}}}),
        json!({"and":[{"text":"alpha"},{"not":{"text":"beta"}}]}),
        json!({"not":{"or":[{"text":"alpha"},{"not":{"text":"beta"}}]}}),
    ];
    let tx = conn.unchecked_transaction().unwrap();
    for (scope, alpha_unknown) in [
        (SearchScope::All, 8),
        (SearchScope::Account(account.parse().unwrap()), 7),
        (archive, 6),
        (SearchScope::Account(OTHER_ACCOUNT.parse().unwrap()), 1),
        (SearchScope::Mailbox(other_box), 1),
        (SearchScope::Mailbox(empty_box), 0),
    ] {
        let coverage = counted(&tx, &scope);
        assert_eq!(coverage.forbidden(), 0);
        for node in &cases {
            same(&tx, node, &scope, &coverage);
            let oracle = counts(&tx, &query(node.clone()), scope.clone()).unwrap();
            if node == &cases[0] {
                assert_eq!(oracle.excluded_unknown(), alpha_unknown, "{scope:?}");
            }
            let kept = Kept {
                coverage: Some(&coverage),
                ..Kept::default()
            };
            for order in [Order::Date, Order::Relevance] {
                let PageRead::Ready(result) =
                    page(&tx, node, &scope, order, SearchAnchor::Top, kept).unwrap()
                else {
                    panic!("a current page");
                };
                assert_eq!(
                    result.excluded_unknown(),
                    oracle.excluded_unknown(),
                    "{node}"
                );
            }
        }
    }
}

#[test]
fn other_leaves_empty_ands_and_unproven_guards_keep_evaluating_counts() {
    let (conn, archive) = store();
    let tx = conn.unchecked_transaction().unwrap();
    let coverage = counted(&tx, &SearchScope::All);
    for node in [
        json!({"or":[{"text":"alpha"},{"flag":"seen"}]}),
        json!({"and":[{"text":"alpha"},{"has":"attachment"}]}),
        json!({"and":[{"text":"alpha"},{"size":{"op":"gt","bytes":0}}]}),
        json!({"and":[{"text":"alpha"},{"field":"from","op":"is","value":"x@y.test"}]}),
        json!({"or":[{"text":"alpha"},{"and":[]}]}),
        json!({"and":[{"text":"alpha"},{"and":[]}]}),
        json!({"and":[{"text":"alpha"},{"not":{"and":[]}}]}),
    ] {
        let plan =
            compile_driven(&tx, &query(node.clone()), SearchScope::All, Some(&coverage)).unwrap();
        assert!(
            !plan
                .unknown_source
                .as_deref()
                .unwrap()
                .contains("UNION ALL")
        );
        same(&tx, &node, &SearchScope::All, &coverage);
    }

    let node = json!({"not":{"text":"alpha"}});
    // The same generation alone is insufficient: scope and target matter.
    let plan = compile_driven(&tx, &query(node.clone()), archive.clone(), Some(&coverage)).unwrap();
    assert_eq!(plan.unknown_source, Some(plan.source));
    same(&tx, &node, &archive, &coverage);
    tx.execute_batch("UPDATE store_meta SET value=value+1 WHERE key='search_generation'")
        .unwrap();
    let plan =
        compile_driven(&tx, &query(node.clone()), SearchScope::All, Some(&coverage)).unwrap();
    assert_eq!(plan.unknown_source, Some(plan.source));
    same(&tx, &node, &SearchScope::All, &coverage);
    tx.commit().unwrap();

    for damage in [
        "UPDATE d_search_docs SET extractor_version=2 WHERE doc_id=11",
        "DELETE FROM d_search_docs WHERE doc_id=12",
    ] {
        let (conn, _) = store();
        conn.execute_batch(damage).unwrap();
        let tx = conn.unchecked_transaction().unwrap();
        let coverage = counted(&tx, &SearchScope::All);
        let plan =
            compile_driven(&tx, &query(node.clone()), SearchScope::All, Some(&coverage)).unwrap();
        assert_eq!(plan.unknown_source, Some(plan.source));
        same(&tx, &node, &SearchScope::All, &coverage);
    }
}

#[test]
fn certain_unknowns_skip_no_binding_errors_and_other_masks_still_fail() {
    for unavailable in [
        "UPDATE d_search_docs SET state='failed' WHERE doc_id=12",
        "UPDATE acquisitions SET object_state='missing' WHERE acquisition_id=12",
    ] {
        let conn = both();
        conn.execute_batch(unavailable).unwrap();
        let tx = conn.unchecked_transaction().unwrap();
        // 12 has a real completed assembly binding. Capture proof first,
        // then corrupt its source so the read's own binding check must fail.
        let scopes = [
            SearchScope::All,
            SearchScope::Account(archive_account(&tx).parse().unwrap()),
            SearchScope::Mailbox(1),
        ];
        let coverage = scopes.each_ref().map(|scope| counted(&tx, scope));
        tx.execute(
            "UPDATE acquisitions SET content_sha256='00' WHERE acquisition_id=12",
            [],
        )
        .unwrap();
        let node = json!({"not":{"text":"alpha"}});
        for (scope, coverage) in scopes.iter().zip(&coverage) {
            for kept in [
                Kept::default(),
                Kept {
                    coverage: Some(coverage),
                    ..Kept::default()
                },
            ] {
                for order in [Order::Date, Order::Relevance] {
                    let result = page(&tx, &node, scope, order, SearchAnchor::Top, kept);
                    assert!(
                        matches!(result, Err(StoreError::Corrupt { .. })),
                        "{result:?}"
                    );
                }
                for read in [search_date_ids, search_relevance_ids] {
                    assert!(matches!(
                        read(
                            &tx,
                            &query(node.clone()),
                            scope.clone(),
                            None,
                            kept.coverage
                        ),
                        Err(StoreError::Corrupt { .. })
                    ));
                }
            }
        }
    }

    let (conn, _) = store();
    conn.execute(
        "UPDATE d_search_docs SET truncated_fields='[\"bogus\"]' WHERE doc_id=13",
        [],
    )
    .unwrap();
    let tx = conn.unchecked_transaction().unwrap();
    let coverage = counted(&tx, &SearchScope::All);
    let node = json!({"not":{"text":"alpha"}});
    let plan =
        compile_driven(&tx, &query(node.clone()), SearchScope::All, Some(&coverage)).unwrap();
    let sql = format!(
        "SELECT coalesce(sum(v IS NULL),0) FROM ({})",
        plan.unknown_source.as_deref().unwrap()
    );
    // The count itself must decode other masks, even past the first page.
    assert!(matches!(
        plan.read(&tx, &sql, &[], |statement, values| {
            statement.query_row(params_from_iter(values), |row| row.get::<_, i64>(0))
        }),
        Err(StoreError::Corrupt { .. })
    ));
    for kept in [
        Kept::default(),
        Kept {
            coverage: Some(&coverage),
            ..Kept::default()
        },
    ] {
        assert!(matches!(
            page(
                &tx,
                &node,
                &SearchScope::All,
                Order::Date,
                SearchAnchor::Top,
                kept
            ),
            Err(StoreError::Corrupt { .. })
        ));
    }
}
