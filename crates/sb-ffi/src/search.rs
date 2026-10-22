//! Checked Search text/JSON queries across FFI (spec/06, spec/07).

use std::sync::Arc;

use crate::{CancelToken, Core, CoreError};

/// Required defaults, consumed only by date values that omit context.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ParseContext {
    pub tz: String,
    /// UTC milliseconds since the Unix epoch.
    pub clock: i64,
}

impl ParseContext {
    fn into_service(self) -> sb_service::ParseContext {
        sb_service::ParseContext {
            tz: self.tz,
            clock: self.clock,
        }
    }
}

#[uniffi::export]
impl Core {
    /// Parse text to canonical v1 JSON, preserving typed UTF8 error spans.
    pub async fn parse_search(
        &self,
        token: Arc<CancelToken>,
        text: String,
        context: ParseContext,
    ) -> Result<String, CoreError> {
        self.query(&token, async |core| {
            core.parse_search(text, context.into_service()).await
        })
        .await
    }

    /// Validate JSON then format canonical text with explicit date context.
    pub async fn format_search(
        &self,
        token: Arc<CancelToken>,
        query_json: String,
    ) -> Result<String, CoreError> {
        self.query(&token, async |core| core.format_search(query_json).await)
            .await
    }
}

#[cfg(test)]
mod tests;
