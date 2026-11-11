//! A restore through the resources (spec/03 "Restore", plan M1.8g1):
//! back up, stage, qualify and swap, then the restored store under the
//! same resources; a refused swap hands back resources that still read
//! and write and a stage that swaps on retry; a failure after closing
//! leaves a state the next open resolves. A store that latched
//! read-only restores the same way (plan M1.8h5b4).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use sb_resources::{
    BackupDestination, BackupReceipt, IntegrityCheck, QualifiedStage, Resources,
    ResourcesSwapError, RestoreMetadata, StoreError, WritePriority,
};
use sb_test_support::{ScratchDir, block_on, damage_an_index};

const KEPT: &str = "00000000-0000-4000-8000-00000000a001";
const LATER: &str = "00000000-0000-4000-8000-00000000a002";

fn data_root(scratch: &ScratchDir) -> PathBuf {
    std::fs::canonicalize(scratch.path()).unwrap().join("store")
}

/// One active IMAP account row.
fn add_account(resources: &Resources, id: &'static str) {
    block_on(resources.write(WritePriority::User, move |tx| {
        tx.execute(
            "INSERT INTO accounts
             (account_id, kind, provider, display_name, state, sort_order, created_at)
             VALUES (?1, 'imap', 'generic_imap', 'Fixture', 'active', 1, 0)",
            [id],
        )
        .map(|_| ())
        .map_err(|e| StoreError::internal(e.to_string()))
    }))
    .unwrap();
}

/// Every account's id and state, in id order.
fn accounts(resources: &Resources) -> Vec<(String, String)> {
    resources
        .read(|tx| {
            let mut statement = tx
                .prepare(
                    "SELECT account_id, state FROM accounts
                     WHERE kind = 'imap' ORDER BY account_id",
                )
                .map_err(|e| StoreError::internal(e.to_string()))?;
            let rows = statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .map_err(|e| StoreError::internal(e.to_string()))?;
            rows.collect::<Result<_, _>>()
                .map_err(|e| StoreError::internal(e.to_string()))
        })
        .unwrap()
}

fn incarnation(resources: &Resources) -> String {
    resources
        .read(|tx| {
            tx.query_row(
                "SELECT value FROM store_meta WHERE key = 'incarnation'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| StoreError::internal(e.to_string()))
        })
        .unwrap()
}

/// The generation's restore metadata, as its completed row records it.
fn metadata(resources: &Resources, generation: &str) -> RestoreMetadata {
    let generation = generation.to_owned();
    resources
        .read(move |tx| {
            tx.query_row(
                "SELECT manifest_sha256, object_count, total_bytes, completed_at
                 FROM backup_generations WHERE generation_id = ?1",
                [&generation],
                |row| {
                    let hex: String = row.get(0)?;
                    let mut manifest_sha256 = [0u8; 32];
                    for (byte, pair) in manifest_sha256.iter_mut().zip(hex.as_bytes().chunks(2)) {
                        *byte = u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap();
                    }
                    Ok(RestoreMetadata {
                        manifest_sha256,
                        object_count: row.get(1)?,
                        total_bytes: row.get(2)?,
                        completed_at: row.get(3)?,
                    })
                },
            )
            .map_err(|e| StoreError::internal(e.to_string()))
        })
        .unwrap()
}

/// A store with one account, backed up inside its data root; then a
/// second account the generation does not hold.
fn backed_up(scratch: &ScratchDir) -> (Resources, BackupReceipt) {
    let resources = Resources::open(&data_root(scratch)).unwrap();
    add_account(&resources, KEPT);
    let snapshot = resources
        .backup_snapshot(BackupDestination::DataRoot)
        .unwrap();
    let receipt = resources.complete_backup(snapshot).unwrap();
    add_account(&resources, LATER);
    (resources, receipt)
}

fn qualified(resources: &Resources, receipt: &BackupReceipt) -> QualifiedStage {
    let cancel = AtomicBool::new(false);
    let location = receipt.location();
    let selection = resources
        .verify_external_backup(location.parent().unwrap(), receipt.generation_id(), &cancel)
        .unwrap()
        .expect("not cancelled");
    let prepared = resources
        .stage_restore(
            selection,
            location,
            metadata(resources, receipt.generation_id()),
            &cancel,
        )
        .unwrap();
    resources
        .qualify_restore(prepared, &Arc::new(AtomicBool::new(false)))
        .unwrap()
}

fn sibling(root: &Path, prefix: &str) -> Option<PathBuf> {
    std::fs::read_dir(root.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with(prefix)
        })
}

#[test]
fn a_restore_swaps_the_generation_in_under_the_same_resources() {
    let scratch = ScratchDir::new("resources-restore");
    let root = data_root(&scratch);
    let (resources, receipt) = backed_up(&scratch);
    let old = incarnation(&resources);
    let stage = qualified(&resources, &receipt);

    let restored = resources
        .swap_restore(stage, &AtomicBool::new(false))
        .unwrap();

    // The generation's rows, every account paused, a new incarnation.
    assert_eq!(
        accounts(&restored),
        [(KEPT.to_owned(), "paused".to_owned())]
    );
    assert_ne!(incarnation(&restored), old);
    // Still writable through the same resources.
    add_account(&restored, LATER);
    assert_eq!(accounts(&restored).len(), 2);
    restored.close().unwrap();

    // The old root is preserved beside it, checkpointed before it moved:
    // no write-ahead log was left behind (spec/03 "quiesce"). The next
    // open is the restored store.
    let preserved = sibling(&root, "store.preserved-").expect("the old root is preserved");
    assert!(preserved.join("state.sqlite").is_file());
    assert!(!preserved.join("state.sqlite-wal").exists());
    assert!(sibling(&root, "store.restoring-").is_none());
    let reopened = Resources::open(&root).unwrap();
    assert_eq!(
        accounts(&reopened)[0],
        (KEPT.to_owned(), "paused".to_owned())
    );
    reopened.close().unwrap();
}

#[test]
fn a_refused_swap_hands_back_working_resources_and_a_stage_that_swaps_on_retry() {
    let scratch = ScratchDir::new("resources-restore-refused");
    let (resources, receipt) = backed_up(&scratch);
    let stage = qualified(&resources, &receipt);

    let Err(ResourcesSwapError::Refused {
        cause,
        resources,
        stage,
    }) = resources.swap_restore(stage, &AtomicBool::new(true))
    else {
        panic!("a cancelled swap is refused");
    };
    assert!(matches!(cause, StoreError::Unavailable { .. }), "{cause:?}");
    // Nothing moved: the live store reads and writes as before.
    assert_eq!(accounts(&resources).len(), 2);
    add_account(&resources, "00000000-0000-4000-8000-00000000a003");
    assert_eq!(accounts(&resources).len(), 3);

    let restored = resources
        .swap_restore(*stage, &AtomicBool::new(false))
        .unwrap();
    assert_eq!(
        accounts(&restored),
        [(KEPT.to_owned(), "paused".to_owned())]
    );
    restored.close().unwrap();
}

#[test]
fn a_failure_after_closing_is_failed_and_the_next_open_resolves_it() {
    let scratch = ScratchDir::new("resources-restore-failed");
    let root = data_root(&scratch);
    let (resources, receipt) = backed_up(&scratch);
    let old = incarnation(&resources);
    let stage = qualified(&resources, &receipt);
    // A directory under the marker's temporary name: writing the marker
    // fails once the store has closed.
    let temporary = root.with_extension("restore-pending.tmp");
    std::fs::create_dir(&temporary).unwrap();

    let result = resources.swap_restore(stage, &AtomicBool::new(false));

    assert!(
        matches!(result, Err(ResourcesSwapError::Failed(_))),
        "{result:?}"
    );
    std::fs::remove_dir(&temporary).unwrap();
    let reopened = Resources::open(&root).unwrap();
    assert_eq!(incarnation(&reopened), old);
    assert_eq!(accounts(&reopened).len(), 2);
    reopened.close().unwrap();
}

/// Every directory under `root`, and every file but SQLite's volatile
/// `-shm` with its bytes.
fn tree(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    let mut found = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let relative = path.strip_prefix(root).unwrap().to_path_buf();
            if path.is_dir() {
                pending.push(path);
                found.insert(relative, None);
            } else if !relative.ends_with("state.sqlite-shm") {
                found.insert(relative, Some(std::fs::read(&path).unwrap()));
            }
        }
    }
    found
}

/// The backed-up store, damaged where only the full check looks and
/// latched by it; reopened latched when `reopened`.
fn latched(scratch: &ScratchDir, reopened: bool) -> (Resources, BackupReceipt) {
    let root = data_root(scratch);
    let (resources, receipt) = backed_up(scratch);
    resources.close().unwrap();
    damage_an_index(&root.join("state.sqlite"));
    let resources = Resources::open(&root).unwrap();
    let found = resources.check_integrity(&AtomicBool::new(false)).unwrap();
    assert!(matches!(found, IntegrityCheck::Finding(_)), "{found:?}");
    if !reopened {
        return (resources, receipt);
    }
    resources.close().unwrap();
    let resources = Resources::open(&root).unwrap();
    assert!(resources.integrity_latch().is_some());
    (resources, receipt)
}

/// A latched store verifies, stages and qualifies a generation without
/// changing a file in its root, then swaps it in: the restored store
/// is sound and writable, and the damaged root is preserved with its
/// latch. The same from the finding on and after a latched reopen.
#[test]
fn a_latched_store_restores_and_leaves_its_root_as_it_was() {
    for reopened in [false, true] {
        let scratch = ScratchDir::new("resources-restore-latched");
        let root = data_root(&scratch);
        let (resources, receipt) = latched(&scratch, reopened);
        let old = incarnation(&resources);
        let damaged = tree(&root);

        let stage = qualified(&resources, &receipt);

        assert_eq!(tree(&root), damaged, "the restore's preparation wrote");
        // The verification worked beside the root and cleaned up.
        let beside = sibling(&root, "store.verifier").expect("the verifier's sibling");
        let left: Vec<_> = std::fs::read_dir(&beside)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .filter(|name| !name.starts_with("ledger.sqlite"))
            .collect();
        assert_eq!(left, Vec::<String>::new());

        let restored = resources
            .swap_restore(stage, &AtomicBool::new(false))
            .unwrap();

        assert!(restored.integrity_latch().is_none());
        assert_ne!(incarnation(&restored), old);
        assert_eq!(
            accounts(&restored),
            [(KEPT.to_owned(), "paused".to_owned())]
        );
        add_account(&restored, LATER);
        restored.close().unwrap();
        // The damaged root went aside whole, its latch with it.
        let preserved = sibling(&root, "store.preserved-").expect("the old root is preserved");
        assert_eq!(tree(&preserved), damaged);
        assert!(preserved.join("integrity-latch.json").is_file());
        // The next open is the restored store, with no latch.
        let reopened = Resources::open(&root).unwrap();
        assert!(reopened.integrity_latch().is_none());
        assert_eq!(accounts(&reopened).len(), 2);
        reopened.close().unwrap();
    }
}
