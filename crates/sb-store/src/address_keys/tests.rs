use sb_test_support::real_fs;

use super::*;
use crate::imports::{ImportParseState, ImportSummary};
use crate::repair::{RepairCommit, begin, prepare, publish};
use crate::schema::{MIGRATIONS, run_migrations};
use crate::testing::{fixtures, test_seams};
use crate::{Timestamp, messages::ReaderAddress};

fn entry(name: Option<&str>, address: Option<&str>) -> ReaderAddress {
    ReaderAddress {
        name: name.map(str::to_owned),
        address: address.map(str::to_owned),
    }
}

fn keys(pairs: &[(&str, &str)]) -> BTreeSet<(String, String)> {
    pairs
        .iter()
        .map(|(address, domain)| (address.to_string(), domain.to_string()))
        .collect()
}

#[test]
fn keys_follow_the_leafs_received_address_rules() {
    // Empty lists hold no key and nothing unidentified: the leaf is false.
    assert_eq!(derive(Ok(Vec::new())), FieldKeys::default());

    // The local part keeps its case, the domain is lowercased, and equal
    // keys collapse.
    let both = derive(Ok(vec![
        entry(Some("Ada"), Some("Ada@Example.TEST")),
        entry(None, Some("Ada@example.test")),
        entry(None, Some("bob@other.test")),
    ]));
    assert_eq!(
        both.keys,
        keys(&[
            ("Ada@example.test", "example.test"),
            ("bob@other.test", "other.test")
        ])
    );
    assert!(!both.unidentified);

    // A name-only entry, an invalid address or an undecodable list leaves
    // the leaf unknown; identifiable keys beside them are still recorded.
    for unknown in [
        derive(Ok(vec![
            entry(Some("Ada"), None),
            entry(None, Some("x@y.test")),
        ])),
        derive(Ok(vec![entry(None, Some("not an address"))])),
        derive(Err(StoreError::corrupt("invalid reader address JSON"))),
    ] {
        assert!(unknown.unidentified);
    }
    let named = derive(Ok(vec![
        entry(Some("Ada"), None),
        entry(None, Some("x@y.test")),
    ]));
    assert_eq!(named.keys, keys(&[("x@y.test", "y.test")]));

    // From is one mailbox: both parts absent is an empty list, a name alone
    // is unidentified; To and Cc decode as stored JSON.
    let columns = |from_addr, from_name, to_json, cc_json| Columns {
        from_addr,
        from_name,
        to_json,
        cc_json,
    };
    let [from, to, cc] = derive_columns(&columns(
        None,
        None,
        Some(r#"[{"address":"to@x.test","name":null}]"#),
        Some("not json"),
    ));
    assert_eq!(from, FieldKeys::default());
    assert_eq!(to.keys, keys(&[("to@x.test", "x.test")]));
    assert!(cc.unidentified && cc.keys.is_empty());
    let [from, ..] = derive_columns(&columns(None, Some("Ada"), None, None));
    assert!(from.unidentified);
}

fn summary(from: &str, to: &str, cc: &str) -> ImportSummary {
    ImportSummary {
        parser_version: 2,
        parse_state: ImportParseState::Parsed,
        subject: Some("keys".into()),
        from_addr: Some(from.into()),
        to_json: Some(to.into()),
        cc_json: Some(cc.into()),
        ..ImportSummary::default()
    }
}

#[test]
fn completing_an_assembly_writes_its_keys() {
    let mut conn = fixtures::index_connection();
    fixtures::acquisitions(&conn, 1);
    let tx = conn.transaction().unwrap();
    let repaired = summary(
        "ada@example.test",
        r#"[{"address":"to@x.test","name":"To"}]"#,
        r#"[{"name":"Name only"}]"#,
    );
    let prepared = prepare(begin(&tx, 1, 2).unwrap().unwrap(), repaired).unwrap();
    assert!(matches!(
        publish(&tx, prepared, Timestamp::from_millis(1)).unwrap(),
        RepairCommit::Published
    ));
    assert_current(&tx);
    let from: i64 = tx
        .query_row(
            "SELECT count(*) FROM d_address_keys WHERE field='from'
             AND address_key='ada@example.test' AND acquisition_id=1 AND parser_target=2",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let cc: i64 = tx
        .query_row(
            "SELECT count(*) FROM d_address_unidentified WHERE field='cc' AND parser_target=2",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!((from, cc), (1, 1));
}

/// The migrations a store had before migration 14.
const BEFORE_14: usize = 13;

#[test]
fn migration_14_derives_keys_for_existing_summaries_and_assemblies() {
    let seams = test_seams(real_fs());
    let mut conn = fixtures::index_connection_through(BEFORE_14);
    fixtures::acquisitions(&conn, 3);
    conn.execute_batch(
        r#"INSERT INTO d_message_summary (acquisition_id, parser_version, parse_state, from_addr,
             from_name, to_json, cc_json, has_attachments) VALUES
           (1, 2, 'ok', 'ada@example.test', 'Ada', '[{"address":"to@x.test","name":null}]', NULL, 0),
           (2, 1, 'partial', NULL, 'Name only', 'not json', NULL, 0)"#,
    )
    .unwrap();
    run_migrations(&mut conn, &seams, MIGRATIONS).unwrap();
    assert_current(&conn);
    let unidentified: BTreeSet<(String, i64)> = conn
        .prepare("SELECT field, acquisition_id FROM d_address_unidentified")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        unidentified,
        BTreeSet::from([("from".to_owned(), 2), ("to".to_owned(), 2)])
    );

    // Backfill also covers completed assemblies, the same as their writer.
    let tx = conn.transaction().unwrap();
    let prepared = prepare(
        begin(&tx, 3, 2).unwrap().unwrap(),
        summary("cy@example.test", "[]", "[]"),
    )
    .unwrap();
    publish(&tx, prepared, Timestamp::from_millis(1)).unwrap();
    tx.execute_batch("DELETE FROM d_address_keys; DELETE FROM d_address_unidentified")
        .unwrap();
    backfill(&tx).unwrap();
    assert_current(&tx);
}

#[test]
fn a_summary_that_no_longer_decodes_is_unidentified_and_the_migration_succeeds() {
    let seams = test_seams(real_fs());
    let mut conn = fixtures::index_connection_through(BEFORE_14);
    fixtures::acquisitions(&conn, 1);
    conn.execute_batch(
        "INSERT INTO d_message_summary (acquisition_id, parser_version, parse_state, from_addr,
           to_json, has_attachments) VALUES (1, 2, 'ok', 'ada@example.test', CAST(x'FF' AS TEXT), 0)",
    )
    .unwrap();
    run_migrations(&mut conn, &seams, MIGRATIONS).unwrap();
    let fields: Vec<String> = conn
        .prepare("SELECT field FROM d_address_unidentified ORDER BY field")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(fields, ["cc", "from", "to"]);
    assert_current(&conn);
}

#[test]
#[should_panic(expected = "address keys drifted")]
fn the_drift_oracle_notices_missing_keys() {
    let mut conn = fixtures::index_connection();
    fixtures::acquisitions(&conn, 1);
    let tx = conn.transaction().unwrap();
    let prepared = prepare(
        begin(&tx, 1, 2).unwrap().unwrap(),
        summary("ada@example.test", "[]", "[]"),
    )
    .unwrap();
    publish(&tx, prepared, Timestamp::from_millis(1)).unwrap();
    tx.execute_batch("DELETE FROM d_address_keys").unwrap();
    assert_current(&tx);
}

#[test]
fn an_assembly_that_no_longer_reads_back_is_unidentified() {
    let mut conn = fixtures::index_connection();
    fixtures::acquisitions(&conn, 1);
    let tx = conn.transaction().unwrap();
    let prepared = prepare(
        begin(&tx, 1, 2).unwrap().unwrap(),
        summary("ada@example.test", "[]", "[]"),
    )
    .unwrap();
    publish(&tx, prepared, Timestamp::from_millis(1)).unwrap();
    tx.execute_batch(
        "DROP TRIGGER d_repair_pieces_immutable_delete;
         DELETE FROM d_repair_pieces;
         DELETE FROM d_address_keys; DELETE FROM d_address_unidentified",
    )
    .unwrap();
    backfill(&tx).unwrap();
    let fields: Vec<String> = tx
        .prepare("SELECT field FROM d_address_unidentified ORDER BY field")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(fields, ["cc", "from", "to"]);
    assert_current(&tx);
}
