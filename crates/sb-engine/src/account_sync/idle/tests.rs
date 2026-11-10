//! The pass schedule (spec/04 "Pace" and step 8): a full pass first
//! and every 15 minutes or when the mailbox list changed, INBOX on
//! each hint, and an INBOX poll every 60 s only while IDLE is down.

use super::*;

fn idle(live: bool, inbox_hints: u64, list_hints: u64) -> IdleState {
    IdleState {
        live,
        inbox_hints,
        list_hints,
    }
}

#[test]
fn the_first_pass_is_full_then_inbox_until_the_full_interval() {
    let start = Instant::now();
    let mut schedule = Schedule::new(start);

    assert_eq!(schedule.scope(start, idle(false, 0, 0)), Scope::All);
    assert_eq!(
        schedule.scope(start + POLL_INTERVAL, idle(true, 1, 0)),
        Scope::Inbox
    );
    assert_eq!(
        schedule.scope(start + FULL_INTERVAL, idle(true, 2, 0)),
        Scope::All
    );
}

#[test]
fn a_changed_mailbox_list_makes_the_next_pass_full() {
    let start = Instant::now();
    let mut schedule = Schedule::new(start);
    schedule.scope(start, idle(true, 0, 0));

    assert!(schedule.hinted(idle(true, 0, 1)));
    assert_eq!(schedule.scope(start, idle(true, 0, 1)), Scope::All);
    assert!(!schedule.hinted(idle(true, 0, 1)));
}

#[test]
fn only_hints_wake_a_pass_and_idle_s_state_sets_the_rest() {
    let start = Instant::now();
    let mut schedule = Schedule::new(start);
    schedule.scope(start, idle(false, 3, 0));

    // IDLE starting or stopping is no hint.
    assert!(!schedule.hinted(idle(true, 3, 0)));
    assert!(schedule.hinted(idle(true, 4, 0)));
    // Live IDLE rests until the full pass; without it, INBOX polls.
    assert_eq!(schedule.rest(start, true), FULL_INTERVAL);
    assert_eq!(schedule.rest(start, false), POLL_INTERVAL);
    assert_eq!(
        schedule.rest(start + FULL_INTERVAL - Duration::from_secs(5), false),
        Duration::from_secs(5)
    );
    // An overdue full pass rests not at all.
    assert_eq!(
        schedule.rest(start + 2 * FULL_INTERVAL, true),
        Duration::ZERO
    );
}

#[test]
fn inbox_is_matched_case_insensitively() {
    for name in ["INBOX", "Inbox", "inbox"] {
        assert!(is_inbox(name), "{name}");
    }
    for name in ["INBOX/Sub", "INBOX.Sub", "Archive", "INBOXES", ""] {
        assert!(!is_inbox(name), "{name}");
    }
}
