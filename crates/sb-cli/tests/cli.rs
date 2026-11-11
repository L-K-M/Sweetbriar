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
    assert_eq!(
        stdout(&out),
        "appearance.compose_cue=standard\nbackups.destination=\n"
    );

    let out = cli(&root, &["settings", "get", "appearance.compose_cue"]);
    assert!(out.status.success(), "get by key failed: {}", stderr(&out));
    assert_eq!(stdout(&out), "appearance.compose_cue=standard\n");
}

#[test]
fn destination_settings_are_escaped_display_lines_not_terminal_controls() {
    let (_scratch, root) = scratch("cli-destination-escaped");
    let value = "/volume/with \"quotes\"\\and café\nnext\t\u{1b}path";
    let out = cli(&root, &["settings", "set", "backups.destination", value]);
    assert!(out.status.success(), "{}", stderr(&out));
    let expected =
        "backups.destination=/volume/with \\\"quotes\\\"\\\\and café\\nnext\\t\\u{1b}path\n";
    let out = cli(&root, &["settings", "get", "backups.destination"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), expected);
    let out = cli(&root, &["settings", "get"]);
    assert_eq!(
        stdout(&out),
        format!("appearance.compose_cue=standard\n{expected}")
    );
}

#[test]
fn rejected_settings_arguments_cannot_inject_terminal_controls() {
    let (_scratch, root) = scratch("cli-setting-error-display");
    let hostile = "relative \"quoted\"\\ café\nforged\r\t\u{1b}[2J";
    let cases: [(&[&str], i32); 4] = [
        (&["settings", "set", "backups.destination", hostile], 1),
        (&["settings", "set", hostile, "standard"], 1),
        (&["settings", "get", hostile], 1),
        (&["settings", hostile], 2),
    ];
    for (args, exit) in cases {
        let out = cli(&root, args);
        let diagnostic = stderr(&out);
        assert_eq!(out.status.code(), Some(exit), "{args:?}: {diagnostic:?}");
        assert!(out.stdout.is_empty());
        for control in ['\r', '\t', '\u{1b}'] {
            assert!(!diagnostic.contains(control), "{args:?}: {diagnostic:?}");
        }
        assert!(!diagnostic.contains("\nforged"), "{args:?}: {diagnostic:?}");
        assert!(
            diagnostic.contains(&hostile.escape_debug().to_string()),
            "{args:?}: {diagnostic:?}"
        );
    }
}

#[test]
fn backup_uses_the_configured_directory_across_cli_processes() {
    let (scratch, root) = scratch("cli-destination-backup");
    let parent = std::fs::canonicalize(scratch.path()).unwrap();
    let volume = parent.join("volume");
    let destination = volume.join("backups");
    std::fs::create_dir_all(&destination).unwrap();
    let out = cli(
        &root,
        &[
            "settings",
            "set",
            "backups.destination",
            destination.to_str().unwrap(),
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let out = cli(&root, &["backup"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    let id = text.split_whitespace().next().unwrap();
    assert!(
        destination.join(id).join("manifest.json").is_file(),
        "{text}"
    );
    assert_eq!(std::fs::read_dir(root.join("backups")).unwrap().count(), 0);
    std::fs::remove_dir_all(&volume).unwrap();
    let out = cli(&root, &["backup"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("Unavailable"), "{}", stderr(&out));
    assert!(!volume.exists());
    assert_eq!(std::fs::read_dir(root.join("backups")).unwrap().count(), 0);
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
    // Startup may upgrade the extractor. Park this fixture's owner before
    // measuring writes attributable only to the refused CLI process.
    let core = sb_test_support::block_on(open_core(config.clone())).expect("initialize the store");
    sb_test_support::block_on(core.shutdown());
    sb_test_support::execute_sql(
        &root.join("state.sqlite"),
        "UPDATE search_index_state SET user_paused = 1 WHERE id = 0",
    );
    let core = sb_test_support::block_on(open_core(config)).expect("the test opens the core");

    // Both maintenance lanes acknowledge empty startup sweeps before the snapshot.
    let db = root.join("state.sqlite");
    let inputs = sb_test_support::store_meta_value(&db, "seen_address_inputs")
        .expect("the input revision exists");
    let repair_inputs = sb_test_support::store_meta_value(&db, "repair_inputs")
        .expect("the corpus input revision exists");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while sb_test_support::store_meta_value(&db, "seen_address_swept").as_ref() != Some(&inputs)
        || sb_test_support::store_meta_value(&db, "repair_swept").as_ref() != Some(&repair_inputs)
    {
        assert!(
            std::time::Instant::now() < deadline,
            "startup maintenance did not settle"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    // A checkpoint can return busy while the owner's idle checkpoint holds its lock.
    while std::fs::metadata(root.join("state.sqlite-wal"))
        .unwrap()
        .len()
        != 0
    {
        sb_test_support::execute_sql(
            &db,
            "PRAGMA busy_timeout=30000; SELECT value FROM store_meta LIMIT 1; PRAGMA wal_checkpoint(TRUNCATE)",
        );
        assert!(
            std::time::Instant::now() < deadline,
            "startup WAL did not settle"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

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
    for (args, expected) in [
        (
            &["settings", "get"][..],
            "appearance.compose_cue=strong\nbackups.destination=\n",
        ),
        (
            &["settings", "get", "appearance.compose_cue"][..],
            "appearance.compose_cue=strong\n",
        ),
    ] {
        let out = cli(&root, args);
        assert!(out.status.success(), "get failed: {}", stderr(&out));
        assert_eq!(stdout(&out), expected);
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
    // The choice reaches the engine as given, not its opposite.
    let report = std::fs::read_to_string(report_path(&out)).expect("the report was written");
    assert!(report.contains("\"variant\": \"mboxrd\""), "{report}");
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
        &["import", "--varaint", "mboxo", "x"],
        &["import", "--"],
    ] {
        let out = cli(&root, args);
        assert_eq!(out.status.code(), Some(2), "args {args:?} must be usage");
    }
}

/// `backup list` (plan M1.8i1) prints the recorded generations, newest
/// first, one tab-separated line each: a fresh store prints none, and
/// rows a backup, an interrupted one and the weekly check left print
/// as recorded.
#[test]
fn backup_list_prints_the_recorded_generations() {
    let (_scratch, root) = scratch("cli-backup-list");
    let out = cli(&root, &["backup", "list"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "");
    assert!(stderr(&out).contains("no backups are recorded"));

    // The store exists and no process holds it now. 1794182400000 is
    // 2026-11-09 00:00 UTC.
    sb_test_support::execute_sql(
        &root.join("state.sqlite"),
        "INSERT INTO backup_generations
           (generation_id, started_at, completed_at, state, location, manifest_sha256,
            object_count, total_bytes, verified_at, verification)
         VALUES
           ('gen-complete', 1794182400000, 1794182401000, 'complete', '/backups/gen-complete',
            'ab', 2, 4096, 1794268800000, 'intact'),
           ('gen-failed', 1794186000000, NULL, 'failed', '/backups/gen-failed',
            NULL, NULL, NULL, NULL, NULL),
           ('gen-running', 1794189600000, NULL, 'in_progress', '/backups/gen-running',
            NULL, NULL, NULL, NULL, NULL);",
    );

    let out = cli(&root, &["backup", "list"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "gen-running\tin progress\t2026-11-09T02:00:00Z\t-\tnot checked\t/backups/gen-running\n\
         gen-failed\tfailed\t2026-11-09T01:00:00Z\t-\tnot checked\t/backups/gen-failed\n\
         gen-complete\tcomplete\t2026-11-09T00:00:00Z\t4096 bytes in 2 objects\t\
         intact 2026-11-10T00:00:00Z\t/backups/gen-complete\n"
    );
    assert_eq!(stderr(&out), "");

    for args in [&["backup", "lst"][..], &["backup", "list", "extra"]] {
        let out = cli(&root, args);
        assert_eq!(out.status.code(), Some(2), "args {args:?} must be usage");
    }
}

/// `backup` (plan M1.8i3) takes a generation now, waits for its copy
/// and prints its line. `backup list` in a new process prints the
/// same line, and the generation's directory holds its manifest.
#[test]
fn backup_takes_a_generation_and_prints_its_line() {
    let (_scratch, root) = scratch("cli-backup");
    let out = cli(&root, &["backup"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stderr(&out), "");
    let printed = stdout(&out);
    let fields: Vec<&str> = printed.trim_end_matches('\n').split('\t').collect();
    assert_eq!(fields.len(), 6, "{printed:?}");
    assert_eq!(fields[1], "complete");
    assert!(fields[3].ends_with(" bytes in 0 objects"), "{printed:?}");
    assert_eq!(fields[4], "not checked");
    let directory = Path::new(fields[5]);
    assert!(directory.ends_with(fields[0]), "{printed:?}");
    assert!(directory.join("manifest.json").is_file());
    assert!(directory.join("state.sqlite").is_file());

    let listed = cli(&root, &["backup", "list"]);
    assert!(listed.status.success(), "{}", stderr(&listed));
    assert_eq!(stdout(&listed), printed);

    // A second one is a new generation, listed first. (Retention then
    // keeps one of the two when both started in the same hour.)
    let again = cli(&root, &["backup"]);
    assert!(again.status.success(), "{}", stderr(&again));
    let second = stdout(&again);
    assert_ne!(second, printed);
    let listed = cli(&root, &["backup", "list"]);
    assert!(stdout(&listed).starts_with(&second), "{}", stdout(&listed));
}

/// The roots earlier restores preserved beside the data root.
fn preserved(root: &Path) -> usize {
    std::fs::read_dir(root.parent().unwrap())
        .unwrap()
        .filter(|entry| {
            let name = entry.as_ref().unwrap().file_name();
            name.to_string_lossy().contains(".preserved-")
        })
        .count()
}

/// A real swap refusal exits 1, not just a failed preparation. An
/// occupied preserved-root name is left alone, as is the live store.
#[test]
fn a_refused_restore_exits_one_without_replacing_the_store() {
    let (_scratch, root) = scratch("cli-restore-refused");
    let cue = "appearance.compose_cue";
    let set = |value| {
        let out = cli(&root, &["settings", "set", cue, value]);
        assert!(out.status.success(), "{}", stderr(&out));
    };
    set("strong");
    let backup = cli(&root, &["backup"]);
    assert!(backup.status.success(), "{}", stderr(&backup));
    let line = stdout(&backup);
    let id = line.split('\t').next().unwrap();
    set("standard");

    let db = root.join("state.sqlite");
    let incarnation = sb_test_support::store_meta_value(&db, "incarnation").unwrap();
    let occupied = root.with_file_name(format!("store.preserved-{incarnation}"));
    std::fs::create_dir(&occupied).unwrap();
    std::fs::write(occupied.join("kept"), b"not a restore target").unwrap();

    let out = cli(&root, &["restore", id]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(stdout(&out), "");
    assert!(stderr(&out).contains("refused"), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("store is unchanged"),
        "{}",
        stderr(&out)
    );
    assert!(stderr(&out).contains("prepared copy"), "{}", stderr(&out));
    let staged = std::fs::read_dir(root.parent().unwrap())
        .unwrap()
        .filter(|entry| {
            entry
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".restoring-")
        })
        .count();
    assert_eq!(
        staged, 1,
        "the refused preparation remains beside the store"
    );
    assert_eq!(
        std::fs::read(occupied.join("kept")).unwrap(),
        b"not a restore target"
    );
    assert_eq!(
        sb_test_support::store_meta_value(&db, "incarnation"),
        Some(incarnation)
    );
    let got = cli(&root, &["settings", "get", cue]);
    assert!(got.status.success(), "{}", stderr(&got));
    assert_eq!(stdout(&got), format!("{cue}=standard\n"));
}

/// `restore <generation_id>` (plan M1.8i5) puts a recorded generation
/// in the store's place. What was set since is gone, the replaced
/// store is kept beside the data root, and the next process opens the
/// restored one, where the generation is still listed.
#[test]
fn restore_puts_a_recorded_generation_in_the_store_s_place() {
    let (_scratch, root) = scratch("cli-restore");
    let cue = "appearance.compose_cue";
    let set = |value| {
        let out = cli(&root, &["settings", "set", cue, value]);
        assert!(out.status.success(), "{}", stderr(&out));
    };
    set("strong");
    let backup = cli(&root, &["backup"]);
    assert!(backup.status.success(), "{}", stderr(&backup));
    let line = stdout(&backup);
    let id = line.split('\t').next().unwrap();
    set("standard");

    let out = cli(&root, &["restore", id]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), format!("{id}\trestored\n"));
    assert!(stderr(&out).contains("kept beside the data root"));
    assert_eq!(preserved(&root), 1);

    let got = cli(&root, &["settings", "get", cue]);
    assert_eq!(stdout(&got), format!("{cue}=strong\n"));
    let listed = cli(&root, &["backup", "list"]);
    assert!(listed.status.success(), "{}", stderr(&listed));
    assert!(
        stdout(&listed).starts_with(&format!("{id}\tcomplete\t")),
        "{}",
        stdout(&listed)
    );

    // An id no generation has is an error, and nothing moves.
    let out = cli(&root, &["restore", "no-such-generation"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(stdout(&out), "");
    assert!(
        stderr(&out).contains("no-such-generation"),
        "{}",
        stderr(&out)
    );
    assert_eq!(preserved(&root), 1);
    let got = cli(&root, &["settings", "get", cue]);
    assert_eq!(stdout(&got), format!("{cue}=strong\n"));
}
