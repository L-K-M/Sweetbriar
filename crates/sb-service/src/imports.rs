//! Import DTOs and `Core`'s import commands and query (spec/07
//! "Commands", spec/09 "Job lifecycle"). The DTOs are service-owned
//! mirrors of the engine's types; nothing from below crosses into L1
//! (spec/01 rule 3), so the methods convert field by field.

use std::path::PathBuf;

use crate::{Core, CoreError, Receipt};

/// Where an import lands. The pilot has only the local archive (spec/09).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportTarget {
    LocalArchive,
}

/// How an mbox escapes `From ` lines (spec/09).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MboxVariant {
    Mboxo,
    Mboxrd,
}

impl MboxVariant {
    fn to_engine(self) -> sb_engine::MboxVariant {
        match self {
            Self::Mboxo => sb_engine::MboxVariant::Mboxo,
            Self::Mboxrd => sb_engine::MboxVariant::Mboxrd,
        }
    }

    fn from_engine(variant: sb_engine::MboxVariant) -> Self {
        match variant {
            sb_engine::MboxVariant::Mboxo => Self::Mboxo,
            sb_engine::MboxVariant::Mboxrd => Self::Mboxrd,
        }
    }
}

/// An import job's state (spec/02 `import_jobs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportJobState {
    Inventory,
    AwaitingConfirmation,
    Running,
    Completed,
    Cancelled,
    Failed,
}

impl ImportJobState {
    fn from_engine(state: sb_engine::ImportJobState) -> Self {
        match state {
            sb_engine::ImportJobState::Inventory => Self::Inventory,
            sb_engine::ImportJobState::AwaitingConfirmation => Self::AwaitingConfirmation,
            sb_engine::ImportJobState::Running => Self::Running,
            sb_engine::ImportJobState::Completed => Self::Completed,
            sb_engine::ImportJobState::Cancelled => Self::Cancelled,
            sb_engine::ImportJobState::Failed => Self::Failed,
        }
    }

    /// Completed, cancelled or failed: nothing more will change.
    pub fn is_finished(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled | Self::Failed)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportSourceKind {
    Mbox,
    Eml,
}

/// Why a chosen or discovered path did not become a source (spec/09).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportProblemKind {
    Inaccessible,
    Unsupported,
    NonUtf8,
    Failed,
}

/// One inventoried source and how far its ingest has got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportSource {
    pub ordinal: u64,
    pub path: PathBuf,
    pub mailbox: String,
    pub kind: ImportSourceKind,
    pub messages: u64,
    pub bytes: u64,
    pub sha256: String,
    /// A `>>From ` line exists, so `confirm_import` must choose a variant.
    pub needs_variant: bool,
    pub variant: Option<MboxVariant>,
    pub messages_done: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportProblem {
    pub path: PathBuf,
    pub kind: ImportProblemKind,
    pub detail: String,
}

/// Running totals (spec/09 "Report").
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImportTotals {
    pub processed: u64,
    pub imported: u64,
    pub duplicates: u64,
    pub rejected: u64,
    pub bytes_stored: u64,
    pub read_from_headers: u64,
    pub read_default: u64,
    pub flags_incomplete: u64,
}

/// One import job as `import_job` reports it (spec/07).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportJob {
    pub id: String,
    pub state: ImportJobState,
    /// The inventory in ordinal order; empty until it finishes.
    pub sources: Vec<ImportSource>,
    /// The first problems; `problem_count` counts them all.
    pub problems: Vec<ImportProblem>,
    pub problem_count: u64,
    pub totals: ImportTotals,
    pub failure: Option<String>,
    /// The report's path, once it is written (spec/09).
    pub report_path: Option<PathBuf>,
}

/// A finished job's published report (spec/07 `import_report`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportReport {
    pub path: PathBuf,
    /// The version-1 JSON object (spec/09 "Report").
    pub json: String,
}

/// What `start_import` answers: its receipt and the job it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportStarted {
    pub receipt: Receipt,
    pub job_id: String,
}

impl Core {
    /// `start_import(paths, target)` (spec/07): record the job and
    /// inventory it in the background. A repeated key names the first
    /// job and starts nothing.
    pub async fn start_import(
        &self,
        idempotency_key: &str,
        paths: Vec<PathBuf>,
        target: ImportTarget,
    ) -> Result<ImportStarted, CoreError> {
        let ImportTarget::LocalArchive = target;
        let started = self.engine().start_import(idempotency_key, paths).await?;
        Ok(ImportStarted {
            receipt: Receipt::from_engine(started.receipt()),
            job_id: started.job_id().to_owned(),
        })
    }

    /// `confirm_import(job_id, variants)` (spec/07, 09): a variant for
    /// exactly the sources that need one, by ordinal; then ingest.
    pub async fn confirm_import(
        &self,
        idempotency_key: &str,
        job_id: &str,
        variants: Vec<(u64, MboxVariant)>,
    ) -> Result<Receipt, CoreError> {
        let variants = variants
            .into_iter()
            .map(|(ordinal, variant)| (ordinal, variant.to_engine()))
            .collect();
        let receipt = self
            .engine()
            .confirm_import(idempotency_key, job_id, variants)
            .await?;
        Ok(Receipt::from_engine(&receipt))
    }

    /// Cancel an import or durably pause Search index work (spec/07, 09).
    /// Committed work stays; an index user hold survives reopen until resume.
    pub async fn cancel_job(
        &self,
        idempotency_key: &str,
        job_id: &str,
    ) -> Result<Receipt, CoreError> {
        let receipt = self.engine().cancel_job(idempotency_key, job_id).await?;
        Ok(Receipt::from_engine(&receipt))
    }

    /// `import_report(job_id)` (spec/07, 09): a finished job's report,
    /// or `None` while it is still being written. An unfinished job is
    /// `Conflict`.
    pub async fn import_report(&self, job_id: &str) -> Result<Option<ImportReport>, CoreError> {
        let report = self.engine().import_report(job_id).await?;
        Ok(report.map(|report| ImportReport {
            path: report.path().to_path_buf(),
            json: report.json().to_owned(),
        }))
    }

    /// `import_job(job_id)` (spec/07): state, inventory, problems,
    /// counts and, once written, the report's path.
    pub async fn import_job(&self, job_id: &str) -> Result<ImportJob, CoreError> {
        let status = self.engine().import_job(job_id).await?;
        let checkpoint = status.checkpoint();
        let sources = (0..)
            .zip(status.sources())
            .map(|(ordinal, row)| {
                let source = row.source();
                ImportSource {
                    ordinal,
                    path: source.path().to_path_buf(),
                    mailbox: source.mailbox().to_owned(),
                    kind: match source.kind() {
                        sb_engine::SourceKind::Mbox => ImportSourceKind::Mbox,
                        sb_engine::SourceKind::Eml => ImportSourceKind::Eml,
                    },
                    messages: source.message_count(),
                    bytes: source.snapshot().byte_length(),
                    sha256: source.snapshot().sha256_hex(),
                    needs_variant: source.quoting() == sb_engine::MboxQuoting::Ambiguous,
                    variant: checkpoint.variant(ordinal).map(MboxVariant::from_engine),
                    messages_done: row.position().messages(),
                }
            })
            .collect();
        let problems = checkpoint
            .problems()
            .iter()
            .map(|problem| ImportProblem {
                path: problem.path().to_path_buf(),
                kind: match problem.kind() {
                    sb_engine::ImportProblemKind::Inaccessible => ImportProblemKind::Inaccessible,
                    sb_engine::ImportProblemKind::Unsupported => ImportProblemKind::Unsupported,
                    sb_engine::ImportProblemKind::NonUtf8 => ImportProblemKind::NonUtf8,
                    sb_engine::ImportProblemKind::Failed => ImportProblemKind::Failed,
                },
                detail: problem.detail().to_owned(),
            })
            .collect();
        let totals = checkpoint.totals();
        Ok(ImportJob {
            id: job_id.to_owned(),
            state: ImportJobState::from_engine(status.state()),
            sources,
            problems,
            problem_count: checkpoint.problem_count(),
            totals: ImportTotals {
                processed: totals.processed,
                imported: totals.imported,
                duplicates: totals.duplicates,
                rejected: totals.rejected,
                bytes_stored: totals.bytes_stored,
                read_from_headers: totals.read_from_headers,
                read_default: totals.read_default,
                flags_incomplete: totals.flags_incomplete,
            },
            failure: checkpoint.failure().map(str::to_owned),
            report_path: status.report_path().map(|path| path.to_path_buf()),
        })
    }
}
