//! The store's scheduled-maintenance calls through the resources
//! (spec/03 "Scheduled maintenance", plan M1.8h3b): whether a backup is
//! due, the cancellable halves of a backup and a GC pass, and each
//! job's run records. The engine's scheduler decides when they run;
//! nothing here schedules (spec/01). Each call is the store's own,
//! blocking and refused on the writer thread.

use std::sync::atomic::AtomicBool;
use std::time::Duration;

use sb_store::{
    BackupReceipt, BackupSnapshot, GcReport, MaintenanceJob, MaintenanceOutcome, MaintenanceRun,
    StoreError, Timestamp,
};

use crate::Resources;

impl Resources {
    /// Whether the store owes a backup generation: data changed since
    /// the newest complete generation's snapshot, or there is none.
    pub fn backup_due(&self) -> Result<bool, StoreError> {
        self.store.backup_due()
    }

    /// [`Resources::complete_backup`], stopping at the next object once
    /// `cancelled` is raised; the generation stays in progress for the
    /// next run's recovery.
    pub fn complete_backup_with(
        &self,
        snapshot: BackupSnapshot,
        cancelled: &AtomicBool,
    ) -> Result<BackupReceipt, StoreError> {
        self.store.complete_backup_with(snapshot, cancelled)
    }

    /// One GC pass with the grace period `grace`, stopping between its
    /// steps once `stop` is raised.
    pub fn collect_garbage_with(
        &self,
        grace: Duration,
        stop: &AtomicBool,
    ) -> Result<GcReport, StoreError> {
        self.store.collect_garbage_with(grace, stop)
    }

    /// Record `job`'s run as started now.
    pub fn start_maintenance(&self, job: MaintenanceJob) -> Result<Timestamp, StoreError> {
        self.store.start_maintenance(job)
    }

    /// Record the end of `job`'s run in flight; `Conflict` without one.
    pub fn finish_maintenance(
        &self,
        job: MaintenanceJob,
        outcome: MaintenanceOutcome,
        detail: Option<&str>,
    ) -> Result<(), StoreError> {
        self.store.finish_maintenance(job, outcome, detail)
    }

    /// Every maintenance job's latest run.
    pub fn maintenance_runs(&self) -> Result<Vec<MaintenanceRun>, StoreError> {
        self.store.maintenance_runs()
    }
}
