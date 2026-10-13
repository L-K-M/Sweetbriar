//! The re-export check (spec/01): each first-party crate's public API
//! comes from rustdoc's own JSON output — every `use` is already
//! resolved to its origin crate and item kind, so nothing here
//! re-implements name resolution. `--output-format json` is unstable,
//! so rustdoc runs on a pinned nightly; the pin (and the format
//! version it writes) moves only deliberately, as its own PR.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Deserialize;

use super::config::Config;
use super::meta::Member;
use super::{Class, Error, Features, Violation};
use crate::{rel, run, run_env, underscored};

/// The pinned nightly rustdoc runs on (spec/01 "Toolchain").
const NIGHTLY: &str = "nightly-2026-10-10";

/// The rustdoc JSON `format_version` `NIGHTLY` writes; a different
/// value means the pin drifted or rustdoc changed the schema.
const FORMAT_VERSION: u32 = 61;

/// Item `kind`s that re-export a macro — `macro_rules!`, attribute
/// and derive proc macros, and the inlined `proc_macro` shape — whose
/// `$crate` expansion would skip layers.
const MACRO_KINDS: &[&str] = &["macro", "proc_attribute", "proc_derive", "proc_macro"];

/// rustdoc writes `<crate>.json` at a fixed path in its target dir, so
/// document+read is serialized in case two checks share one.
static DOC_LOCK: Mutex<()> = Mutex::new(());

/// Only the fields the check reads; the rest of the JSON is ignored.
/// `index` is item id → item (public and private alike), `paths` the
/// item id → its true definition, `external_crates` crate id → its
/// real name; `root` is the crate module — the public API's entry.
#[derive(Deserialize)]
struct CrateDoc {
    format_version: u32,
    root: u32,
    #[serde(default)]
    index: BTreeMap<u32, Item>,
    #[serde(default)]
    paths: BTreeMap<u32, PathEntry>,
    #[serde(default)]
    external_crates: BTreeMap<u32, External>,
}

#[derive(Deserialize)]
struct Item {
    id: u32,
    /// The item's home crate; != 0 is a foreign item rustdoc inlined
    /// (a re-export by another name).
    crate_id: u32,
    name: Option<String>,
    span: Option<Span>,
    /// `"public"`, `"crate"`, `"default"` (private) or a restricted
    /// object — compared as JSON; only `"public"` counts (a
    /// `#[doc(hidden)]` item stays "public").
    #[serde(default)]
    visibility: serde_json::Value,
    /// Exactly one key — `use`, `module`, `extern_crate`, `struct`, …
    inner: BTreeMap<String, serde_json::Value>,
}

#[derive(Deserialize)]
struct Span {
    filename: PathBuf,
    begin: [usize; 2],
}

/// A `paths` entry: where the item's true definition lives.
/// `crate_id` indexes `external_crates`; 0 is the crate itself.
#[derive(Clone, Deserialize)]
struct PathEntry {
    crate_id: u32,
    path: Vec<String>,
    kind: String,
}

#[derive(Deserialize)]
struct External {
    /// The origin crate's real name — right even on a renamed dep.
    name: String,
}

/// `inner.use`: `id` is the target's item id — null for primitive
/// re-exports.
#[derive(Deserialize)]
struct UseItem {
    source: String,
    id: Option<u32>,
    is_glob: bool,
}

/// `inner.module`: the module's documented items.
#[derive(Deserialize)]
struct ModuleItem {
    items: Vec<u32>,
}

/// `inner.extern_crate`: `name` is the local binding; `rename` — set
/// by `as` — is the real crate.
#[derive(Deserialize)]
struct ExternCrate {
    name: String,
    rename: Option<String>,
}

/// The whole re-export check: rustdoc every lib member under each
/// feature configuration and apply the spec/01 rules to each public
/// re-export it reports.
pub(crate) fn check(
    root: &Path,
    config: &Config,
    members: &[Member],
    target: &Path,
    host: &str,
    configs: &[Features],
) -> Result<Vec<Violation>, Error> {
    // The pin must exist before anything runs; the error is the
    // install command so a fresh machine self-documents.
    if run(
        "rustup",
        &["run", NIGHTLY, "rustc", "--version"],
        root,
        None,
    )
    .is_err()
    {
        return Err(Error::Workspace(format!(
            "check-layers requires the {NIGHTLY} toolchain; install it with \
             `rustup toolchain install {NIGHTLY} --profile minimal`"
        )));
    }
    let _serial = DOC_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut out = Vec::new();
    for member in members.iter().filter(|m| m.has_lib) {
        for &features in configs {
            let doc = document(root, member, target, host, features)?;
            Check {
                member,
                manifest: rel(&member.manifest, root),
                doc: &doc,
                config,
                out: &mut out,
            }
            .run();
        }
    }
    Ok(out)
}

/// Runs `cargo +NIGHTLY rustdoc` for one crate's lib target — the
/// given feature configuration, the host triple explicit — and parses
/// its JSON from `doc/` under `target` (proc-macro) or
/// `target/<host>`. `rustup run` (not `cargo +…`) so the real `cargo`
/// binary works too — `+toolchain` is rustup-shim syntax.
/// `RUSTDOCFLAGS`/`CARGO_ENCODED_RUSTDOCFLAGS` are overridden empty so
/// an inherited `--document-private-items` can't enlarge the public
/// API (env vars supersede `[build] rustdocflags` config too).
fn document(
    root: &Path,
    member: &Member,
    target: &Path,
    host: &str,
    features: Features,
) -> Result<CrateDoc, Error> {
    let mut args = vec![
        "run",
        NIGHTLY,
        "cargo",
        "rustdoc",
        "-p",
        &member.package,
        "--lib",
        "--target",
        host,
    ];
    args.extend_from_slice(features.args());
    args.extend([
        "--",
        "-Z",
        "unstable-options",
        "--output-format",
        "json",
        "--document-hidden-items",
    ]);
    run_env(
        "rustup",
        &args,
        root,
        Some(target),
        &[
            ("RUSTDOCFLAGS", Some("")),
            ("CARGO_ENCODED_RUSTDOCFLAGS", Some("")),
        ],
    )?;
    let doc_dir = if member.proc_macro {
        target.join("doc")
    } else {
        target.join(host).join("doc")
    };
    let file = doc_dir.join(format!("{}.json", member.krate));
    let err = |e: &dyn std::fmt::Display| Error::Workspace(format!("{}: {e}", file.display()));
    let text = std::fs::read_to_string(&file).map_err(|e| err(&e))?;
    let doc: CrateDoc = serde_json::from_str(&text).map_err(|e| err(&e))?;
    if doc.format_version != FORMAT_VERSION {
        return Err(err(&format!(
            "rustdoc JSON format {} (expected {FORMAT_VERSION}); \
             move the nightly pin deliberately",
            doc.format_version
        )));
    }
    Ok(doc)
}

/// The item's `inner` discriminant — rustdoc serializes exactly one.
fn kind(item: &Item) -> &str {
    item.inner.keys().next().map_or("", String::as_str)
}

/// `inner`'s payload parsed as T; `None` is unverifiable, not a panic.
fn inner_as<T: serde::de::DeserializeOwned>(item: &Item) -> Option<T> {
    serde_json::from_value(item.inner.values().next()?.clone()).ok()
}

/// A `PathEntry` for an item inlined into `index`: `paths` has one
/// when the item is itself documented there, else the path is its
/// bare name and the kind its `inner` key.
fn entry_for(item: &Item, doc: &CrateDoc) -> PathEntry {
    doc.paths
        .get(&item.id)
        .cloned()
        .unwrap_or_else(|| PathEntry {
            crate_id: item.crate_id,
            path: item.name.iter().cloned().collect(),
            kind: kind(item).to_string(),
        })
}

/// Whether a re-export names one item or a module's whole surface.
enum Scope {
    Item,
    Glob,
}

/// One crate's walk over `doc`: state plus the violation sink.
struct Check<'a> {
    member: &'a Member,
    manifest: PathBuf,
    doc: &'a CrateDoc,
    config: &'a Config,
    out: &'a mut Vec<Violation>,
}

impl<'a> Check<'a> {
    /// Applies the spec/01 rules to everything reachable through the
    /// crate's public API: module `items` from `root` down, plus what
    /// local glob/module re-exports expose. Orphan index entries (a
    /// `pub use` in a private module, stripped items) are never
    /// visited — they aren't public API.
    fn run(&mut self) {
        let mut seen = BTreeSet::new();
        let mut queue = Vec::new();
        let Some(root) = self.doc.index.get(&self.doc.root) else {
            return self.unverifiable(None, "rustdoc JSON root missing from `index`".to_string());
        };
        queue.push(root);
        while let Some(item) = queue.pop() {
            if !seen.insert(item.id) {
                continue;
            }
            match kind(item) {
                "module" => self.module(item, &mut queue),
                "use" => self.reexport(item, &mut queue),
                "extern_crate" => self.extern_crate(item),
                // An inlined foreign item is itself a re-export.
                _ if item.crate_id != 0 => {
                    self.apply(&entry_for(item, self.doc), Scope::Item, item)
                }
                _ => {}
            }
        }
    }

    /// `file:line` for a diagnostic: the item's rustdoc span, or the
    /// crate's manifest:1 when it has none.
    fn spot(&self, item: Option<&Item>) -> (PathBuf, usize) {
        item.and_then(|i| i.span.as_ref()).map_or_else(
            || (self.manifest.clone(), 1),
            |s| (s.filename.clone(), s.begin[0]),
        )
    }

    /// Fail closed: `what` can't be verified at `item`'s span.
    fn unverifiable(&mut self, item: Option<&Item>, what: String) {
        let (file, line) = self.spot(item);
        self.out.push(Violation::at(
            file,
            line,
            &self.member.krate,
            format!("{what}; can't verify"),
        ));
    }

    /// A module's `items` are its public surface: resolve each child
    /// like a re-export target. Only `visibility: "public"` children
    /// count — rustdoc also lists private items when told to document
    /// them, and those aren't API. A private target a *public*
    /// re-export names is still followed through `resolved`.
    fn module(&mut self, item: &'a Item, queue: &mut Vec<&'a Item>) {
        if item.crate_id != 0 {
            self.apply(&entry_for(item, self.doc), Scope::Item, item);
        }
        let Some(m) = inner_as::<ModuleItem>(item) else {
            return self.unverifiable(Some(item), "module without an item list".to_string());
        };
        for child in m.items {
            if self
                .doc
                .index
                .get(&child)
                .is_some_and(|t| t.visibility != "public")
            {
                continue;
            }
            self.resolved(
                child,
                Scope::Item,
                item,
                queue,
                format!("public item {child} documented nowhere"),
            );
        }
    }

    /// A `use` resolves its target, `is_glob` setting the scope.
    fn reexport(&mut self, item: &'a Item, queue: &mut Vec<&'a Item>) {
        let Some(u) = inner_as::<UseItem>(item) else {
            return self.unverifiable(Some(item), "unresolved re-export".to_string());
        };
        let unresolved = || format!("unresolved re-export `{}`", u.source);
        // rustdoc's schema: `id` is null exactly for re-exports of
        // primitives (`pub use u8`, `pub use crate::m::Octet` where
        // `Octet` aliases one), whatever the source spelling. Never
        // first-party, so nothing to check.
        let Some(tid) = u.id else {
            return;
        };
        let scope = if u.is_glob { Scope::Glob } else { Scope::Item };
        self.resolved(tid, scope, item, queue, unresolved());
    }

    /// An item id resolves through `index` first (local items —
    /// private modules and nested `use`s — have no `paths` entry),
    /// then `paths` for foreign targets. A local
    /// module/`use`/`extern_crate` is queued: the re-export exposes it
    /// publicly. A foreign target is rule-checked here. Truly
    /// unresolved fails closed.
    fn resolved(
        &mut self,
        tid: u32,
        scope: Scope,
        item: &'a Item,
        queue: &mut Vec<&'a Item>,
        unresolved: String,
    ) {
        if let Some(t) = self.doc.index.get(&tid) {
            if t.crate_id != 0 {
                self.apply(&entry_for(t, self.doc), scope, item);
            } else if matches!(kind(t), "module" | "use" | "extern_crate") {
                queue.push(t);
            }
            return;
        }
        match self.doc.paths.get(&tid) {
            Some(pe) if pe.crate_id != 0 => self.apply(pe, scope, item),
            None => self.unverifiable(Some(item), unresolved),
            _ => {}
        }
    }

    /// A public `extern crate` re-exports a whole crate — a module
    /// re-export of its root, resolved through `external_crates`.
    fn extern_crate(&mut self, item: &Item) {
        let Some(ec) = inner_as::<ExternCrate>(item) else {
            return self.unverifiable(Some(item), "unresolved extern crate".to_string());
        };
        let real = ec.rename.unwrap_or(ec.name);
        // `pub extern crate self as api;` aliases the crate itself —
        // not a foreign re-export.
        if real == "self" {
            return;
        }
        let Some(&crate_id) = self
            .doc
            .external_crates
            .iter()
            .find_map(|(id, e)| (e.name == real).then_some(id))
        else {
            return self.unverifiable(Some(item), format!("unresolved extern crate `{real}`"));
        };
        self.apply(
            &PathEntry {
                crate_id,
                path: vec![real],
                kind: "module".to_string(),
            },
            Scope::Item,
            item,
        );
    }

    /// The spec/01 rules for a resolved first-party origin O: never a
    /// glob (an enum's variants excepted — that set is bounded), never
    /// a module (the crate itself is kind `module`), never a macro,
    /// and an L2 crate (sb-service) re-exports nothing. Third-party
    /// origins aren't layering concerns.
    fn apply(&mut self, entry: &PathEntry, scope: Scope, item: &Item) {
        let Some(origin) = self.doc.external_crates.get(&entry.crate_id) else {
            return self.unverifiable(
                Some(item),
                "origin missing from `external_crates`".to_string(),
            );
        };
        let origin_name = underscored(&origin.name);
        if self.config.classify(&origin_name).is_none() {
            return;
        }
        let path = entry.path.join("::");
        let why = if matches!(scope, Scope::Glob) && entry.kind != "enum" {
            format!("glob re-export of `{origin_name}`")
        } else if entry.kind == "module" {
            format!("re-exports module `{path}` of `{origin_name}`; re-export items")
        } else if MACRO_KINDS.contains(&entry.kind.as_str()) {
            format!("re-exports macro `{path}` of `{origin_name}`")
        } else if self.config.classify(&self.member.krate) == Some(Class::Layer(2)) {
            format!("re-exports `{path}`; sb-service re-exports nothing (spec/01 rule 3)")
        } else {
            return; // a plain item re-export — the legal shape.
        };
        let (file, line) = self.spot(Some(item));
        self.out.push(Violation::edge(
            file,
            line,
            self.config,
            &self.member.krate,
            &origin_name,
            why,
        ));
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    /// A minimal format-61 doc: `index` plus resolved `paths`.
    fn doc(index: Value, paths: Value) -> CrateDoc {
        serde_json::from_value(json!({
            "format_version": FORMAT_VERSION,
            "root": 0,
            "index": index,
            "paths": paths,
            "external_crates": {"1": {"name": "sb_store"}},
        }))
        .expect("doc json")
    }

    fn module(id: u32, visibility: &str, items: Value, is_crate: bool) -> Value {
        json!({"id": id, "crate_id": 0, "name": null, "span": null,
               "visibility": visibility,
               "inner": {"module": {"is_crate": is_crate, "items": items, "is_stripped": false}}})
    }

    fn reexport(id: u32, source: &str, target: Value) -> Value {
        json!({"id": id, "crate_id": 0, "name": "x",
               "span": {"filename": "crates/sb-resources/src/lib.rs", "begin": [1, 0]},
               "visibility": "public",
               "inner": {"use": {"source": source, "name": "x", "id": target, "is_glob": false}}})
    }

    /// `sb-resources` (L4, may reach `sb_store` on L5) on a scratch
    /// config; the member's own paths are unused by the item checks.
    /// `tag` keeps parallel tests from sharing one file.
    fn member_and_config(tag: &str) -> (Member, Config) {
        let root = std::env::temp_dir().join(format!("layers-doc-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("layers.toml");
        std::fs::write(
            &file,
            "[crates]\nsb_resources = { layer = 4, deps = [\"sb_store\"] }\nsb_store = { layer = 5 }\n",
        )
        .unwrap();
        let config = crate::layers::config::load(&file).unwrap();
        let member = Member {
            id: "path+file:///x#sb-resources@0.0.0".to_string(),
            krate: "sb_resources".to_string(),
            package: "sb-resources".to_string(),
            dir: root.clone(),
            manifest: root.join("Cargo.toml"),
            has_lib: true,
            proc_macro: false,
            has_features: false,
            deps: vec![],
        };
        (member, config)
    }

    fn check(member: &Member, config: &Config, d: &CrateDoc) -> Vec<Violation> {
        let mut out = Vec::new();
        Check {
            member,
            manifest: PathBuf::from("crates/sb-resources/Cargo.toml"),
            doc: d,
            config,
            out: &mut out,
        }
        .run();
        out
    }

    /// rustdoc can still list private items (a stray
    /// `--document-private-items` flag); traversal must visit only
    /// `visibility: "public"` items.
    #[test]
    fn private_items_stay_private() {
        let (member, config) = member_and_config("vis");
        let paths = json!({"3": {"crate_id": 1, "path": ["sb_store", "inner"], "kind": "module"}});
        for (visibility, want) in [("crate", false), ("public", true)] {
            let d = doc(
                json!({
                    "0": module(0, "public", json!([1]), true),
                    "1": module(1, visibility, json!([2]), false),
                    "2": reexport(2, "sb_store::Store", json!(3)),
                }),
                paths.clone(),
            );
            let out = check(&member, &config, &d);
            assert_eq!(!out.is_empty(), want, "visibility {visibility:?}");
        }
    }

    /// `id: null` marks a primitive re-export, whatever its source
    /// spelling: never a violation.
    #[test]
    fn null_id_use_sources() {
        let (member, config) = member_and_config("null");
        let d = doc(
            json!({
                "0": module(0, "public", json!([1, 2]), true),
                "1": reexport(1, "core::primitive::u8", Value::Null),
                "2": reexport(2, "sb_store::Store", Value::Null),
            }),
            json!({}),
        );
        let out = check(&member, &config, &d);
        assert!(
            out.is_empty(),
            "null ids are primitives, got {} violations",
            out.len()
        );
    }
}
