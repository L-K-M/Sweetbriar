//! Backup generations and the restore swap through the resources
//! (spec/03 "Backup generations" and "Restore", plan M1.8g1). Each call
//! is the store's own, blocking and never on the writer thread; the
//! swap consumes the resources and hands back new ones around the
//! restored store, keeping the seams, the secret store and the
//! rendition cache, so the engine never holds a bare store.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use sb_store::{
    BackupDestination, BackupGeneration, BackupReceipt, BackupSnapshot, PreparedRestoreStage,
    QualifiedStage, RestoreMetadata, StartedBackup, StoreError, SwapRestoreError,
    VerifiedExternalBackup,
};

use crate::Resources;

/// Why [`Resources::swap_restore`] did not hand back restored
/// resources.
pub enum ResourcesSwapError {
    /// A check before the store closed refused the swap. Nothing moved:
    /// the resources and the qualified stage come back as they were.
    Refused {
        cause: StoreError,
        resources: Box<Resources>,
        stage: Box<QualifiedStage>,
    },
    /// The store had closed: the swap committed or stopped part way,
    /// and the next open resolves what it left (spec/03 "Restore").
    Failed(StoreError),
}

impl std::fmt::Debug for ResourcesSwapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused { cause, .. } => f.debug_struct("Refused").field("cause", cause).finish(),
            Self::Failed(cause) => f.debug_tuple("Failed").field(cause).finish(),
        }
    }
}

impl std::fmt::Display for ResourcesSwapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused { cause, .. } => write!(f, "the restore swap was refused: {cause}"),
            Self::Failed(cause) => write!(f, "the restore swap failed: {cause}"),
        }
    }
}

impl std::error::Error for ResourcesSwapError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Refused { cause, .. } | Self::Failed(cause) => Some(cause),
        }
    }
}

impl Resources {
    /// The recorded backup generations for `list_backups` (spec/07),
    /// newest first, as their rows stand: every complete and
    /// in-progress one, and the failed ones since the newest complete
    /// one. No generation's directory is opened.
    pub fn backup_generations(&self) -> Result<Vec<BackupGeneration>, StoreError> {
        self.store.backup_generations()
    }

    /// Steps 1–3 of a backup generation: its row, the snapshot and the
    /// object pins. First recovers interrupted generations and applies
    /// retention.
    pub fn backup_snapshot(
        &self,
        destination: BackupDestination,
    ) -> Result<BackupSnapshot, StoreError> {
        self.store.backup_snapshot(destination)
    }

    /// [`Resources::backup_snapshot`] as the command `idempotency_key`
    /// names: the key's receipt commits with the generation's row, and
    /// a key that started a generation before answers its receipt and
    /// does nothing else.
    pub fn start_backup(
        &self,
        destination: BackupDestination,
        idempotency_key: &str,
    ) -> Result<StartedBackup, StoreError> {
        self.store.start_backup(destination, idempotency_key)
    }

    /// Copy, verify and publish a snapshot as a complete generation.
    pub fn complete_backup(&self, snapshot: BackupSnapshot) -> Result<BackupReceipt, StoreError> {
        self.store.complete_backup(snapshot)
    }

    /// Verify the generation `generation` under `parent` for a restore.
    /// `None` means cancelled.
    pub fn verify_external_backup(
        &self,
        parent: &Path,
        generation: &str,
        cancelled: &AtomicBool,
    ) -> Result<Option<VerifiedExternalBackup>, StoreError> {
        self.store
            .verify_external_backup(parent, generation, cancelled)
    }

    /// Copy a verified generation into this store's private stage.
    pub fn stage_restore(
        &self,
        selection: VerifiedExternalBackup,
        source_location: &Path,
        metadata: RestoreMetadata,
        cancelled: &AtomicBool,
    ) -> Result<PreparedRestoreStage, StoreError> {
        self.store
            .stage_restore(selection, source_location, metadata, cancelled)
    }

    /// Quarantine and qualify a prepared stage for the swap.
    pub fn qualify_restore(
        &self,
        prepared: PreparedRestoreStage,
        cancelled: &Arc<AtomicBool>,
    ) -> Result<QualifiedStage, StoreError> {
        self.store.qualify_restore(prepared, cancelled)
    }

    /// Swap a qualified stage in as this store and reopen it, the lock
    /// held throughout (spec/03 "Restore" step 4). The caller has
    /// quiesced every user of these resources (plan M1.8g): nothing else
    /// may hold them. A refusal hands both back unchanged.
    pub fn swap_restore(
        self,
        stage: QualifiedStage,
        cancelled: &AtomicBool,
    ) -> Result<Resources, ResourcesSwapError> {
        let Self {
            store,
            renditions,
            fs,
            clock,
            random,
            secrets,
        } = self;
        let rebuild = |store| Resources {
            store,
            renditions,
            fs,
            clock,
            random,
            secrets,
        };
        match store.swap_restore(stage, cancelled) {
            Ok(store) => Ok(rebuild(store)),
            Err(SwapRestoreError::Refused {
                cause,
                store,
                stage,
            }) => Err(ResourcesSwapError::Refused {
                cause,
                resources: Box::new(rebuild(*store)),
                stage,
            }),
            Err(SwapRestoreError::Failed(cause)) => Err(ResourcesSwapError::Failed(cause)),
        }
    }
}
