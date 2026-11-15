//! `cargo xtask reading-corpus` (plan M6.3b1): run the reading
//! document model over every message in an mbox or a directory and
//! print counts only.
//!
//! The command is for the owner's own mail, so it never writes message
//! content anywhere: the answer is `total`, `supported`, `unsupported`
//! and `parse_failures`, nothing else. A parse failure is counted, not
//! reported. An mbox is read as mboxrd (the import default); a
//! directory is walked recursively for `.eml` and `.mbox` files.

use std::fs::{self, File};
use std::io::{BufReader, Read, Seek};
use std::path::{Path, PathBuf};

use sb_mime::{DocumentSupport, Limits, MboxScanner, MboxVariant, mail_document, unquote_range};

/// What the corpus tool counted. No message-derived text is kept.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub total: u64,
    pub supported: u64,
    pub unsupported: u64,
    pub parse_failures: u64,
}

impl Counts {
    pub fn add(&mut self, other: Counts) {
        self.total += other.total;
        self.supported += other.supported;
        self.unsupported += other.unsupported;
        self.parse_failures += other.parse_failures;
    }

    /// The whole answer: four counts, one per line. Never a subject,
    /// body, sender or path from a message.
    pub fn report(&self) -> String {
        format!(
            "total: {}\nsupported: {}\nunsupported: {}\nparse_failures: {}\n",
            self.total, self.supported, self.unsupported, self.parse_failures
        )
    }
}

/// Count every message under `path`: an mbox or `.eml` file, or a
/// directory walked recursively for `.eml` and `.mbox` sources.
pub fn count_path(path: &Path) -> Result<Counts, String> {
    let metadata =
        fs::metadata(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    if metadata.is_dir() {
        let mut sources = Vec::new();
        collect_sources(path, &mut sources)?;
        sources.sort();
        let mut counts = Counts::default();
        for source in &sources {
            counts.add(count_source(source)?);
        }
        Ok(counts)
    } else {
        count_source(path)
    }
}

fn collect_sources(dir: &Path, sources: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries =
        fs::read_dir(dir).map_err(|error| format!("cannot read {}: {error}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("cannot read {}: {error}", dir.display()))?;
        let file_type = entry
            .file_type()
            .map_err(|error| format!("cannot read {}: {error}", dir.display()))?;
        let path = entry.path();
        // A symlink is never followed: the owner's mail tree may alias or
        // cycle, and the tool's reach is the files it was given.
        if file_type.is_dir() {
            collect_sources(&path, sources)?;
        } else if file_type.is_file()
            && (has_extension(&path, "eml") || has_extension(&path, "mbox"))
        {
            sources.push(path);
        }
    }
    Ok(())
}

fn count_source(path: &Path) -> Result<Counts, String> {
    if has_extension(path, "eml") {
        let bytes =
            fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        return Ok(count_message(&bytes));
    }
    let file =
        File::open(path).map_err(|error| format!("cannot open {}: {error}", path.display()))?;
    count_mbox(file)
}

/// One mbox, streamed record by record: the scan collects ranges, then
/// a second handle decodes each record, so no whole source is buffered.
fn count_mbox<Source: Read + Seek>(mut source: Source) -> Result<Counts, String> {
    let ranges = {
        let mut scanner = MboxScanner::new(BufReader::new(&mut source));
        let mut ranges = Vec::new();
        for record in &mut scanner {
            ranges.push(record.map_err(|error| error.to_string())?.byte_range());
        }
        ranges
    };
    let mut counts = Counts::default();
    for range in ranges {
        let mut record = unquote_range(&mut source, range.start, range.end, MboxVariant::Mboxrd)
            .map_err(|error| error.to_string())?;
        let mut bytes = Vec::new();
        record
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        counts.add(count_message(&bytes));
    }
    Ok(counts)
}

/// One message: the model's answer, or a parse failure. The bytes are
/// counted and dropped; nothing derived from them is retained.
pub fn count_message(bytes: &[u8]) -> Counts {
    let mut counts = Counts {
        total: 1,
        ..Counts::default()
    };
    match mail_document(bytes, &Limits::default()) {
        Ok(DocumentSupport::Supported(_)) => counts.supported = 1,
        Ok(DocumentSupport::Unsupported) => counts.unsupported = 1,
        Err(_) => counts.parse_failures = 1,
    }
    counts
}

fn has_extension(path: &Path, wanted: &str) -> bool {
    path.extension()
        .is_some_and(|found| found.eq_ignore_ascii_case(wanted))
}
