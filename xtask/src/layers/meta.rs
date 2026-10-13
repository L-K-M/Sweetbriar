//! Workspace truth from `cargo metadata` (spec/01: "the crate graph
//! from `cargo metadata`"). `--no-deps` still reports members, targets
//! and declared dependencies; no lockfile, nothing fetched.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::config::Config;
use super::{Class, Error, Violation};
use crate::{canonical, rel, run, underscored};

#[derive(Deserialize)]
struct Metadata {
    packages: Vec<Package>,
    workspace_members: Vec<String>,
}

#[derive(Deserialize)]
struct Package {
    /// Matches `package_id` in Cargo's build messages string-for-string.
    id: String,
    name: String,
    manifest_path: PathBuf,
    targets: Vec<Target>,
    #[serde(default)]
    dependencies: Vec<Dep>,
    /// Empty means default and `--all-features` builds are identical.
    #[serde(default)]
    features: BTreeMap<String, Vec<String>>,
}

#[derive(Deserialize)]
struct Target {
    name: String,
    kind: Vec<String>,
}

/// A declared dependency edge (one `[*dependencies]` entry): the
/// *package* name, its `package = …` rename, the dep's dir for
/// `path = …` deps, and which section it came from (`null` = normal,
/// `"dev"`/`"build"` otherwise).
#[derive(Deserialize)]
pub(crate) struct Dep {
    pub(crate) name: String,
    pub(crate) kind: Option<String>,
    pub(crate) rename: Option<String>,
    pub(crate) path: Option<PathBuf>,
}

/// A workspace member: id, crate name, package, dir, whether it
/// exports a lib (the public API rustdoc sees), dependency edges.
pub(crate) struct Member {
    /// `cargo metadata`'s `id`.
    pub(crate) id: String,
    /// The lib-ish target's crate name (underscored); the package name
    /// underscored when there is none.
    pub(crate) krate: String,
    /// `[package].name` — what dependency entries name.
    pub(crate) package: String,
    pub(crate) dir: PathBuf,
    pub(crate) manifest: PathBuf,
    pub(crate) has_lib: bool,
    /// A proc-macro lib compiles for the host even under `--target`;
    /// its rustdoc JSON lands in `<target>/doc/`, not the triple dir.
    pub(crate) proc_macro: bool,
    /// Any feature declared — gates the second toolchain pass.
    pub(crate) has_features: bool,
    pub(crate) deps: Vec<Dep>,
}

/// `cargo metadata`'s `kind` list says lib-family ("lib", "rlib",
/// "dylib", …) or one of the non-lib kinds.
fn lib_kind(kinds: &[String]) -> bool {
    !kinds
        .iter()
        .any(|k| ["bin", "test", "bench", "example", "custom-build"].contains(&k.as_str()))
}

/// Runs `cargo metadata` and returns the workspace members in manifest
/// order. Errors stop the whole check.
pub(crate) fn members(root: &Path) -> Result<Vec<Member>, Error> {
    let stdout = run(
        &crate::cargo(),
        &["metadata", "--format-version", "1", "--no-deps"],
        root,
        None,
    )?;
    let meta: Metadata = serde_json::from_str(&stdout)
        .map_err(|e| Error::Workspace(format!("cargo metadata output: {e}")))?;

    let mut members = Vec::new();
    for package in meta.packages {
        if !meta.workspace_members.contains(&package.id) {
            continue;
        }
        let lib = package.targets.iter().find(|t| lib_kind(&t.kind));
        members.push(Member {
            id: package.id,
            // The crate name is the lib-ish target's name; a bin-only
            // member uses its underscored package name.
            krate: lib
                .map(|t| t.name.clone())
                .unwrap_or_else(|| underscored(&package.name)),
            package: package.name,
            dir: package.manifest_path.parent().map_or_default(PathBuf::from),
            manifest: package.manifest_path,
            has_lib: lib.is_some(),
            proc_macro: lib.is_some_and(|t| t.kind.iter().any(|k| k == "proc-macro")),
            has_features: !package.features.is_empty(),
            deps: package.dependencies,
        });
    }
    Ok(members)
}

/// The crate-graph check (spec/01): every member is classified; every
/// first-party dependency — normal, dev or build — is allowed by
/// `layers.toml` and unrenamed; a dev-role crate appears only as a
/// dev-dependency. First-party means naming a member (verified by
/// `path`), a `layers.toml` entry, or a path under the root.
pub(crate) fn check_graph(members: &[Member], config: &Config, root: &Path) -> Vec<Violation> {
    let mut out = Vec::new();
    for member in members {
        let file = rel(&member.manifest, root);
        let from_class = config.classify(&member.krate);
        let edge = |line: usize, to: &str, why: String| {
            Violation::edge(file.clone(), line, config, &member.krate, to, why)
        };
        if from_class.is_none() {
            out.push(Violation::at(
                file.clone(),
                1,
                &member.krate,
                "not classified in layers.toml".to_string(),
            ));
        }
        let from_layer = matches!(from_class, Some(Class::Layer(_)));
        // Metadata carries no dep spans; the declaring line is found by
        // scanning the manifest for the dep key (the rename, when set).
        let manifest_text = std::fs::read_to_string(&member.manifest).unwrap_or_default();
        for dep in &member.deps {
            let line = dep_line(&manifest_text, dep.rename.as_deref().unwrap_or(&dep.name));
            let dep_name = underscored(&dep.name);
            let by_name = members.iter().find(|m| m.package == dep.name);
            // First-party: the dep names a member, a `layers.toml`
            // crate, or a path under the root.
            let first_party = by_name.is_some()
                || config.classify(&dep_name).is_some()
                || dep
                    .path
                    .as_deref()
                    .is_some_and(|p| canonical(p).starts_with(root));
            if !first_party {
                continue;
            }
            // First-party deps are unrenamed path dependencies on the
            // member itself (spec/01): a version-only dep can reach a
            // member through `[patch]`/`[replace]`, and a `path`
            // elsewhere is not the member.
            let Some(target) = by_name.filter(|m| {
                dep.path
                    .as_ref()
                    .is_some_and(|p| canonical(p) == canonical(&m.dir))
            }) else {
                let why = if dep.path.is_some() {
                    "names a first-party crate that is not a workspace member".to_string()
                } else {
                    format!("first-party dependency `{dep_name}` must be a path dependency")
                };
                out.push(edge(line, &dep_name, why));
                continue;
            };
            if let Some(rename) = &dep.rename {
                out.push(edge(
                    line,
                    &target.krate,
                    format!(
                        "renamed first-party dependency `{rename}` (package {})",
                        dep.name
                    ),
                ));
                continue;
            }
            if config.classify(&target.krate) == Some(Class::Dev) {
                if dep.kind.as_deref() != Some("dev") {
                    out.push(edge(
                        line,
                        &target.krate,
                        "may only appear as a dev-dependency".to_string(),
                    ));
                }
                continue;
            }
            // Unclassified and dev/tool members are not edge-checked.
            if !from_layer || config.dep_allowed(&member.krate, &target.krate) {
                continue;
            }
            out.push(edge(
                line,
                &target.krate,
                format!(
                    "first-party {} on `{}` is not allowed by layers.toml",
                    match dep.kind.as_deref() {
                        Some("dev") => "dev-dependency",
                        Some("build") => "build-dependency",
                        _ => "dependency",
                    },
                    target.krate
                ),
            ));
        }
    }
    // Root `[patch]`/`[replace]` tables can't name first-party crates
    // either (spec/01) — a patch would redirect a version dep onto a
    // member, exactly what the path-dep rule bans.
    let root_manifest = root.join("Cargo.toml");
    let root_text = std::fs::read_to_string(&root_manifest).unwrap_or_default();
    for (name, table) in patch_names(&root_text) {
        let krate = underscored(&name);
        let first_party = members
            .iter()
            .any(|m| m.package == name || m.krate == krate)
            || config.classify(&krate).is_some();
        if first_party {
            out.push(Violation::at(
                rel(&root_manifest, root),
                patch_line(&root_text, &name),
                &krate,
                format!("named by a `[{table}]` entry; first-party deps are unrenamed path deps"),
            ));
        }
    }
    out
}

/// Crate names a root manifest redirects through `[patch]`/`[replace]`
/// — every key under `[patch.<source>]`, and `[replace]`'s `name` or
/// `"name:version"` keys.
fn patch_names(manifest: &str) -> Vec<(String, &'static str)> {
    let Ok(parsed) = toml::from_str::<toml::Value>(manifest) else {
        return Vec::new(); // an invalid manifest fails earlier anyway
    };
    let mut out = Vec::new();
    if let Some(patch) = parsed.get("patch").and_then(|v| v.as_table()) {
        for entries in patch.values().filter_map(|v| v.as_table()) {
            out.extend(entries.keys().map(|k| (k.clone(), "patch")));
        }
    }
    if let Some(replace) = parsed.get("replace").and_then(|v| v.as_table()) {
        out.extend(
            replace
                .keys()
                .map(|k| (k.split(':').next().unwrap_or(k).to_string(), "replace")),
        );
    }
    out
}

/// The line naming `name` inside a `[patch]`/`[replace]` table — a
/// `{name} =` entry under such a header, or a `[patch.<src>.{name}]`/
/// `[replace.{name}]`/`[replace."{name}:…"]` header itself; 1 when
/// unsure.
fn patch_line(manifest: &str, name: &str) -> usize {
    let mut table = false;
    for (i, raw) in manifest.lines().enumerate() {
        let line = raw.trim();
        if line.starts_with('[') {
            table = line.starts_with("[patch.") || line.starts_with("[replace");
            if table && (line.contains(&format!("{name}]")) || line.contains(&format!("{name}:"))) {
                return i + 1;
            }
        } else if table && !line.starts_with('#') && dep_key(line, name) {
            return i + 1;
        }
    }
    1
}

/// A `name = …` dep-key line — bare, quoted, or a `[replace]`-style
/// `"name:version"` key.
fn dep_key(line: &str, name: &str) -> bool {
    line.trim_start_matches(['"', '\''])
        .strip_prefix(name)
        .is_some_and(|rest| rest.starts_with([' ', '=', '"', '\'', ':']))
}

/// A `[<section>.<name>]` dep header — plain (`[dependencies.x]`) or
/// under a target table (`[target.'cfg(…)'.dependencies.x]`).
fn dep_header(line: &str, name: &str) -> bool {
    let Some(h) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) else {
        return false;
    };
    ["dependencies", "dev-dependencies", "build-dependencies"]
        .iter()
        .any(|s| {
            h == format!("{s}.{name}")
                || h == format!("{s}.\"{name}\"")
                || h.ends_with(&format!(".{s}.{name}"))
                || h.ends_with(&format!(".{s}.\"{name}\""))
        })
}

/// The line declaring `name` in a manifest: the `name = ` dep-key,
/// a `name.workspace = true` inherited dep, or a
/// `[dependencies.name]`/`[target.….dependencies.name]`-style header;
/// line 1 when unsure. Commented lines don't count.
fn dep_line(manifest: &str, name: &str) -> usize {
    manifest
        .lines()
        .position(|line| {
            let line = line.trim();
            !line.starts_with('#')
                && (dep_key(line, name)
                    || line.starts_with(&format!("{name}.workspace"))
                    || dep_header(line, name))
        })
        .map_or(1, |i| i + 1)
}
