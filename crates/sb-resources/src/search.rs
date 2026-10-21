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

#[cfg(test)]
mod tests {
    use crate::{Resources, UserPause, WritePriority, set_user_pause};
    use sb_test_support::{ScratchDir, block_on};
    use std::sync::{Arc, Mutex};

    #[test]
    fn resource_observer_forwards_committed_search_changes_without_list_events() {
        let scratch = ScratchDir::new("resource-search-observer");
        let resources = Resources::open(scratch.path()).unwrap();
        let before = resources
            .read(|tx| sb_store::index::search_generation(tx))
            .unwrap() as u64;
        let searches = Arc::new(Mutex::new(Vec::new()));
        let lists = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&searches);
        resources
            .observe_search_generation(Arc::new(move |value| sink.lock().unwrap().push(value)));
        let sink = Arc::clone(&lists);
        resources.observe_list_generation(Arc::new(move |value| sink.lock().unwrap().push(value)));
        block_on(resources.write(WritePriority::User, |tx| {
            set_user_pause(tx, UserPause::Pause)
        }))
        .unwrap();
        assert_eq!(*searches.lock().unwrap(), [before + 1]);
        assert!(lists.lock().unwrap().is_empty());
        resources.close().unwrap();
    }
}
