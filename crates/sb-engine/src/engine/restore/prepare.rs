//! Preparing a restore (spec/03 "Restore" steps 1 to 3, plan M1.8i4):
//! a recorded generation is verified against its manifest, copied
//! beside the data root and qualified, all while the store stays in
//! use. What comes out is what [`Engine::restore`] swaps in.
//!
//! ```text
//!   the row        complete, with its manifest digest and counts
//!   verify         the generation's directory against its manifest
//!   stage          a private copy beside the data root, re-verified
//!   qualify        the copy's database quarantined, bindings checked
//! ```
//!
//! Nothing here stops the engine or changes the store's data. It works
//! on a read-only store too: there a restore is the way out (spec/03
//! "Integrity").

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use sb_resources::{
    BackupGeneration, BackupGenerationState, QualifiedStage, Resources, RestoreMetadata, StoreError,
};

use crate::{Engine, EngineError};

/// A generation ready to be swapped in: verified, staged and qualified
/// at one point in time. The swap checks the stage's bytes and names
/// again.
///
/// It holds the store's backup operation until it is swapped in or
/// dropped: meanwhile no backup, generation check or other restore
/// starts, and the hourly backup records `blocked`. Dropping it
/// deletes nothing. The stage stays beside the data root, and is never
/// used again (spec/03).
#[derive(Debug)]
pub struct PreparedRestore {
    generation_id: String,
    stage: QualifiedStage,
}

impl PreparedRestore {
    /// The generation this was prepared from.
    pub fn generation_id(&self) -> &str {
        &self.generation_id
    }

    /// The qualified stage, for the swap.
    pub fn into_stage(self) -> QualifiedStage {
        self.stage
    }
}

/// What staging checks the generation's manifest against, from its
/// row. A complete row always has all four; one without is damage.
fn metadata(generation: &BackupGeneration) -> Result<RestoreMetadata, StoreError> {
    let id = &generation.generation_id;
    let incomplete = || StoreError::Corrupt {
        detail: format!("backup_generations row {id:?} is complete without its manifest facts"),
    };
    let digest = generation
        .manifest_sha256
        .as_deref()
        .ok_or_else(incomplete)?;
    let mut manifest_sha256 = [0u8; 32];
    let hex = digest.as_bytes();
    if hex.len() != 64 {
        return Err(incomplete());
    }
    for (byte, pair) in manifest_sha256.iter_mut().zip(hex.chunks(2)) {
        let pair = std::str::from_utf8(pair).map_err(|_| incomplete())?;
        *byte = u8::from_str_radix(pair, 16).map_err(|_| incomplete())?;
    }
    let count = |value: Option<u64>| {
        value
            .and_then(|value| i64::try_from(value).ok())
            .ok_or_else(incomplete)
    };
    Ok(RestoreMetadata {
        manifest_sha256,
        object_count: count(generation.object_count)?,
        total_bytes: count(generation.total_bytes)?,
        completed_at: generation.completed_at.ok_or_else(incomplete)?.as_millis(),
    })
}

/// Steps 1 to 3 for the generation `generation_id` names. `None` means
/// `cancelled` ended it; whatever it had staged stays as evidence.
fn prepare(
    resources: &Resources,
    generation_id: &str,
    cancelled: &Arc<AtomicBool>,
) -> Result<Option<QualifiedStage>, StoreError> {
    let Some(generation) = resources.backup_generation(generation_id)? else {
        return Err(StoreError::NotFound {
            what: format!("backup generation {generation_id}"),
        });
    };
    let state = match generation.state {
        BackupGenerationState::Complete => None,
        BackupGenerationState::InProgress => Some("in progress"),
        BackupGenerationState::Failed => Some("failed"),
    };
    if let Some(state) = state {
        return Err(StoreError::Conflict {
            detail: format!(
                "backup generation {generation_id} is {state}; only a complete one can be restored"
            ),
        });
    }
    let metadata = metadata(&generation)?;
    let Some(parent) = generation.location.parent() else {
        return Err(StoreError::Corrupt {
            detail: format!("backup_generations row {generation_id:?} has no directory to look in"),
        });
    };

    // What a step the flag ended answers. An I/O error under a raised
    // flag is still that error.
    let stopped = |error: StoreError| match error {
        StoreError::Unavailable { io_kind: None, .. } if cancelled.load(Ordering::SeqCst) => {
            Ok(None)
        }
        other => Err(other),
    };
    let Some(selection) = resources.verify_external_backup(parent, generation_id, cancelled)?
    else {
        return Ok(None);
    };
    let prepared =
        match resources.stage_restore(selection, &generation.location, metadata, cancelled) {
            Ok(prepared) => prepared,
            Err(error) => return stopped(error),
        };
    match resources.qualify_restore(prepared, cancelled) {
        Ok(stage) => Ok(Some(stage)),
        Err(error) => stopped(error),
    }
}

impl Engine {
    /// Prepare a restore of the recorded generation `generation_id`
    /// (spec/03 "Restore" steps 1 to 3): verify it where its row says
    /// it is, copy it beside the data root and qualify the copy. The
    /// store stays in use throughout, and its data doesn't change.
    ///
    /// `Ok(None)` means `cancelled` ended it. `NotFound` for an id no
    /// row has; `Conflict` for a generation that isn't complete, and
    /// while a backup, a generation check or another restore holds the
    /// backup operation; `Unavailable` for a generation whose
    /// directory can't be reached (a volume that isn't mounted);
    /// `Corrupt` for one that no longer matches its manifest or its
    /// row. A cancelled or failed preparation leaves what it staged
    /// beside the data root: a restore deletes nothing.
    ///
    /// It runs as counted work on the engine's blocking pool, and can
    /// take as long as copying the archive. The work doesn't stop when
    /// the future is dropped: raise `cancelled` and keep awaiting. It
    /// works on a read-only engine.
    pub async fn prepare_restore(
        &self,
        generation_id: &str,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Option<PreparedRestore>, EngineError> {
        let id = generation_id.to_owned();
        self.command(move |worker| async move {
            let generation_id = id.clone();
            let stage = worker
                .blocking(move |resources| prepare(resources, &id, &cancelled))
                .await??;
            Ok(stage.map(|stage| PreparedRestore {
                generation_id,
                stage,
            }))
        })
        .await
    }
}
