use super::*;

fn source(subject: &str) -> (Connection, i64) {
    let conn = migrated(MIGRATIONS);
    let account = archive(&conn);
    let mailbox = add_mailbox(&conn, &account, "Pricing", "folder", Some("inbox"));
    let message = add_message(&conn, &account, None);
    conn.execute(
        "INSERT INTO message_headers(message_id,raw_fields,subject,fetched_at) VALUES(?1,x'',?2,0)",
        params![message, subject],
    )
    .unwrap();
    let occurrence = add_occurrence(&conn, &account, mailbox, message, "present", false, false);
    (conn, occurrence)
}

fn refused(_: usize) -> Result<(), StoreError> {
    Err(StoreError::InvalidInput {
        message: "fixture quota exhausted".into(),
    })
}

#[test]
fn quota_is_checked_before_insert_update_delete_and_representative_mark() {
    let (mut conn, occurrence) = source("é");
    let ids = BTreeSet::from([occurrence]);
    let tx = conn.transaction().unwrap();
    assert!(refresh_priced(&tx, &ids, &mut refused).is_err());
    assert!(snapshot(&tx).is_empty());
    let mut calls = 0;
    assert!(
        refresh_priced(&tx, &ids, &mut |_| {
            calls += 1;
            if calls == 2 {
                return refused(0);
            }
            Ok(())
        })
        .is_err()
    );
    assert!(
        !rows(&tx)[0].6,
        "the refused representative write must not run"
    );
    tx.rollback().unwrap();
    assert!(snapshot(&conn).is_empty());

    let tx = conn.transaction().unwrap();
    refresh(&tx, &ids).unwrap();
    let before = snapshot(&tx);
    tx.execute("UPDATE message_headers SET subject='a longer header'", [])
        .unwrap();
    assert!(refresh_priced(&tx, &ids, &mut refused).is_err());
    assert_eq!(snapshot(&tx), before);
    tx.execute("UPDATE occurrences SET state='vanished'", [])
        .unwrap();
    assert!(refresh_priced(&tx, &ids, &mut refused).is_err());
    assert_eq!(snapshot(&tx), before);
}

#[test]
fn quota_is_checked_before_clearing_a_representative() {
    let (mut conn, first) = source("é");
    let account = archive(&conn);
    let message: i64 = conn
        .query_row(
            "SELECT message_id FROM occurrences WHERE occurrence_id=?1",
            [first],
            |r| r.get(0),
        )
        .unwrap();
    let label = add_mailbox(&conn, &account, "Label", "label", None);
    let second = add_occurrence(&conn, &account, label, message, "present", false, false);
    let ids = BTreeSet::from([first, second]);
    let tx = conn.transaction().unwrap();
    refresh(&tx, &ids).unwrap();
    let before = snapshot(&tx);
    tx.execute("UPDATE mailboxes SET kind='virtual' WHERE mailbox_id=(SELECT mailbox_id FROM occurrences WHERE occurrence_id=?1)", [first]).unwrap();
    assert!(refresh_priced(&tx, &ids, &mut refused).is_err());
    assert_eq!(snapshot(&tx), before);
    assert!(rows(&tx)[0].6);
}

#[test]
fn native_row_images_determine_utf8_charge_and_unchanged_rows_cost_nothing() {
    let (mut conn, occurrence) = source("é");
    let ids = BTreeSet::from([occurrence]);
    let tx = conn.transaction().unwrap();
    let mut charges = Vec::new();
    refresh_priced(&tx, &ids, &mut |bytes| {
        charges.push(bytes);
        Ok(())
    })
    .unwrap();
    let payload: i64 = tx
        .query_row(
            "SELECT length(CAST(subject AS BLOB)) FROM d_list_rows",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(payload, 2);
    assert_eq!(
        charges,
        [ROW_FRAME + TEXT_COPIES * 2, ROW_FRAME + REP_TEXT_COPIES * 2]
    );
    let changes = tx.total_changes();
    assert!(
        !refresh_priced(&tx, &ids, &mut |_| panic!(
            "an unchanged image has no mutation charge"
        ))
        .unwrap()
    );
    assert_eq!(tx.total_changes(), changes);
    tx.execute("UPDATE message_headers SET subject='你好'", [])
        .unwrap();
    charges.clear();
    refresh_priced(&tx, &ids, &mut |bytes| {
        charges.push(bytes);
        Ok(())
    })
    .unwrap();
    assert_eq!(charges, [ROW_FRAME + TEXT_COPIES * (2 + 6)]);
    tx.execute("UPDATE occurrences SET state='vanished'", [])
        .unwrap();
    charges.clear();
    refresh_priced(&tx, &ids, &mut |bytes| {
        charges.push(bytes);
        Ok(())
    })
    .unwrap();
    assert_eq!(charges, [ROW_FRAME + TEXT_COPIES * 6]);
    assert!(snapshot(&tx).is_empty());
}

#[test]
fn native_catalog_and_point_plan_pin_projection_text_copies() {
    let (conn, occurrence) = source("header");
    for column in ["sender", "subject", "snippet"] {
        let copies: i64 = conn.query_row("SELECT count(*) FROM pragma_index_list('d_list_rows') il JOIN pragma_index_xinfo(il.name) xi WHERE xi.key=1 AND xi.name=?1", [column], |r| r.get(0)).unwrap();
        assert_eq!(copies + 1, TEXT_COPIES as i64);
    }
    for index in ["d_list_rows_window", "d_list_rows_unified"] {
        let rep: i64 = conn
            .query_row(
                "SELECT count(*) FROM pragma_index_xinfo(?1) WHERE name='representative'",
                [index],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            rep, 0,
            "representative flips do not rewrite text-covering keys"
        );
    }
    for index in ["d_list_rows_flagged", "d_list_rows_one_representative"] {
        let text: i64 = conn.query_row("SELECT count(*) FROM pragma_index_xinfo(?1) WHERE name IN ('sender','subject','snippet')", [index], |r| r.get(0)).unwrap();
        assert_eq!(text, 0);
    }
    let mut statement = conn
        .prepare(&format!("EXPLAIN QUERY PLAN {}", price_sql()))
        .unwrap();
    let plan: Vec<String> = statement
        .query_map([occurrence], |r| r.get(3))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    for alias in ["o", "lm", "s", "h", "r"] {
        assert!(
            plan.iter()
                .any(|line| line.contains(&format!("SEARCH {alias} USING INTEGER PRIMARY KEY"))),
            "{plan:?}"
        );
    }
}
