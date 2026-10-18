//! Imports over FFI (spec/07 "Commands", spec/09 "Job lifecycle"):
//! `start_import`, `confirm_import` with one variant choice per
//! ambiguous source, and `cancel_job` — each a command under an
//! idempotency key the shell made, so the token counts only until the
//! command holds the core (`Core::command`) — and the `import_job`
//! query with the job's vocabulary. Paths cross as UTF-8 strings
//! (spec/07 "DTOs"); one that isn't UTF-8 is spelled lossily, since
//! the shell only shows it.

use std::path::PathBuf;
use std::sync::Arc;

use crate::api::{Core, Receipt};
use crate::cancel::CancelToken;
use crate::error::CoreError;

/// Where an import lands. The pilot has only the local archive (spec/09).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum ImportTarget {
    LocalArchive,
}

/// How an mbox escapes `From ` lines (spec/09).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum MboxVariant {
    Mboxo,
    Mboxrd,
}

impl MboxVariant {
    fn to_service(self) -> sb_service::MboxVariant {
        match self {
            Self::Mboxo => sb_service::MboxVariant::Mboxo,
            Self::Mboxrd => sb_service::MboxVariant::Mboxrd,
        }
    }

    fn from_service(variant: sb_service::MboxVariant) -> Self {
        match variant {
            sb_service::MboxVariant::Mboxo => Self::Mboxo,
            sb_service::MboxVariant::Mboxrd => Self::Mboxrd,
        }
    }
}

/// An import job's state (spec/02 `import_jobs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum ImportJobState {
    Inventory,
    AwaitingConfirmation,
    Running,
    Completed,
    Cancelled,
    Failed,
}

impl ImportJobState {
    fn from_service(state: sb_service::ImportJobState) -> Self {
        match state {
            sb_service::ImportJobState::Inventory => Self::Inventory,
            sb_service::ImportJobState::AwaitingConfirmation => Self::AwaitingConfirmation,
            sb_service::ImportJobState::Running => Self::Running,
            sb_service::ImportJobState::Completed => Self::Completed,
            sb_service::ImportJobState::Cancelled => Self::Cancelled,
            sb_service::ImportJobState::Failed => Self::Failed,
        }
    }
}

/// What a source holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum ImportSourceKind {
    Mbox,
    Eml,
}

/// Why a chosen or discovered path did not become a source (spec/09).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum ImportProblemKind {
    Inaccessible,
    Unsupported,
    NonUtf8,
    Failed,
}

/// One inventoried source and how far its ingest has got.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ImportSource {
    pub ordinal: u64,
    pub path: String,
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

impl ImportSource {
    fn from_service(source: sb_service::ImportSource) -> Self {
        Self {
            ordinal: source.ordinal,
            path: source.path.to_string_lossy().into_owned(),
            mailbox: source.mailbox,
            kind: match source.kind {
                sb_service::ImportSourceKind::Mbox => ImportSourceKind::Mbox,
                sb_service::ImportSourceKind::Eml => ImportSourceKind::Eml,
            },
            messages: source.messages,
            bytes: source.bytes,
            sha256: source.sha256,
            needs_variant: source.needs_variant,
            variant: source.variant.map(MboxVariant::from_service),
            messages_done: source.messages_done,
        }
    }
}

/// A path that did not become a source, and why.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ImportProblem {
    pub path: String,
    pub kind: ImportProblemKind,
    pub detail: String,
}

impl ImportProblem {
    fn from_service(problem: sb_service::ImportProblem) -> Self {
        Self {
            path: problem.path.to_string_lossy().into_owned(),
            kind: match problem.kind {
                sb_service::ImportProblemKind::Inaccessible => ImportProblemKind::Inaccessible,
                sb_service::ImportProblemKind::Unsupported => ImportProblemKind::Unsupported,
                sb_service::ImportProblemKind::NonUtf8 => ImportProblemKind::NonUtf8,
                sb_service::ImportProblemKind::Failed => ImportProblemKind::Failed,
            },
            detail: problem.detail,
        }
    }
}

/// Running totals (spec/09 "Report").
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
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

impl ImportTotals {
    fn from_service(totals: sb_service::ImportTotals) -> Self {
        Self {
            processed: totals.processed,
            imported: totals.imported,
            duplicates: totals.duplicates,
            rejected: totals.rejected,
            bytes_stored: totals.bytes_stored,
            read_from_headers: totals.read_from_headers,
            read_default: totals.read_default,
            flags_incomplete: totals.flags_incomplete,
        }
    }
}

/// One import job as `import_job` reports it (spec/07).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
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
    /// The report's path, once the job has finished.
    pub report_path: Option<String>,
}

impl ImportJob {
    fn from_service(job: sb_service::ImportJob) -> Self {
        Self {
            id: job.id,
            state: ImportJobState::from_service(job.state),
            sources: job
                .sources
                .into_iter()
                .map(ImportSource::from_service)
                .collect(),
            problems: job
                .problems
                .into_iter()
                .map(ImportProblem::from_service)
                .collect(),
            problem_count: job.problem_count,
            totals: ImportTotals::from_service(job.totals),
            failure: job.failure,
            report_path: job
                .report_path
                .map(|path| path.to_string_lossy().into_owned()),
        }
    }
}

/// The variant chosen for one ambiguous source, by inventory ordinal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct VariantChoice {
    pub ordinal: u64,
    pub variant: MboxVariant,
}

/// What `start_import` answers: its receipt and the job it names.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ImportStarted {
    pub receipt: Receipt,
    pub job_id: String,
}

#[uniffi::export]
impl Core {
    /// `start_import(paths, target)` (spec/07): record the job and
    /// inventory it in the background. A repeated key names the first
    /// job and starts nothing.
    pub async fn start_import(
        &self,
        token: Arc<CancelToken>,
        idempotency_key: String,
        paths: Vec<String>,
        target: ImportTarget,
    ) -> Result<ImportStarted, CoreError> {
        let paths = paths.into_iter().map(PathBuf::from).collect();
        let ImportTarget::LocalArchive = target;
        let started = self
            .command(&token, async |core| {
                core.start_import(
                    &idempotency_key,
                    paths,
                    sb_service::ImportTarget::LocalArchive,
                )
                .await
            })
            .await?;
        Ok(ImportStarted {
            receipt: Receipt::from_service(&started.receipt),
            job_id: started.job_id,
        })
    }

    /// `confirm_import(job_id, variants)` (spec/07, 09): a variant for
    /// exactly the sources that need one; then ingest. A missing,
    /// extra or repeated choice is `InvalidInput`.
    pub async fn confirm_import(
        &self,
        token: Arc<CancelToken>,
        idempotency_key: String,
        job_id: String,
        variants: Vec<VariantChoice>,
    ) -> Result<Receipt, CoreError> {
        let variants = variants
            .into_iter()
            .map(|choice| (choice.ordinal, choice.variant.to_service()))
            .collect();
        let receipt = self
            .command(&token, async |core| {
                core.confirm_import(&idempotency_key, &job_id, variants)
                    .await
            })
            .await?;
        Ok(Receipt::from_service(&receipt))
    }

    /// `cancel_job(id)` (spec/07, 09): the job is cancelled once this
    /// returns; committed batches stay imported. A finished job is
    /// `Conflict`.
    pub async fn cancel_job(
        &self,
        token: Arc<CancelToken>,
        idempotency_key: String,
        job_id: String,
    ) -> Result<Receipt, CoreError> {
        let receipt = self
            .command(&token, async |core| {
                core.cancel_job(&idempotency_key, &job_id).await
            })
            .await?;
        Ok(Receipt::from_service(&receipt))
    }

    /// `import_job(job_id)` (spec/07): state, inventory, problems and
    /// counts. A query: the token races every wait. An unknown job is
    /// `NotFound`.
    pub async fn import_job(
        &self,
        token: Arc<CancelToken>,
        job_id: String,
    ) -> Result<ImportJob, CoreError> {
        let job = self
            .query(&token, async |core| core.import_job(&job_id).await)
            .await?;
        Ok(ImportJob::from_service(job))
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::api::{CoreConfig, open_core};
    use sb_test_support::ScratchDir;

    /// How long a test waits for the import worker before failing.
    const PATIENCE: Duration = Duration::from_secs(300);
    const POLL: Duration = Duration::from_millis(20);
    /// A body line mboxo and mboxrd decode differently.
    const AMBIGUOUS: &str = "From sender@example.test Sat Oct 17 12:00:00 2026\n\
        Subject: quoted\n\n>>From the archive\n";

    fn token() -> Arc<CancelToken> {
        CancelToken::new()
    }

    fn scratch_core(scratch: &ScratchDir) -> Arc<Core> {
        let config = CoreConfig {
            data_root: scratch.path().join("store").to_string_lossy().into_owned(),
            cache_root: scratch.path().join("cache").to_string_lossy().into_owned(),
        };
        sb_test_support::block_on(open_core(token(), config)).unwrap()
    }

    fn mbox(dir: &Path) -> String {
        let path = dir.join("Inbox.mbox");
        std::fs::write(&path, AMBIGUOUS).unwrap();
        path.to_string_lossy().into_owned()
    }

    fn job(core: &Core, job_id: &str) -> ImportJob {
        sb_test_support::block_on(core.import_job(token(), job_id.to_owned())).unwrap()
    }

    fn finished(state: ImportJobState) -> bool {
        matches!(
            state,
            ImportJobState::Completed | ImportJobState::Cancelled | ImportJobState::Failed
        )
    }

    fn wait(core: &Core, job_id: &str, done: impl Fn(ImportJobState) -> bool) -> ImportJob {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let job = job(core, job_id);
            if done(job.state) {
                return job;
            }
            assert!(Instant::now() < deadline, "the job never settled");
            std::thread::sleep(POLL);
        }
    }

    fn start(core: &Core, key: &str, path: &str) -> ImportStarted {
        sb_test_support::block_on(core.start_import(
            token(),
            key.to_owned(),
            vec![path.to_owned()],
            ImportTarget::LocalArchive,
        ))
        .unwrap()
    }

    /// An import over the exported surface: a repeated key names the
    /// first job, the inventory crosses before confirmation, the
    /// ambiguous source needs its variant, the job completes with its
    /// totals, and a finished job can't be cancelled.
    #[test]
    fn an_import_runs_through_the_exported_surface() {
        let scratch = ScratchDir::new("ffi-import");
        let core = scratch_core(&scratch);
        let path = mbox(scratch.path());

        let started = start(&core, "import-1", &path);
        assert_eq!(started.receipt.command, "start_import");
        assert_eq!(start(&core, "import-1", &path).job_id, started.job_id);
        let id = started.job_id;

        let waiting = wait(&core, &id, |state| state != ImportJobState::Inventory);
        assert_eq!(waiting.id, id);
        assert_eq!(waiting.state, ImportJobState::AwaitingConfirmation);
        assert_eq!(waiting.report_path, None);
        let [source] = waiting.sources.as_slice() else {
            panic!("one source: {:?}", waiting.sources);
        };
        assert_eq!(source.ordinal, 0);
        assert_eq!(source.path, path);
        assert_eq!(source.mailbox, "Inbox");
        assert_eq!(source.kind, ImportSourceKind::Mbox);
        assert_eq!(source.messages, 1);
        assert_eq!(source.bytes, AMBIGUOUS.len() as u64);
        assert!(source.needs_variant);
        assert_eq!(source.variant, None);
        assert_eq!(source.messages_done, 0);

        let err = sb_test_support::block_on(core.confirm_import(
            token(),
            "confirm-0".to_owned(),
            id.clone(),
            Vec::new(),
        ))
        .unwrap_err();
        assert!(matches!(err, CoreError::InvalidInput { .. }), "{err}");

        let choice = VariantChoice {
            ordinal: 0,
            variant: MboxVariant::Mboxrd,
        };
        sb_test_support::block_on(core.confirm_import(
            token(),
            "confirm-1".to_owned(),
            id.clone(),
            vec![choice],
        ))
        .unwrap();
        let done = wait(&core, &id, finished);
        assert_eq!(done.state, ImportJobState::Completed);
        assert_eq!(done.sources[0].variant, Some(MboxVariant::Mboxrd));
        assert_eq!(done.sources[0].messages_done, 1);
        assert_eq!(done.totals.processed, 1);
        assert_eq!(done.totals.imported, 1);
        assert_eq!(done.problem_count, 0);
        assert_eq!(done.failure, None);

        let err = sb_test_support::block_on(core.cancel_job(token(), "cancel-1".to_owned(), id))
            .unwrap_err();
        assert!(matches!(err, CoreError::Conflict { .. }), "{err}");

        sb_test_support::block_on(core.shutdown());
    }

    /// `cancel_job` leaves a waiting job cancelled once it returns; a
    /// command under a cancelled token starts nothing.
    #[test]
    fn cancel_job_cancels_a_waiting_job() {
        let scratch = ScratchDir::new("ffi-import-cancel");
        let core = scratch_core(&scratch);
        let path = mbox(scratch.path());

        let cancelled = token();
        cancelled.cancel();
        let err = sb_test_support::block_on(core.start_import(
            cancelled,
            "import-1".to_owned(),
            vec![path.clone()],
            ImportTarget::LocalArchive,
        ))
        .unwrap_err();
        assert!(matches!(err, CoreError::Cancelled));

        let id = start(&core, "import-2", &path).job_id;
        wait(&core, &id, |state| state != ImportJobState::Inventory);
        let receipt =
            sb_test_support::block_on(core.cancel_job(token(), "cancel-1".to_owned(), id.clone()))
                .unwrap();
        assert_eq!(receipt.command, "cancel_job");
        assert_eq!(job(&core, &id).state, ImportJobState::Cancelled);

        sb_test_support::block_on(core.shutdown());
    }

    /// A path that can't be read is a problem, listed and counted; an
    /// unknown job is `NotFound`.
    #[test]
    fn problems_cross_with_the_job() {
        let scratch = ScratchDir::new("ffi-import-problems");
        let core = scratch_core(&scratch);
        let missing = scratch.path().join("missing.mbox");
        let missing = missing.to_string_lossy().into_owned();

        let id = start(&core, "import-1", &missing).job_id;
        let job = wait(&core, &id, |state| state != ImportJobState::Inventory);
        assert_eq!(job.problem_count, 1);
        let [problem] = job.problems.as_slice() else {
            panic!("one problem: {:?}", job.problems);
        };
        assert_eq!(problem.path, missing);
        assert_eq!(problem.kind, ImportProblemKind::Inaccessible);

        let err = sb_test_support::block_on(core.import_job(token(), "no-such-job".to_owned()))
            .unwrap_err();
        assert!(matches!(err, CoreError::NotFound { .. }), "{err}");

        sb_test_support::block_on(core.shutdown());
    }
}
