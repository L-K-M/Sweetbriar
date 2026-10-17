//! `sb-mime` — L5. Bounded MIME parsing and message-summary
//! extraction. Apple Mail exports, `.eml` on disk and network bytes are
//! all hostile input (spec/10): [`parse`] bounds structure, bytes and
//! decoding *before* handing an admitted input to `mail-parser`, and
//! contains unwinds across the whole pipeline.
//!
//! The output is an owned [`MessageSummary`] — the `d_message_summary`
//! row minus `acquisition_id`, which the caller supplies (spec/02).
//! Upstream `mail-parser` types never appear in this API.

#![forbid(unsafe_code)]

mod extract;
mod limits;
mod mbox;
mod outcome;
mod summary;
mod walk;

pub use limits::{
    Limits, MAX_DECODED_PART_BYTES, MAX_DEPTH, MAX_HEADER_BYTES, MAX_HEADER_FIELDS,
    MAX_MESSAGE_BYTES, MAX_PARTS, SNIPPET_BYTES,
};
pub use mbox::{
    MboxError, MboxInventory, MboxQuoting, MboxReader, MboxRecord, MboxScanner, MboxVariant,
    inventory_mbox,
};
pub use outcome::{ParseFailure, ParseOutcome, ParseWarning};
pub use summary::{MailboxAddress, MessageSummary, PARSER_VERSION};

use mail_parser::parsers::MessageStream;
use mail_parser::{Message, PartType};

/// Parse one raw RFC 5322 / MIME message under `limits`.
///
/// Outcome semantics (spec/02 `parse_state`):
/// - [`ParseOutcome::Parsed`] — fully parsed inside every budget.
/// - [`ParseOutcome::Partial`] — a bounded summary was recovered, but a
///   budget was exceeded or damaged structure was skipped; `warnings`
///   say what was lost.
/// - [`ParseOutcome::Failed`] — unusable input, or a parser unwind
///   contained at this boundary ([`ParseFailure::Panic`]). Panic
///   payloads are discarded, never retained.
///
/// The panic hook is not touched; `panic = "abort"` builds still abort,
/// per spec/10.
pub fn parse(bytes: &[u8], limits: &Limits) -> ParseOutcome {
    match std::panic::catch_unwind(|| run(bytes, limits)) {
        Ok(outcome) => outcome,
        Err(_) => ParseOutcome::Failed(ParseFailure::Panic),
    }
}

fn run(bytes: &[u8], limits: &Limits) -> ParseOutcome {
    let mut warnings = Vec::new();
    let data = if bytes.len() > limits.max_message_bytes() {
        warnings.push(ParseWarning::MessageTooLarge {
            limit: limits.max_message_bytes() as u64,
        });
        &bytes[..limits.max_message_bytes()]
    } else {
        bytes
    };

    // Phase A: iterative admission walk — bounds structure and
    // decoded-size estimates before any eager parsing/decoding.
    let walked = walk::walk(data, limits, warnings);
    if !walked.admitted {
        return partial_summary(walked, data, limits);
    }

    // Phase B: the input passed every budget; let upstream parse it
    // fully, then re-verify the materialized tree (admission is
    // bound-based — a malicious input could slip an estimate) before
    // extracting.
    let Some(message) = extract::summary_parser().parse(data) else {
        // Upstream found no parseable headers at all.
        return ParseOutcome::Failed(ParseFailure::Unparseable);
    };
    let mut warnings = walked.warnings;
    let mut summary = MessageSummary::default();
    let within = verify(&message, limits, &mut warnings);
    // Walk and verify can both flag damaged parts — possibly the same
    // part. Fold into one `Malformed`, keeping the larger count so the
    // same damage isn't reported twice.
    let malformed: u64 = warnings
        .iter()
        .map(|w| match w {
            ParseWarning::Malformed { parts } => *parts,
            _ => 0,
        })
        .max()
        .unwrap_or(0);
    if malformed > 0 {
        warnings.retain(|w| !matches!(w, ParseWarning::Malformed { .. }));
        warnings.push(ParseWarning::Malformed { parts: malformed });
    }
    if let Some(root) = message.parts.first() {
        extract::extract_headers(&root.headers, &mut summary);
    }
    summary.set_has_attachments(!message.attachments.is_empty());
    if let Some(preview) = message.body_preview(SNIPPET_BYTES)
        && !preview.is_empty()
    {
        summary.set_snippet(Some(preview.into_owned()));
    }
    if !within || !warnings.is_empty() {
        ParseOutcome::Partial { summary, warnings }
    } else {
        ParseOutcome::Parsed(summary)
    }
}

/// Re-verify the materialized upstream tree against the same budgets:
/// admission bounds are computed before decode, so this double-checks
/// what upstream actually produced — part count, tree depth (via
/// `Multipart` child links and `Message` nesting), materialized part
/// sizes, and `is_encoding_problem` damage flags.
fn verify(message: &Message<'_>, limits: &Limits, warnings: &mut Vec<ParseWarning>) -> bool {
    // (message, part index, depth). A message's entities all live in
    // one `parts` vec — counted wholesale on the root visit — while
    // depth follows tree links only.
    let mut parts_seen = 0u64;
    let mut malformed = 0u64;
    let mut steps = 0u64;
    let mut stack = vec![(message, 0usize, 1u32)];
    while let Some((m, idx, depth)) = stack.pop() {
        steps += 1;
        if steps > 4 * limits.max_parts() + 64 {
            // Defensive: upstream builds child links forward-only, so
            // revisits can't occur; guard anyway against a structurally
            // impossible cycle — and fail closed: an unverified tree
            // must never be reported as fully parsed.
            warnings.push(ParseWarning::TooManyParts {
                limit: limits.max_parts(),
            });
            return false;
        }
        if idx == 0 {
            parts_seen += m.parts.len() as u64;
            if parts_seen > limits.max_parts() {
                warnings.push(ParseWarning::TooManyParts {
                    limit: limits.max_parts(),
                });
                return false;
            }
            for part in &m.parts {
                malformed += u64::from(part.is_encoding_problem);
                let oversized = match &part.body {
                    PartType::Text(t) | PartType::Html(t) => {
                        t.len() as u64 > limits.max_decoded_part_bytes()
                    }
                    PartType::Binary(b) | PartType::InlineBinary(b) => {
                        b.len() as u64 > limits.max_decoded_part_bytes()
                    }
                    _ => false,
                };
                if oversized {
                    warnings.push(ParseWarning::PartTooLarge {
                        limit: limits.max_decoded_part_bytes(),
                    });
                    return false;
                }
            }
        }
        if depth > limits.max_depth() {
            warnings.push(ParseWarning::TooDeep {
                limit: limits.max_depth(),
            });
            return false;
        }
        let Some(part) = m.parts.get(idx) else {
            continue;
        };
        match &part.body {
            PartType::Multipart(ids) => {
                stack.extend(ids.iter().map(|&id| (m, id as usize, depth + 1)));
            }
            PartType::Message(inner) => stack.push((inner, 0, depth + 1)),
            _ => (),
        }
    }
    if malformed > 0 {
        warnings.push(ParseWarning::Malformed { parts: malformed });
    }
    true
}

/// Build the `Partial` summary from what the admission walk recovered:
/// root header fields re-parsed on the bounded block, a snippet from
/// the first admitted text leaf, and the walk's attachment signal.
fn partial_summary(walked: walk::Walked<'_>, data: &[u8], limits: &Limits) -> ParseOutcome {
    let mut summary = MessageSummary::default();
    if let Some(range) = &walked.root_block {
        // The block was already proven inside the header budgets; the
        // clamp keeps the re-parse inside them when the block itself
        // was what breached.
        let end = range
            .end
            .min(range.start + limits.max_header_bytes())
            .min(data.len());
        let mut stream = MessageStream::new(&data[range.start..end]);
        let mut headers = Vec::new();
        let parser = extract::summary_parser();
        stream.parse_headers(&parser, &mut headers);
        extract::extract_headers(&headers, &mut summary);
    }
    if let Some(extent) = &walked.first_text {
        summary.set_snippet(extract::extent_snippet(&walked.regions, extent));
    }
    summary.set_has_attachments(walked.saw_attachment);
    ParseOutcome::Partial {
        summary,
        warnings: walked.warnings,
    }
}

/// Test-only fault injection inside the `parse` containment boundary:
/// panics when an entity's header block carries the tripwire marker.
/// Compiled out of non-test builds; marker-in-input keeps the test
/// hermetic (no global state, safe under parallel tests).
#[inline(always)]
fn tripwire(_block: &[u8]) {
    #[cfg(test)]
    if _block
        .windows(tests::TRIPWIRE_MARKER.len())
        .any(|w| w == tests::TRIPWIRE_MARKER)
    {
        panic!("sb-mime test tripwire");
    }
}

#[cfg(test)]
mod tests;
