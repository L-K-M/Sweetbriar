//! Same-snapshot coverage facade, including this build's extraction capability.

use sb_store::{StoreError, Transaction};

use crate::{Coverage, SEARCH_EXTRACTOR_VERSION, SearchIndexState, SearchScope};

/// Capture coverage in the supplied transaction; a newer target is observable.
pub fn read_search_coverage(tx: &Transaction, scope: SearchScope) -> Result<Coverage, StoreError> {
    let mut coverage = sb_store::query::read_search_coverage(tx, scope)?;
    if coverage.extractor_version > SEARCH_EXTRACTOR_VERSION {
        coverage.index_state = SearchIndexState::BlockedNewerExtractor;
    }
    Ok(coverage)
}
