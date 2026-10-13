//! The source-boundary check (spec/01): every `.rs` file rustc saw
//! while compiling a crate — every target, under default and all
//! features — must live inside that crate's directory or its own
//! build script's `OUT_DIR` — this is what `#[path]`, `include!` and
//! `include_str!` evasions look like. Non-`.rs` data stays exempt.
//! The files come from Cargo's dep-info output; where the source sits
//! says nothing about the public API, so the check never parses Rust.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::meta::Member;
use super::{Error, Features, Violation};
use crate::{canonical, rel, run};

/// Cargo's `--message-format=json` build output: `compiler-artifact`
/// records each unit's files and target kind, `build-script-executed`
/// the real `out_dir` it linked against — matched on `package_id`,
/// never path shape, so a forged `target/…/build/<pkg>-*/out/` can't
/// pass. A package built both as a dependency and a build dependency
/// with different features gets several `out_dir`s — kept as a set.
#[derive(Deserialize)]
struct Message {
    reason: String,
    package_id: String,
    #[serde(default)]
    target: Target,
    #[serde(default)]
    filenames: Vec<PathBuf>,
    #[serde(default)]
    out_dir: Option<PathBuf>,
}

/// The artifact's target: `kind` says `custom-build` for build-script
/// compilations (their dep-info sits under a different filename).
#[derive(Default, Deserialize)]
struct Target {
    #[serde(default)]
    kind: Vec<String>,
}

/// What a `compiler-artifact` unit is; the two kinds name their
/// dep-info differently.
#[derive(Clone, Copy)]
enum ArtifactKind {
    /// A compiled crate target (lib, bin, test, bench, proc-macro):
    /// rustc writes `<name>-<hash>.d` beside the artifact.
    Crate,
    /// A `custom-build` unit: `build/<pkg>-<fp>/build-script-build`
    /// with dep-info at `build_script_build-<fp>.d`.
    BuildScript,
}

/// The whole source-boundary check: compile every member's every
/// target under each feature configuration and boundary-check the
/// dep-info files Cargo reports for it.
pub(crate) fn check(
    root: &Path,
    members: &[Member],
    target: &Path,
    host: &str,
    configs: &[Features],
) -> Result<Vec<Violation>, Error> {
    // package_id → every real OUT_DIR it ran a build script into, and
    // each artifact unit — unioned across the feature configurations.
    let mut out_dirs: BTreeMap<String, BTreeSet<PathBuf>> = BTreeMap::new();
    let mut artifacts = Vec::new();
    for &features in configs {
        let mut args = vec![
            "check",
            "--workspace",
            "--all-targets",
            "--target",
            host,
            "--message-format=json",
        ];
        args.extend_from_slice(features.args());
        let stdout = run(&crate::cargo(), &args, root, Some(target))?;
        for line in stdout.lines() {
            let Ok(message) = serde_json::from_str::<Message>(line) else {
                continue; // warnings and blank lines aren't Cargo messages
            };
            match message.reason.as_str() {
                "build-script-executed" => {
                    if let Some(out_dir) = message.out_dir {
                        out_dirs
                            .entry(message.package_id)
                            .or_default()
                            .insert(canonical(&out_dir));
                    }
                }
                "compiler-artifact" => artifacts.push(message),
                _ => {}
            }
        }
    }

    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for artifact in &artifacts {
        let Some(member) = members.iter().find(|m| m.id == artifact.package_id) else {
            continue;
        };
        let kind = if artifact.target.kind.iter().any(|k| k == "custom-build") {
            ArtifactKind::BuildScript
        } else {
            ArtifactKind::Crate
        };
        let dep = artifact
            .filenames
            .iter()
            .find_map(|f| dep_info(f, kind).into_iter().find(|d| d.exists()));
        // A member unit Cargo compiled must have dep-info; absent or
        // unreadable the boundary can't be checked — fail closed.
        let Some(dep) = dep else {
            return Err(Error::Workspace(format!(
                "{}: no dep-info file for artifact {}",
                rel(&member.manifest, root).display(),
                artifact
                    .filenames
                    .first()
                    .map_or_else(|| "?".to_string(), |f| f.display().to_string())
            )));
        };
        if !seen.insert(dep.clone()) {
            continue;
        }
        let text = std::fs::read_to_string(&dep)
            .map_err(|e| Error::Workspace(format!("{}: {e}", dep.display())))?;
        let dir = canonical(&member.dir);
        let outs = out_dirs.get(&artifact.package_id);
        for source in dep_files(&text) {
            // Symlinks resolve before the `.rs` rule — an `.inc` link
            // can't hide an outside `.rs` target.
            let abs = canonical(&root.join(&source));
            if !source.ends_with(".rs") && abs.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue; // include_str!/bytes! data is exempt
            }
            if abs.starts_with(&dir) || outs.is_some_and(|s| s.iter().any(|d| abs.starts_with(d))) {
                continue;
            }
            out.push(Violation::at(
                rel(&member.manifest, root),
                1,
                &member.krate,
                format!(
                    "source `{}` lies outside the crate's directory",
                    rel(&abs, root).display()
                ),
            ));
        }
    }
    Ok(out)
}

/// The dep-info candidates an artifact's filename implies. `check`
/// reports only `deps/lib<krate>-<hash>.rmeta` (bins, tests and
/// benches included); rustc writes `<krate>-<hash>.d` beside it. A
/// custom-build artifact reports `build/<pkg>-<fp>/build-script-build`
/// whose dep-info is `build_script_build-<fp>.d` — the fingerprint
/// lives in the parent directory's name.
fn dep_info(artifact: &Path, kind: ArtifactKind) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut dep = artifact.with_extension("d");
    if let Some(name) = dep
        .file_name()
        .and_then(|n| n.to_str().map(str::to_owned))
        .and_then(|n| n.strip_prefix("lib").map(str::to_owned))
    {
        dep.set_file_name(name);
    }
    out.push(dep);
    if matches!(kind, ArtifactKind::BuildScript)
        && let Some(fp) = artifact
            .parent()
            .and_then(Path::file_name)
            .and_then(|n| n.to_str())
            .and_then(|n| n.rsplit('-').next().map(str::to_owned))
    {
        out.push(artifact.with_file_name(format!("build_script_build-{fp}.d")));
    }
    out
}

/// The dependency side of a dep-info file: the right side of every
/// make rule, `\`-continuations folded in first. `# env-dep:` and
/// `# checksum:` lines are comments — env vars and digests, not
/// sources — and non-rule lines (no `:`) are skipped. `\ ` and `\\`
/// escapes are preserved while splitting.
fn dep_files(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rule = String::new();
    for line in text.lines() {
        if line.starts_with('#') {
            continue;
        }
        let continued = line.trim_end().ends_with('\\');
        rule.push_str(line.trim_end_matches('\\'));
        if continued {
            continue;
        }
        if let Some((_, deps)) = rule.split_once(':') {
            out.extend(
                deps.replace("\\\\", "\u{1}")
                    .replace("\\ ", "\u{0}")
                    .split_whitespace()
                    .map(|w| w.replace('\u{1}', "\\").replace('\u{0}', " ")),
            );
        }
        rule.clear();
    }
    out
}
