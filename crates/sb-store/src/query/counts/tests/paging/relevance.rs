use super::*;
use crate::query::{read_search_relevance_page, search_relevance_ids};

const ALL: &str =
    "subject from_text to_text cc_text body_authored body_quoted body_signature attachment_names";

fn read(
    conn: &Connection,
    node: Json,
    scope: SearchScope,
    anchor: SearchAnchor,
    limit: usize,
    at: Option<u64>,
) -> Result<PageRead<SearchPage>, StoreError> {
    let tx = conn.unchecked_transaction().unwrap();
    let result =
        read_search_relevance_page(&tx, &query(node), scope, anchor, limit, at, Kept::default());
    tx.commit().unwrap();
    result
}

fn page(conn: &Connection, node: Json, anchor: SearchAnchor, limit: usize) -> SearchPage {
    ready(read(conn, node, SearchScope::All, anchor, limit, None))
}

fn all_ids(conn: &Connection, node: Json, scope: SearchScope) -> (u64, Vec<i64>) {
    let PageRead::Ready(result) = ids_result(conn, node, scope, None).unwrap() else {
        panic!("expected relevance ids");
    };
    result
}

fn ids_result(
    conn: &Connection,
    node: Json,
    scope: SearchScope,
    at: Option<u64>,
) -> Result<PageRead<(u64, Vec<i64>)>, StoreError> {
    let tx = conn.unchecked_transaction().unwrap();
    let result = search_relevance_ids(&tx, &query(node), scope, at);
    tx.commit().unwrap();
    result
}

fn bits(value: &str) -> u64 {
    let value: Json = serde_json::from_str(value).unwrap();
    u64::from_str_radix(value["score_bits"].as_str().unwrap(), 16).unwrap()
}

// Independent native oracle, with literal spec/06 weights and column names.
fn native(conn: &Connection, doc: i64, columns: &str, tokens: &str) -> f64 {
    conn.query_row(
        "SELECT bm25(d_search_fts,4.0,2.0,1.0,1.0,1.0,0.25,0.1,0.75)
         FROM d_search_fts WHERE d_search_fts MATCH ?1 AND rowid=?2",
        params![format!("{{{columns}}} : \"{tokens}\""), doc],
        |row| row.get(0),
    )
    .unwrap()
}

#[test]
fn three_fifty_hit_pages_keep_native_bits_ties_reverse_and_around_order() {
    let conn = fixture(130);
    for id in 1..=120 {
        doc(
            &conn,
            id,
            "indexed",
            "[]",
            &[("subject", &"alpha ".repeat((id % 5 + 1) as usize))],
        );
    }
    dates(
        &conn,
        &(1..=130).map(|id| (id, 131 - id)).collect::<Vec<_>>(),
    );
    let node = json!({"text":"alpha"});
    let mut scores: Vec<_> = (1..=120)
        .map(|id| (id, native(&conn, id, ALL, "alpha")))
        .collect();
    scores.sort_by(|a, b| a.1.total_cmp(&b.1).then(b.0.cmp(&a.0)));
    assert!(scores.windows(2).any(|pair| pair[0].1 == pair[1].1));
    assert!(scores.first().unwrap().1 < scores.last().unwrap().1);
    let expected: Vec<_> = scores.iter().map(|(id, _)| *id).collect();
    let (generation, ids) = all_ids(&conn, node.clone(), SearchScope::All);
    assert_eq!(ids, expected);
    assert_ne!(
        super::date_ids(&conn, node.clone(), SearchScope::All, None).unwrap(),
        PageRead::Ready((generation, expected.clone()))
    );
    let mut walked = Vec::new();
    let mut anchor = SearchAnchor::Top;
    for number in 0..3 {
        let current = ready(read(
            &conn,
            node.clone(),
            SearchScope::All,
            anchor,
            50,
            Some(generation),
        ));
        let end = expected.len().min((number + 1) * 50);
        assert_eq!(row_ids(&current), expected[number * 50..end]);
        assert_eq!(current.generation(), generation);
        assert_eq!(current.excluded_unknown(), 10);
        assert_eq!(current.has_more_before(), number > 0);
        assert_eq!(current.has_more_after(), number < 2);
        for hit in current.rows() {
            assert_eq!(
                bits(hit.cursor()),
                native(&conn, hit.row().message_id, ALL, "alpha").to_bits()
            );
        }
        anchor = SearchAnchor::After(cursor(&current, current.rows().len() - 1));
        walked.extend(row_ids(&current));
    }
    assert_eq!(walked, expected);
    let last = page(&conn, node.clone(), SearchAnchor::Around(expected[119]), 1);
    let tail = page(
        &conn,
        node.clone(),
        SearchAnchor::After(cursor(&last, 0)),
        50,
    );
    assert!(tail.rows().is_empty() && tail.has_more_before() && !tail.has_more_after());
    let previous = page(
        &conn,
        node.clone(),
        SearchAnchor::Before(cursor(&last, 0)),
        5,
    );
    assert_eq!(row_ids(&previous), expected[114..119]);
    let first = page(&conn, node.clone(), SearchAnchor::Top, 1);
    let head = page(
        &conn,
        node.clone(),
        SearchAnchor::Before(cursor(&first, 0)),
        50,
    );
    assert!(head.rows().is_empty() && !head.has_more_before() && head.has_more_after());
    for limit in [1, 2, 3, 50, 120, MAX_PAGE_ROWS] {
        let len = limit.min(expected.len());
        for rank in [0usize, 1, 53, 118, 119] {
            let start = rank.saturating_sub(len / 2).min(expected.len() - len);
            let around = page(
                &conn,
                node.clone(),
                SearchAnchor::Around(expected[rank]),
                limit,
            );
            assert_eq!(row_ids(&around), expected[start..start + len]);
            assert_eq!(around.has_more_before(), start > 0);
            assert_eq!(around.has_more_after(), start + len < expected.len());
        }
    }
}

#[test]
fn weights_native_tokens_column_sets_and_first_encounter_polarity_are_exact() {
    let conn = fixture(12);
    for (index, (column, _)) in COLUMNS.iter().enumerate() {
        doc(
            &conn,
            index as i64 + 1,
            "indexed",
            "[]",
            &[(column, "alpha")],
        );
    }
    let all = page(&conn, json!({"text":"alpha"}), SearchAnchor::Top, 50);
    for hit in all.rows() {
        assert_eq!(
            bits(hit.cursor()),
            native(&conn, hit.row().message_id, ALL, "alpha").to_bits()
        );
    }
    assert_eq!(all.rows().len(), 8);
    for (field_name, columns, ids) in [
        ("subject", "subject", vec![1]),
        ("from", "from_text", vec![2]),
        ("to", "to_text", vec![3]),
        ("cc", "cc_text", vec![4]),
        (
            "body",
            "body_authored body_quoted body_signature",
            vec![5, 6, 7],
        ),
        ("attachment", "attachment_names", vec![8]),
    ] {
        let scoped = page(
            &conn,
            field(field_name, "contains", "alpha"),
            SearchAnchor::Top,
            50,
        );
        let found: BTreeSet<_> = row_ids(&scoped).into_iter().collect();
        assert_eq!(found, ids.into_iter().collect());
        for hit in scoped.rows() {
            assert_eq!(
                bits(hit.cursor()),
                native(&conn, hit.row().message_id, columns, "alpha").to_bits()
            );
        }
    }
    doc(
        &conn,
        1,
        "indexed",
        "[]",
        &[("subject", "alpha beta gamma gamma gamma")],
    );
    let alpha = native(&conn, 1, ALL, "alpha");
    for node in [
        json!({"text":"alpha"}),
        json!({"or":[{"text":"alpha"},{"text":"ALPHÁ"}]}),
        json!({"and":[{"text":"alpha"},{"text":"alpha"}]}),
        json!({"not":{"not":{"text":"alpha"}}}),
        json!({"or":[{"not":{"text":"alpha"}},{"text":"alpha"}]}),
    ] {
        let result = page(&conn, node, SearchAnchor::Top, 50);
        let hit = result
            .rows()
            .iter()
            .find(|hit| hit.row().message_id == 1)
            .unwrap();
        assert_eq!(bits(hit.cursor()), alpha.to_bits());
    }
    let subject = native(&conn, 1, "subject", "alpha");
    let combined = page(
        &conn,
        json!({"and":[{"text":"alpha"},
        {"field":"subject","op":"contains","value":"alpha"}]}),
        SearchAnchor::Top,
        50,
    );
    assert_eq!(bits(&cursor(&combined, 0)), (alpha + subject).to_bits());
    let orders = [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ];
    let terms = ["alpha", "beta", "gamma"];
    let mut distinguishable = None;
    for frequency in 1..=32 {
        let text = format!("alpha beta {}", "gamma ".repeat(frequency));
        doc(&conn, 1, "indexed", "[]", &[("subject", &text)]);
        let scores = terms.map(|term| native(&conn, 1, ALL, term));
        let sums: BTreeSet<_> = orders
            .iter()
            .map(|[a, b, c]| ((scores[*a] + scores[*b]) + scores[*c]).to_bits())
            .collect();
        if sums.len() > 1 {
            distinguishable = Some(scores);
            break;
        }
    }
    let scores = distinguishable.expect("native fixture must distinguish f64 addition order");
    for [a, b, c] in orders {
        let expected = (scores[a] + scores[b]) + scores[c];
        let leaves: Vec<_> = [a, b, c].map(|index| json!({"text":terms[index]})).into();
        let result = page(&conn, json!({"and":leaves}), SearchAnchor::Top, 50);
        assert_eq!(row_ids(&result), [1]);
        assert_eq!(bits(&cursor(&result, 0)), expected.to_bits());
    }
}

#[test]
fn zero_unknown_and_maximum_distinct_leaves_keep_complete_predicate_rules() {
    let conn = fixture(4);
    doc(&conn, 1, "indexed", "[]", &[("subject", "alpha")]);
    doc(&conn, 2, "indexed", "[]", &[("subject", "other")]);
    doc(
        &conn,
        3,
        "partial",
        "[\"subject\"]",
        &[("subject", "alpha")],
    );
    edit(&conn, "UPDATE occurrences SET f_seen=(message_id>=3)");
    let mixed = page(
        &conn,
        json!({"or":[{"text":"alpha"},{"flag":"seen"}]}),
        SearchAnchor::Top,
        50,
    );
    assert_eq!(row_ids(&mixed), [1, 4, 3]);
    assert_eq!(bits(&cursor(&mixed, 1)), 0);
    assert_eq!(bits(&cursor(&mixed, 2)), 0);
    assert_eq!(mixed.excluded_unknown(), 0);
    let negated = page(
        &conn,
        json!({"not":{"text":"alpha"}}),
        SearchAnchor::Top,
        50,
    );
    assert_eq!(row_ids(&negated), [2]);
    assert_eq!(bits(&cursor(&negated, 0)), 0);
    assert_eq!(negated.excluded_unknown(), 2);
    let node = json!({"and":[]});
    let zero = page(&conn, node.clone(), SearchAnchor::Top, 2);
    let negative_zero = forged(&cursor(&zero, 1), |value| {
        value["score_bits"] = json!("8000000000000000")
    });
    let after = page(&conn, node, SearchAnchor::After(negative_zero), 2);
    assert_eq!(row_ids(&after), [2, 1]);
    let conn = fixture(1);
    doc(&conn, 1, "indexed", "[]", &[("subject", "word0")]);
    let leaves: Vec<_> = (0..MAX_QUERY_NODES - 1)
        .map(|n| json!({"text":format!("word{n}")}))
        .collect();
    let maximum = page(&conn, json!({"or":leaves}), SearchAnchor::Top, 1);
    assert_eq!(row_ids(&maximum), [1]);
    assert_eq!(
        bits(&cursor(&maximum, 0)),
        native(&conn, 1, ALL, "word0").to_bits()
    );
}

#[test]
fn scopes_choose_rows_before_limit_without_changing_corpus_or_display() {
    let mut conn = fixture(2);
    doc(&conn, 1, "indexed", "[]", &[("subject", "alpha")]);
    doc(&conn, 2, "indexed", "[]", &[("subject", "other")]);
    let account = archive_account(&conn);
    let archive = mailbox(&conn, &account, "Archive", "folder", None);
    let tx = conn.transaction().unwrap();
    let first = occurrence(&tx, archive, 1);
    let hidden = occurrence(&tx, archive, 1);
    let shared_message = message(&tx, &account, Some(1));
    let shared = occurrence(&tx, archive, shared_message);
    tx.commit().unwrap();
    summary(&conn, 1, "ok", 2, Summary::sender("sum@example.com"));
    headers(
        &conn,
        Some(r#"[{"name":"Display","address":"head@example.com"}]"#),
        None,
        None,
    );
    edit(
        &conn,
        "UPDATE message_headers SET subject='Header';
        UPDATE d_message_summary SET snippet='Snippet',has_attachments=1;
        INSERT INTO tags(tag_id,name) VALUES('a','Zulu'),('b','Alpha');
        INSERT INTO message_tags(message_id,tag_id,added_at) VALUES(1,'a',1),(1,'b',1)",
    );
    let node = json!({"text":"alpha"});
    let all = page(&conn, node.clone(), SearchAnchor::Top, 50);
    assert_eq!(row_ids(&all), [shared, 1]);
    let chosen = ready(read(
        &conn,
        node.clone(),
        SearchScope::Mailbox(archive),
        SearchAnchor::Around(hidden),
        50,
        None,
    ));
    assert_eq!(row_ids(&chosen), [shared, first]);
    assert_eq!(bits(&cursor(&chosen, 1)), bits(&cursor(&all, 1)));
    assert_eq!(
        all_ids(&conn, node.clone(), SearchScope::Mailbox(archive)).1,
        [shared, first]
    );
    assert_eq!(
        all_ids(
            &conn,
            json!({"and":[{"text":"alpha"},{"mailbox":archive}]}),
            SearchScope::All
        )
        .1,
        [shared, 1]
    );
    let display = chosen.rows()[1].row();
    assert_eq!(display.subject.as_deref(), Some("Header"));
    assert_eq!(display.sender.as_deref(), Some("Display"));
    assert_eq!(display.snippet.as_deref(), Some("Snippet"));
    assert!(display.unread && display.has_attachments);
    assert_eq!(display.tags, ["Alpha", "Zulu"]);
    let (row, token) = chosen.clone().into_rows().pop().unwrap().into_parts();
    assert_eq!(&row, display);
    assert_eq!(bits(&token), native(&conn, 1, ALL, "alpha").to_bits());
    let onward = ready(read(
        &conn,
        node.clone(),
        SearchScope::Mailbox(archive),
        SearchAnchor::After(token.clone()),
        2,
        None,
    ));
    assert!(onward.rows().is_empty() && onward.has_more_before() && !onward.has_more_after());
    let forged_copy = forged(&token, |value| value["occurrence_id"] = json!(hidden));
    assert!(matches!(
        read(
            &conn,
            node.clone(),
            SearchScope::Mailbox(archive),
            SearchAnchor::After(forged_copy),
            2,
            None
        ),
        Err(StoreError::InvalidInput { .. })
    ));
    for id in [2, 99999] {
        assert!(matches!(
            read(
                &conn,
                node.clone(),
                SearchScope::Mailbox(archive),
                SearchAnchor::Around(id),
                2,
                None
            ),
            Ok(PageRead::NotInWindow)
        ));
    }
    conn.execute(
        "INSERT INTO accounts(account_id,kind,provider,display_name,state,sort_order,created_at)
        VALUES(?1,'imap','generic_imap','Small','active',1,1)",
        [OTHER_ACCOUNT],
    )
    .unwrap();
    let small_box = mailbox(&conn, OTHER_ACCOUNT, "Small", "folder", None);
    let tx = conn.transaction().unwrap();
    let small_message = message(&tx, OTHER_ACCOUNT, None);
    let small = occurrence(&tx, small_box, small_message);
    tx.execute(
        "UPDATE occurrences SET f_seen=1,internal_date=-999 WHERE occurrence_id=?1",
        [small],
    )
    .unwrap();
    refresh(&tx);
    tx.commit().unwrap();
    let mixed = json!({"or":[{"text":"alpha"},{"flag":"seen"}]});
    assert_ne!(
        row_ids(&page(&conn, mixed.clone(), SearchAnchor::Top, 1)),
        [small]
    );
    for scope in [
        SearchScope::Account(OTHER_ACCOUNT.parse().unwrap()),
        SearchScope::Mailbox(small_box),
    ] {
        let scoped = ready(read(
            &conn,
            mixed.clone(),
            scope.clone(),
            SearchAnchor::Top,
            1,
            None,
        ));
        assert_eq!(row_ids(&scoped), [small]);
        assert_eq!(bits(&cursor(&scoped, 0)), 0);
        assert_eq!(scoped.excluded_unknown(), 0);
        assert_eq!(all_ids(&conn, mixed.clone(), scope).1, [small]);
    }
}

#[test]
fn relevance_cursor_classes_bind_before_staleness_and_keep_date_compatibility() {
    let conn = alpha_fixture();
    let node = json!({"text":"alpha"});
    let first = page(&conn, node.clone(), SearchAnchor::Top, 2);
    let valid = cursor(&first, 0);
    let after = |token, at| {
        read(
            &conn,
            node.clone(),
            SearchScope::All,
            SearchAnchor::After(token),
            2,
            at,
        )
    };
    assert_eq!(
        row_ids(&ready(after(valid.clone(), Some(first.generation())))),
        [3, 2]
    );
    let mut invalid = vec![
        "not-json".into(),
        "[]".into(),
        format!(" {valid}"),
        " ".repeat(MAX_QUERY_INPUT_BYTES + 1025),
        format!("{{\"v\":1,{}", &valid[1..]),
        valid.replacen(
            "\"sort\":\"relevance\"",
            "\"sort\":\"relevance\",\"sort\":\"relevance\"",
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
        "score_bits",
    ] {
        invalid.push(forged(&valid, |value| {
            value.as_object_mut().unwrap().remove(key);
        }));
    }
    for (key, value) in [
        ("v", json!(2)),
        ("v", json!("1")),
        ("store", json!(OTHER_ACCOUNT)),
        ("store", json!("broken")),
        ("incarnation", json!("broken")),
        ("generation", json!(-1)),
        ("generation", json!(1.0)),
        ("generation", json!("1")),
        ("occurrence_id", json!("4")),
        ("occurrence_id", json!(u64::MAX)),
        ("extra", Json::Null),
        ("sort_date", json!(400)),
        ("score_bits", json!(0)),
    ] {
        invalid.push(forged(&valid, |cursor| cursor[key] = value));
    }
    for text in [
        "",
        "0000",
        "00000000000000000",
        "gg00000000000000",
        "BFF0000000000000",
        "7ff0000000000000",
        "fff0000000000000",
        "7ff8000000000000",
        "fff8000000000001",
    ] {
        invalid.push(forged(&valid, |cursor| cursor["score_bits"] = json!(text)));
    }
    for (key, value) in [
        ("query", json!({"v":1,"q":{"and":[]}})),
        ("scope", json!({"mailbox":1})),
        ("sort", json!("date")),
        ("extra", Json::Null),
    ] {
        invalid.push(forged(&valid, |cursor| cursor["spec"][key] = value));
    }
    invalid.push(cursor(
        &super::page(&conn, node.clone(), SearchAnchor::Top, 2),
        0,
    ));
    for token in invalid {
        assert!(
            matches!(
                after(token.clone(), None),
                Err(StoreError::InvalidInput { .. })
            ),
            "{token}"
        );
        assert!(matches!(
            after(token, Some(first.generation() + 1)),
            Err(StoreError::InvalidInput { .. })
        ));
    }
    for (key, value) in [
        ("score_bits", json!(format!("{:016x}", bits(&valid) ^ 1))),
        ("occurrence_id", json!(99999)),
    ] {
        let token = forged(&valid, |cursor| cursor[key] = value);
        assert!(matches!(
            after(token, None),
            Err(StoreError::InvalidInput { .. })
        ));
    }
    for (key, value) in [
        ("generation", json!(first.generation() + 1)),
        ("incarnation", json!(OTHER_ACCOUNT)),
    ] {
        let token = forged(&valid, |cursor| {
            cursor[key] = value;
            cursor["occurrence_id"] = json!(99999);
        });
        assert_eq!(
            after(token, None).unwrap(),
            PageRead::Stale {
                generation: first.generation()
            }
        );
    }
    for anchor in [SearchAnchor::Top, SearchAnchor::Around(99999)] {
        assert_eq!(
            read(
                &conn,
                node.clone(),
                SearchScope::All,
                anchor,
                2,
                Some(first.generation() + 1)
            )
            .unwrap(),
            PageRead::Stale {
                generation: first.generation()
            }
        );
    }
    let tx = conn.unchecked_transaction().unwrap();
    assert_eq!(
        search_relevance_ids(
            &tx,
            &query(node.clone()),
            SearchScope::All,
            Some(first.generation() + 1)
        )
        .unwrap(),
        PageRead::Stale {
            generation: first.generation()
        }
    );
    tx.commit().unwrap();
    assert!(matches!(
        super::read(
            &conn,
            node,
            SearchScope::All,
            SearchAnchor::After(valid),
            2,
            None
        ),
        Err(StoreError::InvalidInput { .. })
    ));
}

#[test]
fn native_rebuild_and_out_of_scope_wal_writes_preserve_snapshot_scores() {
    use crate::schema::{MIGRATIONS, run_migrations};
    use crate::testing::test_seams;
    use sb_test_support::{ScratchDir, real_fs};

    let scratch = ScratchDir::new("relevance-corpus-snapshot");
    let path = scratch.path().join("state.sqlite");
    let mut writer = Connection::open(&path).unwrap();
    writer
        .execute_batch("PRAGMA journal_mode=WAL;PRAGMA foreign_keys=ON;PRAGMA trusted_schema=OFF")
        .unwrap();
    run_migrations(&mut writer, &test_seams(real_fs()), MIGRATIONS).unwrap();
    populate(&mut writer, 3);
    doc(&writer, 1, "indexed", "[]", &[("subject", "alpha")]);
    doc(
        &writer,
        2,
        "indexed",
        "[]",
        &[("subject", "alpha alpha alpha alpha")],
    );
    let outside = mailbox(
        &writer,
        &archive_account(&writer),
        "Outside",
        "folder",
        None,
    );
    edit(
        &writer,
        &format!("UPDATE occurrences SET mailbox_id={outside} WHERE occurrence_id=2"),
    );
    let node = json!({"text":"alpha"});
    let scope = SearchScope::Mailbox(1);
    let before = ready(read(
        &writer,
        node.clone(),
        scope.clone(),
        SearchAnchor::Top,
        50,
        None,
    ));
    assert_eq!(row_ids(&before), [1]);
    assert_eq!(before.excluded_unknown(), 1);
    let all = page(&writer, node.clone(), SearchAnchor::Top, 50);
    let whole = all
        .rows()
        .iter()
        .find(|hit| hit.row().message_id == 1)
        .unwrap();
    assert_eq!(bits(whole.cursor()), bits(&cursor(&before, 0)));
    let state_counts = |conn: &Connection| -> Vec<(String, i64)> {
        conn.prepare("SELECT state,count(*) FROM d_search_docs GROUP BY state ORDER BY state")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    let states = state_counts(&writer);
    let ids_before = all_ids(&writer, node.clone(), scope.clone());
    writer
        .execute(
            "INSERT INTO d_search_fts(d_search_fts) VALUES('rebuild')",
            [],
        )
        .unwrap();
    let rebuilt = ready(read(
        &writer,
        node.clone(),
        scope.clone(),
        SearchAnchor::Top,
        50,
        None,
    ));
    assert_eq!(rebuilt, before);
    assert_eq!(all_ids(&writer, node.clone(), scope.clone()), ids_before);
    assert_eq!(state_counts(&writer), states);
    let reader = Connection::open(&path).unwrap();
    reader
        .execute_batch("PRAGMA foreign_keys=ON;PRAGMA trusted_schema=OFF")
        .unwrap();
    let held = reader.unchecked_transaction().unwrap();
    let checked = query(node.clone());
    let reopened = ready(read_search_relevance_page(
        &held,
        &checked,
        scope.clone(),
        SearchAnchor::After(cursor(&before, 0)),
        2,
        Some(before.generation()),
        Kept::default(),
    ));
    assert!(reopened.rows().is_empty());
    let write = writer.transaction().unwrap();
    doc(
        &write,
        2,
        "indexed",
        "[]",
        &[("subject", &"noise ".repeat(40))],
    );
    index::enqueue(
        &write,
        &[(2, index::IndexReason::Reextract)],
        crate::Timestamp::from_millis(FIXED_NOW_MS),
    )
    .unwrap();
    write.commit().unwrap();
    let changed = ready(read(
        &writer,
        node.clone(),
        scope.clone(),
        SearchAnchor::Top,
        50,
        None,
    ));
    assert_eq!(row_ids(&changed), [1]);
    assert_eq!(changed.excluded_unknown(), 1);
    assert_eq!(changed.generation(), before.generation() + 1);
    assert_ne!(bits(&cursor(&changed, 0)), bits(&cursor(&before, 0)));
    assert_eq!(
        bits(&cursor(&changed, 0)),
        native(&writer, 1, ALL, "alpha").to_bits()
    );
    assert_eq!(
        ids_result(
            &writer,
            node.clone(),
            scope.clone(),
            Some(before.generation())
        )
        .unwrap(),
        PageRead::Stale {
            generation: changed.generation()
        }
    );
    let write = writer.transaction().unwrap();
    doc(&write, 3, "indexed", "[]", &[("subject", "alpha alpha")]);
    index::enqueue(
        &write,
        &[(3, index::IndexReason::Reextract)],
        crate::Timestamp::from_millis(FIXED_NOW_MS),
    )
    .unwrap();
    write.commit().unwrap();
    let same = ready(read_search_relevance_page(
        &held,
        &checked,
        scope.clone(),
        SearchAnchor::Top,
        50,
        Some(before.generation()),
        Kept::default(),
    ));
    assert_eq!(same, before);
    assert_eq!(
        search_relevance_ids(&held, &checked, scope.clone(), Some(before.generation())).unwrap(),
        PageRead::Ready((before.generation(), vec![1]))
    );
    held.commit().unwrap();
    let fresh = ready(read(
        &reader,
        node.clone(),
        scope.clone(),
        SearchAnchor::Top,
        50,
        None,
    ));
    assert_eq!(fresh.generation(), changed.generation() + 1);
    assert_eq!(fresh.excluded_unknown(), 0);
    let mut expected = [
        (1, native(&writer, 1, ALL, "alpha")),
        (3, native(&writer, 3, ALL, "alpha")),
    ];
    expected.sort_by(|a, b| a.1.total_cmp(&b.1).then(b.0.cmp(&a.0)));
    let expected_ids: Vec<_> = expected.iter().map(|(id, _)| *id).collect();
    assert_eq!(row_ids(&fresh), expected_ids);
    assert_eq!(
        all_ids(&reader, node.clone(), scope.clone()),
        (fresh.generation(), expected_ids)
    );
    for (hit, (_, score)) in fresh.rows().iter().zip(expected) {
        assert_eq!(bits(hit.cursor()), score.to_bits());
    }
    assert_eq!(
        read(
            &reader,
            node,
            scope,
            SearchAnchor::After(cursor(&before, 0)),
            2,
            None
        )
        .unwrap(),
        PageRead::Stale {
            generation: fresh.generation()
        }
    );
}

#[test]
fn relevance_limits_availability_corruption_and_native_errors_are_explicit() {
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
        assert!(empty.rows().is_empty() && !empty.has_more_before() && !empty.has_more_after());
        assert_eq!(empty.excluded_unknown(), 0);
    }
    for (scope, missing) in [
        (SearchScope::Mailbox(0), false),
        (SearchScope::Mailbox(-1), false),
        (SearchScope::Mailbox(99999), true),
        (SearchScope::Account(ABSENT.parse().unwrap()), true),
    ] {
        for result in [
            read(
                &conn,
                node.clone(),
                scope.clone(),
                SearchAnchor::Top,
                1,
                None,
            )
            .map(|_| ()),
            ids_result(&conn, node.clone(), scope, None).map(|_| ()),
        ] {
            if missing {
                assert!(matches!(result, Err(StoreError::NotFound { .. })));
            } else {
                assert!(matches!(result, Err(StoreError::InvalidInput { .. })));
            }
        }
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
                ids_result(&conn, node.clone(), scope, None),
                Err(StoreError::Unavailable { .. })
            ));
        }
    }
    conn.execute("DELETE FROM search_rebuild_jobs", []).unwrap();
    let top = |node| read(&conn, node, SearchScope::All, SearchAnchor::Top, 1, None);
    for key in ["store_uuid", "incarnation"] {
        let original: String = conn
            .query_row("SELECT value FROM store_meta WHERE key=?1", [key], |row| {
                row.get(0)
            })
            .unwrap();
        conn.execute("UPDATE store_meta SET value='broken' WHERE key=?1", [key])
            .unwrap();
        assert!(matches!(top(node.clone()), Err(StoreError::Corrupt { .. })));
        conn.execute("DELETE FROM store_meta WHERE key=?1", [key])
            .unwrap();
        assert!(matches!(top(node.clone()), Err(StoreError::Corrupt { .. })));
        conn.execute(
            "INSERT INTO store_meta(key,value) VALUES(?1,?2)",
            params![key, original],
        )
        .unwrap();
    }
    conn.execute(
        "UPDATE store_meta SET value='-1' WHERE key='search_generation'",
        [],
    )
    .unwrap();
    assert!(matches!(top(node.clone()), Err(StoreError::Corrupt { .. })));
    assert!(matches!(
        ids_result(&conn, node.clone(), SearchScope::All, None),
        Err(StoreError::Corrupt { .. })
    ));
    let conn = fixture(1);
    let top = |node| read(&conn, node, SearchScope::All, SearchAnchor::Top, 1, None);
    let text = json!({"text":"alpha"});
    for _ in 0..2 {
        doc(&conn, 1, "indexed", "[3]", &[("subject", "alpha")]);
        assert!(matches!(top(text.clone()), Err(StoreError::Corrupt { .. })));
        assert!(matches!(
            ids_result(&conn, text.clone(), SearchScope::All, None),
            Err(StoreError::Corrupt { .. })
        ));
        doc(&conn, 1, "indexed", "[]", &[("subject", "alpha")]);
        assert_eq!(
            row_ids(&page(&conn, text.clone(), SearchAnchor::Top, 1)),
            [1]
        );
    }
    conn.execute_batch("DROP TABLE d_search_fts").unwrap();
    assert!(matches!(
        top(text.clone()),
        Err(StoreError::Internal { .. })
    ));
    assert!(matches!(
        ids_result(&conn, text, SearchScope::All, None),
        Err(StoreError::Internal { .. })
    ));
    assert_eq!(row_ids(&page(&conn, node, SearchAnchor::Top, 1)), [1]);
}
