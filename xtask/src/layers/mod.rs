//! `cargo xtask check-layers` — the spec/01 layering check. Three
//! checks, all on data the Rust toolchain produces itself (nothing
//! re-implements name resolution):
//!
//! - `meta`: the crate graph via `cargo metadata --no-deps`;
//! - `rustdoc`: public re-exports via `cargo +nightly rustdoc
//!   --output-format json`;
//! - `depinfo`: source boundaries via `cargo check`'s dep-info files.
//!
//! Whatever can't be resolved with certainty is rejected, not guessed.

mod config;
mod depinfo;
mod meta;
mod rustdoc;

use std::fmt;
use std::path::{Path, PathBuf};

use crate::{ToolError, canonical, run};

/// The config file name at the workspace root.
const LAYERS_FILE: &str = "layers.toml";

/// The check's own target dir — the stable build's `target/` stays
/// untouched.
const CHECK_TARGET_DIR: &str = "target/check-layers";

/// What a `layers.toml` entry makes of a crate.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Class {
    /// First-party code at layer N (1–6).
    Layer(u8),
    /// Test-only support: a dev-dependency only, own deps unrestricted.
    Dev,
    /// Outside the graph (`xtask`, `sb_bindgen`): unrestricted.
    Tool,
}

/// Which feature configuration a toolchain pass runs under — the spec
/// checks each crate with default features and with `--all-features`,
/// so a `cfg(feature)`/`cfg(not(feature))` gate can't hide a re-export
/// or a source read.
#[derive(Clone, Copy)]
pub(crate) enum Features {
    /// The workspace's default features (no flag).
    Default,
    /// `--all-features`.
    All,
}

impl Features {
    fn args(self) -> &'static [&'static str] {
        match self {
            Features::Default => &[],
            Features::All => &["--all-features"],
        }
    }
}

/// A single layering violation, rendered as
/// `file:line: <from> (Lx) -> <to> (Ly): <rule>` for an edge between
/// two crates or `file:line: <crate>: <rule>` inside one crate.
pub struct Violation {
    /// Workspace-relative path.
    pub file: PathBuf,
    /// 1-based line; 1 when no better line is known.
    pub line: usize,
    /// Everything after `file:line:`.
    pub message: String,
}

impl Violation {
    /// An edge violation: `from (Lx) -> to (Ly): rule`.
    fn edge(
        file: PathBuf,
        line: usize,
        config: &config::Config,
        from: &str,
        to: &str,
        why: String,
    ) -> Self {
        let label = |krate: &str| match config.classify(krate) {
            Some(Class::Layer(n)) => format!("L{n}"),
            Some(Class::Dev) => "dev".to_string(),
            Some(Class::Tool) => "tool".to_string(),
            None => "unclassified".to_string(),
        };
        Self {
            file,
            line,
            message: format!("{from} ({}) -> {to} ({}): {why}", label(from), label(to)),
        }
    }

    /// A single-crate violation: `krate: rule`.
    fn at(file: PathBuf, line: usize, krate: &str, why: String) -> Self {
        Self {
            file,
            line,
            message: format!("{krate}: {why}"),
        }
    }
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.file.display(), self.line, self.message)
    }
}

/// The outcome of a check run: crate-graph results first, then
/// re-exports, then source boundaries (spec/01's order).
pub struct Report {
    pub violations: Vec<Violation>,
}

/// Anything that stops the check: bad `layers.toml`, an unreadable
/// manifest, a missing toolchain, a missing dep-info file. The check
/// fails closed.
#[derive(Debug)]
pub enum Error {
    Config(String),
    Workspace(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Config(m) | Error::Workspace(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for Error {}

impl From<ToolError> for Error {
    fn from(e: ToolError) -> Self {
        Error::Workspace(e.0)
    }
}

/// Checks the workspace rooted at `root` against `root/layers.toml`,
/// writing build output into `root/target/check-layers` so the stable
/// build stays untouched.
pub fn check_workspace(root: &Path) -> Result<Report, Error> {
    // CHECK_TARGET_DIR stays relative so `check_at` joins it onto the
    // canonical root exactly once — a joined relative path would land
    // at `root/root/target/…` when `root` itself is relative.
    check_at(root, &root.join(LAYERS_FILE), Path::new(CHECK_TARGET_DIR))
}

/// The full check: `root` against a `layers.toml` at an explicit path
/// (fixtures keep per-case configs), build output under `target`
/// (relative resolves against `root`; fixture copies each check into
/// their own `target/check-layers` — identical crates share artifact
/// hashes, so a shared `CARGO_TARGET_DIR` would swap `.rmeta`/`.d`
/// files under parallel tests).
pub fn check_at(root: &Path, layers_path: &Path, target: &Path) -> Result<Report, Error> {
    let root = &canonical(root);
    let target = &if target.is_absolute() {
        target.to_path_buf()
    } else {
        root.join(target)
    };
    let config = config::load(layers_path)?;
    let members = meta::members(root)?;
    let host = host_triple(root)?;

    // Spec/01 runs every toolchain pass under default features and
    // `--all-features`; with no feature declared anywhere the two are
    // identical, so one pass suffices.
    let configs: &[Features] = if members.iter().any(|m| m.has_features) {
        &[Features::Default, Features::All]
    } else {
        &[Features::All]
    };

    // All three checks run even when one finds violations; an Error
    // (missing toolchain, missing dep-info, wrong JSON version) aborts.
    let mut graph = meta::check_graph(&members, &config, root);
    let mut reexports = rustdoc::check(root, &config, &members, target, &host, configs)?;
    let mut sources = depinfo::check(root, &members, target, &host, configs)?;
    for section in [&mut graph, &mut reexports, &mut sources] {
        section.sort_by(|a, b| (&a.file, a.line, &a.message).cmp(&(&b.file, b.line, &b.message)));
        section.dedup_by(|a, b| a.file == b.file && a.line == b.line && a.message == b.message);
    }
    graph.extend(reexports);
    graph.extend(sources);
    Ok(Report { violations: graph })
}

/// The native target triple from `rustc -vV`. Passing it as `--target`
/// keeps build output and rustdoc JSON under `<target>/<host>/` no
/// matter what `.cargo/config.toml`'s `[build] target` says.
fn host_triple(root: &Path) -> Result<String, Error> {
    let stdout = run("rustc", &["-vV"], root, None)?;
    stdout
        .lines()
        .find_map(|l| l.strip_prefix("host: "))
        .map(str::to_string)
        .ok_or_else(|| Error::Workspace("rustc -vV reported no host triple".to_string()))
}
