//! Checked query conversions, standalone Search coverage and the per-generation
//! Search memo, without window state.

use std::sync::{Arc, Mutex, MutexGuard};

use sb_resources::{
    CoverageCounts, ParseContext, Query, SearchScope as StoreScope, StoreError, Transaction,
    UnknownCount, read_search_coverage_reusing,
};

use crate::{Coverage, Engine, EngineError, Timestamp};

/// Scopes, or queries per scope, the memo keeps at once; the oldest goes first.
const MEMO_ENTRIES: usize = 16;

/// Coverage's scope counts per scope and exact unknown counts per query and
/// scope, each reused only at the search generation it was read at (spec/07:
/// the generation advances with every input either depends on). Failures are
/// never kept. Coverage's bindings check and global state are read every time.
///
/// ```text
///   read at generation g ─► counts(scope) kept at g? ─ yes: reuse the counts
///                                                    └ no:  scan, keep the latest
/// ```
#[derive(Default)]
pub(crate) struct SearchMemo {
    coverage: Mutex<Vec<CoverageCounts>>,
    unknown: Mutex<Vec<KeptUnknown>>,
    #[cfg(test)]
    coverage_scans: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    unknown_reuses: std::sync::atomic::AtomicUsize,
}

struct KeptUnknown {
    query: String,
    scope: StoreScope,
    generation: u64,
    excluded_unknown: u64,
}

impl SearchMemo {
    /// Coverage at the snapshot's generation, scanning the scope once per
    /// generation.
    pub(crate) fn coverage(
        &self,
        tx: &Transaction,
        scope: StoreScope,
    ) -> Result<Coverage, StoreError> {
        let kept = lock(&self.coverage)
            .iter()
            .find(|kept| kept.scope() == &scope)
            .cloned();
        let (coverage, counts) = read_search_coverage_reusing(tx, scope, kept.as_ref())?;
        if kept.as_ref() == Some(&counts) {
            return Ok(coverage);
        }

        #[cfg(test)]
        self.coverage_scans
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut kept = lock(&self.coverage);
        if kept.iter().any(|other| {
            other.scope() == counts.scope() && other.generation() > counts.generation()
        }) {
            return Ok(coverage);
        }
        kept.retain(|other| other.scope() != counts.scope());
        kept.push(counts);
        if kept.len() > MEMO_ENTRIES {
            kept.remove(0);
        }
        Ok(coverage)
    }

    /// The count kept for this query and scope; a page reuses it only at its
    /// own generation.
    pub(crate) fn unknown(&self, query: &str, scope: &StoreScope) -> UnknownCount {
        lock(&self.unknown)
            .iter()
            .find(|kept| kept.query == query && kept.scope == *scope)
            .map_or(UnknownCount::Count, |kept| UnknownCount::Reuse {
                generation: kept.generation,
                excluded_unknown: kept.excluded_unknown,
            })
    }

    pub(crate) fn keep_unknown(
        &self,
        query: &str,
        scope: StoreScope,
        generation: u64,
        excluded_unknown: u64,
    ) {
        let mut kept = lock(&self.unknown);
        if kept.iter().any(|other| {
            other.query == query && other.scope == scope && other.generation > generation
        }) {
            return;
        }
        kept.retain(|other| other.query != query || other.scope != scope);
        kept.push(KeptUnknown {
            query: query.into(),
            scope,
            generation,
            excluded_unknown,
        });
        if kept.len() > MEMO_ENTRIES {
            kept.remove(0);
        }
    }

    /// How often coverage scanned its scope rather than reusing counts.
    #[cfg(test)]
    pub(crate) fn coverage_scans(&self) -> usize {
        self.coverage_scans
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Note a page that used a kept unknown count.
    #[cfg(test)]
    pub(crate) fn reused_unknown(&self) {
        self.unknown_reuses
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    /// How many pages used a kept unknown count.
    #[cfg(test)]
    pub(crate) fn unknown_reuses(&self) -> usize {
        self.unknown_reuses
            .load(std::sync::atomic::Ordering::SeqCst)
    }
}

fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value
        .lock()
        .expect("a panicking thread held the Search memo")
}

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
        let memo = Arc::clone(self.search_memo());
        self.read_through(move |resources| {
            index.readable()?;
            let result = resources.read(|tx| memo.coverage(tx, scope));
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
