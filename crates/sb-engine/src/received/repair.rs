//! Once-per-acquisition finite passes; failed sources cannot block later IDs.

use super::*;
use sb_resources::{
    RepairCapture, RepairCommit, RepairPreparation, RepairPrepared, RepairTerminal,
};
use std::collections::VecDeque;

type Pair = (u64, u32);

#[derive(Default)]
pub(super) struct Sources {
    pass: Option<Pass>,
    attempted: Option<Pair>,
    checked: bool,
    status_parked: bool,
}

struct Pass {
    pair: Pair,
    after: i64,
    cutoff: Option<i64>,
    ids: VecDeque<i64>,
    work: Option<Work>,
    eof: bool,
    errors: [Option<StoreError>; 2],
}

enum Work {
    Prepare(RepairCapture),
    Publish(RepairPrepared),
    Diagnose(RepairTerminal),
}

fn status(resources: &Resources) -> Result<Pair, StoreError> {
    resources
        .catchup_status()
        .map(|status| (status.inputs, status.target))
}

impl Sources {
    pub(super) fn turn(
        &mut self,
        resources: &Resources,
        jobs: &ReceivedJobs,
        stop: &dyn Fn() -> bool,
        woke: bool,
    ) -> bool {
        let due = self.pass.is_none() && jobs.retry_due(Lane::SourceRetry, resources.now());
        if due {
            jobs.disarm(Lane::SourceRetry);
        }
        if woke || !self.checked || due {
            self.checked = true;
            let pair = match status(resources) {
                Ok(pair) => pair,
                Err(error) => {
                    self.status_parked = !retryable(&error);
                    self.abort(resources, jobs, error);
                    return false;
                }
            };
            let recovered_status = std::mem::take(&mut self.status_parked);
            let startup = self.attempted.is_none()
                && jobs.failure_for(Lane::SourceRetry).is_none()
                && jobs.failure_for(Lane::SourcePark).is_none();
            if self.pass.is_none()
                && (self.attempted.is_some_and(|last| pair != last)
                    || startup
                    || due
                    || recovered_status)
            {
                self.attempted = Some(pair);
                jobs.disarm(Lane::SourceRetry);
                self.pass = Some(Pass {
                    pair,
                    after: 0,
                    cutoff: None,
                    ids: VecDeque::new(),
                    work: None,
                    eof: false,
                    errors: [None, None],
                });
            }
        }
        let Some(pass) = &mut self.pass else {
            return false;
        };
        match pass.step(resources, jobs, stop) {
            Ok(true) => true,
            Err(error) => {
                self.abort(resources, jobs, error);
                false
            }
            Ok(false) => {
                let current = match status(resources) {
                    Ok(pair) => pair,
                    Err(error) => {
                        self.status_parked = !retryable(&error);
                        self.abort(resources, jobs, error);
                        return false;
                    }
                };
                let pass = self.pass.take().unwrap();
                for (lane, error) in [Lane::SourceRetry, Lane::SourcePark]
                    .into_iter()
                    .zip(pass.errors)
                {
                    if let Some(error) = error {
                        jobs.failed(lane, error, resources.now());
                    } else {
                        jobs.recovered(lane);
                    }
                }
                self.attempted = Some(pass.pair);
                self.checked = current == pass.pair;
                #[cfg(test)]
                jobs.event(Event::SourceComplete);
                true
            }
        }
    }

    fn abort(&mut self, resources: &Resources, jobs: &ReceivedJobs, error: StoreError) {
        if let Some(pass) = self.pass.take() {
            self.attempted = Some(pass.pair);
        }
        jobs.failed(
            if retryable(&error) {
                Lane::SourceRetry
            } else {
                Lane::SourcePark
            },
            error,
            resources.now(),
        );
    }
}

impl Pass {
    fn failed(&mut self, error: StoreError) {
        let index = usize::from(!retryable(&error));
        if self.errors[index].is_none() {
            self.errors[index] = Some(error);
        }
    }

    fn step(
        &mut self,
        resources: &Resources,
        jobs: &ReceivedJobs,
        stop: &dyn Fn() -> bool,
    ) -> Result<bool, StoreError> {
        if let Some(work) = self.work.take() {
            match advance(resources, jobs, stop, work) {
                Ok(next) => self.work = next,
                Err(error) => self.failed(error),
            }
            return Ok(true);
        }
        if let Some(id) = self.ids.pop_front() {
            #[cfg(test)]
            jobs.event(Event::RepairBegin(id));
            match commit(jobs, stop, Step::BeginRepair, || resources.begin_repair(id)) {
                Ok(Some(Some(capture))) => self.work = Some(Work::Prepare(capture)),
                Err(error) => self.failed(error),
                _ => {}
            }
            return Ok(true);
        }
        if self.eof {
            return Ok(false);
        }
        if stop() {
            return Ok(true);
        }
        let page = resources.repair_sources(self.after, self.cutoff)?;
        if self.cutoff.is_none() {
            self.pair.0 = page.inputs;
            #[cfg(test)]
            jobs.event(Event::SourcePass {
                inputs: page.inputs,
                cutoff: page.cutoff,
            });
        }
        self.eof = page.through == self.after || page.through >= page.cutoff;
        self.after = page.through;
        self.cutoff = Some(page.cutoff);
        self.ids = page.acquisition_ids.into();
        Ok(true)
    }
}

fn advance(
    resources: &Resources,
    jobs: &ReceivedJobs,
    stop: &dyn Fn() -> bool,
    work: Work,
) -> Result<Option<Work>, StoreError> {
    if stop() {
        return Ok(None);
    }
    match work {
        Work::Prepare(capture) => {
            let prepared = resources.prepare_repair(capture, stop)?;
            #[cfg(test)]
            jobs.event(Event::RepairPrepared);
            if stop() {
                return Ok(None);
            }
            Ok(match prepared {
                RepairPreparation::Prepared(value) => Some(Work::Publish(value)),
                RepairPreparation::Terminal(value) => Some(Work::Diagnose(value)),
                _ => None,
            })
        }
        Work::Publish(prepared) => Ok(
            match commit(jobs, stop, Step::Repair, || {
                resources.commit_repair_step(prepared)
            })? {
                Some(RepairCommit::Continue(next)) => Some(Work::Publish(next)),
                _ => None,
            },
        ),
        Work::Diagnose(attempt) => {
            commit(jobs, stop, Step::Diagnosis, || {
                resources.persist_repair_terminal(attempt)
            })?;
            Ok(None)
        }
    }
}
