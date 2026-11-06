//! Export DTOs and `Core`'s export commands and query (spec/07
//! "Commands", spec/09 "Export file output"). The DTOs are
//! service-owned mirrors of the engine's types; nothing from below
//! crosses into L1 (spec/01 rule 3), so the methods convert field by
//! field.

use crate::{Core, CoreError, Receipt, SearchSpec};

/// The output format the job writes (spec/09).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    /// One `.eml` file per exported occurrence.
    Eml,
    /// One mboxrd file per exported mailbox.
    Mboxrd,
}

impl ExportFormat {
    fn to_engine(self) -> sb_engine::ExportFormat {
        match self {
            Self::Eml => sb_engine::ExportFormat::Eml,
            Self::Mboxrd => sb_engine::ExportFormat::Mboxrd,
        }
    }
}

/// An export job's state (spec/02 `export_jobs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportJobState {
    /// The capture ledger is still being written.
    Capturing,
    /// The capture sealed; output can start or resume.
    Ready,
    /// File output in progress.
    Running,
    Completed,
    Cancelled,
    Failed,
}

impl ExportJobState {
    fn from_engine(state: sb_engine::ExportJobState) -> Self {
        match state {
            sb_engine::ExportJobState::Capturing => Self::Capturing,
            sb_engine::ExportJobState::Ready => Self::Ready,
            sb_engine::ExportJobState::Running => Self::Running,
            sb_engine::ExportJobState::Completed => Self::Completed,
            sb_engine::ExportJobState::Cancelled => Self::Cancelled,
            sb_engine::ExportJobState::Failed => Self::Failed,
        }
    }

    /// Completed, cancelled or failed: nothing more will change.
    pub fn is_finished(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled | Self::Failed)
    }
}

/// What the export covers (spec/09 "Export scope"): a generation-fenced
/// mailbox, all mailboxes of one account, a checked Search, or the ids
/// a frozen selection handle holds. A stale generation is `Conflict`
/// at capture; the shell retries with the generation it re-reads.
#[derive(Debug, Clone, PartialEq)]
pub enum ExportScopeInput {
    /// One mailbox's occurrences, in window order.
    Mailbox { mailbox_id: i64, generation: u64 },
    /// Every present occurrence of one account's mailboxes, in id order.
    Account { account_id: String, generation: u64 },
    /// A checked Search spec's chosen occurrences at `generation`.
    Search { spec: SearchSpec, generation: u64 },
    /// A frozen selection handle's immutable ordered ids.
    Selection { selection_id: String },
}

impl ExportScopeInput {
    fn into_engine(self) -> sb_engine::ExportScopeInput {
        match self {
            Self::Mailbox {
                mailbox_id,
                generation,
            } => sb_engine::ExportScopeInput::Mailbox {
                mailbox_id,
                generation,
            },
            Self::Account {
                account_id,
                generation,
            } => sb_engine::ExportScopeInput::Account {
                account_id,
                generation,
            },
            Self::Search { spec, generation } => sb_engine::ExportScopeInput::Search {
                spec: spec.into_engine(),
                generation,
            },
            Self::Selection { selection_id } => {
                sb_engine::ExportScopeInput::Selection(selection_id.into())
            }
        }
    }
}

/// What `start_export` answers: its receipt and the job it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportStarted {
    pub receipt: Receipt,
    pub job_id: String,
}

/// One export job as `export_job` reports it (spec/07): state, the
/// sealed request hash, the last committed checkpoint and the captured
/// chunk count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportJob {
    pub id: String,
    pub state: ExportJobState,
    /// The bounded request record: version, capture version and the
    /// deterministic intent hash — never the intent itself.
    pub request_json: String,
    /// The last committed checkpoint's JSON; failure details land in it.
    pub checkpoint_json: String,
    /// Capture chunks held by the sealed-or-partial ledger.
    pub captured_chunks: u64,
}

impl Core {
    /// `start_export(scope, format, destination)` (spec/07, 09): record
    /// the job under the idempotency key, then run its capture and file
    /// output in the background. A repeated key names the first job and
    /// starts nothing. `destination` is an absolute path the job claims.
    pub async fn start_export(
        &self,
        idempotency_key: &str,
        scope: ExportScopeInput,
        format: ExportFormat,
        destination: String,
    ) -> Result<ExportStarted, CoreError> {
        let started = self
            .engine()
            .start_export(
                idempotency_key,
                scope.into_engine(),
                format.to_engine(),
                destination,
            )
            .await?;
        Ok(ExportStarted {
            receipt: Receipt::from_engine(started.receipt()),
            job_id: started.job_id().to_owned(),
        })
    }

    /// `export_job(job_id)` (spec/07): the durable record — state,
    /// request hash, checkpoint and captured chunk count.
    pub async fn export_job(&self, job_id: &str) -> Result<ExportJob, CoreError> {
        let status = self.engine().export_job(job_id).await?;
        Ok(ExportJob {
            id: job_id.to_owned(),
            state: ExportJobState::from_engine(status.state()),
            request_json: status.request_json().to_owned(),
            checkpoint_json: status.checkpoint_json().to_owned(),
            captured_chunks: status.captured_chunks(),
        })
    }
}

#[cfg(test)]
mod tests;
