//! Backup generations through the engine (spec/07 "Commands", Backup;
//! plan M1.8i). The store takes, retains, checks and restores them
//! (spec/03 "Backup generations", "Restore"); the scheduler takes one
//! an hour while there were changes ([`crate::maintenance`]). This is
//! what a shell asks: which generations there are.

use sb_resources::BackupGeneration;

use crate::{Engine, EngineError};

impl Engine {
    /// `list_backups()` (spec/07): the recorded generations, newest
    /// first. Every complete and in-progress one is listed, and the
    /// failed ones started since the newest complete one, at most
    /// [`sb_resources::FAILED_GENERATIONS_LISTED`]: a failed row stays
    /// as evidence for good, and only the recent ones say anything
    /// about the backups now.
    ///
    /// The answer is the rows as stored. No generation's directory is
    /// opened: whether its bytes are still there is the weekly check's
    /// verdict on the row, and a restore's own verification. A
    /// read-only engine answers like any other.
    pub async fn list_backups(&self) -> Result<Vec<BackupGeneration>, EngineError> {
        self.read_through(|resources| resources.backup_generations())
            .await
    }
}

#[cfg(test)]
mod tests;
