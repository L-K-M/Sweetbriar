use std::time::{Duration, Instant};

use super::*;

/// A message `id` with Message-ID `header`, References `refs` and
/// subject `subject`.
fn input(id: i64, header: Option<&str>, refs: &[&str], subject: &str) -> ThreadInput {
    ThreadInput {
        message_id: id,
        header_id: header.map(str::to_owned),
        references: refs.iter().map(|r| (*r).to_owned()).collect(),
        in_reply_to: None,
        subject: Some(subject.to_owned()),
    }
}

fn reply_to(id: i64, header: &str, parent: &str, subject: &str) -> ThreadInput {
    ThreadInput {
        in_reply_to: Some(parent.to_owned()),
        ..input(id, Some(header), &[], subject)
    }
}

/// Every rotation and the reverse of `inputs` thread the same way.
fn threads_in_any_order(inputs: &[ThreadInput]) -> Vec<Vec<i64>> {
    let expected = threads(inputs);
    let mut reversed = inputs.to_vec();
    reversed.reverse();
    assert_eq!(threads(&reversed), expected, "reversed");
    for shift in 1..inputs.len() {
        let mut rotated = inputs.to_vec();
        rotated.rotate_left(shift);
        assert_eq!(threads(&rotated), expected, "rotated by {shift}");
    }
    expected
}

/// A reply chain threads together whichever arrives first, and an
/// unrelated message stays apart.
#[test]
fn a_reply_chain_is_one_thread_in_any_order() {
    let inputs = [
        input(1, Some("a@x"), &[], "Lunch"),
        input(2, Some("b@x"), &["a@x"], "Re: Lunch"),
        input(3, Some("c@x"), &["a@x", "b@x"], "Re: Lunch"),
        input(4, Some("d@x"), &[], "Budget"),
    ];
    assert_eq!(threads_in_any_order(&inputs), vec![vec![1, 2, 3], vec![4]]);
}

/// Replies to a message never seen meet in its placeholder; a later
/// arrival of the ancestor joins them.
#[test]
fn a_missing_ancestor_still_joins_its_replies() {
    let replies = [
        input(1, Some("b@x"), &["root@x"], "Notes"),
        input(2, Some("c@x"), &["root@x"], "Other notes"),
    ];
    assert_eq!(threads_in_any_order(&replies), vec![vec![1, 2]]);
    let mut all = replies.to_vec();
    all.push(input(3, Some("root@x"), &[], "Agenda"));
    assert_eq!(threads_in_any_order(&all), vec![vec![1, 2, 3]]);
}

/// Earlier references fill in a missing parent: a reply whose direct
/// parent was never seen still joins the thread above it.
#[test]
fn earlier_references_fill_in_missing_parents() {
    let inputs = [
        input(1, Some("a@x"), &[], "Plan"),
        input(2, Some("c@x"), &["a@x", "missing@x"], "Notes"),
    ];
    assert_eq!(threads_in_any_order(&inputs), vec![vec![1, 2]]);
}

/// A message's own references overrule a parent guessed from another
/// message's References (spec/02): message 2 moves from x to y, and
/// x keeps only message 3.
#[test]
fn own_references_overrule_guessed_parents() {
    let inputs = [
        input(1, Some("one@x"), &["x@x", "m@x", "one-parent@x"], "First"),
        input(2, Some("m@x"), &["y@x"], "Second"),
        input(3, Some("three@x"), &["x@x"], "Third"),
    ];
    assert_eq!(threads_in_any_order(&inputs), vec![vec![1, 2], vec![3]]);
}

/// References win over In-Reply-To; In-Reply-To links only a message
/// without References.
#[test]
fn references_come_before_in_reply_to() {
    let mut conflicted = input(3, Some("c@x"), &["a@x"], "Second");
    conflicted.in_reply_to = Some("b@x".to_owned());
    let inputs = [
        input(1, Some("a@x"), &[], "First"),
        input(2, Some("b@x"), &[], "Unrelated"),
        conflicted,
        reply_to(4, "d@x", "b@x", "Reply"),
    ];
    assert_eq!(threads_in_any_order(&inputs), vec![vec![1, 3], vec![2, 4]]);
}

/// Ids compare exactly: case differs, so these are two messages.
/// Missing ids and repeated ids get containers of their own, and a
/// reply to a repeated id joins the first message holding it.
#[test]
fn ids_are_exact_and_repeats_get_their_own_container() {
    let inputs = [
        input(1, Some("Same@X"), &[], "One"),
        input(2, Some("same@x"), &[], "Two"),
        input(3, None, &[], "Three"),
        input(4, None, &[], "Four"),
        input(5, Some("Same@X"), &[], "Five"),
        input(6, Some("r@x"), &["Same@X"], "Six"),
    ];
    assert_eq!(
        threads_in_any_order(&inputs),
        vec![vec![1, 6], vec![2], vec![3], vec![4], vec![5]]
    );
}

/// References that loop, or name the message itself, end in a finite,
/// order-independent grouping.
#[test]
fn cycles_and_self_references_are_dropped() {
    let inputs = [
        input(1, Some("a@x"), &["b@x"], "Alpha"),
        input(2, Some("b@x"), &["a@x"], "Beta"),
        input(3, Some("c@x"), &["c@x"], "Gamma"),
        input(4, Some("d@x"), &["e@x", "d@x", "e@x"], "Delta"),
    ];
    let grouped = threads_in_any_order(&inputs);
    assert!(grouped.contains(&vec![1, 2]), "{grouped:?}");
    assert!(grouped.contains(&vec![3]), "{grouped:?}");
    assert!(grouped.contains(&vec![4]), "{grouped:?}");
}

/// Roots with one base subject share a thread; an empty base subject
/// joins nothing (spec/02).
#[test]
fn roots_merge_by_base_subject() {
    let inputs = [
        input(1, Some("a@x"), &[], "Lunch"),
        input(2, Some("b@x"), &[], "Re: [team] lunch"),
        input(3, Some("c@x"), &[], "Fwd: Lunch (fwd)"),
        input(4, Some("d@x"), &[], ""),
        input(5, Some("e@x"), &[], "Re:"),
        input(6, Some("f@x"), &["f-parent@x"], "Dinner"),
        input(7, Some("g@x"), &[], "dinner"),
    ];
    assert_eq!(
        threads_in_any_order(&inputs),
        vec![vec![1, 2, 3], vec![4], vec![5], vec![6, 7]]
    );
}

/// RFC 5256 base subjects, lowercased.
#[test]
fn base_subjects_follow_rfc_5256() {
    for (subject, base) in [
        ("Lunch", "lunch"),
        ("  Re:   Lunch  ", "lunch"),
        ("RE: re: Lunch", "lunch"),
        ("Re[2]: Lunch", "lunch"),
        ("Fw: Lunch", "lunch"),
        ("FWD: Lunch", "lunch"),
        ("Lunch (fwd)", "lunch"),
        ("Lunch (FWD) (fwd)", "lunch"),
        ("[team] Lunch", "lunch"),
        ("[team] Re: [team] Lunch", "lunch"),
        ("[Fwd: Re: Lunch]", "lunch"),
        ("[team]", "[team]"),
        ("Regarding lunch", "regarding lunch"),
        ("Re lunch", "re lunch"),
        ("Grüße: Re: Café", "grüße: re: café"),
        ("Re: Ünïcödé", "ünïcödé"),
        ("", ""),
    ] {
        assert_eq!(base_subject(subject), base, "{subject:?}");
    }
}

/// A hostile chain of 20,000 references threads quickly: only the
/// last 128 count, and a reply naming them in reverse, which would
/// close cycles everywhere, still joins.
#[test]
fn a_long_reference_chain_is_bounded() {
    const LINKS: usize = 20_000;
    let chain: Vec<String> = (0..LINKS).map(|i| format!("{i}@chain")).collect();
    let refs: Vec<&str> = chain.iter().map(String::as_str).collect();
    let mut reversed = refs[LINKS - MAX_REFERENCES..].to_vec();
    reversed.reverse();
    let inputs = [
        input(1, Some("tip@chain"), &refs, "Chain"),
        input(2, Some("tail@chain"), &reversed, "Other"),
    ];
    let started = Instant::now();
    assert_eq!(threads_in_any_order(&inputs), vec![vec![1, 2]]);
    assert!(started.elapsed() < Duration::from_secs(10));
}

/// A reply chain deeper than the depth bound splits where the bound
/// drops links, and its shared subject joins the parts again.
#[test]
fn a_chain_past_the_depth_bound_rejoins_by_subject() {
    let deep = MAX_DEPTH as i64 + 200;
    let mut inputs = vec![input(1, Some("1@deep"), &[], "Deep")];
    for id in 2..=deep {
        inputs.push(reply_to(
            id,
            &format!("{id}@deep"),
            &format!("{}@deep", id - 1),
            "Re: Deep",
        ));
    }
    let started = Instant::now();
    assert_eq!(threads(&inputs), vec![(1..=deep).collect::<Vec<_>>()]);
    assert!(started.elapsed() < Duration::from_secs(10));

    // With distinct subjects nothing rejoins: the bound is visible.
    for (offset, message) in inputs.iter_mut().enumerate() {
        message.subject = Some(format!("Part {offset}"));
    }
    assert!(threads(&inputs).len() > 1);
}
