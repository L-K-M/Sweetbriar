//! Saved-view row tests (spec/02, spec/07).
use sb_test_support::{ScratchDir, block_on, real_fs};

use crate::StoreError;
use crate::open;
use crate::query::Query;
use crate::testing::{reading, scratch_paths, test_seams};
use crate::views::{ViewSort, delete_saved_view, saved_view, saved_views, upsert_saved_view};

fn views_store(label: &str) -> (ScratchDir, crate::Store) {
    let (scratch, paths) = scratch_paths(label);
    let store = open(&paths, &test_seams(real_fs())).unwrap();
    (scratch, store)
}

const SEEN: &str = r#"{"v":1,"q":{"flag":"seen"}}"#;

/// The first save inserts at revision 1 and appends; a real change
/// bumps it; the same write changes nothing (spec/02 rule 9); bad
/// names and queries are refused; an unknown delete is `NotFound`.
#[test]
fn views_insert_change_list_and_delete() {
    let (_scratch, store) = views_store("views");
    assert!(reading(&store, saved_views).is_empty());
    let save = |tx: &crate::Transaction, id: &str, name: &str, sort| {
        upsert_saved_view(tx, id, name, SEEN, sort).map(|_| ())
    };
    block_on(store.write(crate::WritePriority::User, move |tx| {
        save(tx, "b", "Later", ViewSort::DateDesc)
    }))
    .unwrap();
    block_on(store.write(crate::WritePriority::User, move |tx| {
        save(tx, "a", "Sooner", ViewSort::Relevance)
    }))
    .unwrap();

    let listed = reading(&store, saved_views);
    let names: Vec<&str> = listed.iter().map(|view| view.name.as_str()).collect();
    assert_eq!(
        names,
        ["Later", "Sooner"],
        "new views append in sidebar order"
    );
    assert_eq!(
        listed[0].query_json,
        Query::parse_json(SEEN).unwrap().canonical_json()
    );

    let write = move |tx: &crate::Transaction| save(tx, "a", "Renamed", ViewSort::Relevance);
    block_on(store.write(crate::WritePriority::User, write)).unwrap();
    let write = move |tx: &crate::Transaction| save(tx, "a", "Renamed", ViewSort::Relevance);
    block_on(store.write(crate::WritePriority::User, write)).unwrap();
    let renamed = reading(&store, |tx| saved_view(tx, "a")).expect("the view exists");
    assert_eq!((renamed.name.as_str(), renamed.revision), ("Renamed", 2));

    let write = move |tx: &crate::Transaction| delete_saved_view(tx, "b").map(|_| ());
    block_on(store.write(crate::WritePriority::User, write)).unwrap();
    assert_eq!(reading(&store, saved_views).len(), 1);
    let error = block_on(store.write(
        crate::WritePriority::User,
        move |tx: &crate::Transaction| delete_saved_view(tx, "b").map(|_| ()),
    ));
    assert!(matches!(error, Err(StoreError::NotFound { .. })));

    let long = "x".repeat(129);
    for (name, query) in [
        (String::new(), SEEN.to_owned()),
        (long, SEEN.to_owned()),
        ("N".to_owned(), "bad".to_owned()),
    ] {
        let result = block_on(store.write(
            crate::WritePriority::User,
            move |tx: &crate::Transaction| {
                upsert_saved_view(tx, "new", &name, &query, ViewSort::DateDesc).map(|_| ())
            },
        ));
        assert!(matches!(result, Err(StoreError::InvalidInput { .. })));
    }
}
