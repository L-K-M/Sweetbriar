//! `layers.toml` parsing: the `[crates]` table maps every workspace
//! crate — by crate name, underscored — to a layer (1–6) or a role
//! (`dev`, `tool`), and lists the first-party crates each layered
//! crate may depend on (spec/01 rule 1, spelled out per crate).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Deserialize;

use super::{Class, Error};

/// The checked-in shape of `layers.toml`.
#[derive(Deserialize)]
struct LayersFile {
    /// `sb_store = { layer = 5, deps = ["sb_types"] }`,
    /// `xtask = { role = "tool" }`.
    #[serde(default)]
    crates: BTreeMap<String, CrateSpec>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum CrateSpec {
    /// A layered crate and the first-party crates it may depend on.
    Layer {
        layer: u8,
        #[serde(default)]
        deps: BTreeSet<String>,
    },
    /// `dev`/`tool`: outside the layer graph, unrestricted.
    Role { role: RoleName },
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum RoleName {
    Dev,
    Tool,
}

/// The parsed policy.
pub(crate) struct Config {
    /// Crate name → classification. Absent means unclassified — a
    /// violation when a workspace member isn't listed.
    classes: BTreeMap<String, Class>,
    /// Layered crate name → the first-party crates it may depend on
    /// (normal, dev and build dependencies alike).
    deps: BTreeMap<String, BTreeSet<String>>,
}

impl Config {
    /// What `krate` is classified as; `None` is a violation for a
    /// workspace member.
    pub(crate) fn classify(&self, krate: &str) -> Option<Class> {
        self.classes.get(krate).copied()
    }

    /// May `from` list `to` as a first-party dependency?
    pub(crate) fn dep_allowed(&self, from: &str, to: &str) -> bool {
        self.deps.get(from).is_some_and(|set| set.contains(to))
    }
}

/// Loads and validates `layers.toml`.
pub(crate) fn load(path: &Path) -> Result<Config, Error> {
    let err = |what: String| Error::Config(format!("{}: {what}", path.display()));
    let text = std::fs::read_to_string(path).map_err(|e| err(format!("{e}")))?;
    let parsed: LayersFile = toml::from_str(&text).map_err(|e| err(format!("{e}")))?;

    let mut classes = BTreeMap::new();
    let mut deps = BTreeMap::new();
    for (name, spec) in &parsed.crates {
        if !is_crate_name(name) {
            return Err(err(format!(
                "crate key `{name}` must be an underscored crate name"
            )));
        }
        match spec {
            CrateSpec::Layer { layer, deps: list } => {
                if !(1..=6).contains(layer) {
                    return Err(err(format!(
                        "crate `{name}` has layer {layer}; layers run 1–6"
                    )));
                }
                classes.insert(name.clone(), Class::Layer(*layer));
                deps.insert(name.clone(), list.clone());
            }
            CrateSpec::Role { role } => {
                classes.insert(
                    name.clone(),
                    match role {
                        RoleName::Dev => Class::Dev,
                        RoleName::Tool => Class::Tool,
                    },
                );
            }
        }
    }

    // A dep on a name nothing classifies can never be allowed; catch
    // the typo at load time instead of failing the edge opaquely.
    for (from, list) in &deps {
        for to in list {
            if !classes.contains_key(to) {
                return Err(err(format!(
                    "`{from}` lists `{to}` in deps, but `{to}` is not classified"
                )));
            }
        }
    }

    Ok(Config { classes, deps })
}

/// Crate names are Rust identifiers (underscored); hyphens belong to
/// package names.
fn is_crate_name(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c == '_' || c.is_alphabetic())
        && chars.all(|c| c == '_' || c.is_alphanumeric())
}
