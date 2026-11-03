//! Bundle metadata stored with an import source.

use sb_store::ImportSourceSnapshot;

use super::SourceKind;

/// The format a manifest declares for every file it covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BundleFormat {
    /// One `.eml` file per record.
    Eml,
    /// One mboxrd `.mbox` file per mailbox.
    Mboxrd,
}

impl BundleFormat {
    /// The name the manifest and the inventory JSON spell.
    pub fn name(self) -> &'static str {
        match self {
            Self::Eml => "eml",
            Self::Mboxrd => "mboxrd",
        }
    }

    pub(crate) fn from_name(name: &str) -> Option<Self> {
        match name {
            "eml" => Some(Self::Eml),
            "mboxrd" => Some(Self::Mboxrd),
            _ => None,
        }
    }

    /// The source kind a file of this format has.
    pub(crate) fn source_kind(self) -> SourceKind {
        match self {
            Self::Eml => SourceKind::Eml,
            Self::Mboxrd => SourceKind::Mbox,
        }
    }
}

/// A bundle source's binding to its manifest: the private snapshot the
/// manifest was captured into and the format it declares (spec/09).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceBundle {
    manifest: ImportSourceSnapshot,
    format: BundleFormat,
}

impl SourceBundle {
    pub(crate) fn new(manifest: ImportSourceSnapshot, format: BundleFormat) -> Self {
        Self { manifest, format }
    }

    /// The verified private copy of the bundle's manifest.
    pub fn manifest(&self) -> &ImportSourceSnapshot {
        &self.manifest
    }

    /// The format the manifest declares: `eml` or `mboxrd`.
    pub fn format(&self) -> BundleFormat {
        self.format
    }
}
