//! `Limits` — the bounded-parsing budgets from spec/10 "Hostile
//! content", plus the implementation's chosen header caps. Budgets may
//! be lowered by callers, never raised past the spec ceilings: every
//! `with_*` setter clamps to the ceiling.

/// spec/10: MIME nesting depth ceiling. The root message's own entity
/// counts as depth 1.
pub const MAX_DEPTH: u32 = 50;

/// spec/10: total MIME entities per message (the root counts as part 1;
/// multipart containers, leaves and embedded messages each count).
pub const MAX_PARTS: u64 = 10_000;

/// spec/10: raw bytes per message (decimal MB).
pub const MAX_MESSAGE_BYTES: usize = 100_000_000;

/// spec/10: decoded bytes per MIME part (decimal MB).
pub const MAX_DECODED_PART_BYTES: u64 = 25_000_000;

/// Bytes of a single entity's header block admitted for parsing, folds
/// included. 256 KiB comfortably covers pathological-but-legitimate
/// mail (dozens of Received hops, long DKIM signatures, large unfolded
/// lists) while bounding decode work per block.
pub const MAX_HEADER_BYTES: usize = 262_144;

/// Header fields admitted per entity. 1,024 bounds the per-part header
/// vector; legitimate mail carries a small fraction of that.
pub const MAX_HEADER_FIELDS: usize = 1_024;

/// Maximum snippet bytes stored in `d_message_summary.snippet`:
/// upstream `preview_text` truncates on a UTF-8 boundary and appends
/// "..." inside the budget. 512 B is a few list-row lines.
pub const SNIPPET_BYTES: usize = 512;

/// Encoded (base64/quoted-printable) `message/rfc822` decodes admitted
/// along a nesting path. Mirrors upstream `MAX_NESTED_ENCODED`; deeper
/// encoded nests are treated as opaque attachments.
pub(crate) const MAX_ENCODED_NESTING: u32 = 3;

/// Resource budgets for [`crate::parse`]. `Limits::default()` applies
/// the spec/10 ceilings; setters clamp to them, so callers can tighten
/// but never loosen the ceilings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    max_depth: u32,
    max_parts: u64,
    max_message_bytes: usize,
    max_decoded_part_bytes: u64,
    max_header_bytes: usize,
    max_header_fields: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_depth: MAX_DEPTH,
            max_parts: MAX_PARTS,
            max_message_bytes: MAX_MESSAGE_BYTES,
            max_decoded_part_bytes: MAX_DECODED_PART_BYTES,
            max_header_bytes: MAX_HEADER_BYTES,
            max_header_fields: MAX_HEADER_FIELDS,
        }
    }
}

impl Limits {
    /// Clamps `value` to `ceiling` so budgets may only be lowered.
    fn clamp(value: u64, ceiling: u64) -> u64 {
        value.min(ceiling)
    }

    /// Maximum nesting depth (root entity = 1). Clamped to
    /// [`MAX_DEPTH`].
    pub fn with_max_depth(mut self, value: u32) -> Self {
        self.max_depth = Self::clamp(value.into(), MAX_DEPTH.into()) as u32;
        self
    }

    /// Maximum MIME entities per message. Clamped to [`MAX_PARTS`].
    pub fn with_max_parts(mut self, value: u64) -> Self {
        self.max_parts = Self::clamp(value, MAX_PARTS);
        self
    }

    /// Maximum raw bytes per message. Clamped to [`MAX_MESSAGE_BYTES`].
    pub fn with_max_message_bytes(mut self, value: usize) -> Self {
        self.max_message_bytes = Self::clamp(value as u64, MAX_MESSAGE_BYTES as u64) as usize;
        self
    }

    /// Maximum decoded bytes per MIME part. Clamped to
    /// [`MAX_DECODED_PART_BYTES`].
    pub fn with_max_decoded_part_bytes(mut self, value: u64) -> Self {
        self.max_decoded_part_bytes = Self::clamp(value, MAX_DECODED_PART_BYTES);
        self
    }

    /// Maximum header-block bytes per entity, folds included. Clamped to
    /// [`MAX_HEADER_BYTES`].
    pub fn with_max_header_bytes(mut self, value: usize) -> Self {
        self.max_header_bytes = Self::clamp(value as u64, MAX_HEADER_BYTES as u64) as usize;
        self
    }

    /// Maximum nesting depth (root = 1).
    pub fn max_depth(&self) -> u32 {
        self.max_depth
    }

    pub fn max_parts(&self) -> u64 {
        self.max_parts
    }

    pub fn max_message_bytes(&self) -> usize {
        self.max_message_bytes
    }

    pub fn max_decoded_part_bytes(&self) -> u64 {
        self.max_decoded_part_bytes
    }

    pub fn max_header_bytes(&self) -> usize {
        self.max_header_bytes
    }

    /// Maximum header fields per entity. Clamped to
    /// [`MAX_HEADER_FIELDS`].
    pub fn with_max_header_fields(mut self, value: usize) -> Self {
        self.max_header_fields = Self::clamp(value as u64, MAX_HEADER_FIELDS as u64) as usize;
        self
    }

    pub fn max_header_fields(&self) -> usize {
        self.max_header_fields
    }
}
