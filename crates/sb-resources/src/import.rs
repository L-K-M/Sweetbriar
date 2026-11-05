//! Deterministic discovery and durable source inventory (spec/09).
//! Counts and quoting requirements describe private snapshots (spec/03),
//! so later changes to originals cannot change an inventoried source.

use std::collections::HashSet;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};

use sb_mime::{MboxError, MboxInventory, MboxQuoting, inventory_mbox};
use sb_store::imports::MAX_IMPORT_CHANGES_BYTES;
use sb_store::{FileKind, Fs, ImportSourceCapture, ImportSourceSnapshot, Store, StoreError};

pub(crate) mod bundle;
mod json_syntax;
mod offset_sort;
pub use bundle::{BundleFormat, SourceBundle};

/// The metadata object's format version (spec/09). A bundle source
/// records version 2: the version-1 fields plus its `bundle` object.
const FORMAT_VERSION: u64 = 1;
const BUNDLE_FORMAT_VERSION: u64 = 2;
const MBOX_SEPARATOR_MIN_BYTES: u64 = b"From ".len() as u64;

/// What a discovered source's bytes are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    /// An mbox stream of zero or more message candidates.
    Mbox,
    /// A single message file; its count is always one.
    Eml,
}

impl SourceKind {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Mbox => "mbox",
            Self::Eml => "eml",
        }
    }
}

/// One step of inventory work, handed to the progress callback;
/// `Break` cancels the whole inventory.
#[derive(Debug)]
pub enum ImportProgress<'a> {
    /// Discovery is about to inspect `path`.
    Checking { path: &'a Path },
    /// `bytes` of `path` have been copied into its private snapshot.
    Capturing { path: &'a Path, bytes: u64 },
    /// `bytes` read from `path`'s snapshot for inventory scanning.
    Scanning { path: &'a Path, bytes: u64 },
}

/// Why one requested or discovered path did not become an inventoried
/// source (spec/09: reported visibly, never treated as empty).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportProblemKind {
    /// The path could not be resolved, listed or opened.
    Inaccessible,
    /// Not a regular file or directory, a package without a regular
    /// `mbox` file, or a name no mailbox can be derived from.
    Unsupported,
    /// The path is not UTF-8, so it cannot name a source or mailbox.
    NonUtf8,
    /// The snapshot copy or the mbox scan failed; the detail keeps the
    /// store or scan error, including the source offset for scan I/O.
    Failed,
}

impl ImportProblemKind {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Inaccessible => "inaccessible",
            Self::Unsupported => "unsupported",
            Self::NonUtf8 => "non_utf8",
            Self::Failed => "failed",
        }
    }

    pub(crate) fn from_name(name: &str) -> Option<Self> {
        [
            Self::Inaccessible,
            Self::Unsupported,
            Self::NonUtf8,
            Self::Failed,
        ]
        .into_iter()
        .find(|kind| kind.name() == name)
    }
}

/// One reported path and why it was skipped. A problem never discards
/// other discovered sources.
#[derive(Debug, Clone)]
pub struct ImportProblem {
    path: PathBuf,
    kind: ImportProblemKind,
    detail: String,
}

impl ImportProblem {
    pub(crate) fn new(path: PathBuf, kind: ImportProblemKind, detail: String) -> Self {
        Self { path, kind, detail }
    }

    /// The failed path, canonical when resolution succeeded.
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn kind(&self) -> ImportProblemKind {
        self.kind
    }
    /// The underlying error, keeping the scan's source offset.
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

/// Immutable source identity and count, backed by a durable snapshot.
/// Variant choices live in checkpoints rather than this inventory.
#[derive(Debug, Clone)]
pub struct SourceInventory {
    path: PathBuf,
    kind: SourceKind,
    mailbox: String,
    snapshot: ImportSourceSnapshot,
    messages: u64,
    quoting: MboxQuoting,
    bundle: Option<SourceBundle>,
}

impl SourceInventory {
    /// The canonical source path recorded by the job.
    /// UTF-8 by construction and by [`SourceInventory::from_json`].
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn kind(&self) -> SourceKind {
        self.kind
    }
    /// The mailbox the source imports into (spec/09): the file or
    /// package stem, or the folder an EML was discovered in.
    /// Nonempty and NUL-free.
    pub fn mailbox(&self) -> &str {
        &self.mailbox
    }
    /// The private snapshot the count was taken from (spec/03).
    pub fn snapshot(&self) -> &ImportSourceSnapshot {
        &self.snapshot
    }
    /// Candidate messages: `From ` separators for mbox, one for EML.
    /// An empty mbox is a valid inventory of zero.
    pub fn message_count(&self) -> u64 {
        self.messages
    }
    /// Whether the bytes decide the unquoting variant; `Ambiguous`
    /// means a `>>+From ` line exists and the job must ask (spec/09).
    pub fn quoting(&self) -> MboxQuoting {
        self.quoting
    }

    /// For a bundle source, the manifest it verified against (spec/09
    /// "Export bundles"); a plain source has none.
    pub fn bundle(&self) -> Option<&SourceBundle> {
        self.bundle.as_ref()
    }

    /// Every snapshot this source's inventory references: its own
    /// copy, plus the shared manifest's for a bundle source. Members
    /// of one bundle share that capture, so callers removing the
    /// snapshots dedupe by token.
    pub fn snapshots(&self) -> impl Iterator<Item = &ImportSourceSnapshot> + '_ {
        std::iter::once(&self.snapshot).chain(self.bundle.iter().map(|b| b.manifest()))
    }

    /// Version-1 `import_job_sources.source_json` for a plain source,
    /// or version 2 — the same fields plus `bundle` — for a bundle
    /// source, with sorted keys (spec/09).
    pub fn to_json(&self) -> String {
        let quoting = match self.quoting {
            MboxQuoting::Equivalent => "equivalent",
            MboxQuoting::Ambiguous => "ambiguous",
        };
        let mut value = serde_json::json!({
            "v": if self.bundle.is_some() {
                BUNDLE_FORMAT_VERSION
            } else {
                FORMAT_VERSION
            },
            "path": self.path.to_str().expect("source paths are UTF-8"),
            "mailbox": self.mailbox,
            "kind": self.kind.name(),
            "snapshot": {
                "token": self.snapshot.token(),
                "sha256": self.snapshot.sha256_hex(),
                "byte_length": self.snapshot.byte_length(),
            },
            "messages": self.messages,
            "quoting": quoting,
        });
        if let Some(bundle) = &self.bundle {
            value["bundle"] = serde_json::json!({
                "manifest": {
                    "token": bundle.manifest().token(),
                    "sha256": bundle.manifest().sha256_hex(),
                    "byte_length": bundle.manifest().byte_length(),
                },
                "format": bundle.format().name(),
            });
        }
        value.to_string()
    }

    /// Rebuild validated metadata. Version 1 is a plain source; version 2
    /// adds the required `bundle` object (spec/09 "Export bundles").
    /// Oversized JSON is refused before parsing; malformed fields and
    /// inconsistent counts are `InvalidInput`.
    pub fn from_json(json: &str) -> Result<Self, StoreError> {
        if json.len() > MAX_IMPORT_CHANGES_BYTES {
            return Err(invalid("import source metadata exceeds the write budget"));
        }
        let value: serde_json::Value =
            serde_json::from_str(json).map_err(|e| invalid(format!("import source JSON: {e}")))?;
        let object = value
            .as_object()
            .ok_or_else(|| invalid("import source metadata is not an object"))?;
        let bundle = match field(object, "v")?.as_u64() {
            Some(FORMAT_VERSION) => {
                if object.contains_key("bundle") {
                    return Err(invalid("a version-1 import source carries no bundle"));
                }
                None
            }
            Some(BUNDLE_FORMAT_VERSION) => Some(bundle_field(object)?),
            _ => return Err(invalid("import source metadata must have v: 1 or 2")),
        };
        let path = required_str(object, "path")?;
        if path.is_empty() || path.contains('\0') || !Path::new(path).is_absolute() {
            return Err(invalid(
                "an import source path must be a nonempty absolute path",
            ));
        }
        let mailbox = required_str(object, "mailbox")?;
        if mailbox.is_empty() || mailbox.contains('\0') {
            return Err(invalid(
                "an import source mailbox must be nonempty and NUL-free",
            ));
        }
        let kind = match required_str(object, "kind")? {
            "mbox" => SourceKind::Mbox,
            "eml" => SourceKind::Eml,
            other => return Err(invalid(format!("unknown import source kind {other:?}"))),
        };
        let quoting = match required_str(object, "quoting")? {
            "equivalent" => MboxQuoting::Equivalent,
            "ambiguous" => MboxQuoting::Ambiguous,
            other => return Err(invalid(format!("unknown import quoting {other:?}"))),
        };
        let messages = required_u64(object, "messages")?;
        let snapshot = snapshot_field(object, "snapshot")?;
        match kind {
            // One file is one candidate; no `From ` unquoting applies.
            SourceKind::Eml if messages != 1 || quoting != MboxQuoting::Equivalent => {
                return Err(invalid("an EML source is one message with no quoting"));
            }
            // Each message needs a `From ` separator of at least five
            // bytes, so no count can exceed length / 5.
            SourceKind::Mbox if messages > snapshot.byte_length() / MBOX_SEPARATOR_MIN_BYTES => {
                return Err(invalid("import source count exceeds its byte length"));
            }
            SourceKind::Mbox
                if messages == 0
                    && (snapshot.byte_length() != 0 || quoting != MboxQuoting::Equivalent) =>
            {
                return Err(invalid("an empty mbox has no bytes or quoting requirement"));
            }
            _ => (),
        }
        if let Some(bundle) = &bundle {
            if bundle.format().source_kind() != kind {
                return Err(invalid("a bundle source's format must match its kind"));
            }
            // The manifest fixes mboxrd unquoting, so a bundle source
            // never carries a variant question (spec/09).
            if quoting != MboxQuoting::Equivalent {
                return Err(invalid(
                    "a bundle source's quoting is decided by its manifest",
                ));
            }
        }
        Ok(Self {
            path: PathBuf::from(path),
            kind,
            mailbox: mailbox.to_owned(),
            snapshot,
            messages,
            quoting,
            bundle,
        })
    }
}

/// What an inventory pass produced: accepted sources in deterministic
/// order plus one problem per rejected path.
#[derive(Debug, Default)]
pub struct ImportInventory {
    sources: Vec<SourceInventory>,
    problems: Vec<ImportProblem>,
}

impl ImportInventory {
    pub fn sources(&self) -> &[SourceInventory] {
        &self.sources
    }
    pub fn problems(&self) -> &[ImportProblem] {
        &self.problems
    }
}

/// The inventory's terminal state (spec/09). Cancellation is an
/// outcome alongside `Complete`, never masked as success or error.
#[derive(Debug)]
pub enum ImportInventoryOutcome {
    /// The walk finished; `problems` identifies rejected paths.
    Complete(ImportInventory),
    /// The progress callback asked to stop. Completed sources and
    /// problems seen so far are kept; a capture the walk never counted
    /// is removed, since nothing references it (spec/03).
    Cancelled(ImportInventory),
}

impl ImportInventoryOutcome {
    /// The inventory built so far, complete or partial.
    pub fn inventory(&self) -> &ImportInventory {
        match self {
            Self::Complete(inventory) | Self::Cancelled(inventory) => inventory,
        }
    }
}

/// Walk `roots` in request order, then capture and count each
/// accepted source (spec/09). Deterministic: the same tree and the
/// same `roots` give the same sources and problems in the same order.
pub(crate) fn inventory(
    store: &Store,
    fs: &dyn Fs,
    roots: &[PathBuf],
    mut progress: impl FnMut(ImportProgress<'_>) -> ControlFlow<()>,
) -> ImportInventoryOutcome {
    let mut walker = Walker {
        store,
        fs,
        progress: &mut progress,
        seen_dirs: HashSet::new(),
        seen_files: HashSet::new(),
        inventory: ImportInventory::default(),
        cancelled: false,
        pending: roots
            .iter()
            .rev()
            .map(|path| (path.clone(), Origin::Chosen))
            .collect(),
    };
    while let Some((path, origin)) = walker.pending.pop() {
        if walker.check(&path) {
            break;
        }
        walker.visit(path, origin);
        if walker.cancelled {
            break;
        }
    }
    if walker.cancelled {
        ImportInventoryOutcome::Cancelled(walker.inventory)
    } else {
        ImportInventoryOutcome::Complete(walker.inventory)
    }
}

/// Discovery state for one inventory pass: the canonical sets that
/// visit each directory and source file once, plus the output being
/// built.
struct Walker<'a> {
    store: &'a Store,
    fs: &'a dyn Fs,
    progress: &'a mut dyn FnMut(ImportProgress<'_>) -> ControlFlow<()>,
    seen_dirs: HashSet<PathBuf>,
    seen_files: HashSet<PathBuf>,
    inventory: ImportInventory,
    cancelled: bool,
    pending: Vec<(PathBuf, Origin)>,
}

#[derive(Clone, Copy)]
enum Origin {
    Chosen,
    Listed,
}

/// What scanning one snapshot's bytes produced.
enum Scan {
    Counted(MboxInventory),
    /// The progress callback broke during a scan read.
    Cancelled,
    /// A real scan failure; `MboxError` keeps the source offset.
    Failed(MboxError),
}

impl Walker<'_> {
    /// The shared progress check; a break latches so the walk stops.
    fn check(&mut self, path: &Path) -> bool {
        self.cancelled =
            self.cancelled || (self.progress)(ImportProgress::Checking { path }).is_break();
        self.cancelled
    }

    /// Resolve identity separately from the name used for classification
    /// and mailbox naming. The pending stack preserves depth-first order.
    fn visit(&mut self, named: PathBuf, origin: Origin) {
        if named.to_str().is_none() {
            return self.problem(&named, ImportProblemKind::NonUtf8, "the path is not UTF-8");
        }
        let Some(canonical) = self.canonicalized(&named) else {
            return;
        };
        if canonical.to_str().is_none() {
            return self.problem(
                &canonical,
                ImportProblemKind::NonUtf8,
                "the path is not UTF-8",
            );
        }
        let eml = has_extension(&named, "eml");
        match self.classify(&canonical) {
            Ok(FileKind::File) => {
                if matches!(origin, Origin::Chosen)
                    && (named
                        .file_name()
                        .is_some_and(|name| name == crate::export_files::MANIFEST_NAME)
                        || canonical
                            .file_name()
                            .is_some_and(|name| name == crate::export_files::MANIFEST_NAME))
                {
                    return self.problem(
                        &canonical,
                        ImportProblemKind::Unsupported,
                        "select the export bundle directory, not its manifest",
                    );
                }
                if matches!(origin, Origin::Listed) && !eml && !has_extension(&named, "mbox") {
                    return;
                }
                let name = match (eml, origin) {
                    (true, Origin::Listed) => folder_name(&named),
                    _ => stem(&named),
                };
                match name {
                    Some(name) => {
                        let kind = if eml {
                            SourceKind::Eml
                        } else {
                            SourceKind::Mbox
                        };
                        self.accept(canonical, kind, name);
                    }
                    None => self.name_problem(&canonical),
                }
            }
            Ok(FileKind::Directory) => {
                if !self.seen_dirs.insert(canonical.clone()) {
                    return;
                }
                let manifest = canonical.join(crate::export_files::MANIFEST_NAME);
                match self.fs.metadata(&manifest) {
                    Ok(Some(md)) if md.kind() == FileKind::File => {
                        return self.bundle(&canonical, &manifest);
                    }
                    Ok(_) => {}
                    Err(error) => return self.problem_io(&manifest, error),
                }
                if has_extension(&named, "mbox") || has_extension(&canonical, "mbox") {
                    return self.package(&canonical, &named);
                }
                self.walk_dir(&canonical);
            }
            Ok(FileKind::Other) => self.problem(
                &canonical,
                ImportProblemKind::Unsupported,
                "not a file or directory",
            ),
            Err(error) => self.problem_io(&canonical, error),
        }
    }

    /// A recognized bundle never enters ordinary discovery. The envelope
    /// is checked before later record validation can admit a source.
    fn bundle(&mut self, dir: &Path, manifest: &Path) {
        let entries = match self.fs.read_dir(dir) {
            Ok(entries) => entries,
            Err(error) => return self.problem_io(dir, error),
        };
        let mut children: Vec<_> = entries.into_iter().map(|path| (path, None)).collect();
        // Fs returns sorted paths; UTF-8 byte order agrees with decoded
        // scalar order. Non-UTF-8 names are skipped by the merge and
        // reported. The child callback fires per declaration group in
        // the relationship sort's ascending order — declaration order
        // in the manifest itself is irrelevant — so the two sequences
        // merge in one pass.
        let mut cursor = 0;
        let mut missing = Vec::new();
        let mut missing_overflow = 0u64;
        let captured = self.store.capture_import_source(manifest, |bytes| {
            (self.progress)(ImportProgress::Capturing {
                path: manifest,
                bytes,
            })
        });
        let snapshot = match captured {
            Ok(ImportSourceCapture::Captured { snapshot, .. }) => snapshot,
            Ok(ImportSourceCapture::Cancelled) => {
                self.cancelled = true;
                return;
            }
            Err(error) => return self.failed(dir, error),
        };
        let result = match self
            .store
            .open_import_source_cancellable(&snapshot, |bytes| {
                (self.progress)(ImportProgress::Scanning {
                    path: manifest,
                    bytes,
                })
            }) {
            Ok(sb_store::Scoped::Stopped) => {
                self.cancelled = true;
                None
            }
            Ok(sb_store::Scoped::Done(file)) => {
                let mut reader = BufReader::new(ScanReader {
                    inner: file,
                    path: manifest,
                    scanned: 0,
                    cancelled: false,
                    progress: &mut *self.progress,
                });
                let result = json_syntax::validate_envelope(&mut reader).and_then(|table| {
                    let scratch = self.store.import_validation_scratch().map_err(|e| {
                        io::Error::new(e.io_kind().unwrap_or(io::ErrorKind::Other), e)
                    })?;
                    json_syntax::validate_messages_with_children(
                        &mut reader,
                        &table,
                        &scratch,
                        &mut |reader, offset, bytes, records| {
                            while cursor < children.len() {
                                let Some(name) =
                                    children[cursor].0.file_name().and_then(|n| n.to_str())
                                else {
                                    cursor += 1;
                                    continue;
                                };
                                match json_syntax::compare_child(reader, offset, name)? {
                                    std::cmp::Ordering::Greater => cursor += 1,
                                    std::cmp::Ordering::Equal => {
                                        children[cursor].1 = Some((bytes, records));
                                        cursor += 1;
                                        return Ok(());
                                    }
                                    std::cmp::Ordering::Less => break,
                                }
                            }
                            if missing.len() < 64 {
                                missing.push(json_syntax::child_prefix(reader, offset)?);
                            } else {
                                missing_overflow = missing_overflow.saturating_add(1);
                            }
                            Ok(())
                        },
                    )?;
                    scratch.finish().map_err(|e| {
                        io::Error::new(e.io_kind().unwrap_or(io::ErrorKind::Other), e)
                    })?;
                    Ok(table.format())
                });
                if reader.get_ref().cancelled {
                    self.cancelled = true;
                    None
                } else {
                    Some(result.map_err(|error| error.to_string()))
                }
            }
            Err(error) => Some(Err(error.to_string())),
        };
        self.discard(&snapshot);
        match result {
            None => {}
            Some(Err(error)) => self.failed(dir, error),
            Some(Ok(format)) => {
                let mut valid = true;
                for (name, truncated) in missing {
                    let path = if truncated {
                        dir.to_path_buf()
                    } else {
                        dir.join(&name)
                    };
                    self.failed(
                        &path,
                        format!(
                            "missing declared child {name:?}{}",
                            if truncated { " (truncated)" } else { "" }
                        ),
                    );
                    valid = false;
                }
                if missing_overflow > 0 {
                    self.failed(
                        dir,
                        format!("{missing_overflow} more declared children are missing"),
                    );
                    valid = false;
                }
                for (path, declaration) in children {
                    if self.check(&path) {
                        return;
                    }
                    if path.to_str().is_none() {
                        self.problem(&path, ImportProblemKind::NonUtf8, "the path is not UTF-8");
                        valid = false;
                    } else if let Some((bytes, records)) = declaration {
                        if !self.check_bundle_child(&path, format, bytes, records) {
                            valid = false;
                        }
                        if self.cancelled {
                            return;
                        }
                    } else if has_extension(&path, "eml") || has_extension(&path, "mbox") {
                        match self.fs.metadata(&path) {
                            Ok(Some(md)) if md.kind() == FileKind::File => {
                                self.failed(&path, "bundle file is not listed in the manifest");
                                valid = false;
                            }
                            Err(error) => {
                                self.failed(&path, error);
                                valid = false;
                            }
                            Ok(None) => {
                                self.failed(&path, "bundle child disappeared during discovery");
                                valid = false;
                            }
                            _ => {}
                        }
                    }
                }
                if valid {
                    self.problem(
                        dir,
                        ImportProblemKind::Unsupported,
                        "export bundle manifest captured; bundle import is not yet supported",
                    );
                }
            }
        }
    }

    fn check_bundle_child(
        &mut self,
        path: &Path,
        format: BundleFormat,
        bytes: u64,
        records: u64,
    ) -> bool {
        let extension = match format {
            BundleFormat::Eml => "eml",
            BundleFormat::Mboxrd => "mbox",
        };
        if !has_extension(path, extension) {
            self.failed(path, "bundle child format does not match the manifest");
            return false;
        }
        let capture = self.store.capture_import_source(path, |bytes| {
            (self.progress)(ImportProgress::Capturing { path, bytes })
        });
        let snapshot = match capture {
            Ok(ImportSourceCapture::Captured { snapshot, .. }) => snapshot,
            Ok(ImportSourceCapture::Cancelled) => {
                self.cancelled = true;
                return false;
            }
            Err(error) => {
                self.failed(path, error);
                return false;
            }
        };
        let result = if snapshot.byte_length() != bytes {
            Err("bundle child byte length differs from the manifest".to_owned())
        } else {
            match self
                .store
                .open_import_source_cancellable(&snapshot, |bytes| {
                    (self.progress)(ImportProgress::Scanning { path, bytes })
                }) {
                Ok(sb_store::Scoped::Stopped) => {
                    self.cancelled = true;
                    Ok(())
                }
                // A zero-byte EML is a legitimate emitted child: an
                // acquired-but-empty stored object exports as an empty
                // file that still counts as one record.
                Ok(sb_store::Scoped::Done(_)) if format == BundleFormat::Eml && records == 1 => {
                    Ok(())
                }
                Ok(sb_store::Scoped::Done(_)) if format == BundleFormat::Eml => {
                    Err("bundle child record count differs from the manifest".to_owned())
                }
                Ok(sb_store::Scoped::Done(file)) => match scan_snapshot(
                    path,
                    file.take(bytes.saturating_add(1)),
                    &mut *self.progress,
                ) {
                    Scan::Counted(inventory)
                        if inventory.source_bytes() == bytes && inventory.messages() == records =>
                    {
                        Ok(())
                    }
                    Scan::Counted(_) => Err(
                        "bundle child record count or length differs from the manifest".to_owned(),
                    ),
                    Scan::Cancelled => {
                        self.cancelled = true;
                        Ok(())
                    }
                    Scan::Failed(error) => Err(error.to_string()),
                },
                Err(error) => Err(error.to_string()),
            }
        };
        self.discard(&snapshot);
        if let Err(error) = result {
            self.failed(path, error);
            return false;
        }
        !self.cancelled
    }

    /// A directory contributes its sorted entries once; canonical
    /// dedupe ends symlink cycles and overlapping roots.
    fn walk_dir(&mut self, dir: &Path) {
        let entries = match self.fs.read_dir(dir) {
            Ok(entries) => entries,
            Err(error) => return self.problem_io(dir, error),
        };
        self.pending
            .extend(entries.into_iter().rev().map(|path| (path, Origin::Listed)));
    }

    /// An Apple-style package contributes only its `mbox` file;
    /// `Info.plist` and friends are sidecars (spec/09). `dir` is the
    /// canonical package path; `named` is the spelling the mailbox
    /// takes its stem from.
    fn package(&mut self, dir: &Path, named: &Path) {
        let child = match self.fs.canonicalize(&dir.join("mbox")) {
            Ok(child) => child,
            Err(error) => return self.problem_io(dir, error),
        };
        match self.fs.open_regular(&child) {
            Ok(_) => match stem(named) {
                Some(name) => self.accept(child, SourceKind::Mbox, name),
                None => self.name_problem(dir),
            },
            Err(error) if error.kind() == io::ErrorKind::InvalidInput => self.problem(
                dir,
                ImportProblemKind::Unsupported,
                "the package's mbox entry is not a regular file",
            ),
            Err(error) => self.problem_io(dir, error),
        }
    }

    /// Capture, then count: the inventory binds to the private
    /// snapshot, never to the original's later bytes. The first
    /// spelling wins; aliases and overlapping roots resolve to the
    /// same canonical file (spec/09).
    fn accept(&mut self, canonical: PathBuf, kind: SourceKind, mailbox: String) {
        if !self.seen_files.insert(canonical.clone()) {
            return;
        }
        if canonical.to_str().is_none() {
            return self.problem(
                &canonical,
                ImportProblemKind::NonUtf8,
                "the path is not UTF-8",
            );
        }
        let store = self.store;
        let capture = store.capture_import_source(&canonical, |bytes| {
            (self.progress)(ImportProgress::Capturing {
                path: &canonical,
                bytes,
            })
        });
        match capture {
            Ok(ImportSourceCapture::Captured {
                source_path,
                snapshot,
            }) => {
                if source_path.to_str().is_none() {
                    return self.name_problem(&source_path);
                }
                self.count(source_path, kind, mailbox, snapshot)
            }
            Ok(ImportSourceCapture::Cancelled) => self.cancelled = true,
            Err(error) => self.failed(&canonical, error),
        }
    }

    /// Count the captured copy. An EML file is one candidate; an
    /// mbox is scanned from its verified snapshot, with one hash pass
    /// per source and cancellation checks inside scan reads.
    fn count(
        &mut self,
        canonical: PathBuf,
        kind: SourceKind,
        mailbox: String,
        snapshot: ImportSourceSnapshot,
    ) {
        let (messages, quoting) = match kind {
            SourceKind::Eml => (1, MboxQuoting::Equivalent),
            SourceKind::Mbox => {
                let file = match self.store.open_import_source(&snapshot) {
                    Ok(file) => file,
                    Err(error) => return self.failed(&canonical, error),
                };
                let expected = snapshot.byte_length();
                match scan_snapshot(
                    &canonical,
                    file.take(expected.saturating_add(1)),
                    &mut *self.progress,
                ) {
                    Scan::Counted(inventory) if inventory.source_bytes() == expected => {
                        (inventory.messages(), inventory.quoting())
                    }
                    Scan::Counted(_) => {
                        return self.failed(&canonical, "snapshot length changed during inventory");
                    }
                    // A copy that never becomes a source is referenced by
                    // nothing, so it goes now (spec/03); the original stays.
                    Scan::Cancelled => {
                        self.cancelled = true;
                        return self.discard(&snapshot);
                    }
                    Scan::Failed(error) => {
                        self.discard(&snapshot);
                        return self.failed(&canonical, error);
                    }
                }
            }
        };
        self.inventory.sources.push(SourceInventory {
            path: canonical,
            kind,
            mailbox,
            snapshot,
            messages,
            quoting,
            bundle: None,
        });
    }

    /// Remove a capture that will not become a source, best-effort: a
    /// failed removal leaves an unreferenced leftover (spec/03).
    fn discard(&self, snapshot: &ImportSourceSnapshot) {
        let _ = self
            .store
            .remove_import_sources(std::slice::from_ref(snapshot));
    }

    /// Discovery needs only metadata; capture revalidates the opened handle.
    fn classify(&self, path: &Path) -> Result<FileKind, io::Error> {
        self.fs
            .metadata(path)?
            .map(|md| md.kind())
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
    }

    /// Resolve symlinks; a failure is a reported path, not an empty
    /// one (spec/09).
    fn canonicalized(&mut self, path: &Path) -> Option<PathBuf> {
        match self.fs.canonicalize(path) {
            Ok(canonical) => Some(canonical),
            Err(error) => {
                self.problem_io(path, error);
                None
            }
        }
    }

    fn name_problem(&mut self, path: &Path) {
        self.problem(
            path,
            ImportProblemKind::NonUtf8,
            "no UTF-8 mailbox name can be derived",
        );
    }

    /// A store or scan failure for an accepted candidate.
    fn failed(&mut self, path: &Path, error: impl ToString) {
        self.problem(path, ImportProblemKind::Failed, error.to_string());
    }

    fn problem(&mut self, path: &Path, kind: ImportProblemKind, detail: impl Into<String>) {
        self.inventory.problems.push(ImportProblem {
            path: path.to_path_buf(),
            kind,
            detail: detail.into(),
        });
    }

    fn problem_io(&mut self, path: &Path, error: io::Error) {
        self.problem(path, ImportProblemKind::Inaccessible, error.to_string());
    }
}

/// Inventory one captured mbox stream through the cancel-aware
/// reader. The copy was already verified once on reopen.
fn scan_snapshot(
    path: &Path,
    file: impl Read,
    progress: &mut dyn FnMut(ImportProgress<'_>) -> ControlFlow<()>,
) -> Scan {
    let mut reader = BufReader::new(ScanReader {
        inner: file,
        path,
        scanned: 0,
        cancelled: false,
        progress,
    });
    let result = inventory_mbox(&mut reader);
    // The latch distinguishes an observed cancellation from a real
    // scan I/O failure.
    if reader.get_ref().cancelled {
        return Scan::Cancelled;
    }
    match result {
        Ok(inventory) => Scan::Counted(inventory),
        Err(error) => Scan::Failed(error),
    }
}

/// A scan read that reports progress and honours `Break`.
/// `inventory_mbox` only stops on an I/O error, so a break becomes a
/// non-`Interrupted` failure. The scanner retries `Interrupted`, so
/// the latch distinguishes cancellation from real I/O failure.
struct ScanReader<'a, R> {
    inner: R,
    path: &'a Path,
    scanned: u64,
    cancelled: bool,
    progress: &'a mut dyn FnMut(ImportProgress<'_>) -> ControlFlow<()>,
}

impl<R: Read> Read for ScanReader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.cancelled {
            return Err(io::Error::other("import inventory cancelled"));
        }
        let n = self.inner.read(buf)?;
        if n == 0 {
            return Ok(0);
        }
        self.scanned = self.scanned.saturating_add(n as u64);
        let event = ImportProgress::Scanning {
            path: self.path,
            bytes: self.scanned,
        };
        if (self.progress)(event).is_break() {
            self.cancelled = true;
            return Err(io::Error::other("import inventory cancelled"));
        }
        Ok(n)
    }
}

impl<R: Seek> Seek for ScanReader<'_, R> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        if self.cancelled
            || (self.progress)(ImportProgress::Scanning {
                path: self.path,
                bytes: self.scanned,
            })
            .is_break()
        {
            self.cancelled = true;
            return Err(io::Error::other("import inventory cancelled"));
        }
        self.inner.seek(pos)
    }
}

/// Whether `path`'s extension equals `wanted`, case-insensitively
/// (spec/09); non-UTF-8 extensions match nothing.
fn has_extension(path: &Path, wanted: &str) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case(wanted))
}

/// The mailbox a file or package names: its stem (spec/09).
fn stem(path: &Path) -> Option<String> {
    path.file_stem()?.to_str().map(str::to_owned)
}

/// The mailbox EML files in a folder name: the folder's own name.
fn folder_name(path: &Path) -> Option<String> {
    path.parent()?.file_name()?.to_str().map(str::to_owned)
}

pub(crate) fn invalid(message: impl Into<String>) -> StoreError {
    StoreError::InvalidInput {
        message: message.into(),
    }
}

fn field<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    name: &str,
) -> Result<&'a serde_json::Value, StoreError> {
    object
        .get(name)
        .ok_or_else(|| invalid(format!("import source metadata is missing {name:?}")))
}

fn required_str<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    name: &str,
) -> Result<&'a str, StoreError> {
    field(object, name)?
        .as_str()
        .ok_or_else(|| invalid(format!("import source {name} must be a string")))
}

pub(crate) fn required_u64(
    object: &serde_json::Map<String, serde_json::Value>,
    name: &str,
) -> Result<u64, StoreError> {
    field(object, name)?.as_u64().ok_or_else(|| {
        invalid(format!(
            "import source {name} must be a nonnegative integer"
        ))
    })
}

/// A `{token, sha256, byte_length}` snapshot reference.
fn snapshot_field(
    object: &serde_json::Map<String, serde_json::Value>,
    name: &str,
) -> Result<ImportSourceSnapshot, StoreError> {
    let snapshot = object
        .get(name)
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| invalid(format!("an import source {name} must be an object")))?;
    let sha256 = sha256_from_hex(required_str(snapshot, "sha256")?).ok_or_else(|| {
        invalid(format!(
            "an import source {name} sha256 is 64 lowercase hex digits"
        ))
    })?;
    ImportSourceSnapshot::from_metadata(
        required_str(snapshot, "token")?,
        sha256,
        required_u64(snapshot, "byte_length")?,
    )
}

/// The required `bundle` object of a version-2 source: its manifest
/// snapshot reference and declared format (spec/09 "Export bundles").
fn bundle_field(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<SourceBundle, StoreError> {
    let bundle = field(object, "bundle")?
        .as_object()
        .ok_or_else(|| invalid("an import source bundle must be an object"))?;
    let manifest = snapshot_field(bundle, "manifest")?;
    let format = BundleFormat::from_name(required_str(bundle, "format")?)
        .ok_or_else(|| invalid("an import source bundle's format is eml or mboxrd"))?;
    Ok(SourceBundle::new(manifest, format))
}

fn sha256_from_hex(text: &str) -> Option<[u8; 32]> {
    let digit = |byte: u8| match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    };
    let mut out = [0u8; 32];
    if text.len() != out.len() * 2 {
        return None;
    }
    for (slot, pair) in out.iter_mut().zip(text.as_bytes().as_chunks::<2>().0) {
        *slot = (digit(pair[0])? << 4) | digit(pair[1])?;
    }
    Some(out)
}

#[cfg(test)]
mod tests;
