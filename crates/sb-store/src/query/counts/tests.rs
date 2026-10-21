use std::collections::BTreeSet;

use rusqlite::{Connection, params};
use serde_json::{Value as Json, json};

use super::*;
use crate::query::{MAX_QUERY_DEPTH, MAX_QUERY_INPUT_BYTES, MAX_QUERY_NODES};
use crate::testing::FIXED_NOW_MS;
use crate::testing::fixtures::{acquisitions, archive_account, index_connection, mailbox, message};

const ABSENT: &str = "ffffffff-ffff-ffff-ffff-ffffffffffff";
const OTHER_ACCOUNT: &str = "00000000-0000-0000-0000-000000000000";
const DAY_MS: i64 = 86_400_000;
// Independent column names and tokens: do not derive the oracle from FIELDS.
const COLUMNS: [(&str, &str); 8] = [
    ("subject", "subjectword"),
    ("from_text", "senderword"),
    ("to_text", "recipientword"),
    ("cc_text", "copyword"),
    ("body_authored", "authoredword"),
    ("body_quoted", "quotedword"),
    ("body_signature", "signatureword"),
    ("attachment_names", "filenameword"),
];

fn occurrence(tx: &Transaction, mailbox: i64, message: i64) -> i64 {
    tx.execute(
        "INSERT INTO occurrences (account_id,mailbox_id,message_id,state,first_seen_at,last_seen_at)
         SELECT account_id,?1,message_id,'present',?3,?3 FROM logical_messages WHERE message_id=?2",
        params![mailbox, message, FIXED_NOW_MS],
    ).unwrap();
    let id = tx.last_insert_rowid();
    crate::list_rows::refresh(tx, &BTreeSet::from([id])).unwrap();
    id
}

fn populate(conn: &mut Connection, count: usize) {
    let account = archive_account(conn);
    let inbox = mailbox(conn, &account, "Inbox", "folder", None);
    acquisitions(conn, count);
    let tx = conn.transaction().unwrap();
    for id in 1..=count as i64 {
        assert_eq!(message(&tx, &account, Some(id)), id);
        occurrence(&tx, inbox, id);
    }
    tx.commit().unwrap();
}

fn fixture(count: usize) -> Connection {
    let mut conn = index_connection();
    populate(&mut conn, count);
    conn
}

fn refresh(tx: &Transaction) {
    let ids = tx
        .prepare("SELECT occurrence_id FROM occurrences")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<BTreeSet<i64>, _>>()
        .unwrap();
    crate::list_rows::refresh(tx, &ids).unwrap();
}

fn edit(conn: &Connection, sql: &str) {
    let tx = conn.unchecked_transaction().unwrap();
    tx.execute_batch(sql).unwrap();
    refresh(&tx);
    tx.commit().unwrap();
}

fn doc(conn: &Connection, id: i64, state: &str, mask: &str, columns: &[(&str, &str)]) {
    conn.execute(
        "INSERT OR REPLACE INTO d_search_docs (doc_id,extractor_version,state,truncated_fields)
         VALUES (?1,1,?2,?3)",
        params![id, state, mask],
    )
    .unwrap();
    for (column, text) in columns {
        conn.execute(
            &format!("UPDATE d_search_docs SET {column}=?1 WHERE doc_id=?2"),
            params![text, id],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO d_search_fts(d_search_fts) VALUES('rebuild')",
        [],
    )
    .unwrap();
}

#[derive(Default)]
struct Summary<'a> {
    from: Option<&'a str>,
    name: Option<&'a str>,
    to: Option<&'a str>,
    cc: Option<&'a str>,
    attachment: bool,
}

impl<'a> Summary<'a> {
    fn sender(address: &'a str) -> Self {
        Self {
            from: Some(address),
            ..Self::default()
        }
    }
}

fn summary(conn: &Connection, id: i64, state: &str, version: i64, data: Summary<'_>) {
    conn.execute(
        "INSERT OR REPLACE INTO d_message_summary (acquisition_id,parse_state,parser_version,
         from_addr,from_name,to_json,cc_json,has_attachments) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        params![
            id,
            state,
            version,
            data.from,
            data.name,
            data.to,
            data.cc,
            data.attachment
        ],
    )
    .unwrap();
}

fn headers(conn: &Connection, from: Option<&str>, to: Option<&str>, cc: Option<&str>) {
    conn.execute(
        "INSERT OR REPLACE INTO message_headers (message_id,raw_fields,from_json,to_json,cc_json,fetched_at)
         VALUES (1,x'01',?1,?2,?3,?4)", params![from, to, cc, FIXED_NOW_MS],
    ).unwrap();
}

fn query(node: Json) -> Query {
    Query::parse_json(&json!({"v":1,"q":node}).to_string()).unwrap()
}

fn totals(value: QueryCounts) -> (u64, u64) {
    (value.matching_messages(), value.excluded_unknown())
}

#[track_caller]
fn expect(result: Result<QueryCounts, StoreError>, expected: (u64, u64)) -> QueryCounts {
    let value = result.unwrap();
    assert_eq!(totals(value), expected);
    value
}

#[track_caller]
fn expect_corrupt(result: Result<QueryCounts, StoreError>) {
    assert!(
        matches!(result, Err(StoreError::Corrupt { .. })),
        "{result:?}"
    );
}

fn run(conn: &Connection, node: Json, scope: SearchScope) -> Result<QueryCounts, StoreError> {
    let tx = conn.unchecked_transaction().unwrap();
    let result = counts(&tx, &query(node), scope);
    tx.commit().unwrap();
    result
}

fn check(conn: &Connection, cases: &[(Json, (u64, u64))]) {
    for (node, expected) in cases {
        assert_eq!(
            totals(run(conn, node.clone(), SearchScope::All).unwrap()),
            *expected,
            "{node}"
        );
    }
}

fn field(name: &str, op: &str, value: &str) -> Json {
    json!({"field":name,"op":op,"value":value})
}

fn ids(tx: &Transaction, node: Json, scope: SearchScope, limit: i64) -> Vec<i64> {
    let mut compiled = compile(tx, &query(node), scope).unwrap();
    compiled.values.push(limit.into());
    let sql = format!(
        "SELECT id FROM ({}) WHERE v=1 ORDER BY id LIMIT ?{}",
        compiled.source,
        compiled.values.len()
    );
    compiled
        .read(tx, &sql, |statement, values| {
            statement
                .query_map(params_from_iter(values), |row| row.get(0))?
                .collect()
        })
        .unwrap()
}

#[test]
fn every_column_cut_has_independent_positive_and_negative_controls() {
    let conn = fixture(1);
    let selectors: [(Option<&str>, &[&str]); 7] = [
        (
            None,
            &[
                "subject",
                "from_text",
                "to_text",
                "cc_text",
                "body_authored",
                "body_quoted",
                "body_signature",
                "attachment_names",
            ],
        ),
        (Some("subject"), &["subject"]),
        (Some("from"), &["from_text"]),
        (Some("to"), &["to_text"]),
        (Some("cc"), &["cc_text"]),
        (
            Some("body"),
            &["body_authored", "body_quoted", "body_signature"],
        ),
        (Some("attachment"), &["attachment_names"]),
    ];
    for cut in std::iter::once(None).chain(COLUMNS.iter().map(|(name, _)| Some(*name))) {
        let mask = cut.map_or("[]".into(), |name| json!([name]).to_string());
        doc(
            &conn,
            1,
            if cut.is_some() { "partial" } else { "indexed" },
            &mask,
            &COLUMNS,
        );
        for (selector, needed) in selectors {
            for (column, token) in COLUMNS.into_iter().chain([("absent", "absentword")]) {
                let node =
                    selector.map_or(json!({"text":token}), |name| field(name, "contains", token));
                let expected = if cut.is_some_and(|name| needed.contains(&name)) {
                    (0, 1)
                } else {
                    (u64::from(needed.contains(&column)), 0)
                };
                let got = totals(run(&conn, node.clone(), SearchScope::All).unwrap());
                assert_eq!(got, expected, "cut={cut:?} query={node} column={column}");
            }
        }
    }
}

#[test]
fn document_classes_queue_shared_bytes_and_literals_keep_their_meaning() {
    let conn = fixture(2);
    edit(&conn, "UPDATE logical_messages SET acquisition_id=1");
    let text = json!({"text":"invoice OR body:trap"});
    doc(
        &conn,
        1,
        "indexed",
        "[]",
        &[(
            "subject",
            "invoice OR body:trap paid café a-1234 alice@x.test",
        )],
    );
    let tx = conn.unchecked_transaction().unwrap();
    index::enqueue(
        &tx,
        &[(1, index::IndexReason::Reextract)],
        crate::Timestamp::from_millis(FIXED_NOW_MS),
    )
    .unwrap();
    tx.commit().unwrap();
    check(
        &conn,
        &[
            (text.clone(), (2, 0)),
            (json!({"text":"paid cafe"}), (2, 0)),
            (json!({"text":"a-1234"}), (2, 0)),
            (json!({"text":"alice@x.test"}), (2, 0)),
            (json!({"text":"invoice trap"}), (0, 0)),
            (json!({"text":"' OR 1=1 --"}), (0, 0)),
        ],
    );
    for state in ["missing", "quarantined"] {
        conn.execute("UPDATE acquisitions SET object_state=?1", [state])
            .unwrap();
        check(&conn, &[(text.clone(), (0, 2))]);
    }
    edit(
        &conn,
        "UPDATE acquisitions SET object_state='published'; DELETE FROM d_index_queue;",
    );
    doc(&conn, 1, "failed", "[]", &[]);
    check(&conn, &[(text.clone(), (0, 2))]);
    doc(
        &conn,
        1,
        "indexed",
        "[]",
        &[("subject", "invoice OR body:trap")],
    );
    edit(&conn, "UPDATE d_search_docs SET extractor_version=2");
    check(&conn, &[(text.clone(), (0, 2))]);
    edit(&conn, "DELETE FROM d_search_docs");
    check(&conn, &[(text.clone(), (0, 2))]);
    edit(&conn, "UPDATE logical_messages SET acquisition_id=NULL");
    check(&conn, &[(text, (0, 2))]);
}

#[test]
fn metadata_leaves_use_live_values_and_exact_date_endpoints() {
    let conn = fixture(4);
    edit(&conn, "UPDATE occurrences SET f_seen=(message_id=1),f_flagged=(message_id=2),
        f_answered=(message_id=3),f_draft=(message_id=4),rfc822_size=999;
        UPDATE acquisitions SET byte_length=10 WHERE acquisition_id=1;
        UPDATE acquisitions SET byte_length=40 WHERE acquisition_id=4;
        UPDATE logical_messages SET acquisition_id=NULL WHERE message_id IN (2,3);
        UPDATE occurrences SET rfc822_size=20 WHERE message_id=2;
        UPDATE occurrences SET rfc822_size=NULL WHERE message_id=3;
        INSERT INTO tags(tag_id,name) VALUES('00000000-0000-0000-0000-000000000000','Quoted '' tag');
        INSERT INTO message_tags(message_id,tag_id,added_at) VALUES(1,'00000000-0000-0000-0000-000000000000',1);
        INSERT INTO notes(note_id,message_id,body_source,created_at,updated_at) VALUES('note',1,'x',1,1);");
    let attached = Summary {
        attachment: true,
        ..Summary::default()
    };
    summary(&conn, 1, "ok", 1, attached);
    summary(&conn, 4, "ok", 2, Summary::default());
    let tx = conn.unchecked_transaction().unwrap();
    for (id, offset) in [(1, -1), (2, 0), (3, DAY_MS - 1), (4, DAY_MS)] {
        tx.execute(
            "UPDATE occurrences SET internal_date=?1 WHERE message_id=?2",
            params![FIXED_NOW_MS + DAY_MS + offset, id],
        )
        .unwrap();
    }
    refresh(&tx);
    tx.commit().unwrap();
    for flag in ["seen", "flagged", "answered", "draft"] {
        check(&conn, &[(json!({"flag":flag}), (1, 0))]);
    }
    check(
        &conn,
        &[
            (json!({"tag":OTHER_ACCOUNT}), (1, 0)),
            (json!({"tag":ABSENT}), (0, 0)),
            (json!({"has":"note"}), (1, 0)),
            (json!({"has":"attachment"}), (1, 2)),
            (json!({"not":{"has":"attachment"}}), (1, 2)),
            (json!({"size":{"op":"lt","bytes":15}}), (1, 1)),
            (json!({"size":{"op":"eq","bytes":10}}), (1, 1)),
            (json!({"size":{"op":"eq","bytes":999}}), (0, 1)),
            (json!({"size":{"op":"gt","bytes":15}}), (2, 1)),
            (json!({"date":{"on":"2025-01-02","tz":"UTC"}}), (2, 0)),
            (json!({"date":{"before":"2025-01-02","tz":"UTC"}}), (1, 0)),
            (json!({"date":{"before":"2025-01-03","tz":"UTC"}}), (3, 0)),
            (json!({"date":{"after":"2025-01-01","tz":"UTC"}}), (3, 0)),
            (json!({"date":{"after":"2025-01-02","tz":"UTC"}}), (1, 0)),
            (json!({"date":{"after":"2025-01-03","tz":"UTC"}}), (0, 0)),
            (
                json!({"date":{"within_days":1,"tz":"UTC","clock":FIXED_NOW_MS+2*DAY_MS}}),
                (3, 0),
            ),
            (json!({"account":archive_account(&conn)}), (4, 0)),
            (json!({"account":ABSENT}), (0, 0)),
            (json!({"mailbox":99999}), (0, 0)),
        ],
    );
}

#[test]
fn copies_labels_scopes_and_membership_do_not_change_the_representative() {
    let mut conn = fixture(1);
    let account = archive_account(&conn);
    let archive = mailbox(&conn, &account, "Archive", "folder", None);
    let label = mailbox(&conn, &account, "Label A", "label", None);
    let other_label = mailbox(&conn, &account, "Label B", "label", None);
    let empty = mailbox(&conn, &account, "Empty", "folder", None);
    let tx = conn.transaction().unwrap();
    let first = occurrence(&tx, archive, 1);
    let second = occurrence(&tx, archive, 1);
    occurrence(&tx, label, 1);
    occurrence(&tx, other_label, 1);
    tx.execute(
        "UPDATE occurrences SET f_seen=1,f_flagged=1,f_answered=1,f_draft=1 WHERE occurrence_id=?1",
        [first],
    )
    .unwrap();
    tx.execute(
        "UPDATE occurrences SET f_seen=1 WHERE mailbox_id=?1",
        [label],
    )
    .unwrap();
    refresh(&tx);
    tx.commit().unwrap();
    for flag in ["seen", "flagged", "answered", "draft"] {
        let node = json!({"flag":flag});
        expect(run(&conn, node.clone(), SearchScope::All), (0, 0));
        expect(run(&conn, node, SearchScope::Mailbox(archive)), (1, 0));
    }
    check(
        &conn,
        &[
            (
                json!({"or":[{"mailbox":label},{"mailbox":other_label}]}),
                (1, 0),
            ),
            (json!({"and":[{"mailbox":label},{"flag":"seen"}]}), (0, 0)),
            (json!({"or":[{"mailbox":label},{"flag":"seen"}]}), (1, 0)),
            (
                json!({"and":[{"not":{"mailbox":empty}},{"not":{"flag":"seen"}}]}),
                (1, 0),
            ),
        ],
    );
    expect(
        run(
            &conn,
            json!({"and":[]}),
            SearchScope::Account(account.parse().unwrap()),
        ),
        (1, 0),
    );
    expect(
        run(&conn, json!({"and":[]}), SearchScope::Mailbox(empty)),
        (0, 0),
    );
    for id in [first, second] {
        conn.execute(
            "UPDATE occurrences SET state='vanished' WHERE occurrence_id=?1",
            [id],
        )
        .unwrap();
        edit(&conn, "SELECT 1");
        expect(
            run(&conn, json!({"flag":"seen"}), SearchScope::Mailbox(archive)),
            (0, 0),
        );
    }
    check(&conn, &[(json!({"mailbox":archive}), (0, 0))]);
    for id in [0, -1] {
        assert!(matches!(
            run(&conn, json!({"and":[]}), SearchScope::Mailbox(id)),
            Err(StoreError::InvalidInput { .. })
        ));
    }
    for scope in [
        SearchScope::Mailbox(99999),
        SearchScope::Account(ABSENT.parse().unwrap()),
    ] {
        assert!(matches!(
            run(&conn, json!({"and":[]}), scope),
            Err(StoreError::NotFound { .. })
        ));
    }
}

#[test]
fn all_kleene_pairs_and_checked_tree_budgets_execute_natively() {
    let conn = fixture(9);
    for left in 0..3 {
        for right in 0..3 {
            let mut cuts = Vec::new();
            if left == 2 {
                cuts.push("subject");
            }
            if right == 2 {
                cuts.push("body_authored");
            }
            cuts.sort_unstable();
            let columns = [
                ("subject", if left == 1 { "alpha" } else { "other" }),
                ("body_authored", if right == 1 { "beta" } else { "other" }),
            ];
            doc(
                &conn,
                1 + left * 3 + right,
                if cuts.is_empty() {
                    "indexed"
                } else {
                    "partial"
                },
                &json!(cuts).to_string(),
                &columns,
            );
        }
    }
    let left = field("subject", "contains", "alpha");
    let right = field("body", "contains", "beta");
    check(
        &conn,
        &[
            (json!({"and":[left.clone(),right.clone()]}), (1, 3)),
            (json!({"or":[left.clone(),right]}), (5, 3)),
            (json!({"not":left.clone()}), (3, 3)),
            (json!({"or":[left.clone(),{"not":left.clone()}]}), (6, 3)),
            (json!({"and":[]}), (9, 0)),
            (json!({"not":{"and":[]}}), (0, 0)),
            (json!({"and":vec![left;MAX_QUERY_NODES-1]}), (3, 3)),
        ],
    );
    let mut deep = json!({"and":[]});
    for _ in 0..MAX_QUERY_DEPTH - 1 {
        deep = json!({"not":deep});
    }
    check(&conn, &[(deep, (0, 0))]);
}

#[test]
fn complete_source_precedence_and_unusable_address_classes_are_conservative() {
    let conn = fixture(1);
    let positive = field("from", "is", "Ann@example.com");
    let negative = field("from", "is", "absent@example.com");
    for (state, version, unknown) in [
        ("ok", 2, 0),
        ("partial", 2, 1),
        ("failed", 2, 1),
        ("ok", 1, 1),
    ] {
        summary(&conn, 1, state, version, Summary::sender("Ann@Example.COM"));
        let expected = u64::from(unknown == 0);
        check(
            &conn,
            &[
                (positive.clone(), (expected, unknown)),
                (field("from", "domain", "example.com"), (expected, unknown)),
                (negative.clone(), (0, unknown)),
                (json!({"not":negative.clone()}), (expected, unknown)),
            ],
        );
    }
    edit(&conn, "DELETE FROM d_message_summary");
    check(&conn, &[(positive.clone(), (0, 1))]);
    summary(&conn, 1, "ok", 2, Summary::default());
    check(
        &conn,
        &[
            (positive.clone(), (0, 0)),
            (json!({"not":positive.clone()}), (1, 0)),
        ],
    );
    let name_only = Summary {
        name: Some("name only"),
        ..Summary::default()
    };
    summary(&conn, 1, "ok", 2, name_only);
    check(&conn, &[(positive.clone(), (0, 1))]);
    for list in [
        r#"[{"address":"Ann@example.com"},{"name":"unknown"}]"#,
        r#"[{"address":"Ann@example.com"},{"address":"not-an-address"}]"#,
    ] {
        let unidentifiable = Summary {
            to: Some(list),
            ..Summary::default()
        };
        summary(&conn, 1, "ok", 2, unidentifiable);
        check(
            &conn,
            &[
                (field("to", "is", "Ann@example.com"), (0, 1)),
                (json!({"not":field("to","domain","example.com")}), (0, 1)),
            ],
        );
    }
    // A complete header replaces even unusable or malformed summary fields.
    let partial = Summary {
        to: Some("{"),
        ..Summary::sender("Ann@example.com")
    };
    summary(&conn, 1, "partial", 2, partial);
    headers(&conn, None, None, None);
    check(
        &conn,
        &[
            (positive.clone(), (0, 0)),
            (field("to", "is", "Ann@example.com"), (0, 0)),
        ],
    );
    let list = r#"[{"address":"Ann@example.com"},{"address":"second@example.com"}]"#;
    let to = r#"[{"address":"recipient@to.test"}]"#;
    let cc = r#"[{"address":"copy@cc.test"}]"#;
    headers(&conn, Some(list), Some(to), Some(cc));
    edit(
        &conn,
        "UPDATE store_meta SET value='3' WHERE key='search_extractor_version'",
    );
    for (state, second_from) in [("partial", 1), ("ok", 0)] {
        if state == "ok" {
            edit(&conn, "DELETE FROM message_headers");
        }
        let data = Summary {
            to: Some(to),
            cc: Some(cc),
            ..Summary::sender("Ann@example.com")
        };
        summary(&conn, 1, state, 2, data);
        check(
            &conn,
            &[(field("from", "is", "second@example.com"), (second_from, 0))],
        );
        for (name, address, domain) in [
            ("from", "Ann@example.com", "example.com"),
            ("to", "recipient@to.test", "to.test"),
            ("cc", "copy@cc.test", "cc.test"),
        ] {
            check(
                &conn,
                &[
                    (field(name, "is", address), (1, 0)),
                    (field(name, "domain", domain), (1, 0)),
                ],
            );
        }
    }
}

#[test]
fn exact_keys_preserve_written_locals_domains_and_large_received_locals() {
    let conn = fixture(1);
    for (stored, op, operand, expected) in [
        ("\"Foo\"@Example.com", "is", "Foo@example.com", 0),
        ("Foo@Example.com", "is", "foo@example.com", 0),
        ("\"F\\oo\"@Example.com", "is", "\"Foo\"@example.com", 0),
        ("a@EXAMPLE.com.", "is", "a@example.com", 0),
        ("a@BÜCHER.test", "is", "a@xn--bcher-kva.test", 0),
        ("a@BÜCHER.test", "is", "a@bücher.test", 1),
        ("é@example.com", "is", "e\u{301}@example.com", 0),
        ("café@example.com", "is", "café@example.com", 1),
        ("a@EXAMPLE.com.", "domain", "example.com", 0),
        ("a@BÜCHER.test", "domain", "xn--bcher-kva.test", 0),
        ("a@BÜCHER.test", "domain", "bücher.test", 1),
    ] {
        summary(&conn, 1, "ok", 2, Summary::sender(stored));
        check(&conn, &[(field("from", op, operand), (expected, 0))]);
    }
    let huge = format!("{}@large.test", "h".repeat(MAX_QUERY_INPUT_BYTES + 1));
    assert!(AddressOperand::parse(&huge).is_err());
    summary(&conn, 1, "ok", 2, Summary::sender(&huge));
    check(&conn, &[(field("from", "domain", "large.test"), (1, 0))]);
}

#[test]
fn corruption_callbacks_recover_and_native_failures_keep_their_type() {
    let conn = fixture(1);
    let from = field("from", "is", "a@b.test");
    let to = field("to", "is", "a@b.test");
    for bad in [
        "{",
        "{}",
        "5",
        "[false]",
        r#"[{"address":3}]"#,
        r#"[{"name":false}]"#,
    ] {
        headers(&conn, None, Some(bad), None);
        expect_corrupt(run(&conn, to.clone(), SearchScope::All));
    }
    for (state, mask) in [
        ("partial", r#"["bogus"]"#),
        ("partial", r#"["to_text","subject"]"#),
        ("partial", r#"["subject","subject"]"#),
        ("partial", "[]"),
        ("indexed", r#"["subject"]"#),
        ("indexed", "{"),
        ("indexed", "[3]"),
    ] {
        doc(&conn, 1, state, mask, &[]);
        expect_corrupt(run(&conn, json!({"text":"x"}), SearchScope::All));
    }
    headers(&conn, Some(r#"[{"address":"a@b.test"}]"#), Some("{"), None);
    let tx = conn.unchecked_transaction().unwrap();
    for bad in [to, json!({"text":"x"})] {
        for _ in 0..2 {
            expect(counts(&tx, &query(from.clone()), SearchScope::All), (1, 0));
            expect_corrupt(counts(&tx, &query(bad.clone()), SearchScope::All));
            expect(counts(&tx, &query(from.clone()), SearchScope::All), (1, 0));
        }
    }
    tx.execute_batch("DROP TABLE d_search_fts").unwrap();
    assert!(matches!(
        counts(&tx, &query(json!({"text":"x"})), SearchScope::All),
        Err(StoreError::Internal { .. })
    ));
    expect(counts(&tx, &query(from), SearchScope::All), (1, 0));
    tx.commit().unwrap();
}

#[test]
fn scoped_predicates_precede_limits_and_counts_cover_all_candidates() {
    let mut conn = fixture(31);
    edit(&conn, "UPDATE occurrences SET f_seen=1");
    conn.execute(
        "INSERT INTO accounts(account_id,kind,provider,display_name,state,sort_order,created_at)
         VALUES(?1,'imap','generic_imap','Other','active',1,1)",
        [OTHER_ACCOUNT],
    )
    .unwrap();
    let box_id = mailbox(&conn, OTHER_ACCOUNT, "Small", "folder", None);
    let tx = conn.transaction().unwrap();
    let id = message(&tx, OTHER_ACCOUNT, Some(1));
    occurrence(&tx, box_id, id);
    tx.execute("UPDATE occurrences SET f_seen=1 WHERE message_id=?1", [id])
        .unwrap();
    refresh(&tx);
    tx.commit().unwrap();
    let scope = SearchScope::Account(OTHER_ACCOUNT.parse().unwrap());
    let tx = conn.unchecked_transaction().unwrap();
    let seen = json!({"flag":"seen"});
    expect(counts(&tx, &query(seen.clone()), SearchScope::All), (32, 0));
    assert_eq!(ids(&tx, seen.clone(), SearchScope::All, 10).len(), 10);
    assert_eq!(ids(&tx, seen.clone(), scope.clone(), 1), vec![id]);
    assert_eq!(
        ids(&tx, json!({"account":OTHER_ACCOUNT}), SearchScope::All, 1),
        vec![id]
    );
    expect(counts(&tx, &query(seen), scope), (1, 0));
    expect(
        counts(&tx, &query(json!({"text":"x"})), SearchScope::All),
        (0, 32),
    );
    tx.commit().unwrap();
}

#[test]
fn reset_and_enqueue_gate_even_empty_filter_only_reads() {
    let conn = fixture(0);
    for phase in ["reset", "enqueue", "index", "complete"] {
        conn.execute("DELETE FROM search_rebuild_jobs", []).unwrap();
        conn.execute(
            "INSERT INTO search_rebuild_jobs(job_id,epoch,phase) VALUES('job',1,?1)",
            [phase],
        )
        .unwrap();
        for node in [json!({"and":[]}), json!({"text":"x"})] {
            let result = run(&conn, node, SearchScope::Mailbox(1));
            if matches!(phase, "reset" | "enqueue") {
                assert!(
                    matches!(result, Err(StoreError::Unavailable { .. })),
                    "{phase}"
                );
            } else {
                assert_eq!(totals(result.unwrap()), (0, 0), "{phase}");
            }
        }
        assert!(matches!(
            run(&conn, json!({"and":[]}), SearchScope::Mailbox(99999)),
            Err(StoreError::NotFound { .. })
        ));
    }
}

#[test]
fn a_wal_writer_cannot_mix_target_generation_and_results_with_a_held_snapshot() {
    use crate::schema::{MIGRATIONS, run_migrations};
    use crate::testing::test_seams;
    use sb_test_support::{ScratchDir, real_fs};

    let scratch = ScratchDir::new("predicate-snapshot");
    let path = scratch.path().join("state.sqlite");
    let mut writer = Connection::open(&path).unwrap();
    writer
        .execute_batch("PRAGMA journal_mode=WAL;PRAGMA foreign_keys=ON;PRAGMA trusted_schema=OFF")
        .unwrap();
    run_migrations(&mut writer, &test_seams(real_fs()), MIGRATIONS).unwrap();
    populate(&mut writer, 1);
    doc(&writer, 1, "indexed", "[]", &[("subject", "before")]);
    let reader = Connection::open(&path).unwrap();
    let tx = reader.unchecked_transaction().unwrap();
    let node = query(json!({"text":"before"}));
    let before = expect(counts(&tx, &node, SearchScope::All), (1, 0));
    let write = writer.transaction().unwrap();
    write
        .execute_batch(
            "UPDATE store_meta SET value='2' WHERE key='search_extractor_version';
        UPDATE d_search_docs SET extractor_version=2,subject='after';
        INSERT INTO d_search_fts(d_search_fts) VALUES('rebuild')",
        )
        .unwrap();
    index::enqueue(
        &write,
        &[(1, index::IndexReason::Reextract)],
        crate::Timestamp::from_millis(FIXED_NOW_MS),
    )
    .unwrap();
    write.commit().unwrap();
    assert_eq!(counts(&tx, &node, SearchScope::All).unwrap(), before);
    tx.commit().unwrap();
    let tx = reader.unchecked_transaction().unwrap();
    let after = expect(counts(&tx, &node, SearchScope::All), (0, 0));
    assert_eq!(after.search_generation(), before.search_generation() + 1);
    expect(
        counts(&tx, &query(json!({"text":"after"})), SearchScope::All),
        (1, 0),
    );
    tx.commit().unwrap();
}
