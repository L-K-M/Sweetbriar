//! Versioned, reader-independent FTS extraction (spec/06).

use std::collections::HashSet;

mod html;

use html::html_lines;

use crate::{Limits, SearchBodyKind, SearchInputs, search_inputs};

/// Bump whenever extraction semantics change; stored with every search row.
pub const SEARCH_EXTRACTOR_VERSION: u32 = 2;
const MAX_DOCUMENT_BYTES: usize = 1_000_000;
const MAX_NAME_BYTES: usize = 64_000;
const COLUMNS: [&str; 8] = [
    "subject",
    "from_text",
    "to_text",
    "cc_text",
    "body_authored",
    "body_quoted",
    "body_signature",
    "attachment_names",
];

/// Publication state for `d_search_docs.state`, independent of summary parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchState {
    Indexed,
    Partial,
    Failed,
}

impl SearchState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Indexed => "indexed",
            Self::Partial => "partial",
            Self::Failed => "failed",
        }
    }
}

/// Exactly the eight FTS columns and extraction metadata. No reader rendition,
/// attachment contents or upstream MIME/HTML types cross this boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchDocument {
    columns: [String; 8],
    state: SearchState,
    truncated_fields: Vec<&'static str>,
}

impl SearchDocument {
    pub fn subject(&self) -> &str {
        &self.columns[0]
    }
    pub fn from_text(&self) -> &str {
        &self.columns[1]
    }
    pub fn to_text(&self) -> &str {
        &self.columns[2]
    }
    pub fn cc_text(&self) -> &str {
        &self.columns[3]
    }
    pub fn body_authored(&self) -> &str {
        &self.columns[4]
    }
    pub fn body_quoted(&self) -> &str {
        &self.columns[5]
    }
    pub fn body_signature(&self) -> &str {
        &self.columns[6]
    }
    pub fn attachment_names(&self) -> &str {
        &self.columns[7]
    }
    pub fn extractor_version(&self) -> u32 {
        SEARCH_EXTRACTOR_VERSION
    }
    pub fn state(&self) -> SearchState {
        self.state
    }
    /// Sorted, unique schema column names whose text may be missing.
    pub fn truncated_fields(&self) -> &[&'static str] {
        &self.truncated_fields
    }
    /// The spec/06 storage mask, without adding a JSON dependency for fixed names.
    pub fn truncated_fields_json(&self) -> String {
        let names: Vec<_> = self
            .truncated_fields
            .iter()
            .map(|name| format!("\"{name}\""))
            .collect();
        format!("[{}]", names.join(","))
    }

    /// The document for an acquisition refused on size alone, before
    /// any MIME work (spec/06 "Preparation boundary"): nothing was
    /// extracted, so every field is unknown and the mask names all
    /// eight columns. Pure: no bytes were faked or parsed.
    pub fn size_refused() -> Self {
        cap(Default::default(), COLUMNS.to_vec())
    }

    fn failed() -> Self {
        Self {
            columns: Default::default(),
            state: SearchState::Failed,
            truncated_fields: Vec::new(),
        }
    }
}

/// Extract directly from acquisition bytes. MIME refusals and damaged parses
/// produce partial documents with a nonempty mask; parser failures (including
/// contained unwinds) produce failed documents with empty columns. This pure
/// function performs no I/O: callers must report acquisition read failures
/// separately, never substitute empty bytes for a failed read.
pub fn search_document(bytes: &[u8], limits: &Limits) -> SearchDocument {
    std::panic::catch_unwind(|| match search_inputs(bytes, limits) {
        Ok(inputs) => extract(inputs),
        Err(_) => SearchDocument::failed(),
    })
    .unwrap_or_else(|_| SearchDocument::failed())
}

fn extract(inputs: SearchInputs) -> SearchDocument {
    let mut columns: [String; 8] = Default::default();
    columns[0] = normalize_lf(inputs.subject());
    columns[1] = normalize_lf(inputs.from_text());
    columns[2] = normalize_lf(inputs.to_text());
    columns[3] = normalize_lf(inputs.cc_text());
    let mut affected = if inputs.parse_warnings().is_empty() {
        Vec::new()
    } else {
        // The input boundary cannot identify which fields lost data.
        COLUMNS.to_vec()
    };
    let mut seen = HashSet::new();
    for body in inputs.bodies() {
        if body.charset_replaced() {
            // Lost bytes may belong to any body partition, even an empty one.
            affected.extend_from_slice(&COLUMNS[4..7]);
        }
        let mut candidate_seen = HashSet::new();
        let lines = match body.kind() {
            SearchBodyKind::Plain => plain_lines(body.text()),
            SearchBodyKind::Html => match html_lines(body.text()) {
                Ok(lines) => lines,
                Err(_) => {
                    affected.extend_from_slice(&COLUMNS[4..7]);
                    continue;
                }
            },
        };
        for paragraph in lines.split(|line| line.text.trim().is_empty()) {
            let key = paragraph
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>()
                .join("\n")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            if key.is_empty() || seen.contains(&key) {
                continue;
            }
            // Preserve repetition within a candidate; only earlier candidates
            // can suppress this paragraph.
            candidate_seen.insert(key);
            // Dedup the whole paragraph before splitting it by classification,
            // so the first candidate retains both its text and classification.
            let mut portions: [String; 3] = Default::default();
            for line in paragraph {
                let portion = &mut portions[line.kind as usize];
                if !portion.is_empty() {
                    portion.push('\n');
                }
                portion.push_str(&line.text);
            }
            for (column, portion) in columns[4..7].iter_mut().zip(portions) {
                if portion.is_empty() {
                    continue;
                }
                if !column.is_empty() {
                    column.push_str("\n\n");
                }
                column.push_str(&portion);
            }
        }
        seen.extend(candidate_seen);
    }
    columns[7] = inputs
        .attachment_names()
        .iter()
        .map(|name| normalize_lf(name.name()))
        .collect::<Vec<_>>()
        .join("\n");
    cap(columns, affected)
}

fn cap(mut columns: [String; 8], mut truncated_fields: Vec<&'static str>) -> SearchDocument {
    let mut remaining = MAX_DOCUMENT_BYTES;
    for (index, text) in columns.iter_mut().enumerate() {
        let budget = if index == 7 {
            remaining.min(MAX_NAME_BYTES)
        } else {
            remaining
        };
        if text.len() > budget {
            let mut end = budget;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
            truncated_fields.push(COLUMNS[index]);
        }
        remaining -= text.len();
    }
    truncated_fields.sort_unstable();
    truncated_fields.dedup();
    let state = if truncated_fields.is_empty() {
        SearchState::Indexed
    } else {
        SearchState::Partial
    };
    SearchDocument {
        columns,
        state,
        truncated_fields,
    }
}

fn normalize_lf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

#[derive(Clone, Copy)]
enum Kind {
    Authored,
    Quoted,
    Signature,
}

struct Line {
    kind: Kind,
    text: String,
}

fn plain_lines(text: &str) -> Vec<Line> {
    let mut signature = false;
    normalize_lf(text)
        .split('\n')
        .map(|line| {
            let quoted = line.trim_start().starts_with('>');
            if line == "-- " {
                signature = true;
            }
            let kind = if quoted {
                Kind::Quoted
            } else if signature {
                Kind::Signature
            } else {
                Kind::Authored
            };
            Line {
                kind,
                text: line.to_owned(),
            }
        })
        .collect()
}

fn append_html_text(lines: &mut Vec<Line>, text: &str, quoted: bool) {
    let text = normalize_lf(text);
    for (index, text) in text.split('\n').enumerate() {
        if index > 0 {
            lines.push(Line {
                kind: Kind::Authored,
                text: String::new(),
            });
        }
        let line = lines.last_mut().expect("HTML always has a current line");
        if line.text.is_empty() {
            line.kind = if quoted { Kind::Quoted } else { Kind::Authored };
        }
        line.text.push_str(text);
    }
}

fn boundary(lines: &mut Vec<Line>) {
    // A block boundary separates paragraphs; <br> only separates lines.
    for _ in 0..2 {
        lines.push(Line {
            kind: Kind::Authored,
            text: String::new(),
        });
    }
}

fn is_block(tag: &str) -> bool {
    matches!(
        tag,
        "address"
            | "article"
            | "aside"
            | "blockquote"
            | "caption"
            | "center"
            | "dd"
            | "details"
            | "dialog"
            | "dir"
            | "div"
            | "dl"
            | "dt"
            | "fieldset"
            | "figcaption"
            | "figure"
            | "footer"
            | "form"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "header"
            | "hgroup"
            | "hr"
            | "legend"
            | "li"
            | "listing"
            | "main"
            | "menu"
            | "nav"
            | "ol"
            | "p"
            | "plaintext"
            | "pre"
            | "search"
            | "section"
            | "summary"
            | "table"
            | "tbody"
            | "td"
            | "tfoot"
            | "th"
            | "thead"
            | "tr"
            | "ul"
            | "xmp"
    )
}

#[cfg(test)]
mod tests;
