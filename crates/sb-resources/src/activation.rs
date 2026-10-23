//! Owned single-message activation; accepted writes survive dropped request futures.

use sb_store::activation;

use crate::{Resources, StoreError, Timestamp};

#[derive(Debug)]
pub struct ActivationWork(activation::ActivationWork);

#[derive(Debug)]
pub struct ActivationPrepared(activation::ActivationPrepared);

#[derive(Debug)]
pub enum ActivationPreparation {
    Continue(ActivationWork),
    Prepared(ActivationPrepared),
    Current,
    Recapture,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationCommit {
    Activated,
    Deferred,
    Current,
    Recapture,
}

#[derive(Debug)]
pub struct ActivationStatus {
    pub target: u32,
    pub bound_target: Option<u32>,
    pub candidate_at: Option<Timestamp>,
    pub recorded_deferral: Option<RecordedActivationDeferral>,
}

#[derive(Debug)]
pub struct RecordedActivationDeferral {
    pub recorded_cost: u64,
    pub recorded_at: Timestamp,
    pub count: u32,
    pub cutoff: i64,
}

impl Resources {
    pub fn begin_activation(
        &self,
        message: i64,
    ) -> impl Future<Output = Result<Option<ActivationWork>, StoreError>> + use<> {
        let submitted = self
            .store
            .begin_activation(message, sb_mime::PARSER_VERSION);
        async move { submitted.await.map(|work| work.map(ActivationWork)) }
    }

    pub fn prepare_activation(
        &self,
        work: ActivationWork,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<ActivationPreparation, StoreError> {
        self.store
            .prepare_activation(work.0, cancelled)
            .map(|value| match value {
                activation::ActivationPreparation::Continue(work) => {
                    ActivationPreparation::Continue(ActivationWork(work))
                }
                activation::ActivationPreparation::Prepared(prepared) => {
                    ActivationPreparation::Prepared(ActivationPrepared(prepared))
                }
                activation::ActivationPreparation::Current => ActivationPreparation::Current,
                activation::ActivationPreparation::Recapture => ActivationPreparation::Recapture,
                activation::ActivationPreparation::Cancelled => ActivationPreparation::Cancelled,
            })
    }

    pub fn commit_activation(
        &self,
        prepared: ActivationPrepared,
    ) -> impl Future<Output = Result<ActivationCommit, StoreError>> + use<> {
        let submitted = self.store.commit_activation(prepared.0, self.now());
        async move {
            submitted.await.map(|value| match value {
                activation::ActivationCommit::Activated => ActivationCommit::Activated,
                activation::ActivationCommit::Deferred => ActivationCommit::Deferred,
                activation::ActivationCommit::Current => ActivationCommit::Current,
                activation::ActivationCommit::Recapture => ActivationCommit::Recapture,
            })
        }
    }

    pub fn activation_status(&self, message: i64) -> Result<ActivationStatus, StoreError> {
        let status = self.store.activation_status(message)?;
        Ok(ActivationStatus {
            target: status.target,
            bound_target: status.bound_target,
            candidate_at: status.candidate_at.map(Timestamp::from_millis),
            recorded_deferral: status
                .recorded_deferral
                .map(|record| RecordedActivationDeferral {
                    recorded_cost: record.recorded_cost,
                    recorded_at: Timestamp::from_millis(record.recorded_at),
                    count: record.count,
                    cutoff: record.cutoff,
                }),
        })
    }
}
