//! The flag commands' refusals cross the FFI boundary on their fields;
//! the Proton path is the service's and the engine's to test.

use super::*;
use crate::{CoreConfig, open_core};
use sb_test_support::{ScratchDir, block_on};

fn field(error: CoreError) -> String {
    match error {
        CoreError::InvalidInput { field, .. } => field,
        other => panic!("expected InvalidInput, got {other:?}"),
    }
}

#[test]
fn flag_refusals_name_their_field() {
    let scratch = ScratchDir::new("ffi-flags");
    let core = block_on(open_core(
        CancelToken::new(),
        CoreConfig {
            data_root: scratch.path().join("store").to_string_lossy().into_owned(),
            cache_root: scratch.path().join("cache").to_string_lossy().into_owned(),
        },
    ))
    .unwrap();
    let set = |key: &str, ids: Vec<i64>, add: &[&str], remove: &[&str]| {
        block_on(core.set_flags(
            CancelToken::new(),
            key.to_owned(),
            MessageSelection::OccurrenceIds(ids),
            add.iter().map(|name| (*name).to_owned()).collect(),
            remove.iter().map(|name| (*name).to_owned()).collect(),
        ))
    };

    let unknown = set(
        "30000000-0000-4000-8000-000000000001",
        vec![1],
        &["\\Draft"],
        &[],
    );
    assert_eq!(field(unknown.unwrap_err()), "add");
    let removed = set(
        "30000000-0000-4000-8000-000000000004",
        vec![1],
        &[],
        &["\\Draft"],
    );
    assert_eq!(field(removed.unwrap_err()), "remove");
    let empty = set(
        "30000000-0000-4000-8000-000000000002",
        vec![],
        &["\\Seen"],
        &[],
    );
    assert_eq!(field(empty.unwrap_err()), "selection");
    let undo = block_on(core.undo(
        CancelToken::new(),
        "30000000-0000-4000-8000-000000000003".to_owned(),
        "{}".to_owned(),
    ));
    assert_eq!(field(undo.unwrap_err()), "token");
    block_on(core.shutdown());
}
