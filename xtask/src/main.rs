//! `xtask` — tool (outside the L1–L6 graph). Sweetbriar's task runner.
//!
//! `cargo xtask ci` mirrors `.github/workflows/ci.yml` on Linux: every
//! gate except the Docker-based Dovecot suite (lands in M2) and the
//! macOS-only checks, which `.github/workflows/macos.yml` covers.

use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// The generated notices file the drift check diffs against.
const NOTICES_FILE: &str = "THIRD_PARTY_NOTICES.md";
/// cargo-about's Handlebars template, at the repo root.
const NOTICES_TEMPLATE: &str = "notices.hbs";

type Step = fn(&Path) -> bool;

fn main() -> ExitCode {
    let Some(task) = env::args().nth(1) else {
        usage();
        return ExitCode::FAILURE;
    };

    match task.as_str() {
        "ci" => ci(),
        "notices" => notices(),
        // M0.2 implements the syn-based adjacent-layer checker here and
        // adds it to `ci` in the same PR.
        "check-layers" => pending("check-layers", "M0.2"),
        _ => {
            usage();
            ExitCode::FAILURE
        }
    }
}

/// The full Linux CI gate, in the same order as `ci.yml`; first failure
/// stops the run.
fn ci() -> ExitCode {
    let root = workspace_root();
    let steps: [(&str, Step); 6] = [
        ("fmt", step_fmt),
        ("clippy", step_clippy),
        ("test", step_test),
        ("doc", step_doc),
        ("deny", step_deny),
        ("notices-drift", step_notices_drift),
    ];

    for (name, step) in steps {
        eprintln!("==> cargo xtask ci: {name}");
        if step(&root) {
            continue;
        }
        eprintln!("==> cargo xtask ci: step `{name}` failed; stopping");
        return ExitCode::FAILURE;
    }

    eprintln!("==> cargo xtask ci: all steps passed");
    ExitCode::SUCCESS
}

/// Regenerates `THIRD_PARTY_NOTICES.md` at the repo root.
fn notices() -> ExitCode {
    let root = workspace_root();
    if generate_notices(&root, &root.join(NOTICES_FILE)) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn step_fmt(root: &Path) -> bool {
    run(&mut cargo(root, &["fmt", "--all", "--check"]))
}

fn step_clippy(root: &Path) -> bool {
    run(&mut cargo(
        root,
        &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
    ))
}

fn step_test(root: &Path) -> bool {
    run(&mut cargo(root, &["test", "--workspace"]))
}

fn step_doc(root: &Path) -> bool {
    let mut cmd = cargo(root, &["doc", "--workspace", "--no-deps"]);
    cmd.env("RUSTDOCFLAGS", "-D warnings");
    run(&mut cmd)
}

fn step_deny(root: &Path) -> bool {
    run(&mut cargo(root, &["deny", "check"]))
}

/// Regenerates the notices into a temp file and diffs it against the
/// committed one; any drift is an error with a remediation hint.
fn step_notices_drift(root: &Path) -> bool {
    let tmp = env::temp_dir().join(format!("sb-notices-{}.md", std::process::id()));
    if !generate_notices(root, &tmp) {
        return false;
    }

    let mut diff = Command::new("diff");
    let clean = run(diff.current_dir(root).arg("-u").arg(NOTICES_FILE).arg(&tmp));
    let _ = std::fs::remove_file(&tmp);
    if clean {
        return true;
    }

    eprintln!(
        "{NOTICES_FILE} is stale or missing. Regenerate it with \
         `cargo xtask notices` and commit the result."
    );
    false
}

fn generate_notices(root: &Path, out: &Path) -> bool {
    let mut cmd = cargo(root, &["about", "generate", NOTICES_TEMPLATE]);
    run(cmd.arg("--output-file").arg(out))
}

/// Reports a task that exists in the plan but not yet in code.
fn pending(name: &str, milestone: &str) -> ExitCode {
    eprintln!("{name}: not implemented yet (lands in {milestone})");
    ExitCode::FAILURE
}

/// A `cargo` invocation pinned to the workspace root so `xtask` works
/// from any directory.
fn cargo(root: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new(env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    cmd.current_dir(root).args(args);
    cmd
}

/// Runs a command to completion, echoing it first; false on nonzero
/// exit or a spawn error.
fn run(cmd: &mut Command) -> bool {
    eprintln!("+ {}", describe(cmd));
    match cmd.status() {
        Ok(status) => status.success(),
        Err(err) => {
            eprintln!("failed to spawn: {err}");
            false
        }
    }
}

fn describe(cmd: &Command) -> String {
    let mut out = cmd.get_program().to_string_lossy().into_owned();
    for arg in cmd.get_args() {
        out.push(' ');
        out.push_str(&arg.to_string_lossy());
    }
    out
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives at <workspace>/xtask")
        .to_path_buf()
}

fn usage() {
    eprintln!("usage: cargo xtask <ci|notices|check-layers>");
}
