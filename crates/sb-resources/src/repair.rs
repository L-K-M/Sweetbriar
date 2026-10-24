//! Verified full versions and durable terminal attempts; activation follows separately.

use sb_mime::{Limits, PARSER_VERSION, ParseOutcome};
use sb_store::repair;

use crate::{
    ImportParseState, ImportSummary, Resources, StoreError, Timestamp, WriteFuture, WritePriority,
};

#[cfg(test)]
mod tests;

#[derive(Debug)]
pub struct RepairCapture {
    inner: repair::RepairCapture,
}

#[derive(Debug)]
pub struct RepairPrepared {
    inner: repair::RepairPrepared,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairTerminalKind {
    Partial,
    Failed,
    Oversized,
}

impl RepairTerminalKind {
    fn stored(self) -> repair::DiagnosisKind {
        match self {
            Self::Partial => repair::DiagnosisKind::Partial,
            Self::Failed => repair::DiagnosisKind::Failed,
            Self::Oversized => repair::DiagnosisKind::Oversized,
        }
    }

    fn from_stored(kind: repair::DiagnosisKind) -> Self {
        match kind {
            repair::DiagnosisKind::Partial => Self::Partial,
            repair::DiagnosisKind::Failed => Self::Failed,
            repair::DiagnosisKind::Oversized => Self::Oversized,
        }
    }
}

#[derive(Debug)]
pub struct RepairTerminal {
    capture: repair::RepairCapture,
    kind: RepairTerminalKind,
}

impl RepairTerminal {
    pub fn kind(&self) -> RepairTerminalKind {
        self.kind
    }
    pub fn acquisition_id(&self) -> i64 {
        self.capture.acquisition().acquisition_id()
    }
    pub fn target(&self) -> u32 {
        self.capture.target()
    }
}

#[derive(Debug)]
pub enum RepairPreparation {
    Prepared(RepairPrepared),
    Terminal(RepairTerminal),
    Current,
    Stale,
    Cancelled,
}

#[derive(Debug)]
pub enum RepairCommit {
    Continue(RepairPrepared),
    Published,
    Current,
    Recapture,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairPersist {
    Diagnosed,
    Current,
    Recapture,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RepairDiagnosis {
    pub kind: RepairTerminalKind,
    pub diagnosed_at: Timestamp,
}

#[derive(Debug)]
pub struct RepairStatus {
    pub target: u32,
    pub base_version: Option<u32>,
    pub base_state: Option<ImportParseState>,
    pub completed_at: Option<Timestamp>,
    pub pieces: u32,
    pub bytes: u64,
    pub diagnosis: Option<RepairDiagnosis>,
}

/// A conservative seek page, not a verified repair plan or sweep acknowledgement.
#[derive(Debug)]
pub struct RepairSources {
    pub acquisition_ids: Vec<i64>,
    pub through: i64,
    pub cutoff: i64,
    pub inputs: u64,
}

impl Resources {
    /// Scan at most 200 raw acquisition IDs before filtering, in one short read.
    pub fn repair_sources(
        &self,
        after: i64,
        cutoff: Option<i64>,
    ) -> Result<RepairSources, StoreError> {
        self.read(|tx| repair::sources(tx, after, cutoff, PARSER_VERSION))
            .map(|page| RepairSources {
                acquisition_ids: page.acquisition_ids,
                through: page.through,
                cutoff: page.cutoff,
                inputs: page.inputs,
            })
    }

    pub fn begin_repair(&self, acquisition: i64) -> WriteFuture<Option<RepairCapture>> {
        self.write(WritePriority::Index, move |tx| {
            repair::begin(tx, acquisition, PARSER_VERSION)
                .map(|value| value.map(|inner| RepairCapture { inner }))
        })
    }

    /// The short gate releases before verified object I/O and MIME work begin.
    pub fn prepare_repair(
        &self,
        capture: RepairCapture,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<RepairPreparation, StoreError> {
        if cancelled() {
            return Ok(RepairPreparation::Cancelled);
        }
        match self.read(|tx| repair::prepare_gate(tx, &capture.inner))? {
            repair::RepairGate::Current => return Ok(RepairPreparation::Current),
            repair::RepairGate::Stale => return Ok(RepairPreparation::Stale),
            repair::RepairGate::Open => {}
        }
        let limits = Limits::default();
        let read = self.store.read_acquisition_bounded(
            capture.inner.acquisition(),
            limits.max_message_bytes() as u64,
            cancelled,
        )?;
        if cancelled() {
            return Ok(RepairPreparation::Cancelled);
        }
        let kind = match read {
            sb_store::messages::AcquisitionRead::Cancelled => {
                return Ok(RepairPreparation::Cancelled);
            }
            sb_store::messages::AcquisitionRead::Oversized => RepairTerminalKind::Oversized,
            sb_store::messages::AcquisitionRead::Verified(bytes) => {
                let outcome = sb_mime::parse(&bytes, &limits);
                if cancelled() {
                    return Ok(RepairPreparation::Cancelled);
                }
                match &outcome {
                    ParseOutcome::Parsed(_) => {
                        return repair::prepare(capture.inner, crate::acquire::summary(&outcome))
                            .map(|inner| RepairPreparation::Prepared(RepairPrepared { inner }));
                    }
                    ParseOutcome::Partial { .. } => RepairTerminalKind::Partial,
                    ParseOutcome::Failed(_) => RepairTerminalKind::Failed,
                }
            }
        };
        Ok(RepairPreparation::Terminal(RepairTerminal {
            capture: capture.inner,
            kind,
        }))
    }

    pub fn commit_repair_step(&self, prepared: RepairPrepared) -> WriteFuture<RepairCommit> {
        let now = self.now();
        self.write(WritePriority::Index, move |tx| {
            repair::publish(tx, prepared.inner, now).map(|value| match value {
                repair::RepairCommit::Continue(inner) => {
                    RepairCommit::Continue(RepairPrepared { inner })
                }
                repair::RepairCommit::Published => RepairCommit::Published,
                repair::RepairCommit::Current => RepairCommit::Current,
                repair::RepairCommit::Recapture => RepairCommit::Recapture,
            })
        })
    }

    pub fn persist_repair_terminal(&self, attempt: RepairTerminal) -> WriteFuture<RepairPersist> {
        let now = self.now();
        self.write(WritePriority::Index, move |tx| {
            repair::persist(tx, attempt.capture, attempt.kind.stored(), now).map(
                |value| match value {
                    repair::RepairPersist::Diagnosed => RepairPersist::Diagnosed,
                    repair::RepairPersist::Current => RepairPersist::Current,
                    repair::RepairPersist::Recapture => RepairPersist::Recapture,
                },
            )
        })
    }

    pub fn repair_status(&self, acquisition: i64) -> Result<RepairStatus, StoreError> {
        let status = self.read(|tx| repair::status(tx, acquisition))?;
        let state = match status.base_state.as_deref() {
            None => None,
            Some("ok") => Some(ImportParseState::Parsed),
            Some("partial") => Some(ImportParseState::Partial),
            Some("failed") => Some(ImportParseState::Failed),
            _ => {
                return Err(StoreError::Corrupt {
                    detail: "invalid MIME repair base state".into(),
                });
            }
        };
        Ok(RepairStatus {
            target: status.target,
            base_version: status.base_version,
            base_state: state,
            completed_at: status.completed_at.map(Timestamp::from_millis),
            pieces: status.pieces,
            bytes: status.bytes,
            diagnosis: status.diagnosis.map(|value| RepairDiagnosis {
                kind: RepairTerminalKind::from_stored(value.kind),
                diagnosed_at: Timestamp::from_millis(value.diagnosed_at),
            }),
        })
    }

    pub fn repaired_summary(&self, acquisition: i64) -> Result<Option<ImportSummary>, StoreError> {
        self.read(|tx| repair::read_version(tx, acquisition, PARSER_VERSION))
    }
}
