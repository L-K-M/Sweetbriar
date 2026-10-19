//! Blocking composition for body leases. The engine owns their lifetime.
use std::path::{Path, PathBuf};

use sb_mime::{Limits, RenderKind, render, render_input};
use sb_platform::RealFs;

use crate::{
    Fs, RenderWarningCode, RenditionCache, RenditionError, RenditionFiles, RenditionProtection,
    Resources, StoreError,
};

/// spec/07: a reader refuses a manifest over this, so none is written.
const MAX_MANIFEST_BYTES: usize = 4_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyKind {
    SanitizedHtml,
    PlainText,
    Source,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BodyLease {
    pub lease_id: String,
    pub kind: BodyKind,
    pub file_path: String,
    pub byte_length: u64,
    pub remote_content_blocked: u32,
    pub warnings: Vec<crate::RenderWarning>,
    pub manifest_path: Option<String>,
}

/// Keep `files` alive until the engine revokes the public lease.
pub struct PublishedBody {
    pub lease: BodyLease,
    pub files: RenditionFiles,
}

fn unavailable(reason: impl Into<String>) -> StoreError {
    StoreError::Unavailable {
        reason: reason.into(),
    }
}

fn path_text(path: &Path) -> Result<String, StoreError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| unavailable("rendition paths must be valid UTF-8"))
}

fn cache_error(e: RenditionError) -> StoreError {
    unavailable(e.to_string())
}

/// `path` with its existing part resolved through symlinks and the
/// missing rest appended, so roots compare before either is created and
/// a rejected open creates no cache inside the data tree.
fn resolved(fs: &dyn Fs, path: &Path) -> std::io::Result<PathBuf> {
    if fs.metadata(path)?.is_some() {
        return fs.canonicalize(path);
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut root = resolved(fs, parent)?;
    root.push(
        path.file_name()
            .ok_or_else(|| std::io::Error::other("invalid root"))?,
    );
    Ok(root)
}

impl Resources {
    /// Validate canonical roots before touching the disposable namespace.
    /// A held cache lock disables leases, not the rest of the core.
    pub fn open_with_cache(data_root: &Path, cache_root: &Path) -> Result<Self, StoreError> {
        let data = resolved(&RealFs, data_root).map_err(|e| unavailable(e.to_string()))?;
        let cache = resolved(&RealFs, cache_root).map_err(|e| unavailable(e.to_string()))?;
        if cache.starts_with(&data) || data.starts_with(cache.join("renditions")) {
            return Err(StoreError::InvalidInput {
                message: "cache root must be outside the data root, and renditions must not contain the data root".into(),
            });
        }
        let mut resources = Self::open(data_root)?;
        resources.renditions = match RenditionCache::open(cache_root) {
            Ok(cache) => Some(cache),
            Err(RenditionError::Locked) => None,
            Err(e) => return Err(cache_error(e)),
        };
        Ok(resources)
    }

    pub fn publish_body(
        &self,
        message: i64,
        preferred: BodyKind,
    ) -> Result<PublishedBody, StoreError> {
        let acquisition = self
            .read(|tx| crate::best_acquisition(tx, message))?
            .ok_or_else(|| unavailable("body is not acquired"))?;
        let cache = self
            .renditions
            .as_ref()
            .ok_or_else(|| unavailable("rendition cache is in use or not configured"))?;
        let bytes = self
            .read_acquisition(&acquisition)
            .map_err(|error| match error {
                StoreError::NotFound { .. } => {
                    unavailable("body is not acquired: acquisition object is missing")
                }
                other => other,
            })?;
        if preferred == BodyKind::Source {
            let mut files = cache.reserve(&[bytes.len() as u64]).map_err(cache_error)?;
            let path = files
                .publish_named(0, "source.eml", &bytes, RenditionProtection::Internal)
                .map_err(cache_error)?;
            return Ok(PublishedBody {
                lease: BodyLease {
                    lease_id: files.token().into(),
                    kind: BodyKind::Source,
                    file_path: path_text(&path)?,
                    byte_length: bytes.len() as u64,
                    remote_content_blocked: 0,
                    warnings: vec![],
                    manifest_path: None,
                },
                files,
            });
        }
        let input = render_input(&bytes, &Limits::default())
            .map_err(|e| unavailable(format!("cannot render body: {e:?}")))?;
        let preferred = match preferred {
            BodyKind::SanitizedHtml => RenderKind::Html,
            _ => RenderKind::PlainText,
        };
        // Reserve exact sizes before publication. Tokens have a fixed length,
        // so rendering with the reservation's token preserves the sizes.
        let rendition = render(&input, &"0".repeat(64), preferred);
        // spec/07: past a budget the open is Unavailable, not the
        // renderer's placeholder.
        if rendition
            .warnings()
            .iter()
            .any(|warning| warning.code() == RenderWarningCode::RenditionLimit)
        {
            return Err(unavailable("rendition quota: 50,000,000 bytes per file"));
        }
        let manifest = rendition.manifest().to_json();
        if manifest.len() > MAX_MANIFEST_BYTES {
            return Err(unavailable("rendition quota: 4,000,000 bytes per manifest"));
        }
        let mut sizes = vec![rendition.content().len() as u64, manifest.len() as u64];
        sizes.extend(
            rendition
                .manifest()
                .resources()
                .iter()
                .map(|r| r.bytes().len() as u64),
        );
        let mut files = cache.reserve(&sizes).map_err(cache_error)?;
        drop(rendition);
        drop(manifest);
        let rendition = render(&input, files.token(), preferred);
        let manifest = rendition.manifest().to_json();
        let kind = match rendition.kind() {
            RenderKind::Html => BodyKind::SanitizedHtml,
            RenderKind::PlainText => BodyKind::PlainText,
        };
        let name = if kind == BodyKind::SanitizedHtml {
            "body.html"
        } else {
            "body.txt"
        };
        let path = files
            .publish_named(
                0,
                name,
                rendition.content().as_bytes(),
                RenditionProtection::Internal,
            )
            .map_err(cache_error)?;
        for (i, resource) in rendition.manifest().resources().iter().enumerate() {
            files
                .publish_named(
                    i + 2,
                    resource.file(),
                    resource.bytes(),
                    RenditionProtection::Internal,
                )
                .map_err(cache_error)?;
        }
        let manifest_path = files
            .publish_named(
                1,
                "manifest.json",
                manifest.as_bytes(),
                RenditionProtection::Internal,
            )
            .map_err(cache_error)?;
        Ok(PublishedBody {
            lease: BodyLease {
                lease_id: files.token().into(),
                kind,
                file_path: path_text(&path)?,
                byte_length: rendition.content().len() as u64,
                remote_content_blocked: rendition.remote_content_blocked(),
                warnings: rendition.warnings().to_vec(),
                manifest_path: Some(path_text(&manifest_path)?),
            },
            files,
        })
    }
}
