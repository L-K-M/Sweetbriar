//! Retained restore paths for Settings → Backups (spec/07; M1.8j1).
//! These are names to inspect, never authority to delete or reuse anything.

use std::ops::ControlFlow;
use std::path::PathBuf;

use crate::{Store, StoreError, Uuid};

/// One name beside the data root that follows the restore naming protocol.
/// Its contents, type and ownership have not been inspected. It may still be
/// in use, or disappear after enumeration; it is never a reusable stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreArtifact {
    pub path: PathBuf,
    pub kind: RestoreArtifactKind,
}

/// Why the restore protocol uses this sibling name, not a claim about its bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreArtifactKind {
    Preserved,
    Staged,
}

impl Store {
    /// List retained restore names beside the resolved data root, sorted by
    /// path, without opening/following entries or changing anything. Works
    /// while latched. Enumeration errors discard the entire partial answer.
    pub fn restore_artifacts(&self) -> Result<Vec<RestoreArtifact>, StoreError> {
        let root = self.paths.data_root();
        let name = crate::restore_pending::root_name(root)?;
        let parent = root
            .parent()
            .ok_or_else(|| StoreError::corrupt("the data root has no parent"))?;
        let prefixes = [
            (format!("{name}.preserved-"), RestoreArtifactKind::Preserved),
            (format!("{name}.restoring-"), RestoreArtifactKind::Staged),
        ];
        let unavailable = |error: std::io::Error| {
            StoreError::unavailable_io(
                format!(
                    "list retained restore paths beside {}: {error}",
                    parent.display()
                ),
                error.kind(),
            )
        };
        let directory = self
            .seams
            .fs()
            .open_confined_dir(parent)
            .map_err(unavailable)?;
        let mut artifacts = Vec::new();
        let _ = directory
            .visit_children(&mut |child| {
                let Some(name) = child.to_str() else {
                    return Ok(ControlFlow::Continue(()));
                };
                for (prefix, kind) in &prefixes {
                    if let Some(id) = name.strip_prefix(prefix)
                        && id.parse::<Uuid>().is_ok_and(|uuid| uuid.to_string() == id)
                    {
                        artifacts.push(RestoreArtifact {
                            path: parent.join(child),
                            kind: *kind,
                        });
                        break;
                    }
                }
                Ok(ControlFlow::Continue(()))
            })
            .map_err(unavailable)?;
        artifacts.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(artifacts)
    }
}

#[cfg(test)]
mod tests;
