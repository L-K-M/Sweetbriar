//! Standalone coverage through the counted read boundary, without window state.

use std::sync::Arc;

use sb_resources::{SearchScope as StoreScope, read_search_coverage};

use crate::{Coverage, Engine, EngineError};

/// An outer coverage scope; account strings are validated before store access.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchScope {
    All,
    Account(String),
    Mailbox(i64),
}

impl SearchScope {
    fn stored(self) -> Result<StoreScope, EngineError> {
        match self {
            Self::All => Ok(StoreScope::All),
            Self::Account(id) => id
                .parse()
                .map(StoreScope::Account)
                .map_err(|_| EngineError::invalid_input("scope", "an account scope needs a UUID")),
            Self::Mailbox(id) if id > 0 => Ok(StoreScope::Mailbox(id)),
            Self::Mailbox(_) => Err(EngineError::invalid_input(
                "scope",
                "a mailbox scope needs a positive id",
            )),
        }
    }
}

impl Engine {
    /// Query-independent scoped facts and global worker state in one snapshot.
    pub async fn coverage(&self, scope: SearchScope) -> Result<Coverage, EngineError> {
        let scope = scope.stored()?;
        let index = Arc::clone(self.indexer());
        self.read_through(move |resources| {
            index.readable()?;
            let result = resources.read(|tx| read_search_coverage(tx, scope));
            index.readable()?;
            result
        })
        .await
        .map_err(|error| match error {
            EngineError::InvalidInput { message, .. } => {
                EngineError::invalid_input("scope", message)
            }
            error => error,
        })
    }
}
