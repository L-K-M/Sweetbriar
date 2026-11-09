//! M1.8g's acceptance (plan M1.8g4): a write in progress and a running
//! engine job both stop before the swap, the write committed to the
//! replaced store, the job left to resume there; and once the restored
//! store is in place no file of the replaced root stays open.

use sb_resources::Resources;

use super::*;
use crate::drafts::{DraftWriteHook, DraftWritePoint};
use crate::{DraftSource, MboxVariant};

/// The preserved root a swap renamed the old data root to.
fn preserved_root(root: &Path) -> PathBuf {
    std::fs::read_dir(root.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.is_dir()
                && path
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .starts_with("store.preserved-")
        })
        .expect("the old root is preserved")
}

/// Count rows in the preserved store, opened on its own.
fn preserved_count(preserved: &Path, sql: &'static str) -> i64 {
    let resources = Resources::open(preserved).unwrap();
    let count = resources
        .read(|tx| tx.query_row(sql, [], |row| row.get(0)).map_err(sql_error))
        .unwrap();
    resources.close().unwrap();
    count
}

/// Every file this process has open, as `lsof` names it.
fn open_files() -> Vec<PathBuf> {
    let output = std::process::Command::new("lsof")
        .args(["-p", &std::process::id().to_string(), "-Fn"])
        .output()
        .expect("lsof runs");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.strip_prefix('n'))
        .map(PathBuf::from)
        .collect()
}

/// A draft write held inside its transaction when the restore begins:
/// the quiesce waits for it, it commits to the replaced store, and the
/// swap happens only after. No file of the replaced root stays open.
#[test]
fn a_write_in_progress_commits_to_the_replaced_store_before_the_swap() {
    let scratch = ScratchDir::new("engine-restore-pending-write");
    let root = data_root(&scratch);
    let (engine, receipt) = backed_up(&root);
    let stage = qualified(&engine, &receipt);
    let (inside, entered) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    let released = Mutex::new(released);
    let hook: DraftWriteHook = Arc::new(move |point| {
        if point == DraftWritePoint::InsideBeforeCommit {
            inside.send(()).unwrap();
            let _ = released.lock().unwrap().recv();
        }
    });
    *engine.drafts_hook.lock().unwrap() = Some(hook);
    // Accept the command, then stop waiting for it: an accepted command
    // runs to completion without its caller (spec/07).
    {
        let key = engine.new_idempotency_key();
        let mut created = pin!(engine.create_draft(&key, DraftSource::New));
        let mut cx = Context::from_waker(Waker::noop());
        assert!(created.as_mut().poll(&mut cx).is_pending());
    }
    entered.recv_timeout(PATIENCE).unwrap();

    let restoring = std::thread::spawn(move || {
        block_on(engine.restore(stage, Arc::new(AtomicBool::new(false))))
    });
    // The quiesce waits for the write: the swap cannot happen yet.
    std::thread::sleep(Duration::from_millis(300));
    assert!(!restoring.is_finished(), "the swap waited for the write");
    assert!(!root.with_extension("restore-pending").exists());
    release.send(()).unwrap();
    let engine = restoring.join().unwrap().expect("the restore succeeds");

    // The write landed in the replaced store, which the restore
    // preserved; the restored store is the generation, without it.
    let preserved = preserved_root(&root);
    assert!(
        open_files()
            .iter()
            .all(|file| !file.starts_with(&preserved)),
        "no file of the replaced root stays open"
    );
    let drafts: i64 = engine
        .resources()
        .read(|tx| {
            tx.query_row("SELECT count(*) FROM drafts", [], |row| row.get(0))
                .map_err(sql_error)
        })
        .unwrap();
    assert_eq!(drafts, 0);
    block_on(engine.shutdown()).unwrap();
    assert_eq!(
        preserved_count(&preserved, "SELECT count(*) FROM drafts"),
        1
    );
}

/// An import's ingest running when the restore begins stops at its next
/// safe point; the swap follows, and the replaced store keeps the job
/// running from its checkpoint, never failed.
#[test]
fn a_running_job_stops_before_the_swap_and_keeps_its_checkpoint() {
    let scratch = ScratchDir::new("engine-restore-running-job");
    let root = data_root(&scratch);
    let (engine, receipt) = backed_up(&root);
    let stage = qualified(&engine, &receipt);
    let path = scratch.path().join("One.mbox");
    std::fs::write(
        &path,
        "From sender@example.test Sat Oct 17 12:00:00 2026\nSubject: one\n\nbody\n",
    )
    .unwrap();
    let started = block_on(engine.start_import(&engine.new_idempotency_key(), vec![path])).unwrap();
    let deadline = Instant::now() + PATIENCE;
    while block_on(engine.import_job(started.job_id()))
        .unwrap()
        .state()
        != ImportJobState::AwaitingConfirmation
    {
        assert!(Instant::now() < deadline, "the inventory never finished");
        std::thread::sleep(Duration::from_millis(20));
    }
    // The ingest worker spins at its entry until told to stop.
    let (entered, reached) = mpsc::channel();
    let entered = Mutex::new(entered);
    *engine
        .imports()
        .before_task
        .lock()
        .expect("a panicking thread held the import job registry") =
        Some(Arc::new(move |stopping| {
            entered.lock().unwrap().send(()).unwrap();
            while !stopping() {
                std::thread::yield_now();
            }
        }));
    block_on(engine.confirm_import(
        &engine.new_idempotency_key(),
        started.job_id(),
        Vec::<(u64, MboxVariant)>::new(),
    ))
    .unwrap();
    reached.recv_timeout(PATIENCE).unwrap();

    let engine = finish(engine.restore(stage, Arc::new(AtomicBool::new(false))))
        .expect("the restore succeeds");

    let preserved = preserved_root(&root);
    assert!(
        open_files()
            .iter()
            .all(|file| !file.starts_with(&preserved)),
        "no file of the replaced root stays open"
    );
    block_on(engine.shutdown()).unwrap();
    assert_eq!(
        preserved_count(
            &preserved,
            "SELECT count(*) FROM import_jobs WHERE state = 'running'"
        ),
        1,
        "the interrupted ingest keeps its job, never failed"
    );
}
