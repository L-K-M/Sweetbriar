//! The proptest state machine of the outbox (spec/11 "Model"; spec/05
//! "Outbox state machine", spec/02 "Sending"). Random commands run
//! against a real store and an in-memory model; after every command
//! the tables are read back, compared with the model and checked
//! against spec/05's invariants. Each command calls a production
//! store function in its own write transaction at the model's clock.

use proptest::prelude::*;

use super::{attempt, publish, scratch, seed};
use crate::drafts::{ComposeFormat, RevisionInput, save_draft};
use crate::send::{
    SendFailure, SendFailureRejection, UnknownResolution, cancel_send, claim_send_attempt,
    commit_send_requested, confirm_send_route, mark_outcome_unknown, mark_send_accepted,
    mark_send_failed, queue_due_held, record_proton_draft, recover_proton_submitting,
    requeue_before_request, resolve_unknown_send, send_draft,
};
use crate::testing::{FIXED_NOW_MS, reading};
use crate::{Store, StoreError, Timestamp, WritePriority};

fn cases(per_pr: u32) -> u32 {
    let scale = std::env::var("SB_PROPTEST_SCALE")
        .map_or(1, |scale| scale.parse().expect("a whole number"));
    per_pr * scale
}

const UNDO_SECONDS: i64 = 7;

fn revision_id(index: usize) -> String {
    format!("r{}", index + 1)
}

fn revision_input(revision_id: &str) -> RevisionInput {
    RevisionInput {
        revision_id: revision_id.to_owned(),
        identity_id: Some("id".to_owned()),
        format: ComposeFormat::Plain,
        plain_flowed: true,
        subject: "s".to_owned(),
        to: Vec::new(),
        cc: Vec::new(),
        bcc: Vec::new(),
        body_source: "b".to_owned(),
        attachments: Vec::new(),
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Held,
    Queued,
    Submitting,
    Accepted,
    Failed,
    OutcomeUnknown,
    Cancelled,
}

const STATE_WORDS: [&str; 7] = [
    "held",
    "queued",
    "submitting",
    "accepted",
    "failed",
    "outcome_unknown",
    "cancelled",
];

impl State {
    fn stored(self) -> &'static str {
        STATE_WORDS[self as usize]
    }
}

#[derive(Clone)]
struct Attempt {
    id: String,
    revision: usize,
    created_at: i64,
    state: State,
    config_generation: i64,
    requested: bool,
    resolution: Option<UnknownResolution>,
}

impl Attempt {
    fn live(&self) -> bool {
        matches!(self.state, State::Held | State::Queued | State::Submitting)
            || (self.state == State::OutcomeUnknown && self.resolution.is_none())
    }

    fn terminal_sent(&self) -> bool {
        self.state == State::Accepted
            || (self.state == State::OutcomeUnknown
                && self.resolution == Some(UnknownResolution::MarkedSent))
    }
}

#[derive(Clone)]
struct Model {
    now_ms: i64,
    account_generation: i64,
    draft_state: &'static str,
    revision_count: usize,
    attempts: Vec<Attempt>,
}

impl Model {
    fn new() -> Self {
        Self {
            now_ms: FIXED_NOW_MS,
            account_generation: 1,
            draft_state: "editing",
            revision_count: 1,
            attempts: Vec::new(),
        }
    }

    fn stamp(&self) -> Timestamp {
        Timestamp::from_millis(self.now_ms)
    }

    fn current(&self) -> usize {
        self.revision_count - 1
    }

    fn fresh_attempt_id(&self) -> String {
        format!("a{}", self.attempts.len() + 1)
    }

    fn indices_where(&self, want: impl Fn(&Attempt) -> bool) -> Vec<usize> {
        self.attempts
            .iter()
            .enumerate()
            .filter(|(_, attempt)| want(attempt))
            .map(|(index, _)| index)
            .collect()
    }

    fn held(&self, id: &str, revision: usize) -> Attempt {
        Attempt {
            id: id.to_owned(),
            revision,
            created_at: self.now_ms,
            state: State::Held,
            config_generation: self.account_generation,
            requested: false,
            resolution: None,
        }
    }

    fn draft_after(&mut self, revision: usize, state: &'static str) {
        if revision == self.current() {
            self.draft_state = state;
        }
    }
}

/// Which eligible attempt or revision a command targets: the candidate
/// at this position, wrapping. A pinned sequence names one directly.
#[derive(Clone, Copy, Debug)]
struct Pick(usize);

impl Pick {
    fn of(self, len: usize) -> usize {
        self.0 % len
    }
}

fn target() -> impl Strategy<Value = Pick> {
    any::<usize>().prop_map(Pick)
}

fn pick(candidates: &[usize], index: Pick) -> Option<usize> {
    (!candidates.is_empty()).then(|| candidates[index.of(candidates.len())])
}

fn pick_any(len: usize, index: Pick) -> Option<usize> {
    (len > 0).then(|| index.of(len))
}

macro_rules! refused {
    ($result:expr, $what:literal) => {{
        let result = $result;
        assert!(
            matches!(result, Err(StoreError::Conflict { .. })),
            "{}: {result:?}",
            $what
        );
    }};
}

#[derive(Clone, Debug)]
enum Submit {
    RequestSent,
    Requeue,
    Accept,
    Fail(bool),
    Unknown,
}

#[derive(Clone, Debug)]
enum Op {
    SaveRevision,
    Send,
    SendStale(Pick),
    Cancel(Pick),
    Tick(u32),
    Claim,
    Submit(Submit, Pick),
    Restart,
    Resolve(Pick, UnknownResolution),
    Reconfigure,
    ConfirmRoute(Pick),
}

/// The weights favour the long road (send, tick, claim, request,
/// requeue or a reply) over cancels and saves that end an attempt
/// early; the pinned sequences below walk each road for certain.
fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        2 => Just(Op::SaveRevision),
        4 => Just(Op::Send),
        1 => target().prop_map(Op::SendStale),
        2 => target().prop_map(Op::Cancel),
        4 => (0..120u32).prop_map(Op::Tick),
        5 => Just(Op::Claim),
        4 => target().prop_map(|index| Op::Submit(Submit::RequestSent, index)),
        3 => target().prop_map(|index| Op::Submit(Submit::Requeue, index)),
        2 => target().prop_map(|index| Op::Submit(Submit::Accept, index)),
        2 => (target(), any::<bool>())
            .prop_map(|(index, reject)| Op::Submit(Submit::Fail(reject), index)),
        2 => target().prop_map(|index| Op::Submit(Submit::Unknown, index)),
        2 => Just(Op::Restart),
        2 => (
            target(),
            prop_oneof![
                Just(UnknownResolution::MarkedSent),
                Just(UnknownResolution::SendAgain),
                Just(UnknownResolution::KeptAsDraft),
            ],
        )
            .prop_map(|(index, kind)| Op::Resolve(index, kind)),
        1 => Just(Op::Reconfigure),
        1 => target().prop_map(Op::ConfirmRoute),
    ]
}

fn apply(store: &Store, model: &mut Model, op: Op) {
    match op {
        Op::SaveRevision => {
            let allowed = matches!(model.draft_state, "editing" | "queued");
            let at = model.stamp();
            let base = revision_id(model.current());
            let new_id = revision_id(model.revision_count);
            let input = revision_input(&new_id);
            let result = write(store, move |tx| {
                save_draft(tx, "d1", Some(&base), input, at)
            });
            if allowed {
                result.unwrap();
                model.revision_count += 1;
                model.draft_state = "editing";
            } else {
                refused!(result, "an unwritable draft was not refused");
            }
        }
        Op::Send => {
            let revision = model.current();
            let at = model.stamp();
            let allowed = model.draft_state == "editing"
                && !model
                    .attempts
                    .iter()
                    .any(|a| a.revision == revision && (a.live() || a.terminal_sent()));
            let object = publish(store);
            let id = model.fresh_attempt_id();
            let mut queued = attempt(&object);
            queued.attempt_id = id.clone();
            queued.revision_id = revision_id(revision);
            let result = write(store, move |tx| {
                send_draft(tx, &queued, &object, UNDO_SECONDS, at)
            });
            if allowed {
                result.unwrap();
                model.attempts.push(model.held(&id, revision));
                model.draft_state = "queued";
            } else {
                refused!(result, "a refused send was not Conflict");
            }
        }
        Op::SendStale(index) => {
            let stale = model.revision_count - 1;
            if stale == 0 {
                return;
            }
            let at = model.stamp();
            let object = publish(store);
            let mut queued = attempt(&object);
            queued.attempt_id = model.fresh_attempt_id();
            queued.revision_id = revision_id(index.of(stale));
            let result = write(store, move |tx| {
                send_draft(tx, &queued, &object, UNDO_SECONDS, at)
            });
            refused!(result, "a non-current revision was not refused");
        }
        Op::Cancel(index) => {
            let Some(target) = pick_any(model.attempts.len(), index) else {
                return;
            };
            let at = model.stamp();
            let id = model.attempts[target].id.clone();
            let revision = model.attempts[target].revision;
            let allowed = matches!(model.attempts[target].state, State::Held | State::Queued);
            let result = write(store, move |tx| cancel_send(tx, &id, at));
            if allowed {
                result.unwrap();
                model.attempts[target].state = State::Cancelled;
                model.draft_after(revision, "editing");
            } else {
                refused!(result, "uncancellable attempt");
            }
        }
        Op::Tick(seconds) => {
            model.now_ms = model.now_ms.saturating_add(i64::from(seconds) * 1000);
            let at = model.stamp();
            let due: Vec<usize> = model
                .attempts
                .iter()
                .enumerate()
                .filter(|(_, a)| {
                    a.state == State::Held && a.created_at + UNDO_SECONDS * 1000 <= model.now_ms
                })
                .map(|(index, _)| index)
                .collect();
            let moved = write(store, move |tx| queue_due_held(tx, at)).unwrap();
            assert_eq!(moved, due.len());
            for index in due {
                model.attempts[index].state = State::Queued;
            }
        }
        Op::Claim => {
            let at = model.stamp();
            let expected = model
                .attempts
                .iter()
                .filter(|a| {
                    a.state == State::Queued && a.config_generation == model.account_generation
                })
                .min_by_key(|a| (a.created_at, a.id.clone()))
                .map(|a| a.id.clone());
            let claimed = write(store, move |tx| claim_send_attempt(tx, "acct", at)).unwrap();
            match (expected, claimed) {
                (Some(id), Some(claimed)) => {
                    assert_eq!(claimed.attempt_id, id);
                    let index = model.attempts.iter().position(|a| a.id == id).unwrap();
                    model.attempts[index].state = State::Submitting;
                }
                (None, None) => {}
                (expected, claimed) => panic!("claim mismatch: {expected:?} vs {claimed:?}"),
            }
        }
        Op::Submit(cmd, index) => {
            let candidates = model.indices_where(|a| a.state == State::Submitting);
            let Some(target) = pick(&candidates, index) else {
                return;
            };
            let at = model.stamp();
            let id = model.attempts[target].id.clone();
            let revision = model.attempts[target].revision;
            match cmd {
                Submit::RequestSent => {
                    let draft_id = format!("pd-{id}");
                    write(store, move |tx| {
                        record_proton_draft(tx, &id, &draft_id)?;
                        commit_send_requested(tx, &id, at)
                    })
                    .unwrap();
                    model.attempts[target].requested = true;
                }
                // The engine's word that no connection was established
                // (spec/05 step 5): even a recorded send request is
                // cleared, since nothing reached Proton.
                Submit::Requeue => {
                    write(store, move |tx| requeue_before_request(tx, &id, at)).unwrap();
                    let attempt = &mut model.attempts[target];
                    attempt.state = State::Queued;
                    attempt.requested = false;
                }
                Submit::Accept | Submit::Fail(_) | Submit::Unknown => {
                    match cmd {
                        Submit::Accept => {
                            write(store, move |tx| mark_send_accepted(tx, &id, at)).unwrap()
                        }
                        Submit::Fail(reject) => {
                            let failure = SendFailure {
                                code: None,
                                message: "qualified pre-submission failure".to_owned(),
                                rejected: reject.then(|| SendFailureRejection {
                                    canonical: "a@x.net".to_owned(),
                                    reason: "keys known; clear text refused".to_owned(),
                                }),
                            };
                            write(store, move |tx| mark_send_failed(tx, &id, &failure, at)).unwrap()
                        }
                        _ => write(store, move |tx| mark_outcome_unknown(tx, &id, at)).unwrap(),
                    };
                    let (state, draft) = match cmd {
                        Submit::Accept => (State::Accepted, "sent"),
                        Submit::Fail(_) => (State::Failed, "editing"),
                        _ => (State::OutcomeUnknown, "queued"),
                    };
                    model.attempts[target].state = state;
                    model.draft_after(revision, draft);
                }
            }
        }
        Op::Restart => {
            let at = model.stamp();
            let expected = model
                .attempts
                .iter()
                .filter(|a| a.state == State::Submitting && a.requested)
                .count();
            let recovered = write(store, move |tx| recover_proton_submitting(tx, at)).unwrap();
            assert_eq!(recovered, expected);
            for index in 0..model.attempts.len() {
                if model.attempts[index].state != State::Submitting {
                    continue;
                }
                if model.attempts[index].requested {
                    model.attempts[index].state = State::OutcomeUnknown;
                    let revision = model.attempts[index].revision;
                    model.draft_after(revision, "queued");
                } else {
                    model.attempts[index].state = State::Queued;
                }
            }
        }
        Op::Resolve(index, kind) => {
            let candidates =
                model.indices_where(|a| a.state == State::OutcomeUnknown && a.resolution.is_none());
            let Some(target) = pick(&candidates, index) else {
                return;
            };
            let at = model.stamp();
            let id = model.attempts[target].id.clone();
            let revision = model.attempts[target].revision;
            let new_id = (kind == UnknownResolution::SendAgain).then(|| model.fresh_attempt_id());
            let expected = new_id.clone();
            let created = write(store, move |tx| {
                resolve_unknown_send(tx, &id, kind, new_id.as_deref(), UNDO_SECONDS, at)
            })
            .unwrap();
            assert_eq!(created, expected);
            model.attempts[target].resolution = Some(kind);
            match kind {
                UnknownResolution::MarkedSent => model.draft_after(revision, "sent"),
                UnknownResolution::KeptAsDraft => model.draft_after(revision, "editing"),
                UnknownResolution::SendAgain => {
                    model.draft_after(revision, "queued");
                    model
                        .attempts
                        .push(model.held(&expected.unwrap(), revision));
                }
            }
        }
        Op::Reconfigure => {
            write(store, |tx| {
                tx.execute(
                    "UPDATE accounts SET config_generation = config_generation + 1
                     WHERE account_id = 'acct'",
                    [],
                )
                .map_err(|e| StoreError::internal(e.to_string()))?;
                Ok(())
            })
            .unwrap();
            model.account_generation += 1;
        }
        Op::ConfirmRoute(index) => {
            let candidates = model.indices_where(|a| a.state == State::Queued);
            let Some(target) = pick(&candidates, index) else {
                return;
            };
            let at = model.stamp();
            let id = model.attempts[target].id.clone();
            write(store, move |tx| confirm_send_route(tx, &id, at)).unwrap();
            model.attempts[target].config_generation = model.account_generation;
        }
    }
}

fn write<T: Send + 'static>(
    store: &Store,
    closure: impl FnOnce(&rusqlite::Transaction) -> Result<T, StoreError> + Send + 'static,
) -> Result<T, StoreError> {
    sb_test_support::block_on(store.write(WritePriority::User, closure))
}

fn stored(store: &Store) -> (String, String, String) {
    reading(store, |tx| {
        let read = |e: rusqlite::Error| StoreError::internal(e.to_string());
        let count = |sql: &str| -> Result<i64, StoreError> {
            tx.query_row(sql, [], |row| row.get(0)).map_err(read)
        };
        let attempts: String = tx
            .query_row(
                "SELECT coalesce(group_concat(
                     attempt_id || '|' || revision_id || '|' || state || '|'
                     || config_generation || '|' || (send_requested_at IS NOT NULL)
                     || '|' || coalesce(resolution, '-') || '|' || created_at, ';'), '')
                 FROM (SELECT * FROM send_attempts ORDER BY created_at, attempt_id)",
                [],
                |row| row.get(0),
            )
            .map_err(read)?;
        let (draft_state, current) = tx
            .query_row(
                "SELECT state, current_revision_id FROM drafts WHERE draft_id = 'd1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(read)?;
        // Invariant 2: at most one live attempt per revision.
        let duplicates = count(
            "SELECT count(*) FROM (
                 SELECT revision_id FROM send_attempts
                 WHERE state IN ('held', 'queued', 'submitting')
                    OR (state = 'outcome_unknown' AND resolution IS NULL)
                 GROUP BY revision_id HAVING count(*) > 1)",
        )?;
        assert_eq!(duplicates, 0, "two live attempts share a revision");
        // Invariant 3: a revision with an accepted or marked-sent
        // attempt never gains another one.
        let after_terminal = count(
            "SELECT count(*) FROM send_attempts t
             JOIN send_attempts a ON a.revision_id = t.revision_id
              AND (a.created_at, a.attempt_id) > (t.created_at, t.attempt_id)
             WHERE t.state = 'accepted'
                OR (t.state = 'outcome_unknown' AND t.resolution = 'marked_sent')",
        )?;
        assert_eq!(after_terminal, 0, "a sent revision gained another attempt");
        // Invariant 4: a queued attempt records no send request.
        let queued_requested = count(
            "SELECT count(*) FROM send_attempts
             WHERE state = 'queued' AND send_requested_at IS NOT NULL",
        )?;
        assert_eq!(
            queued_requested, 0,
            "a queued attempt records a send request"
        );
        // Invariant 5: the draft follows only its current revision's
        // attempts.
        let expected_draft: String = tx
            .query_row(
                "SELECT CASE
                     WHEN EXISTS (
                         SELECT 1 FROM send_attempts s JOIN drafts d ON d.draft_id = s.draft_id
                         WHERE s.revision_id = d.current_revision_id
                           AND (s.state IN ('held', 'queued', 'submitting')
                                OR (s.state = 'outcome_unknown' AND s.resolution IS NULL)))
                     THEN 'queued'
                     WHEN EXISTS (
                         SELECT 1 FROM send_attempts s JOIN drafts d ON d.draft_id = s.draft_id
                         WHERE s.revision_id = d.current_revision_id
                           AND (s.state = 'accepted'
                                OR (s.state = 'outcome_unknown'
                                    AND s.resolution = 'marked_sent')))
                     THEN 'sent'
                     ELSE 'editing'
                 END",
                [],
                |row| row.get(0),
            )
            .map_err(read)?;
        assert_eq!(
            draft_state, expected_draft,
            "the draft does not follow its current revision"
        );
        Ok((attempts, draft_state, current))
    })
}

fn expected(model: &Model) -> String {
    let mut rows: Vec<(i64, String)> = model
        .attempts
        .iter()
        .map(|attempt| {
            (
                attempt.created_at,
                format!(
                    "{}|{}|{}|{}|{}|{}|{}",
                    attempt.id,
                    revision_id(attempt.revision),
                    attempt.state.stored(),
                    attempt.config_generation,
                    u8::from(attempt.requested),
                    attempt
                        .resolution
                        .map_or("-".to_owned(), |r| r.as_str().to_owned()),
                    attempt.created_at
                ),
            )
        })
        .collect();
    rows.sort_by(|a, b| (a.0, a.1.as_str()).cmp(&(b.0, b.1.as_str())));
    rows.into_iter()
        .map(|(_, row)| row)
        .collect::<Vec<_>>()
        .join(";")
}

/// spec/05's diagram. A submitting attempt returns to `queued` only by
/// the engine's requeue (no connection was established) or, at a
/// restart, when it never recorded its send request: a restart never
/// requeues a requested one (no automatic resend).
fn transition_allowed(from: State, to: State, requested: bool, requeue: bool) -> bool {
    match (from, to) {
        (State::Held, State::Queued | State::Cancelled) => true,
        (State::Queued, State::Submitting | State::Cancelled) => true,
        (State::Submitting, State::Accepted | State::Failed | State::OutcomeUnknown) => true,
        (State::Submitting, State::Queued) => requeue || !requested,
        _ => false,
    }
}

/// Applies each command to the store and the model, then checks that
/// they agree and that spec/05's invariants hold.
fn run(ops: Vec<Op>) {
    let (_scratch, _paths, store) = scratch();
    seed(&store);
    let mut model = Model::new();
    for op in ops {
        let before: Vec<(String, State, bool)> = model
            .attempts
            .iter()
            .map(|a| (a.id.clone(), a.state, a.requested))
            .collect();
        let requeue = matches!(op, Op::Submit(Submit::Requeue, _));
        apply(&store, &mut model, op);
        let (stored, draft_state, current) = stored(&store);
        assert_eq!(stored, expected(&model), "the store and the model disagree");
        assert_eq!(draft_state, model.draft_state);
        assert_eq!(current, revision_id(model.current()));
        for ((id, from, requested), attempt) in before.iter().zip(model.attempts.iter()) {
            assert_eq!(&attempt.id, id);
            if attempt.state != *from {
                assert!(
                    transition_allowed(*from, attempt.state, *requested, requeue),
                    "attempt {id} moved {from:?} -> {:?}",
                    attempt.state
                );
            }
        }
    }
    store.close().unwrap();
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases(32)))]

    #[test]
    fn outbox_transitions_match_the_model(
        ops in proptest::collection::vec(op(), 1..40)
    ) {
        run(ops);
    }
}

/// The long roads random sequences reach only now and then, each
/// walked once through the same model and checks.
#[test]
fn the_long_roads_match_the_model() {
    use Op::{Claim, Reconfigure, Restart, SaveRevision, Send, SendStale, Tick};
    use Submit::{Accept, Fail, RequestSent, Requeue, Unknown};
    let first = Pick(0);
    let submit = |command| Op::Submit(command, first);
    let roads = [
        // A requeue clears a recorded request (no connection was
        // established); a restart after one makes the outcome unknown;
        // sending again ends accepted, and the sent draft takes no more.
        vec![
            Send,
            Tick(10),
            Claim,
            submit(RequestSent),
            submit(Requeue),
            Claim,
            submit(RequestSent),
            Restart,
            Op::Resolve(first, UnknownResolution::SendAgain),
            Tick(10),
            Claim,
            submit(Accept),
            Send,
            SaveRevision,
        ],
        // A restart requeues a claim that recorded no request; a
        // qualified failure frees the draft; held and queued attempts
        // cancel.
        vec![
            Send,
            Tick(10),
            Claim,
            Restart,
            Claim,
            submit(Fail(true)),
            Send,
            Op::Cancel(Pick(1)),
            Send,
            Tick(3),
            Tick(5),
            Op::Cancel(Pick(2)),
        ],
        // A newer revision keeps its state while the older attempt is
        // accepted; the stale revision can't be sent; kept as a draft,
        // the newer one sends again.
        vec![
            Send,
            SaveRevision,
            Send,
            Tick(10),
            Claim,
            submit(Accept),
            SendStale(first),
            Claim,
            submit(Unknown),
            Op::Resolve(first, UnknownResolution::KeptAsDraft),
            Send,
        ],
        // A reconfigured account's attempt waits for the confirmation;
        // marked sent, the draft takes no other send.
        vec![
            Send,
            Tick(10),
            Reconfigure,
            Claim,
            Op::ConfirmRoute(first),
            Claim,
            submit(RequestSent),
            submit(Unknown),
            Op::Resolve(first, UnknownResolution::MarkedSent),
            Send,
        ],
    ];
    for road in roads {
        run(road);
    }
}
