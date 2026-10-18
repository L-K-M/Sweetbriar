//! Bounded archive flag extraction (spec/09). Only root header values
//! contribute; text in the body or embedded messages never sets flags.

use crate::Limits;

const READ: u8 = b'R';
const FLAGGED: u8 = b'F';
const ANSWERED: u8 = b'A';
const DELETED: u8 = b'D';
const DRAFT: u8 = b'T';

/// Flags recovered from admitted Status/X-Status headers. `seen = None`
/// means no such header was observed, not necessarily that none exists:
/// callers must report incomplete extraction before applying archive defaults.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImportFlags {
    seen: Option<bool>,
    flagged: bool,
    answered: bool,
    deleted: bool,
    draft: bool,
    complete: bool,
}

impl ImportFlags {
    pub fn seen(&self) -> Option<bool> {
        self.seen
    }
    pub fn flagged(&self) -> bool {
        self.flagged
    }
    pub fn answered(&self) -> bool {
        self.answered
    }
    pub fn deleted(&self) -> bool {
        self.deleted
    }
    pub fn draft(&self) -> bool {
        self.draft
    }

    /// Whether the root header block was fully examined within both
    /// header budgets. This says nothing about MIME body completeness.
    pub fn is_complete(&self) -> bool {
        self.complete
    }
}

#[derive(Clone, Copy)]
enum Field {
    Status,
    ExtendedStatus,
    Other,
}

impl Field {
    fn named(name: &[u8]) -> Self {
        if name.eq_ignore_ascii_case(b"status") {
            return Self::Status;
        }
        if name.eq_ignore_ascii_case(b"x-status") {
            return Self::ExtendedStatus;
        }
        Self::Other
    }

    fn add(self, value: &[u8], flags: &mut ImportFlags) {
        if matches!(self, Self::Other) {
            return;
        }
        let seen = flags.seen.get_or_insert(false);
        for &byte in value {
            if byte == READ {
                *seen = true;
            }
            if !matches!(self, Self::ExtendedStatus) {
                continue;
            }
            match byte {
                FLAGGED => flags.flagged = true,
                ANSWERED => flags.answered = true,
                DELETED => flags.deleted = true,
                DRAFT => flags.draft = true,
                _ => (),
            }
        }
    }
}

/// Read complete root-header lines only, without allocating a header
/// vector or decoding MIME. Known flags survive a budget breach; an
/// unterminated or over-budget block has `is_complete() == false`.
pub fn import_flags(bytes: &[u8], limits: &Limits) -> ImportFlags {
    let bound = bytes
        .len()
        .min(limits.max_header_bytes())
        .min(limits.max_message_bytes());
    let mut flags = ImportFlags::default();
    let mut field = Field::Other;
    let mut fields = 0;
    for line in bytes[..bound].split_inclusive(|&byte| byte == b'\n') {
        // A cut line could still change its value: retain earlier facts,
        // but never interpret a partial status token as a complete one.
        if !line.ends_with(b"\n") {
            return flags;
        }
        if line.trim_ascii().is_empty() {
            flags.complete = true;
            return flags;
        }
        if fields > 0 && matches!(line.first(), Some(b' ' | b'\t')) {
            field.add(line, &mut flags);
            continue;
        }
        if fields >= limits.max_header_fields() {
            return flags;
        }
        fields += 1;
        let Some(colon) = line.iter().position(|&byte| byte == b':') else {
            field = Field::Other;
            continue;
        };
        field = Field::named(line[..colon].trim_ascii());
        field.add(&line[colon + 1..], &mut flags);
    }
    flags
}

#[cfg(test)]
mod tests;
