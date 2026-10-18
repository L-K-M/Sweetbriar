use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sb_resources::{ImportJobState, list_generation};
use sb_test_support::{ScratchDir, block_on};

use super::*;

const PATIENCE: Duration = Duration::from_secs(300);
const ACCOUNT: &str = "00000000-0000-4000-8000-0000000000aa";

fn source(message_id: i64, header: &str, references: &[&str], subject: &str) -> ThreadSource {
    ThreadSource {
        message_id,
        account_id: ACCOUNT.to_owned(),
        header_id: Some(header.to_owned()),
        references: references.iter().map(|r| (*r).to_owned()).collect(),
        in_reply_to: None,
        subject: Some(subject.to_owned()),
    }
}

fn kept(plan: &Plan) -> Vec<Option<i64>> {
    plan.threads.iter().map(|thread| thread.kept).collect()
}

/// A first rebuild makes a thread per group: describe, every member,
/// then the date; nothing is removed.
#[test]
fn a_first_plan_creates_every_thread() {
    let sources = [
        source(1, "a@x", &[], "Lunch"),
        source(2, "b@x", &["a@x"], "Re: Lunch"),
        source(3, "c@x", &[], "Budget"),
    ];
    let plan = plan(&sources, &HashMap::new()).unwrap();
    assert_eq!(kept(&plan), vec![None, None]);
    assert_eq!(
        plan.steps,
        vec![
            Step::Describe(0),
            Step::Assign {
                message: 1,
                thread: 0
            },
            Step::Assign {
                message: 2,
                thread: 0
            },
            Step::Date(0),
            Step::Describe(1),
            Step::Assign {
                message: 3,
                thread: 1
            },
            Step::Date(1),
        ]
    );
    assert_eq!(plan.threads[0].subject_norm.as_deref(), Some("lunch"));
    assert_eq!(plan.threads[0].account_scope.as_deref(), Some(ACCOUNT));
}

/// A rebuild over unchanged memberships writes nothing.
#[test]
fn an_unchanged_grouping_plans_no_writes() {
    let sources = [
        source(1, "a@x", &[], "Lunch"),
        source(2, "b@x", &["a@x"], "Re: Lunch"),
    ];
    let memberships = HashMap::from([(1, 7), (2, 7)]);
    let plan = plan(&sources, &memberships).unwrap();
    assert_eq!(kept(&plan), vec![Some(7)]);
    assert!(plan.steps.is_empty());
}

/// Merging threads keeps the smallest id and removes the other; a
/// split keeps the id for the earlier group on a tie, and the other
/// group gets a new thread (spec/02).
#[test]
fn merges_and_splits_keep_ids_by_spec_rule() {
    let merged = [
        source(1, "a@x", &[], "Plan"),
        source(2, "b@x", &["a@x"], "Re: Plan"),
        source(3, "c@x", &["b@x"], "Re: Plan"),
    ];
    let plan_merged = plan(&merged, &HashMap::from([(1, 9), (2, 9), (3, 5)])).unwrap();
    assert_eq!(kept(&plan_merged), vec![Some(5)]);
    assert_eq!(
        plan_merged.steps,
        vec![
            Step::Describe(0),
            Step::Assign {
                message: 1,
                thread: 0
            },
            Step::Assign {
                message: 2,
                thread: 0
            },
            Step::Date(0),
            Step::Remove(9),
        ]
    );

    let split = [
        source(1, "a@x", &[], "One"),
        source(2, "b@x", &["a@x"], "Re: One"),
        source(3, "c@x", &[], "Two"),
        source(4, "d@x", &["c@x"], "Re: Two"),
    ];
    let memberships = HashMap::from([(1, 4), (2, 4), (3, 4), (4, 4)]);
    let plan_split = plan(&split, &memberships).unwrap();
    assert_eq!(kept(&plan_split), vec![Some(4), None]);
    // The kept thread lost members, so it is described and dated again.
    assert_eq!(
        plan_split.steps,
        vec![
            Step::Describe(0),
            Step::Date(0),
            Step::Describe(1),
            Step::Assign {
                message: 3,
                thread: 1
            },
            Step::Assign {
                message: 4,
                thread: 1
            },
            Step::Date(1),
        ]
    );
}

/// Mixed accounts leave a thread unscoped.
#[test]
fn mixed_accounts_leave_the_scope_empty() {
    let mut other = source(2, "b@x", &["a@x"], "Re: Plan");
    other.account_id = "00000000-0000-4000-8000-0000000000bb".to_owned();
    let plan = plan(&[source(1, "a@x", &[], "Plan"), other], &HashMap::new()).unwrap();
    assert_eq!(plan.threads[0].account_scope, None);
}

/// Publication splits into writes of at most `MAX_CHANGES` steps.
#[test]
fn chunks_stay_within_the_batch_bound() {
    let sources: Vec<ThreadSource> = (1..=600)
        .map(|id| source(id, &format!("{id}@x"), &["root@x"], "Many"))
        .collect();
    let plan = plan(&sources, &HashMap::new()).unwrap();
    let chunks = plan.chunks();
    assert_eq!(
        chunks.iter().map(|c| c.len()).sum::<usize>(),
        plan.steps.len()
    );
    assert!(chunks.len() >= 3);
    assert!(chunks.iter().all(|chunk| chunk.len() <= MAX_CHANGES));
}

fn open(root: &Path) -> Engine {
    block_on(Engine::open(root)).expect("the engine opens")
}

/// A reply chain and an unrelated message, as one mbox.
fn conversation(dir: &Path) -> PathBuf {
    let path = dir.join("Inbox.mbox");
    let messages = [
        ("first@x", "", "Plans"),
        ("second@x", "References: <first@x>\n", "Re: Plans"),
        ("third@x", "", "Unrelated"),
    ];
    let text: String = messages
        .iter()
        .map(|(id, references, subject)| {
            format!(
                "From sender@example.test Sat Oct 17 12:00:00 2026\n\
                 Message-ID: <{id}>\n{references}Subject: {subject}\n\nbody\n"
            )
        })
        .collect();
    fs::write(&path, text).unwrap();
    path
}

fn import(engine: &Engine, path: PathBuf) {
    let key = engine.new_idempotency_key();
    let id = block_on(engine.start_import(&key, vec![path]))
        .unwrap()
        .job_id()
        .to_owned();
    wait_for(engine, &id, ImportJobState::AwaitingConfirmation);
    block_on(engine.confirm_import(&engine.new_idempotency_key(), &id, Vec::new())).unwrap();
    wait_for(engine, &id, ImportJobState::Completed);
}

fn wait_for(engine: &Engine, id: &str, state: ImportJobState) {
    let deadline = Instant::now() + PATIENCE;
    while block_on(engine.import_job(id)).unwrap().state() != state {
        assert!(
            Instant::now() < deadline,
            "job {id} never reached {state:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Wait until no rebuild is due or running.
fn threaded(engine: &Engine) {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let stale = engine.resources().read(thread_revisions).unwrap().stale();
        if !stale && !engine.imports().threads().lock().running {
            return;
        }
        assert!(Instant::now() < deadline, "threads were never rebuilt");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Every membership, as groups of message ids by thread.
fn groups(engine: &Engine) -> Vec<Vec<i64>> {
    let memberships = engine
        .resources()
        .read(|tx| thread_memberships(tx, 0, usize::MAX))
        .unwrap();
    let mut by_thread: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
    for (message, thread) in memberships {
        by_thread.entry(thread).or_default().push(message);
    }
    let mut groups: Vec<Vec<i64>> = by_thread.into_values().collect();
    groups.sort();
    groups
}

/// After an import's turn the engine threads the new messages, and
/// the membership change advances the list generation (spec/02).
#[test]
fn an_import_is_threaded_when_its_turn_ends() {
    let scratch = ScratchDir::new("threads-after-import");
    let engine = open(&scratch.path().join("store"));
    import(&engine, conversation(scratch.path()));
    threaded(&engine);
    let messages: Vec<i64> = engine
        .resources()
        .read(|tx| thread_sources(tx, 0, usize::MAX))
        .unwrap()
        .iter()
        .map(|source| source.message_id)
        .collect();
    assert_eq!(
        groups(&engine),
        vec![vec![messages[0], messages[1]], vec![messages[2]]]
    );
    block_on(engine.shutdown()).expect("the engine shuts down");
}

/// Open rebuilds threads a store owes: here, with every membership
/// gone and the watermark behind, as after migration 4. Ids come back
/// and the generation moves.
#[test]
fn open_threads_messages_imported_before() {
    let scratch = ScratchDir::new("threads-at-open");
    let root = scratch.path().join("store");
    let engine = open(&root);
    import(&engine, conversation(scratch.path()));
    threaded(&engine);
    let expected = groups(&engine);
    block_on(engine.resources().write(PRIORITY, |tx| {
        tx.execute_batch(
            "DELETE FROM d_thread_members; DELETE FROM d_threads;
             UPDATE store_meta SET value = '0' WHERE key = 'threads_built';",
        )
        .map_err(|e| StoreError::internal(e.to_string()))
    }))
    .unwrap();
    let before = engine.resources().read(list_generation).unwrap();
    block_on(engine.shutdown()).expect("the engine shuts down");

    let engine = open(&root);
    threaded(&engine);
    assert_eq!(groups(&engine), expected);
    assert!(engine.resources().read(list_generation).unwrap() > before);
    block_on(engine.shutdown()).expect("the engine shuts down");
}
