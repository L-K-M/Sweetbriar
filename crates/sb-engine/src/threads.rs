//! Thread rebuilds (spec/02 "Threads"): after an import's turn and at
//! open, while imports added messages since the last complete rebuild,
//! one background worker reads every message's threading fields,
//! groups them (`threading`) and publishes the changed memberships in
//! bounded writes.
//!
//! ```text
//!   schedule ─► claim? ─yes─► worker: read sources ─► group ─► plan ─► publish
//!                 │ no (running)                                    chunk by chunk
//!                 └─► "again": the worker runs once more when done
//! ```

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, MutexGuard};

use sb_resources::{
    MAX_IMPORT_CHANGES_BYTES, Resources, StoreError, StoredThread, ThreadDescription, ThreadSource,
    WritePriority, assign_thread, create_thread, describe_thread, mark_threads_built,
    note_membership_change, remove_empty_threads, remove_thread, stored_threads,
    thread_memberships, thread_revisions, thread_sources,
};

use crate::Engine;
use crate::engine::{Spawn, Worker};
use crate::threading::{ThreadInput, base_subject, threads};

/// Rebuilds are background work behind user and sync writes (spec/03).
const PRIORITY: WritePriority = WritePriority::Index;

/// Rows per read while gathering sources and memberships: each read
/// stays short (spec/03).
const PAGE: usize = 5_000;

/// Most changes one publishing write makes (spec/03 batch bound).
const MAX_CHANGES: usize = 250;

/// What a change costs the write budget beyond any subject it stores.
const CHANGE_BYTES: usize = 128;

/// The one rebuild worker: whether it runs, and whether it was asked
/// to run again meanwhile.
#[derive(Default)]
pub(crate) struct ThreadRebuilds {
    state: Mutex<RebuildState>,
}

#[derive(Default)]
struct RebuildState {
    running: bool,
    again: bool,
}

impl ThreadRebuilds {
    fn lock(&self) -> MutexGuard<'_, RebuildState> {
        self.state
            .lock()
            .expect("a panicking thread held the thread rebuild state")
    }

    /// Take the worker's slot, or ask the running worker to go again.
    fn claim(&self) -> bool {
        let mut state = self.lock();
        if state.running {
            state.again = true;
            return false;
        }
        state.running = true;
        true
    }

    /// After a pass: go again if asked, else give the slot back.
    fn next(&self) -> bool {
        let mut state = self.lock();
        if state.again {
            state.again = false;
            return true;
        }
        state.running = false;
        false
    }

    fn release(&self) {
        self.lock().running = false;
    }
}

impl Engine {
    /// Start a rebuild if none runs (spec/02); one finding nothing to
    /// do ends at once.
    pub(crate) fn schedule_threads(&self) {
        schedule_threads(self, self.imports().threads());
    }
}

/// [`Engine::schedule_threads`] from anything that spawns workers. A
/// refusal means shutdown began: the next open rebuilds instead.
pub(crate) fn schedule_threads(spawner: &impl Spawn, rebuilds: &Arc<ThreadRebuilds>) {
    if !rebuilds.claim() {
        return;
    }
    let owner = Arc::clone(rebuilds);
    if spawner
        .spawn_worker(move |worker| run(worker, owner))
        .is_err()
    {
        rebuilds.release();
    }
}

async fn run(worker: Worker, rebuilds: Arc<ThreadRebuilds>) {
    loop {
        // A failed pass leaves the watermark behind; the next import's
        // turn or the next open retries it.
        let _ = rebuild(&worker).await;
        if !rebuilds.next() {
            return;
        }
    }
}

/// One pass: nothing when threads are current; otherwise read, group,
/// plan and publish, marking the inputs read as built only when every
/// change landed.
async fn rebuild(worker: &Worker) -> Result<(), StoreError> {
    let stop = worker.shutdown_check();
    let revisions = read(worker, |resources| resources.read(thread_revisions)).await?;
    if !revisions.stale() {
        return Ok(());
    }
    let sources = gather(
        worker,
        |tx, after| thread_sources(tx, after, PAGE),
        |s| s.message_id,
    )
    .await?;
    let memberships: HashMap<i64, i64> = gather(
        worker,
        |tx, after| thread_memberships(tx, after, PAGE),
        |m| m.0,
    )
    .await?
    .into_iter()
    .collect();
    let stored: HashMap<i64, StoredThread> = gather(
        worker,
        |tx, after| stored_threads(tx, after, PAGE),
        |t| t.thread_id,
    )
    .await?
    .into_iter()
    .map(|thread| (thread.thread_id, thread))
    .collect();
    let plan = read(worker, move |_| plan(&sources, &memberships, &stored)).await?;

    let mut created: HashMap<usize, i64> = HashMap::new();
    for chunk in plan.chunks() {
        if stop() {
            return Ok(());
        }
        let known = created.clone();
        let chunk = chunk.to_vec();
        let threads = Arc::clone(&plan.threads);
        let made = worker
            .resources()
            .write(PRIORITY, move |tx| publish(tx, &threads, &chunk, known))
            .await?;
        created.extend(made);
    }
    // Threads a crash left before their members arrived go too.
    worker
        .resources()
        .write(PRIORITY, move |tx| {
            remove_empty_threads(tx)?;
            mark_threads_built(tx, revisions.inputs)
        })
        .await
}

/// Run `f` on the worker's blocking pool, flattening the join error.
async fn read<T, F>(worker: &Worker, f: F) -> Result<T, StoreError>
where
    T: Send + 'static,
    F: FnOnce(&Resources) -> Result<T, StoreError> + Send + 'static,
{
    worker
        .blocking(f)
        .await
        .map_err(|e| StoreError::internal(e.to_string()))?
}

/// Every row `page` yields, read `PAGE` at a time after the last row's
/// message id, one short read each.
async fn gather<T, P, K>(worker: &Worker, page: P, key: K) -> Result<Vec<T>, StoreError>
where
    T: Send + 'static,
    P: Fn(&sb_resources::Transaction, i64) -> Result<Vec<T>, StoreError>
        + Send
        + Sync
        + Copy
        + 'static,
    K: Fn(&T) -> i64,
{
    let mut all = Vec::new();
    let mut after = 0;
    loop {
        let rows = read(worker, move |resources| {
            resources.read(|tx| page(tx, after))
        })
        .await?;
        let Some(last) = rows.last() else {
            return Ok(all);
        };
        after = key(last);
        all.extend(rows);
    }
}

/// One thread the rebuild publishes: the id it keeps, if any, and what
/// it stores.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PlannedThread {
    kept: Option<i64>,
    members: Vec<i64>,
    account_scope: Option<String>,
    subject_norm: Option<String>,
    last_date: Option<i64>,
}

impl PlannedThread {
    fn description(&self) -> ThreadDescription<'_> {
        ThreadDescription {
            account_scope: self.account_scope.as_deref(),
            subject_norm: self.subject_norm.as_deref(),
            last_date: self.last_date,
        }
    }
}

/// One step of publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// Create the planned thread, or rewrite the one it keeps.
    Describe(usize),
    /// Move a message into the planned thread.
    Assign { message: i64, thread: usize },
    /// Remove an id no group was given.
    Remove(i64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Plan {
    threads: Arc<Vec<PlannedThread>>,
    steps: Vec<Step>,
}

impl Plan {
    /// The steps in writes within the batch bounds (spec/03): at most
    /// [`MAX_CHANGES`] steps and the write budget each.
    fn chunks(&self) -> Vec<&[Step]> {
        let mut chunks = Vec::new();
        let (mut start, mut bytes) = (0, 0);
        for (at, step) in self.steps.iter().enumerate() {
            let cost = match step {
                Step::Describe(thread) => CHANGE_BYTES.saturating_add(
                    self.threads[*thread]
                        .subject_norm
                        .as_ref()
                        .map_or(0, String::len),
                ),
                _ => CHANGE_BYTES,
            };
            if at > start && (at - start == MAX_CHANGES || bytes + cost > MAX_IMPORT_CHANGES_BYTES)
            {
                chunks.push(&self.steps[start..at]);
                (start, bytes) = (at, 0);
            }
            bytes += cost;
        }
        if start < self.steps.len() {
            chunks.push(&self.steps[start..]);
        }
        chunks
    }
}

/// Group `sources` and plan the writes that move `memberships`
/// (message → thread) there (spec/02): each existing id goes to the
/// group holding most of its members, ties to the group with the
/// earliest message; a group given several keeps the smallest; ids
/// given to no group are removed. A kept thread whose `stored`
/// description differs from the computed one is rewritten, so a
/// publication cut short converges at the next rebuild. Pure.
fn plan(
    sources: &[ThreadSource],
    memberships: &HashMap<i64, i64>,
    stored: &HashMap<i64, StoredThread>,
) -> Result<Plan, StoreError> {
    let inputs: Vec<ThreadInput> = sources
        .iter()
        .map(|source| ThreadInput {
            message_id: source.message_id,
            header_id: source.header_id.clone(),
            references: source.references.clone(),
            in_reply_to: source.in_reply_to.clone(),
            subject: source.subject.clone(),
        })
        .collect();
    let groups = threads(&inputs);
    let by_id: HashMap<i64, &ThreadSource> = sources.iter().map(|s| (s.message_id, s)).collect();

    // For each existing id, its members per new group; groups are in
    // order of their earliest message, so the first maximum wins ties.
    let mut shares: BTreeMap<i64, BTreeMap<usize, usize>> = BTreeMap::new();
    for (group, members) in groups.iter().enumerate() {
        for message in members {
            if let Some(&thread) = memberships.get(message) {
                *shares.entry(thread).or_default().entry(group).or_default() += 1;
            }
        }
    }
    let mut given: Vec<Option<i64>> = vec![None; groups.len()];
    let mut removed = Vec::new();
    for (thread, counts) in shares {
        let best = counts
            .iter()
            .max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0)))
            .map(|(&group, _)| group)
            .expect("a share exists for every counted thread");
        match given[best] {
            Some(kept) if kept < thread => removed.push(thread),
            Some(kept) => {
                removed.push(kept);
                given[best] = Some(thread);
            }
            None => given[best] = Some(thread),
        }
    }
    removed.sort_unstable();

    let mut threads = Vec::with_capacity(groups.len());
    let mut steps = Vec::new();
    for (group, members) in groups.into_iter().enumerate() {
        let kept = given[group];
        let accounts: BTreeSet<&str> = members
            .iter()
            .filter_map(|m| by_id.get(m).map(|s| s.account_id.as_str()))
            .collect();
        let account_scope = match accounts.len() {
            1 => accounts.first().map(|account| (*account).to_owned()),
            _ => None,
        };
        let subject_norm = by_id
            .get(&members[0])
            .and_then(|s| s.subject.as_deref())
            .map(base_subject);
        let last_date = members
            .iter()
            .filter_map(|m| by_id.get(m).and_then(|s| s.representative_date))
            .max();
        let planned = PlannedThread {
            kept,
            members,
            account_scope,
            subject_norm,
            last_date,
        };
        let moving: Vec<i64> = planned
            .members
            .iter()
            .copied()
            .filter(|m| kept.is_none() || memberships.get(m).copied() != kept)
            .collect();
        // Describing first means a thread exists before its members
        // move in; a kept one is rewritten only when it differs.
        let described = kept.and_then(|id| stored.get(&id)).is_some_and(|stored| {
            let wanted = planned.description();
            stored.account_scope.as_deref() == wanted.account_scope
                && stored.subject_norm.as_deref() == wanted.subject_norm
                && stored.last_date == wanted.last_date
        });
        if !described {
            steps.push(Step::Describe(group));
        }
        steps.extend(moving.iter().map(|&message| Step::Assign {
            message,
            thread: group,
        }));
        threads.push(planned);
    }
    steps.extend(removed.into_iter().map(Step::Remove));
    Ok(Plan {
        threads: Arc::new(threads),
        steps,
    })
}

/// Apply one chunk in the caller's write: `known` maps planned threads
/// created by earlier chunks to their ids. Returns the ids this chunk
/// created, and advances the list generation once if a membership
/// changed (spec/02).
fn publish(
    tx: &sb_resources::Transaction,
    threads: &[PlannedThread],
    steps: &[Step],
    mut known: HashMap<usize, i64>,
) -> Result<HashMap<usize, i64>, StoreError> {
    let mut made = HashMap::new();
    let mut changed = false;
    for step in steps {
        match *step {
            Step::Describe(index) => {
                let planned = &threads[index];
                match planned.kept {
                    Some(id) => {
                        describe_thread(tx, id, planned.description())?;
                    }
                    None => {
                        let id = create_thread(tx, planned.description())?;
                        known.insert(index, id);
                        made.insert(index, id);
                    }
                }
            }
            Step::Assign { message, thread } => {
                changed |= assign_thread(tx, message, id_of(threads, &known, thread)?)?;
            }
            Step::Remove(id) => changed |= remove_thread(tx, id)?,
        }
    }
    if changed {
        note_membership_change(tx)?;
    }
    Ok(made)
}

/// The id a planned thread has by now: kept, or created earlier.
fn id_of(
    threads: &[PlannedThread],
    known: &HashMap<usize, i64>,
    index: usize,
) -> Result<i64, StoreError> {
    threads[index]
        .kept
        .or_else(|| known.get(&index).copied())
        .ok_or_else(|| StoreError::internal("a thread was used before it was created"))
}

#[cfg(test)]
mod tests;
