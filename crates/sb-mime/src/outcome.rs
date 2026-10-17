//! The `parse` result vocabulary: `ParseOutcome` plus its `ParseWarning`
//! and `ParseFailure` payloads (spec/10, spec/02 `parse_state`).

use crate::summary::MessageSummary;

/// What [`crate::parse`] produced for one raw message.
///
/// Maps to `d_message_summary.parse_state`: `Parsed` → `ok`,
/// `Partial` → `partial`, `Failed` → `failed` (the acquisition is still
/// stored; the summary row just records the state).
#[derive(Debug)]
#[must_use]
pub enum ParseOutcome {
    /// Parsed within every budget with no damage detected.
    Parsed(MessageSummary),
    /// A bounded summary was recovered, but something was lost or
    /// malformed: a budget was exceeded (the affected content was not
    /// parsed) or damaged structure was skipped. `warnings` is never
    /// empty — a partial without a reason would be `Parsed`.
    Partial {
        summary: MessageSummary,
        warnings: Vec<ParseWarning>,
    },
    /// The input yielded nothing usable. See [`ParseFailure`].
    Failed(ParseFailure),
}

impl ParseOutcome {
    /// The recovered summary, for `Parsed` and `Partial`.
    pub fn summary(&self) -> Option<&MessageSummary> {
        match self {
            Self::Parsed(summary) | Self::Partial { summary, .. } => Some(summary),
            Self::Failed(_) => None,
        }
    }

    /// What was lost or malformed; empty for `Parsed` and `Failed`.
    pub fn warnings(&self) -> &[ParseWarning] {
        match self {
            Self::Partial { warnings, .. } => warnings,
            _ => &[],
        }
    }
}

/// Why a parse yielded nothing usable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseFailure {
    /// No headers could be parsed at all — the input is not an
    /// RFC 5322 message (empty, or not mail-shaped).
    Unparseable,
    /// The parser panicked mid-parse. The unwind was contained at the
    /// `parse` boundary; the panic payload is not retained.
    Panic,
}

/// A recoverable problem found while bounding or parsing. Each variant
/// carries the violated budget (or, for malformed input, which kind of
/// damage) so the caller can surface a visible warning per spec/10.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseWarning {
    /// Raw message bytes exceeded `max_message_bytes`; only the first
    /// `limit` bytes were examined.
    MessageTooLarge { limit: u64 },
    /// One entity's header block (folds included) exceeded
    /// `max_header_bytes`.
    HeaderBlockTooLarge { limit: u64 },
    /// One entity carried more than `max_header_fields` fields.
    TooManyHeaderFields { limit: u64 },
    /// MIME nesting went deeper than `max_depth` (root entity = 1).
    TooDeep { limit: u32 },
    /// The message had more than `max_parts` MIME entities.
    TooManyParts { limit: u64 },
    /// One part's bounded decoded size exceeded
    /// `max_decoded_part_bytes`.
    PartTooLarge { limit: u64 },
    /// An encoded `message/rfc822`/`message/global` nested deeper than
    /// the parser's encoded-nesting bound; treated as opaque data.
    NestedTooDeep { limit: u32 },
    /// Damaged structure was recovered: an unterminated header block, a
    /// declared boundary never found, or a part upstream flagged with an
    /// encoding problem. `parts` counts flagged entities.
    Malformed { parts: u64 },
}
