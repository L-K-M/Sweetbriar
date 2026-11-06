//! Run export commands through the real CLI and service over synthetic mail.

use std::path::Path;
use std::process::{Command, Output};

use sb_service::{CoreConfig, SearchIndexState, SearchScope, open_core};
use sb_test_support::{ScratchDir, block_on};

const BIN: &str = env!("CARGO_BIN_EXE_sb-cli");

fn cli(directory: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .current_dir(directory)
        .arg("--data-root")
        .arg(directory.join("store"))
        .env("HOME", directory)
        .env("XDG_CACHE_HOME", directory.join("cache"))
        .args(args)
        .output()
        .expect("the CLI runs")
}

fn success(output: &Output) -> String {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn imported(directory: &Path) -> (String, String) {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/import/Inbox.mbox");
    success(&cli(directory, &["import", fixture.to_str().unwrap()]));
    let core = block_on(open_core(CoreConfig {
        data_root: directory.join("store"),
        cache_root: directory.join(if cfg!(target_os = "macos") {
            "Library/Caches/Sweetbriar"
        } else {
            "cache/sweetbriar"
        }),
    }))
    .unwrap();
    let accounts = block_on(core.accounts()).unwrap();
    let account = accounts[0].account_id.clone();
    let mailboxes = block_on(core.mailboxes(&account)).unwrap();
    let mailbox = mailboxes.mailboxes[0].mailbox_id.to_string();
    // Export deliberately fences membership. Settle the fixture's ingestion
    // work so this test does not race a legitimate generation change.
    let db = directory.join("store/state.sqlite");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let watermarks = || {
        [
            ("thread_inputs", "threads_built"),
            ("seen_address_inputs", "seen_address_swept"),
            ("repair_inputs", "repair_swept"),
        ]
        .map(|(input, completed)| {
            (
                sb_test_support::store_meta_value(&db, input).expect("input watermark"),
                sb_test_support::store_meta_value(&db, completed).expect("completed watermark"),
            )
        })
    };
    loop {
        // These counters are monotonic. Identical reads bracketing coverage
        // avoid combining a completed old lane with another lane's new work.
        let before = watermarks();
        let coverage = block_on(core.coverage(SearchScope::All)).unwrap();
        let after = watermarks();
        if before == after
            && before.iter().all(|(input, completed)| input == completed)
            && coverage.pending == 0
            && coverage.queue_length == 0
            && coverage.index_state == SearchIndexState::Ready
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "fixture maintenance did not settle: {coverage:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    block_on(core.shutdown());
    (account, mailbox)
}

#[test]
fn relative_destinations_export_both_formats_and_report_durable_status() {
    let scratch = ScratchDir::new("cli-export-relative");
    let directory = scratch.path();
    let (account, mailbox) = imported(directory);
    let empty = cli(directory, &["export", "mailbox", &mailbox, ""]);
    assert_eq!(empty.status.code(), Some(1));
    assert!(
        empty.stdout.is_empty(),
        "an empty path must not start a job"
    );

    for (args, name, extension) in [
        (
            vec!["export", "mailbox", &mailbox, "mailbox-export"],
            "mailbox-export",
            "mbox",
        ),
        (
            vec![
                "export",
                "account",
                &account,
                "account-export",
                "--format",
                "eml",
            ],
            "account-export",
            "eml",
        ),
    ] {
        let text = success(&cli(directory, &args));
        assert!(text.contains("Completed:"), "{text}");
        let destination = directory.join(name);
        let manifest = destination.join("sweetbriar-metadata.json");
        assert!(manifest.is_file());
        assert!(
            text.lines()
                .any(|line| line == format!("manifest: {}", manifest.display())),
            "{text}"
        );
        assert!(std::fs::read_dir(&destination).unwrap().any(|entry| {
            entry
                .unwrap()
                .path()
                .extension()
                .is_some_and(|ext| ext == extension)
        }));

        let job = text
            .lines()
            .find_map(|line| line.strip_prefix("job "))
            .unwrap();
        let status = success(&cli(directory, &["export", "status", job]));
        assert!(status.contains("state: Completed"), "{status}");
        assert!(status.contains("captured_chunks: "), "{status}");
        let cancelled = cli(directory, &["export", "cancel", job]);
        assert_eq!(cancelled.status.code(), Some(1));
        let error = String::from_utf8_lossy(&cancelled.stderr);
        assert!(error.contains("Conflict"), "{error}");
    }
}

#[test]
fn malformed_export_arguments_are_usage_errors() {
    let scratch = ScratchDir::new("cli-export-usage");
    for args in [
        &["export"][..],
        &["export", "unknown"],
        &["export", "mailbox", "not-an-id", "out"],
        &["export", "account", "account-id"],
        &["export", "mailbox", "1", "out", "extra"],
        &["export", "mailbox", "1", "out", "--format"],
        &["export", "mailbox", "1", "out", "--format", "mboxo"],
        &[
            "export", "mailbox", "1", "out", "--format", "eml", "--format", "eml",
        ],
        &["export", "status"],
        &["export", "status", "job", "extra"],
        &["export", "cancel", "--unknown"],
    ] {
        let output = cli(scratch.path(), args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(!scratch.path().join("store").exists(), "{args:?}");
    }
}
