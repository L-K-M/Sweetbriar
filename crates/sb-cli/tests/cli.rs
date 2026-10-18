//! Integration tests for the CLI binary (plan M0.7): the settings
//! round trip across processes, the `InvalidInput` and `Locked` exits,
//! and that a locked store creates nothing. The tests run the built
//! binary against scratch data roots — never the platform's real
//! default — and remove their scratch dirs.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use sb_service::{CoreConfig, open_core};

/// The built `sb-cli` binary cargo exposes to integration tests.
const BIN: &str = env!("CARGO_BIN_EXE_sb-cli");

/// Run the binary with `--data-root` under a scratch dir and the given
/// subcommand arguments.
fn cli(data_root: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .arg("--data-root")
        .arg(data_root)
        .args(args)
        .output()
        .expect("the sb-cli binary runs")
}

/// A fresh scratch dir with its store root, removed on drop.
fn scratch(label: &str) -> (sb_test_support::ScratchDir, PathBuf) {
    let scratch = sb_test_support::ScratchDir::new(label);
    let root = scratch.path().join("store");
    (scratch, root)
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Names and sizes of everything under `root`, recursively, sorted —
/// to prove a refused open creates and touches nothing.
fn listing(root: &Path) -> Vec<(PathBuf, u64)> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("read a scratch dir") {
            let entry = entry.expect("a scratch entry");
            let metadata = entry.metadata().expect("scratch metadata");
            if metadata.is_dir() {
                stack.push(entry.path());
            }
            found.push((entry.path(), metadata.len()));
        }
    }
    found.sort();
    found
}

/// `settings set` prints the receipt's result JSON; a new process's
/// `settings get` reads the value back (plan M0.7's acceptance: the
/// round trip survives process restarts), with and without a key.
#[test]
fn settings_round_trip_across_processes() {
    let (_scratch, root) = scratch("cli-round-trip");

    let out = cli(
        &root,
        &["settings", "set", "appearance.compose_cue", "standard"],
    );
    assert!(out.status.success(), "set failed: {}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "{\"key\":\"appearance.compose_cue\",\"revision\":1}\n"
    );

    let out = cli(&root, &["settings", "get"]);
    assert!(out.status.success(), "get failed: {}", stderr(&out));
    assert_eq!(stdout(&out), "appearance.compose_cue=standard\n");

    let out = cli(&root, &["settings", "get", "appearance.compose_cue"]);
    assert!(out.status.success(), "get by key failed: {}", stderr(&out));
    assert_eq!(stdout(&out), "appearance.compose_cue=standard\n");
}

/// An unknown key exits 1 naming `InvalidInput`; so does an unknown
/// value, and a `get` of an unknown key.
#[test]
fn unknown_keys_and_values_exit_one_naming_invalid_input() {
    let (_scratch, root) = scratch("cli-invalid");

    let out = cli(&root, &["settings", "set", "bogus.key", "standard"]);
    assert_eq!(out.status.code(), Some(1), "{:?}", out.status.code());
    assert!(stderr(&out).contains("InvalidInput"), "{}", stderr(&out));

    let out = cli(
        &root,
        &["settings", "set", "appearance.compose_cue", "ultra"],
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("InvalidInput"), "{}", stderr(&out));

    let out = cli(&root, &["settings", "get", "bogus.key"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("InvalidInput"), "{}", stderr(&out));
}

/// Usage errors exit 2: a missing or unknown command, a wrong argument
/// count, an unknown option, an option without its value.
#[test]
fn usage_errors_exit_two() {
    let (_scratch, root) = scratch("cli-usage");
    let cases: [&[&str]; 6] = [
        &["nope"],
        &["settings"],
        &["settings", "wibble"],
        &["settings", "set", "only-one"],
        &["settings", "get", "a", "b"],
        &["--wat", "settings", "get"],
    ];
    for args in cases {
        let out = cli(&root, args);
        assert_eq!(out.status.code(), Some(2), "args {args:?} must be usage");
    }

    // `--data-root` without a value: the helper's own --data-root
    // makes this a second, valueless one.
    let out = Command::new(BIN).arg("--data-root").output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}

/// While the test process holds the store through `sb-service`, the
/// CLI exits 1 naming `Locked` and the scratch dir is unchanged: a
/// refused open creates nothing (plan M0.7).
#[test]
fn a_locked_store_exits_one_naming_locked_and_creates_nothing() {
    let (scratch, root) = scratch("cli-locked");
    let mut config = CoreConfig::platform_default().expect("a home directory in tests");
    config.data_root = root.clone();
    let core = sb_test_support::block_on(open_core(config)).expect("the test opens the core");

    let before = listing(scratch.path());
    let out = cli(&root, &["settings", "get"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("Locked"), "{}", stderr(&out));
    assert_eq!(
        listing(scratch.path()),
        before,
        "a refused open must create nothing"
    );

    sb_test_support::block_on(core.shutdown());
}

/// A non-default value survives process restarts, read with and
/// without a key, and switching back to the default in another process
/// sticks too: `get` reads what was stored, not the registry default.
#[test]
fn a_non_default_value_survives_process_restarts() {
    let (_scratch, root) = scratch("cli-strong");

    let out = cli(
        &root,
        &["settings", "set", "appearance.compose_cue", "strong"],
    );
    assert!(out.status.success(), "set failed: {}", stderr(&out));
    for args in [
        &["settings", "get"][..],
        &["settings", "get", "appearance.compose_cue"][..],
    ] {
        let out = cli(&root, args);
        assert!(out.status.success(), "get failed: {}", stderr(&out));
        assert_eq!(stdout(&out), "appearance.compose_cue=strong\n");
    }

    let out = cli(
        &root,
        &["settings", "set", "appearance.compose_cue", "standard"],
    );
    assert!(out.status.success(), "set failed: {}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "{\"key\":\"appearance.compose_cue\",\"revision\":2}\n"
    );
    let out = cli(&root, &["settings", "get", "appearance.compose_cue"]);
    assert_eq!(stdout(&out), "appearance.compose_cue=standard\n");
}

/// Without `--data-root` the CLI uses the platform's default root, and
/// on a fresh system — none of its ancestors exist yet — it creates
/// them. `HOME` and the XDG variable point into a scratch dir, so the
/// real default root is never touched.
#[test]
fn the_default_root_works_on_a_fresh_system() {
    let scratch = sb_test_support::ScratchDir::new("cli-default-root");
    // A fresh system still has a home directory; nothing under it.
    let home = scratch.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let xdg_data = scratch.path().join("xdg/data");
    let out = Command::new(BIN)
        .args(["settings", "set", "appearance.compose_cue", "strong"])
        .env("HOME", &home)
        .env("XDG_DATA_HOME", &xdg_data)
        .env("XDG_CACHE_HOME", scratch.path().join("xdg/cache"))
        .output()
        .expect("the sb-cli binary runs");
    assert!(
        out.status.success(),
        "set at the default root failed: {}",
        stderr(&out)
    );

    let created = if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Sweetbriar/state.sqlite")
    } else {
        xdg_data.join("sweetbriar/state.sqlite")
    };
    assert!(created.exists(), "no store at {}", created.display());
}

fn fixture(name: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/import")
        .join(name)
        .to_str()
        .expect("fixture paths are UTF-8")
        .to_owned()
}

/// The report path `import` printed.
fn report_path(out: &Output) -> PathBuf {
    let text = stdout(out);
    let line = text
        .lines()
        .find_map(|line| line.strip_prefix("report: "))
        .unwrap_or_else(|| panic!("no report line in {text}"));
    PathBuf::from(line)
}

/// `import` runs one job to completion and prints its inventory,
/// totals and report; importing the same file again adds no
/// occurrences (spec/09 re-import idempotence).
#[test]
fn import_runs_to_completion_and_reimport_adds_nothing() {
    let (_scratch, root) = scratch("cli-import");
    let inbox = fixture("Inbox.mbox");

    let out = cli(&root, &["import", &inbox]);
    assert!(out.status.success(), "import failed: {}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("inventory: 1 sources, 2 messages"), "{text}");
    assert!(text.contains("-> Inbox: 2 messages"), "{text}");
    assert!(
        text.contains("Completed: 2 imported of 2 processed, 1 duplicates, 0 unparseable"),
        "{text}"
    );
    assert!(
        text.contains("read state: 0 from headers, 2 by default"),
        "{text}"
    );
    let report = std::fs::read_to_string(report_path(&out)).expect("the report was written");
    assert!(report.contains("\"state\": \"completed\""), "{report}");

    let out = cli(&root, &["import", &inbox]);
    assert!(out.status.success(), "re-import failed: {}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.contains("Completed: 0 imported of 2 processed, 2 duplicates"),
        "{text}"
    );
}

/// A source whose `>>From ` lines decode differently needs `--variant`;
/// without it the job is cancelled, never guessed (spec/09).
#[test]
fn an_ambiguous_mbox_needs_a_variant() {
    let (_scratch, root) = scratch("cli-variant");
    let quoting = fixture("quoting.mbox");

    let out = cli(&root, &["import", &quoting]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stdout(&out).contains("needs a variant"), "{}", stdout(&out));
    assert!(stderr(&out).contains("--variant mboxo or --variant mboxrd"));
    assert!(stderr(&out).contains("the import was cancelled"));

    let out = cli(&root, &["import", "--variant", "mboxrd", &quoting]);
    assert!(out.status.success(), "import failed: {}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.contains("Completed: 1 imported of 1 processed"),
        "{text}"
    );
    assert!(text.contains("read state: 1 from headers"), "{text}");
}

/// A missing path is reported as a problem, not an empty import, and
/// `import`'s own argument errors are usage errors.
#[test]
fn import_reports_problems_and_rejects_bad_arguments() {
    let (scratch, root) = scratch("cli-import-args");
    let missing = scratch.path().join("missing.mbox");
    let out = cli(&root, &["import", missing.to_str().unwrap()]);
    assert!(out.status.success(), "import failed: {}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("inventory: 0 sources"), "{text}");
    assert!(text.contains("problems: 1"), "{text}");
    assert!(text.contains("Inaccessible"), "{text}");

    for args in [
        &["import"][..],
        &["import", "--variant"],
        &["import", "--variant", "mbox", "x"],
        &["import", "--variant", "mboxo", "--variant", "mboxrd", "x"],
    ] {
        let out = cli(&root, args);
        assert_eq!(out.status.code(), Some(2), "args {args:?} must be usage");
    }
}
