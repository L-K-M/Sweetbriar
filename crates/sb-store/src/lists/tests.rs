use std::collections::BTreeSet;

use rusqlite::{Connection, params};
use sb_test_support::real_fs;

use super::*;
use crate::schema::{MIGRATIONS, run_migrations};
use crate::testing::{FIXED_NOW_MS, test_seams};

/// A migrated in-memory store and the local archive's account.
fn store() -> (Connection, String) {
    let mut conn = Connection::open_in_memory().unwrap();
    run_migrations(&mut conn, &test_seams(real_fs()), MIGRATIONS).unwrap();
    let account = conn
        .query_row(
            "SELECT account_id FROM accounts WHERE provider = 'local_archive'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    (conn, account)
}

fn mailbox(conn: &Connection, account: &str, name: &str, kind: &str, role: Option<&str>) -> i64 {
    conn.execute(
        "INSERT INTO mailboxes (account_id, remote_name, role, kind, selectable, sync_state)
         VALUES (?1, ?2, ?3, ?4, 1, 'synced')",
        params![account, name, role, kind],
    )
    .unwrap();
    conn.last_insert_rowid()
}

fn message(conn: &Connection, account: &str) -> i64 {
    conn.execute(
        "INSERT INTO logical_messages (account_id, created_at) VALUES (?1, ?2)",
        params![account, FIXED_NOW_MS],
    )
    .unwrap();
    conn.last_insert_rowid()
}

/// A listed occurrence of `message` dated `date`.
fn occurrence(
    conn: &mut Connection,
    account: &str,
    mailbox: i64,
    message: i64,
    date: i64,
    flagged: bool,
) -> i64 {
    let tx = conn.transaction().unwrap();
    tx.execute(
        "INSERT INTO occurrences (account_id, mailbox_id, message_id, internal_date, f_flagged,
         state, first_seen_at, last_seen_at) VALUES (?1, ?2, ?3, ?4, ?5, 'present', ?6, ?6)",
        params![account, mailbox, message, date, flagged, FIXED_NOW_MS],
    )
    .unwrap();
    let id = tx.last_insert_rowid();
    list_rows::refresh(&tx, &BTreeSet::from([id])).unwrap();
    list_rows::advance_generation(&tx).unwrap();
    tx.commit().unwrap();
    id
}

/// `count` rows in one mailbox, two per date so ties are broken by id,
/// inserted oldest-last so id order is not window order. Returns the
/// mailbox and the ids in window order.
fn filled(conn: &mut Connection, account: &str, count: i64) -> (i64, Vec<i64>) {
    let inbox = mailbox(conn, account, "Inbox", "folder", Some("inbox"));
    let mut keyed = Vec::new();
    for i in 0..count {
        let message = message(conn, account);
        let date = 1_000 * ((i * 7) % count / 2);
        let id = occurrence(conn, account, inbox, message, date, false);
        keyed.push((date, id));
    }
    keyed.sort_by(|a, b| b.cmp(a));
    (inbox, keyed.into_iter().map(|(_, id)| id).collect())
}

fn page(
    conn: &mut Connection,
    scope: ListScope,
    anchor: ListAnchor,
    limit: usize,
    at: Option<u64>,
) -> PageRead<ListPage> {
    let tx = conn.transaction().unwrap();
    read_list_page(&tx, scope, anchor, limit, at).unwrap()
}

fn ready(read: PageRead<ListPage>) -> ListPage {
    match read {
        PageRead::Ready(page) => page,
        other => panic!("expected a page, got {other:?}"),
    }
}

fn ids(page: &ListPage) -> Vec<i64> {
    page.rows.iter().map(|row| row.occurrence_id).collect()
}

/// Paging forward from `Top` with `After`, then back with `Before`,
/// visits every row once in window order — `sort_date` descending, ties
/// by `occurrence_id` descending — and the edge flags hold on every page.
#[test]
fn pages_walk_the_window_in_order_both_ways() {
    let (mut conn, account) = store();
    let (inbox, order) = filled(&mut conn, &account, 23);
    let scope = ListScope::Mailbox(inbox);
    for limit in [1, 3, 5, 23, 40] {
        let mut seen = Vec::new();
        let mut current = ready(page(&mut conn, scope, ListAnchor::Top, limit, None));
        assert!(!current.has_more_before);
        loop {
            seen.extend(ids(&current));
            // A page that fails to advance would loop forever.
            assert!(
                seen.len() <= order.len(),
                "forward walk repeats, limit {limit}"
            );
            if !current.has_more_after {
                break;
            }
            let last = current.rows.last().unwrap().key();
            current = ready(page(&mut conn, scope, ListAnchor::After(last), limit, None));
            assert!(current.has_more_before, "limit {limit}");
        }
        assert_eq!(seen, order, "forward, limit {limit}");

        let mut back = Vec::new();
        loop {
            back.splice(0..0, ids(&current));
            assert!(
                back.len() <= order.len(),
                "backward walk repeats, limit {limit}"
            );
            if !current.has_more_before {
                break;
            }
            let first = current.rows.first().unwrap().key();
            current = ready(page(
                &mut conn,
                scope,
                ListAnchor::Before(first),
                limit,
                None,
            ));
            assert!(current.has_more_after, "limit {limit}");
        }
        // The walk back starts from the last page, so it covers it too.
        assert_eq!(back, order, "backward, limit {limit}");
    }
}

/// `Around(id)` with the target at rank `r` of `N` and `L = min(limit,
/// N)` starts at `clamp(r − ⌊L/2⌋, 0, N − L)` (spec/07), for every rank
/// and limit, including ties and both ends.
#[test]
fn around_centres_on_the_target_and_clamps_at_both_ends() {
    let (mut conn, account) = store();
    let (inbox, order) = filled(&mut conn, &account, 9);
    let n = order.len();
    for limit in 1..=n + 2 {
        for (rank, &target) in order.iter().enumerate() {
            let read = ready(page(
                &mut conn,
                ListScope::Mailbox(inbox),
                ListAnchor::Around(target),
                limit,
                None,
            ));
            let len = limit.min(n);
            let start = rank.saturating_sub(len / 2).min(n - len);
            assert_eq!(
                ids(&read),
                order[start..start + len],
                "rank {rank}, limit {limit}"
            );
            assert_eq!(read.has_more_before, start > 0);
            assert_eq!(read.has_more_after, start + len < n);
        }
    }
}

/// `Unified` lists every occurrence in mailboxes of one role, in window
/// order; `Flagged` lists flagged logical messages once, through their
/// representatives, so a label-only flag does not count (spec/02).
#[test]
fn scopes_pick_by_mailbox_role_or_flagged_representative() {
    let (mut conn, account) = store();
    let inbox = mailbox(&conn, &account, "Inbox", "folder", Some("inbox"));
    let other = mailbox(&conn, &account, "Other Inbox", "folder", Some("inbox"));
    let work = mailbox(&conn, &account, "Work", "label", None);
    let home = mailbox(&conn, &account, "Home", "label", None);

    let everywhere = message(&conn, &account);
    // The labels' occurrences come first, so lower ids alone would pick one.
    let in_work = occurrence(&mut conn, &account, work, everywhere, 5_000, true);
    let in_home = occurrence(&mut conn, &account, home, everywhere, 5_000, true);
    let filed = occurrence(&mut conn, &account, inbox, everywhere, 5_000, true);
    let label_only = message(&conn, &account);
    occurrence(&mut conn, &account, inbox, label_only, 4_000, false);
    occurrence(&mut conn, &account, work, label_only, 4_000, true);
    let elsewhere = message(&conn, &account);
    let older = occurrence(&mut conn, &account, other, elsewhere, 3_000, false);

    let flagged = ready(page(
        &mut conn,
        ListScope::Flagged,
        ListAnchor::Top,
        10,
        None,
    ));
    // Once, through its folder occurrence.
    assert_eq!(ids(&flagged), vec![filed]);
    let unified = ready(page(
        &mut conn,
        ListScope::Unified(MailboxRole::Inbox),
        ListAnchor::Top,
        10,
        None,
    ));
    let inbox_rows: Vec<i64> = ids(&unified);
    assert_eq!(inbox_rows.first(), Some(&filed));
    assert_eq!(inbox_rows.last(), Some(&older));
    assert_eq!(inbox_rows.len(), 3);
    let labelled = ready(page(
        &mut conn,
        ListScope::Mailbox(work),
        ListAnchor::Top,
        10,
        None,
    ));
    assert_eq!(ids(&labelled).first(), Some(&in_work));
    assert!(!ids(&labelled).contains(&in_home));
}

/// Every row carries its message's tags, sorted, from one batched
/// lookup; rows of one message share them.
#[test]
fn rows_carry_their_messages_tags() {
    let (mut conn, account) = store();
    let inbox = mailbox(&conn, &account, "Inbox", "folder", None);
    let work = mailbox(&conn, &account, "Work", "label", None);
    let tagged = message(&conn, &account);
    let plain = message(&conn, &account);
    occurrence(&mut conn, &account, inbox, tagged, 3_000, false);
    occurrence(&mut conn, &account, work, tagged, 3_000, false);
    occurrence(&mut conn, &account, inbox, plain, 2_000, false);
    for (tag, name) in [("t-b", "receipts"), ("t-a", "family")] {
        conn.execute(
            "INSERT INTO tags (tag_id, name) VALUES (?1, ?2)",
            params![tag, name],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO message_tags (message_id, tag_id, added_at) VALUES (?1, ?2, ?3)",
            params![tagged, tag, FIXED_NOW_MS],
        )
        .unwrap();
    }
    let both = vec!["family".to_owned(), "receipts".to_owned()];
    let read = ready(page(
        &mut conn,
        ListScope::Mailbox(inbox),
        ListAnchor::Top,
        10,
        None,
    ));
    let tags: Vec<Vec<String>> = read.rows.iter().map(|row| row.tags.clone()).collect();
    assert_eq!(tags, vec![both.clone(), Vec::new()]);
    let read = ready(page(
        &mut conn,
        ListScope::Mailbox(work),
        ListAnchor::Top,
        10,
        None,
    ));
    assert_eq!(read.rows[0].tags, both);
}

/// A read at another generation is `Stale` naming the current one; an
/// anchor whose row is outside the scope is `NotInWindow`; an empty
/// page request is `InvalidInput`. The id index is window order.
#[test]
fn stale_generations_and_foreign_anchors_are_reported() {
    let (mut conn, account) = store();
    let (inbox, order) = filled(&mut conn, &account, 5);
    let other = mailbox(&conn, &account, "Other", "folder", None);
    let stranger = message(&conn, &account);
    let outside = occurrence(&mut conn, &account, other, stranger, 1, false);
    let scope = ListScope::Mailbox(inbox);
    let top = ready(page(&mut conn, scope, ListAnchor::Top, 2, None));
    assert_eq!(
        page(
            &mut conn,
            scope,
            ListAnchor::Top,
            2,
            Some(top.generation - 1)
        ),
        PageRead::Stale {
            generation: top.generation
        }
    );
    assert_eq!(
        ready(page(
            &mut conn,
            scope,
            ListAnchor::Top,
            2,
            Some(top.generation)
        )),
        top
    );
    assert_eq!(
        page(&mut conn, scope, ListAnchor::Around(outside), 2, None),
        PageRead::NotInWindow
    );
    let mut forged = top.rows[0].key();
    forged.sort_date += 1;
    for anchor in [ListAnchor::After(forged), ListAnchor::Before(forged)] {
        assert_eq!(
            page(&mut conn, scope, anchor, 2, None),
            PageRead::NotInWindow
        );
    }
    let tx = conn.transaction().unwrap();
    assert!(matches!(
        read_list_page(&tx, scope, ListAnchor::Top, 0, None),
        Err(StoreError::InvalidInput { .. })
    ));
    assert_eq!(
        list_ids(&tx, scope, None).unwrap(),
        PageRead::Ready((top.generation, order))
    );
    assert_eq!(
        list_ids(&tx, scope, Some(0)).unwrap(),
        PageRead::Stale {
            generation: top.generation
        }
    );
}

/// The plan of `sql` bound with `scope`'s parameters, one detail line
/// per step.
fn plan(conn: &Connection, sql: &str, scope: ListScope, keyed: bool) -> String {
    let mut extra = vec![(":count", 10)];
    if keyed {
        extra.extend([(":date", 0), (":occurrence", 0)]);
    }
    let params = scope.params(&extra);
    let mut statement = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
    statement
        .query_map(bound(&params).as_slice(), |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .join("\n")
}

/// Page reads seek the list indexes in window order, with no sort step
/// (plan M1.3: `d_list_rows_window`, `d_list_rows_unified`, and
/// `d_list_rows_flagged` for `Flagged`), in both directions.
#[test]
fn page_reads_use_the_list_indexes() {
    let (conn, _account) = store();
    for (scope, index) in [
        (ListScope::Mailbox(1), "d_list_rows_window"),
        (
            ListScope::Unified(MailboxRole::Inbox),
            "d_list_rows_unified",
        ),
        (ListScope::Flagged, "d_list_rows_flagged"),
    ] {
        for keyed in [false, true] {
            for direction in [Direction::Older, Direction::Newer] {
                let detail = plan(&conn, &page_sql(scope, keyed, direction), scope, keyed);
                assert!(detail.contains(index), "{scope:?} {direction:?}: {detail}");
                assert!(
                    !detail.contains("TEMP B-TREE"),
                    "{scope:?} {direction:?}: {detail}"
                );
                // A cursor seeks into the index instead of scanning to it.
                assert!(
                    !keyed || detail.starts_with("SEARCH"),
                    "{scope:?}: {detail}"
                );
            }
        }
    }
}
