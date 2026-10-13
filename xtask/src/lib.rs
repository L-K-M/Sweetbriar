//! `xtask` — the library half of Sweetbriar's task runner, split from
//! the binary so integration tests can drive the layering checker
//! directly (`xtask/tests/check_layers.rs`).

pub mod layers;

use std::fmt;
use std::path::{Component, Path, PathBuf};

/// An external command or tool the task runner invokes failed: a bad
/// spawn, a nonzero exit, or output it couldn't read.
#[derive(Debug)]
pub struct ToolError(pub String);

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ToolError {}

/// The cargo binary to run: `$CARGO` inside build scripts, `cargo`
/// elsewhere.
pub(crate) fn cargo() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string())
}

/// Runs `prog args` in `dir` with `CARGO_TARGET_DIR` set when given,
/// returning stdout. A failed spawn or a nonzero exit is an error
/// whose message carries the first compiler error or stderr's
/// `error:` lines — the first line is usually just progress
/// (`Documenting …`), the last the cause.
pub(crate) fn run(
    prog: &str,
    args: &[&str],
    dir: &Path,
    target: Option<&Path>,
) -> Result<String, ToolError> {
    run_env(prog, args, dir, target, &[])
}

/// `run` plus environment overrides: a `None` value removes the
/// variable, a `Some` sets it — cargo's own environment wins over
/// config files, so setting a var empty also neutralizes a
/// `[build]`-table flag.
pub(crate) fn run_env(
    prog: &str,
    args: &[&str],
    dir: &Path,
    target: Option<&Path>,
    env: &[(&str, Option<&str>)],
) -> Result<String, ToolError> {
    let mut cmd = std::process::Command::new(prog);
    cmd.args(args).current_dir(dir);
    if let Some(t) = target {
        cmd.env("CARGO_TARGET_DIR", t);
    }
    for (key, value) in env {
        match value {
            Some(value) => cmd.env(key, value),
            None => cmd.env_remove(key),
        };
    }
    let out = cmd
        .output()
        .map_err(|e| ToolError(format!("{prog} {}: {e}", args.join(" "))))?;
    if !out.status.success() {
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(ToolError(format!(
            "{prog} {}: {}",
            args.join(" "),
            error_detail(&stdout, &stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// What a failed tool actually complained about. With
/// `--message-format=json` the compiler's diagnostics are on stdout —
/// the first error-level message's rendered text says what broke —
/// else every stderr line starting with `error`, else stderr's last
/// lines. A silent failure still yields `failed`, never an empty
/// message.
fn error_detail(stdout: &str, stderr: &str) -> String {
    for line in stdout.lines() {
        let Ok(msg) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let level = &msg["message"]["level"];
        if msg["reason"] == "compiler-message"
            && level.as_str().is_some_and(|l| l.starts_with("error"))
            && let Some(rendered) = msg["message"]["rendered"].as_str()
        {
            return rendered.trim_end().to_string();
        }
    }
    let errors: Vec<&str> = stderr.lines().filter(|l| l.starts_with("error")).collect();
    if !errors.is_empty() {
        return errors.join("; ");
    }
    let tail = stderr
        .lines()
        .rev()
        .filter(|l| !l.trim().is_empty())
        .take(3)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("; ");
    if tail.is_empty() {
        "failed".to_string()
    } else {
        tail
    }
}

/// Canonicalize for comparisons; when `canonicalize` fails (the file
/// doesn't exist), still collapse `..`/`.` lexically so containment
/// checks can't be dodged by path shape.
pub(crate) fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| normalize(path))
}

/// `path` relative to `root` for diagnostics (unchanged when outside).
pub(crate) fn rel(path: &Path, root: &Path) -> PathBuf {
    path.strip_prefix(root).unwrap_or(path).to_path_buf()
}

/// `pkg`'s crate name — hyphens belong to packages, underscores to
/// crates.
pub(crate) fn underscored(pkg: &str) -> String {
    pkg.replace('-', "_")
}

/// `a/../b` → `b`, without touching the filesystem: `.` drops, `..`
/// pops the last real component and is kept only when nothing is left
/// to pop (an escape attempt then stays visibly outside the crate).
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir if out.file_name().is_some() => {
                out.pop();
            }
            Component::ParentDir => out.push(".."),
            other => out.push(other.as_os_str()),
        }
    }
    out
}
