//! Owned corpus continuations; every returned cursor follows a committed chunk.

use crate::{Resources, StoreError};
use sb_store::activation::corpus;

#[derive(Debug)]
pub struct CatchupSweep(corpus::CatchupSweep);
#[derive(Debug)]
pub struct CatchupBatch(corpus::CatchupBatch);
#[derive(Debug)]
pub enum CatchupPreparation {
    Continue(CatchupSweep),
    Prepared(CatchupBatch),
    Recapture,
    Cancelled,
}
#[derive(Debug)]
pub enum CatchupCommit {
    Continue(CatchupSweep, CatchupReport),
    Complete(CatchupReport),
    Recapture,
}

#[derive(Debug)]
pub struct CatchupReport {
    pub processed: u32,
    pub activated: u32,
    pub deferred: u32,
    pub unready: u32,
    pub charged_bytes: u64,
    pub repair_needed: Vec<i64>,
}

impl CatchupReport {
    fn from_stored(value: corpus::CatchupReport) -> Self {
        Self {
            processed: value.processed,
            activated: value.activated,
            deferred: value.deferred,
            unready: value.unready,
            charged_bytes: value.charged_bytes,
            repair_needed: value.repair_needed,
        }
    }
}

#[derive(Debug)]
pub struct CatchupStatus {
    pub target: u32,
    pub inputs: u64,
    pub swept: u64,
    pub deferred: u64,
    pub unready: u64,
}

impl CatchupStatus {
    pub fn caught_up(&self) -> bool {
        self.inputs == self.swept
    }
    pub fn complete(&self) -> bool {
        self.caught_up() && self.deferred == 0 && self.unready == 0
    }
    pub fn recorded(&self) -> bool {
        !self.caught_up()
    }
}

impl Resources {
    pub fn begin_catchup(
        &self,
    ) -> impl Future<Output = Result<Option<CatchupSweep>, StoreError>> + use<> {
        let submitted = self.store.begin_catchup(sb_mime::PARSER_VERSION);
        async move { submitted.await.map(|scan| scan.map(CatchupSweep)) }
    }

    pub fn prepare_catchup_step(
        &self,
        scan: CatchupSweep,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<CatchupPreparation, StoreError> {
        self.store
            .prepare_catchup_step(scan.0, cancelled)
            .map(|outcome| match outcome {
                corpus::CatchupPreparation::Continue(scan) => {
                    CatchupPreparation::Continue(CatchupSweep(scan))
                }
                corpus::CatchupPreparation::Prepared(batch) => {
                    CatchupPreparation::Prepared(CatchupBatch(batch))
                }
                corpus::CatchupPreparation::Recapture => CatchupPreparation::Recapture,
                corpus::CatchupPreparation::Cancelled => CatchupPreparation::Cancelled,
            })
    }

    pub fn commit_catchup_batch(
        &self,
        batch: CatchupBatch,
    ) -> impl Future<Output = Result<CatchupCommit, StoreError>> + use<> {
        let submitted =
            self.store
                .commit_catchup_batch(batch.0, sb_mime::PARSER_VERSION, self.now());
        async move {
            submitted.await.map(|outcome| match outcome {
                corpus::CatchupCommit::Continue(scan, report) => {
                    CatchupCommit::Continue(CatchupSweep(scan), CatchupReport::from_stored(report))
                }
                corpus::CatchupCommit::Complete(report) => {
                    CatchupCommit::Complete(CatchupReport::from_stored(report))
                }
                corpus::CatchupCommit::Recapture => CatchupCommit::Recapture,
            })
        }
    }

    pub fn catchup_status(&self) -> Result<CatchupStatus, StoreError> {
        let value = self.store.catchup_status(sb_mime::PARSER_VERSION)?;
        Ok(CatchupStatus {
            target: value.target,
            inputs: value.inputs,
            swept: value.swept,
            deferred: value.deferred,
            unready: value.unready,
        })
    }
}
