//! The import commands over FFI (spec/07 "Commands", spec/09 "Job
//! lifecycle"): `start_import`, `confirm_import` with one variant
//! choice per ambiguous source, and `cancel_job`. Each is a command
//! under an idempotency key the shell made: the token counts only
//! until the command holds the core (`Core::command`). Paths cross as
//! UTF-8 strings (spec/07 "DTOs").

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

    /// The job's state through the service: `import_job` crosses the
    /// FFI only in M1.4g.
    fn state(core: &Core, job_id: &str) -> sb_service::ImportJobState {
        sb_test_support::block_on(async {
            let guard = core.service.read().await;
            guard
                .as_ref()
                .unwrap()
                .import_job(job_id)
                .await
                .unwrap()
                .state
        })
    }

    fn wait(core: &Core, job_id: &str, done: impl Fn(sb_service::ImportJobState) -> bool) {
        let deadline = Instant::now() + PATIENCE;
        while !done(state(core, job_id)) {
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

    /// An import over the exported commands: a repeated key names the
    /// first job, a confirmation must choose the ambiguous source's
    /// variant, the job completes, and a finished job can't be
    /// cancelled.
    #[test]
    fn an_import_runs_through_the_exported_commands() {
        let scratch = ScratchDir::new("ffi-import");
        let core = scratch_core(&scratch);
        let path = mbox(scratch.path());

        let started = start(&core, "import-1", &path);
        assert_eq!(started.receipt.command, "start_import");
        assert_eq!(start(&core, "import-1", &path).job_id, started.job_id);
        let job = started.job_id;
        wait(&core, &job, |state| {
            state != sb_service::ImportJobState::Inventory
        });
        assert_eq!(
            state(&core, &job),
            sb_service::ImportJobState::AwaitingConfirmation
        );

        let err = sb_test_support::block_on(core.confirm_import(
            token(),
            "confirm-0".to_owned(),
            job.clone(),
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
            job.clone(),
            vec![choice],
        ))
        .unwrap();
        wait(&core, &job, sb_service::ImportJobState::is_finished);
        assert_eq!(state(&core, &job), sb_service::ImportJobState::Completed);

        let err = sb_test_support::block_on(core.cancel_job(token(), "cancel-1".to_owned(), job))
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

        let job = start(&core, "import-2", &path).job_id;
        wait(&core, &job, |state| {
            state != sb_service::ImportJobState::Inventory
        });
        let receipt =
            sb_test_support::block_on(core.cancel_job(token(), "cancel-1".to_owned(), job.clone()))
                .unwrap();
        assert_eq!(receipt.command, "cancel_job");
        assert_eq!(state(&core, &job), sb_service::ImportJobState::Cancelled);

        sb_test_support::block_on(core.shutdown());
    }

    /// A command cancelled before it holds the core records nothing:
    /// its idempotency key is still free for another command.
    #[test]
    fn a_cancelled_start_leaves_its_key_unused() {
        let scratch = ScratchDir::new("ffi-import-cancel-key");
        let core = scratch_core(&scratch);
        let path = mbox(scratch.path());

        let cancelled = token();
        cancelled.cancel();
        let err = sb_test_support::block_on(core.start_import(
            cancelled,
            "key-1".to_owned(),
            vec![path],
            ImportTarget::LocalArchive,
        ))
        .unwrap_err();
        assert!(matches!(err, CoreError::Cancelled));

        let receipt = sb_test_support::block_on(core.set_setting(
            token(),
            "key-1".to_owned(),
            "appearance.compose_cue".to_owned(),
            "strong".to_owned(),
        ))
        .unwrap();
        assert_eq!(receipt.command, "set_setting");

        sb_test_support::block_on(core.shutdown());
    }

    /// Each ambiguous source keeps the variant chosen for it, by
    /// ordinal: both variants, past ordinal 0.
    #[test]
    fn each_source_keeps_its_chosen_variant() {
        let scratch = ScratchDir::new("ffi-import-variants");
        let core = scratch_core(&scratch);
        let paths: Vec<String> = ["A.mbox", "B.mbox"]
            .iter()
            .map(|name| {
                let path = scratch.path().join(name);
                std::fs::write(&path, AMBIGUOUS).unwrap();
                path.to_string_lossy().into_owned()
            })
            .collect();
        let job = sb_test_support::block_on(core.start_import(
            token(),
            "import-1".to_owned(),
            paths,
            ImportTarget::LocalArchive,
        ))
        .unwrap()
        .job_id;
        wait(&core, &job, |state| {
            state != sb_service::ImportJobState::Inventory
        });

        let choices = vec![
            VariantChoice {
                ordinal: 0,
                variant: MboxVariant::Mboxo,
            },
            VariantChoice {
                ordinal: 1,
                variant: MboxVariant::Mboxrd,
            },
        ];
        sb_test_support::block_on(core.confirm_import(
            token(),
            "confirm-1".to_owned(),
            job.clone(),
            choices,
        ))
        .unwrap();
        wait(&core, &job, sb_service::ImportJobState::is_finished);

        let variants: Vec<_> = sb_test_support::block_on(async {
            let guard = core.service.read().await;
            let job = guard.as_ref().unwrap().import_job(&job).await.unwrap();
            assert_eq!(job.state, sb_service::ImportJobState::Completed);
            job.sources
                .into_iter()
                .map(|source| source.variant)
                .collect()
        });
        assert_eq!(
            variants,
            vec![
                Some(sb_service::MboxVariant::Mboxo),
                Some(sb_service::MboxVariant::Mboxrd),
            ]
        );

        sb_test_support::block_on(core.shutdown());
    }
}
