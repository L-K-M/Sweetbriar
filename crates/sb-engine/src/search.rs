//! Checked query conversions and standalone Search coverage, without window state.

use std::sync::Arc;

use sb_resources::{ParseContext, Query, SearchScope as StoreScope, read_search_coverage};

use crate::{Coverage, Engine, EngineError, Timestamp};

/// An outer coverage scope; account strings are validated before store access.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchScope {
    All,
    Account(String),
    Mailbox(i64),
}

impl SearchScope {
    pub(crate) fn stored(&self) -> Result<StoreScope, EngineError> {
        match self {
            Self::All => Ok(StoreScope::All),
            Self::Account(id) => id
                .parse()
                .map(StoreScope::Account)
                .map_err(|_| EngineError::invalid_input("scope", "an account scope needs a UUID")),
            Self::Mailbox(id) if *id > 0 => Ok(StoreScope::Mailbox(*id)),
            Self::Mailbox(_) => Err(EngineError::invalid_input(
                "scope",
                "a mailbox scope needs a positive id",
            )),
        }
    }
}

impl Engine {
    /// Convert text to canonical v1 JSON off the caller's thread, with explicit defaults.
    pub async fn parse_search(
        &self,
        text: String,
        tz: String,
        clock: Timestamp,
    ) -> Result<String, EngineError> {
        self.compute(move || {
            let context = ParseContext::new(&tz, clock);
            Ok(Query::parse_text(&text, &context)?
                .canonical_json()
                .to_owned())
        })
        .await
    }

    /// Validate JSON before formatting; typed format limits carry no source span.
    pub async fn format_search(&self, query_json: String) -> Result<String, EngineError> {
        self.compute(move || Ok(Query::parse_json(&query_json)?.canonical_text()?))
            .await
    }

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
