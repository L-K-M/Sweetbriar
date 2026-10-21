use super::*;
use crate::lists::{MAX_PAGE_ROWS, PageRead};
use crate::query::{SearchAnchor, SearchPage, read_search_date_page, search_date_ids};

fn read(
    conn: &Connection,
    node: Json,
    scope: SearchScope,
    anchor: SearchAnchor,
    limit: usize,
    at: Option<u64>,
) -> Result<PageRead<SearchPage>, StoreError> {
    let tx = conn.unchecked_transaction().unwrap();
    let result = read_search_date_page(&tx, &query(node), scope, anchor, limit, at);
    tx.commit().unwrap();
    result
}

#[track_caller]
fn ready(result: Result<PageRead<SearchPage>, StoreError>) -> SearchPage {
    match result.unwrap() {
        PageRead::Ready(page) => page,
        other => panic!("expected page, got {other:?}"),
    }
}

fn page(conn: &Connection, node: Json, anchor: SearchAnchor, limit: usize) -> SearchPage {
    ready(read(conn, node, SearchScope::All, anchor, limit, None))
}

fn row_ids(page: &SearchPage) -> Vec<i64> {
    page.rows()
        .iter()
        .map(|hit| hit.row().occurrence_id)
        .collect()
}

fn cursor(page: &SearchPage, index: usize) -> String {
    page.rows()[index].cursor().to_owned()
}

fn forged(value: &str, edit: impl FnOnce(&mut Json)) -> String {
    let mut parsed: Json = serde_json::from_str(value).unwrap();
    edit(&mut parsed);
    parsed.to_string()
}

fn dates(conn: &Connection, values: &[(i64, i64)]) {
    let tx = conn.unchecked_transaction().unwrap();
    for (id, date) in values {
        tx.execute(
            "UPDATE occurrences SET internal_date=?1 WHERE occurrence_id=?2",
            params![date, id],
        )
        .unwrap();
    }
    refresh(&tx);
    tx.commit().unwrap();
}

fn date_ids(
    conn: &Connection,
    node: Json,
    scope: SearchScope,
    at: Option<u64>,
) -> Result<PageRead<(u64, Vec<i64>)>, StoreError> {
    let tx = conn.unchecked_transaction().unwrap();
    let result = search_date_ids(&tx, &query(node), scope, at);
    tx.commit().unwrap();
    result
}

#[test]
fn date_walks_ties_reverse_tails_and_every_around_clamp() {
    let conn = fixture(7);
    dates(
        &conn,
        &[
            (1, 200),
            (2, 200),
            (3, 100),
            (4, 400),
            (5, 250),
            (6, 300),
            (7, 300),
        ],
    );
    let order = [4, 7, 6, 5, 2, 1, 3];
    let node = json!({"and":[]});
    let first = page(&conn, node.clone(), SearchAnchor::Top, 3);
    assert_eq!(row_ids(&first), order[..3]);
    assert!(!first.has_more_before());
    assert!(first.has_more_after());
    let PageRead::Ready((generation, all_ids)) =
        date_ids(&conn, node.clone(), SearchScope::All, None).unwrap()
    else {
        panic!("expected id index");
    };
    assert_eq!(all_ids, order);
    assert_eq!(generation, first.generation());
    let mut walked = Vec::new();
    let mut current = first;
    loop {
        walked.extend(row_ids(&current));
        if !current.has_more_after() {
            break;
        }
        current = page(
            &conn,
            node.clone(),
            SearchAnchor::After(cursor(&current, current.rows().len() - 1)),
            3,
        );
        assert!(current.has_more_before());
    }
    assert_eq!(walked, order);
    assert_eq!(row_ids(&current), [3]);
    let empty = page(
        &conn,
        node.clone(),
        SearchAnchor::After(cursor(&current, 0)),
        3,
    );
    assert!(empty.rows().is_empty());
    assert!(empty.has_more_before());
    assert!(!empty.has_more_after());
    let previous = page(
        &conn,
        node.clone(),
        SearchAnchor::Before(cursor(&current, 0)),
        3,
    );
    assert_eq!(row_ids(&previous), order[3..6]);
    let top = page(
        &conn,
        node.clone(),
        SearchAnchor::Before(cursor(&previous, 0)),
        3,
    );
    assert_eq!(row_ids(&top), order[..3]);
    let empty = page(
        &conn,
        node.clone(),
        SearchAnchor::Before(cursor(&top, 0)),
        3,
    );
    assert!(empty.rows().is_empty());
    assert!(!empty.has_more_before());
    assert!(empty.has_more_after());
    for limit in [1, 2, 3, 6, 7, MAX_PAGE_ROWS] {
        let len = limit.min(order.len());
        for (rank, id) in order.iter().enumerate() {
            let start = rank.saturating_sub(len / 2).min(order.len() - len);
            let around = page(&conn, node.clone(), SearchAnchor::Around(*id), limit);
            assert_eq!(
                row_ids(&around),
                order[start..start + len],
                "id={id} limit={limit}"
            );
            assert_eq!(around.has_more_before(), start > 0);
            assert_eq!(around.has_more_after(), start + len < order.len());
        }
    }
    assert!(matches!(
        read(
            &conn,
            node,
            SearchScope::All,
            SearchAnchor::Around(99999),
            3,
            None
        ),
        Ok(PageRead::NotInWindow)
    ));
}

#[test]
fn date_keysets_preserve_both_i64_extremes_and_ties() {
    let conn = fixture(4);
    dates(
        &conn,
        &[(1, i64::MIN), (2, i64::MIN), (3, i64::MAX), (4, i64::MAX)],
    );
    let node = json!({"and":[]});
    let mut current = page(&conn, node.clone(), SearchAnchor::Top, 1);
    let mut walked = row_ids(&current);
    while current.has_more_after() {
        current = page(
            &conn,
            node.clone(),
            SearchAnchor::After(cursor(&current, 0)),
            1,
        );
        walked.extend(row_ids(&current));
    }
    assert_eq!(walked, [4, 3, 2, 1]);
    assert_eq!(current.rows()[0].row().sort_date, i64::MIN);
    let before = page(&conn, node, SearchAnchor::Before(cursor(&current, 0)), 4);
    assert_eq!(row_ids(&before), [4, 3, 2]);
    assert_eq!(before.rows()[0].row().sort_date, i64::MAX);
}

#[test]
fn first_fifty_and_id_index_keep_full_unknowns_and_scope_before_limit() {
    let mut conn = fixture(120);
    edit(&conn, "UPDATE occurrences SET f_seen=(message_id>10)");
    let node = json!({"or":[{"flag":"seen"},{"text":"not-indexed"}]});
    let first = page(&conn, node.clone(), SearchAnchor::Top, 50);
    assert_eq!(row_ids(&first), (71..=120).rev().collect::<Vec<_>>());
    assert_eq!(first.excluded_unknown(), 10);
    let second = page(
        &conn,
        node.clone(),
        SearchAnchor::After(cursor(&first, 49)),
        50,
    );
    assert_eq!(row_ids(&second), (21..=70).rev().collect::<Vec<_>>());
    assert_eq!(second.excluded_unknown(), 10);
    let tail = page(
        &conn,
        node.clone(),
        SearchAnchor::After(cursor(&second, 49)),
        50,
    );
    assert_eq!(row_ids(&tail), (11..=20).rev().collect::<Vec<_>>());
    assert!(!tail.has_more_after());
    let PageRead::Ready((_, ids)) = date_ids(&conn, node, SearchScope::All, None).unwrap() else {
        panic!("expected ids");
    };
    assert_eq!(ids, (11..=120).rev().collect::<Vec<_>>());
    conn.execute(
        "INSERT INTO accounts(account_id,kind,provider,display_name,state,sort_order,created_at)
        VALUES(?1,'imap','generic_imap','Small','active',1,1)",
        [OTHER_ACCOUNT],
    )
    .unwrap();
    let mb = mailbox(&conn, OTHER_ACCOUNT, "Small", "folder", None);
    let tx = conn.transaction().unwrap();
    let message = message(&tx, OTHER_ACCOUNT, Some(1));
    let occurrence = occurrence(&tx, mb, message);
    tx.execute(
        "UPDATE occurrences SET internal_date=0,f_seen=1 WHERE occurrence_id=?1",
        [occurrence],
    )
    .unwrap();
    refresh(&tx);
    tx.commit().unwrap();
    let small = ready(read(
        &conn,
        json!({"flag":"seen"}),
        SearchScope::Account(OTHER_ACCOUNT.parse().unwrap()),
        SearchAnchor::Top,
        1,
        None,
    ));
    assert_eq!(row_ids(&small), [occurrence]);
    let filtered = page(
        &conn,
        json!({"account":OTHER_ACCOUNT}),
        SearchAnchor::Top,
        1,
    );
    assert_eq!(row_ids(&filtered), [occurrence]);
}

fn alpha_fixture() -> Connection {
    let conn = fixture(4);
    for id in 1..=4 {
        doc(&conn, id, "indexed", "[]", &[("subject", "alpha")]);
    }
    dates(&conn, &[(1, 100), (2, 200), (3, 300), (4, 400)]);
    conn
}

#[test]
fn cursor_wire_binds_exact_keys_before_generation_then_reopens() {
    let conn = alpha_fixture();
    let node = json!({"text":"alpha"});
    let first = page(&conn, node.clone(), SearchAnchor::Top, 2);
    let valid = cursor(&first, 0);
    let fields: Json = serde_json::from_str(&valid).unwrap();
    assert_eq!(
        fields
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [
            "generation",
            "incarnation",
            "occurrence_id",
            "sort_date",
            "spec",
            "store",
            "v"
        ]
    );
    assert_eq!(fields["sort_date"], 400);
    assert_eq!(
        fields["spec"],
        json!({"query":{"q":{"text":"alpha"},"v":1},"scope":"all","sort":"date"})
    );
    let reopened = ready(read(
        &conn,
        node.clone(),
        SearchScope::All,
        SearchAnchor::After(valid.clone()),
        2,
        Some(first.generation()),
    ));
    assert_eq!(row_ids(&reopened), [3, 2]);
    let mut bad = vec![
        "not-json".into(),
        "[]".into(),
        format!(" {valid}"),
        serde_json::to_string_pretty(&fields).unwrap(),
        " ".repeat(MAX_QUERY_INPUT_BYTES + 1025),
        format!("{{\"generation\":0,{}", &valid[1..]),
        valid.replacen(
            "\"sort\":\"date\"",
            "\"sort\":\"date\",\"sort\":\"date\"",
            1,
        ),
    ];
    for key in [
        "v",
        "store",
        "incarnation",
        "spec",
        "generation",
        "occurrence_id",
        "sort_date",
    ] {
        bad.push(forged(&valid, |value| {
            value.as_object_mut().unwrap().remove(key);
        }));
    }
    for (key, value) in [
        ("v", json!(2)),
        ("v", json!("1")),
        ("generation", json!(-1)),
        ("generation", json!(1.0)),
        ("generation", json!("1")),
        ("occurrence_id", json!("4")),
        ("sort_date", json!(400.0)),
        ("sort_date", Json::Null),
        ("store", json!("invalid")),
        ("incarnation", json!("invalid")),
        ("store", json!(OTHER_ACCOUNT)),
        ("extra", json!(0)),
        ("score_bits", json!("0000000000000000")),
    ] {
        bad.push(forged(&valid, |cursor| {
            cursor[key] = value;
        }));
    }
    bad.push(forged(&valid, |value| {
        value["spec"]["query"] = json!({"q":{"text":"other"},"v":1});
    }));
    bad.push(forged(&valid, |value| {
        value["spec"]["scope"] = json!({"mailbox":1});
    }));
    bad.push(forged(&valid, |value| {
        value["spec"]["sort"] = json!("relevance");
    }));
    bad.push(forged(&valid, |value| {
        value["spec"]["extra"] = json!(0);
    }));
    for cursor in bad {
        assert!(
            matches!(
                read(
                    &conn,
                    node.clone(),
                    SearchScope::All,
                    SearchAnchor::After(cursor.clone()),
                    2,
                    None
                ),
                Err(StoreError::InvalidInput { .. })
            ),
            "{cursor}"
        );
    }
    for cursor in [
        forged(&valid, |value| {
            value["sort_date"] = json!(401);
        }),
        forged(&valid, |value| {
            value["occurrence_id"] = json!(3);
        }),
        forged(&valid, |value| {
            value["occurrence_id"] = json!(99999);
        }),
    ] {
        assert!(matches!(
            read(
                &conn,
                node.clone(),
                SearchScope::All,
                SearchAnchor::After(cursor),
                2,
                None
            ),
            Err(StoreError::InvalidInput { .. })
        ));
    }
    let stale = forged(&valid, |value| {
        value["generation"] = json!(first.generation() + 1);
    });
    let restored = forged(&valid, |value| {
        value["incarnation"] = json!(OTHER_ACCOUNT);
    });
    for cursor in [
        stale.clone(),
        restored,
        forged(&stale, |value| {
            value["sort_date"] = json!(999);
        }),
    ] {
        assert!(
            matches!(read(&conn,node.clone(),SearchScope::All,SearchAnchor::Before(cursor),2,None),
            Ok(PageRead::Stale{generation}) if generation==first.generation())
        );
    }
    for cursor in [
        forged(&stale, |value| {
            value["v"] = json!(9);
        }),
        forged(&stale, |value| {
            value["spec"]["sort"] = json!("relevance");
        }),
    ] {
        assert!(matches!(
            read(
                &conn,
                node.clone(),
                SearchScope::All,
                SearchAnchor::After(cursor),
                2,
                None
            ),
            Err(StoreError::InvalidInput { .. })
        ));
    }
    assert!(matches!(
        read(
            &conn,
            node.clone(),
            SearchScope::All,
            SearchAnchor::Top,
            2,
            Some(first.generation() + 1)
        ),
        Ok(PageRead::Stale { .. })
    ));
    assert!(matches!(
        date_ids(&conn, node, SearchScope::All, Some(first.generation() + 1)),
        Ok(PageRead::Stale { .. })
    ));
}

#[test]
fn other_copies_are_around_aliases_but_never_cursor_rows() {
    let mut conn = fixture(2);
    doc(&conn, 1, "indexed", "[]", &[("subject", "alpha")]);
    doc(&conn, 2, "indexed", "[]", &[("subject", "other")]);
    let account = archive_account(&conn);
    let archive = mailbox(&conn, &account, "Archive", "folder", None);
    let tx = conn.transaction().unwrap();
    let first = occurrence(&tx, archive, 1);
    let other = occurrence(&tx, archive, 1);
    tx.commit().unwrap();
    dates(&conn, &[(1, 400), (first, 200), (other, 100)]);
    let node = json!({"text":"alpha"});
    let chosen = ready(read(
        &conn,
        node.clone(),
        SearchScope::Mailbox(archive),
        SearchAnchor::Around(1),
        3,
        None,
    ));
    assert_eq!(row_ids(&chosen), [first]);
    let alias = ready(read(
        &conn,
        node.clone(),
        SearchScope::Mailbox(archive),
        SearchAnchor::Around(other),
        3,
        None,
    ));
    assert_eq!(chosen, alias);
    let bad = forged(&cursor(&chosen, 0), |value| {
        value["occurrence_id"] = json!(other);
        value["sort_date"] = json!(100);
    });
    assert!(matches!(
        read(
            &conn,
            node.clone(),
            SearchScope::Mailbox(archive),
            SearchAnchor::After(bad),
            2,
            None
        ),
        Err(StoreError::InvalidInput { .. })
    ));
    for (scope, id) in [
        (SearchScope::All, 2),
        (SearchScope::Mailbox(archive), 2),
        (SearchScope::All, 99999),
    ] {
        assert!(matches!(
            read(
                &conn,
                node.clone(),
                scope,
                SearchAnchor::Around(id),
                2,
                None
            ),
            Ok(PageRead::NotInWindow)
        ));
    }
    edit(
        &conn,
        &format!(
            "UPDATE occurrences SET state='vanished' WHERE occurrence_id IN ({first},{other})"
        ),
    );
    let empty = ready(read(
        &conn,
        node,
        SearchScope::Mailbox(archive),
        SearchAnchor::Top,
        2,
        None,
    ));
    assert!(empty.rows().is_empty());
}

#[test]
fn limits_scope_phase_and_identity_failures_are_explicit() {
    let conn = fixture(0);
    let node = json!({"and":[]});
    for limit in [0, MAX_PAGE_ROWS + 1, usize::MAX] {
        assert!(matches!(
            read(
                &conn,
                node.clone(),
                SearchScope::All,
                SearchAnchor::Top,
                limit,
                None
            ),
            Err(StoreError::InvalidInput { .. })
        ));
    }
    for limit in [1, 50, MAX_PAGE_ROWS] {
        let empty = page(&conn, node.clone(), SearchAnchor::Top, limit);
        assert!(empty.rows().is_empty());
        assert_eq!(empty.excluded_unknown(), 0);
        assert!(!empty.has_more_before() && !empty.has_more_after());
    }
    for scope in [SearchScope::Mailbox(0), SearchScope::Mailbox(-1)] {
        assert!(matches!(
            read(
                &conn,
                node.clone(),
                scope.clone(),
                SearchAnchor::Top,
                1,
                None
            ),
            Err(StoreError::InvalidInput { .. })
        ));
        assert!(matches!(
            date_ids(&conn, node.clone(), scope, None),
            Err(StoreError::InvalidInput { .. })
        ));
    }
    for scope in [
        SearchScope::Mailbox(99999),
        SearchScope::Account(ABSENT.parse().unwrap()),
    ] {
        assert!(matches!(
            read(
                &conn,
                node.clone(),
                scope.clone(),
                SearchAnchor::Top,
                1,
                None
            ),
            Err(StoreError::NotFound { .. })
        ));
        assert!(matches!(
            date_ids(&conn, node.clone(), scope, None),
            Err(StoreError::NotFound { .. })
        ));
    }
    for phase in ["reset", "enqueue"] {
        conn.execute("DELETE FROM search_rebuild_jobs", []).unwrap();
        conn.execute(
            "INSERT INTO search_rebuild_jobs(job_id,epoch,phase) VALUES('job',1,?1)",
            [phase],
        )
        .unwrap();
        for scope in [SearchScope::All, SearchScope::Mailbox(1)] {
            assert!(matches!(
                read(
                    &conn,
                    node.clone(),
                    scope.clone(),
                    SearchAnchor::Top,
                    1,
                    None
                ),
                Err(StoreError::Unavailable { .. })
            ));
            assert!(matches!(
                date_ids(&conn, node.clone(), scope, None),
                Err(StoreError::Unavailable { .. })
            ));
        }
    }
    conn.execute("DELETE FROM search_rebuild_jobs", []).unwrap();
    let store: String = conn
        .query_row(
            "SELECT value FROM store_meta WHERE key='store_uuid'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    for sql in [
        "UPDATE store_meta SET value='broken' WHERE key='store_uuid'",
        "DELETE FROM store_meta WHERE key='store_uuid'",
    ] {
        conn.execute_batch(sql).unwrap();
        assert!(matches!(
            read(
                &conn,
                node.clone(),
                SearchScope::All,
                SearchAnchor::Top,
                1,
                None
            ),
            Err(StoreError::Corrupt { .. })
        ));
        conn.execute(
            "INSERT OR REPLACE INTO store_meta(key,value) VALUES('store_uuid',?1)",
            [&store],
        )
        .unwrap();
    }
    conn.execute(
        "UPDATE store_meta SET value='-1' WHERE key='search_generation'",
        [],
    )
    .unwrap();
    assert!(matches!(
        date_ids(&conn, node, SearchScope::All, None),
        Err(StoreError::Corrupt { .. })
    ));
}

#[test]
fn display_fields_tags_and_owned_accessors_share_the_page() {
    let conn = fixture(1);
    summary(&conn, 1, "ok", 2, Summary::sender("sum@example.com"));
    edit(
        &conn,
        "UPDATE d_message_summary SET subject='Summary',snippet='Snippet',has_attachments=1;
        INSERT INTO message_headers(message_id,raw_fields,subject,from_json,fetched_at)
        VALUES(1,x'01','Header','[{\"name\":\"Display\",\"address\":\"head@example.com\"}]',1);
        UPDATE occurrences SET f_seen=1,f_flagged=1;
        INSERT INTO tags(tag_id,name) VALUES('a','Zulu'),('b','Alpha');
        INSERT INTO message_tags(message_id,tag_id,added_at) VALUES(1,'a',1),(1,'b',1);",
    );
    let page = page(&conn, json!({"and":[]}), SearchAnchor::Top, 50);
    let row = page.rows()[0].row();
    assert_eq!(row.sender.as_deref(), Some("Display"));
    assert_eq!(row.subject.as_deref(), Some("Header"));
    assert_eq!(row.snippet.as_deref(), Some("Snippet"));
    assert!(!row.unread);
    assert!(row.flagged && row.has_attachments);
    assert_eq!(row.tags, ["Alpha", "Zulu"]);
    let expected = row.clone();
    let (row, cursor) = page.into_rows().pop().unwrap().into_parts();
    assert_eq!(row, expected);
    assert_eq!(
        serde_json::from_str::<Json>(&cursor).unwrap()["occurrence_id"],
        row.occurrence_id
    );
}

#[test]
fn a_wal_writer_cannot_move_page_rows_ids_unknowns_or_generation() {
    use crate::schema::{MIGRATIONS, run_migrations};
    use crate::testing::test_seams;
    use sb_test_support::{ScratchDir, real_fs};

    let scratch = ScratchDir::new("date-page-snapshot");
    let path = scratch.path().join("state.sqlite");
    let mut writer = Connection::open(&path).unwrap();
    writer
        .execute_batch("PRAGMA journal_mode=WAL;PRAGMA foreign_keys=ON;PRAGMA trusted_schema=OFF")
        .unwrap();
    run_migrations(&mut writer, &test_seams(real_fs()), MIGRATIONS).unwrap();
    populate(&mut writer, 4);
    edit(&writer, "UPDATE occurrences SET f_seen=(message_id>=3)");
    let reader = Connection::open(&path).unwrap();
    reader
        .execute_batch("PRAGMA foreign_keys=ON;PRAGMA trusted_schema=OFF")
        .unwrap();
    let node = query(json!({"or":[{"flag":"seen"},{"text":"unindexed"}]}));
    let tx = reader.unchecked_transaction().unwrap();
    let before = ready(read_search_date_page(
        &tx,
        &node,
        SearchScope::All,
        SearchAnchor::Top,
        2,
        None,
    ));
    assert_eq!(row_ids(&before), [4, 3]);
    assert_eq!(before.excluded_unknown(), 2);
    let write = writer.transaction().unwrap();
    write
        .execute(
            "UPDATE occurrences SET internal_date=?1,f_seen=1 WHERE occurrence_id=1",
            [FIXED_NOW_MS + DAY_MS],
        )
        .unwrap();
    refresh(&write);
    index::enqueue(
        &write,
        &[(1, index::IndexReason::Reextract)],
        crate::Timestamp::from_millis(FIXED_NOW_MS),
    )
    .unwrap();
    write.commit().unwrap();
    let held = ready(read_search_date_page(
        &tx,
        &node,
        SearchScope::All,
        SearchAnchor::Top,
        2,
        Some(before.generation()),
    ));
    assert_eq!(held, before);
    assert_eq!(
        search_date_ids(&tx, &node, SearchScope::All, Some(before.generation())).unwrap(),
        PageRead::Ready((before.generation(), vec![4, 3]))
    );
    tx.commit().unwrap();
    let tx = reader.unchecked_transaction().unwrap();
    assert!(
        matches!(read_search_date_page(&tx,&node,SearchScope::All,SearchAnchor::After(cursor(&before,0)),2,None),
        Ok(PageRead::Stale{generation}) if generation==before.generation()+1)
    );
    let fresh = ready(read_search_date_page(
        &tx,
        &node,
        SearchScope::All,
        SearchAnchor::Top,
        2,
        None,
    ));
    assert_eq!(fresh.generation(), before.generation() + 1);
    assert_eq!(row_ids(&fresh), [1, 4]);
    assert_eq!(fresh.excluded_unknown(), 1);
    tx.commit().unwrap();
}
