//! Iterative admission walk over MIME structure, run before upstream
//! parses anything. `mail-parser` is best-effort and decodes bodies
//! eagerly, so spec/10's budgets are enforced on the way in: this walk
//! mirrors upstream's state machine (`parsers/message.rs`) using only
//! its public `MessageStream` scanners, and allocates nothing except
//! decoded `message/rfc822` bodies — at most `MAX_ENCODED_NESTING` deep
//! and `max_decoded_part_bytes` each.
//!
//! Leaf extents are borrowed scans (`seek_part_end`); the decoded-size
//! budget is applied to a bound computed from the *encoded* bytes before
//! any decode. A budget breach sets `violated` and stops the walk — the
//! eager upstream parser cannot be run safely on that input, so `parse`
//! reports `Partial` with whatever the walk recovered. Recoverable
//! damage (unterminated header blocks, missing boundaries, decode
//! failures, over-nested encoded messages) only counts toward the
//! `Malformed` warning, matching upstream's own recovery.

use std::ops::Range;

use mail_parser::parsers::MessageStream;
use mail_parser::{Header, HeaderName, MessageParser};

use crate::limits::MAX_ENCODED_NESTING;
use crate::{Limits, ParseWarning};

/// Decoded materialization bound per encoded byte for text parts: every
/// supported charset decoder emits at most 3 bytes of UTF-8 per input
/// byte, and `from_utf8_lossy` emits at most a U+FFFD (3 bytes) per bad
/// sequence.
const TEXT_EXPANSION: u64 = 3;

/// How upstream would transfer-decode an entity body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Cte {
    /// `7bit`/`8bit`/`binary`/absent/unknown: upstream takes the raw
    /// extent via `mime_part`.
    Identity,
    Base64,
    QuotedPrintable,
}

/// A leaf extent captured for the partial-path snippet; `region`
/// indexes `Walked::regions`.
#[derive(Debug)]
pub(crate) struct TextExtent {
    pub(crate) region: usize,
    pub(crate) range: Range<usize>,
    pub(crate) cte: Cte,
    pub(crate) charset: Option<Vec<u8>>,
    pub(crate) html: bool,
}

/// Leaf flavor for [`Walker::leaf`]: text leaves may seed the snippet,
/// binary leaves always count as attachments.
enum Leaf {
    Text {
        charset: Option<Vec<u8>>,
        html: bool,
    },
    Binary,
}

/// What the walk learned. `regions` keeps the input borrow and decoded
/// `message/rfc822` bodies alive for partial-path decoding.
pub(crate) struct Walked<'x> {
    /// `false` when a budget was breached: the input must not be handed
    /// to the eager upstream parser.
    pub(crate) admitted: bool,
    pub(crate) warnings: Vec<ParseWarning>,
    /// Root entity's header-block byte range in region 0 (the input).
    pub(crate) root_block: Option<Range<usize>>,
    /// First admitted text leaf, for the partial-path snippet.
    pub(crate) first_text: Option<TextExtent>,
    /// Best-effort attachment signal for partial input.
    pub(crate) saw_attachment: bool,
    pub(crate) regions: Vec<Region<'x>>,
}

/// A byte region a frame walks: the input itself or a decoded
/// `message/rfc822` body.
pub(crate) enum Region<'x> {
    Borrowed(&'x [u8]),
    Owned(Vec<u8>),
}

impl Region<'_> {
    pub(crate) fn bytes(&self) -> &[u8] {
        match self {
            Self::Borrowed(b) => b,
            Self::Owned(v) => v.as_slice(),
        }
    }
}

/// Per-entity facts distilled from its header block; owned so the
/// region borrow can be released before frames mutate.
struct Head {
    ctype: Option<Vec<u8>>,
    csubtype: Option<Vec<u8>>,
    boundary: Option<Vec<u8>>,
    charset: Option<Vec<u8>>,
    cte: Cte,
    attachment: bool,
}

impl Head {
    fn parse(headers: &[Header<'_>]) -> Self {
        let ct = headers
            .iter()
            .find(|h| h.name == HeaderName::ContentType)
            .and_then(|h| h.value.as_content_type());
        Self {
            ctype: ct.map(|c| c.ctype().as_bytes().to_vec()),
            csubtype: ct.and_then(|c| c.subtype().map(|s| s.as_bytes().to_vec())),
            boundary: ct.and_then(|c| c.attribute("boundary").map(|b| b.as_bytes().to_vec())),
            charset: ct.and_then(|c| c.attribute("charset").map(|s| s.as_bytes().to_vec())),
            cte: headers
                .iter()
                .find(|h| h.name == HeaderName::ContentTransferEncoding)
                .and_then(|h| h.value.as_text())
                .map_or(Cte::Identity, |e| {
                    if e.eq_ignore_ascii_case("base64") {
                        Cte::Base64
                    } else if e.eq_ignore_ascii_case("quoted-printable") {
                        Cte::QuotedPrintable
                    } else {
                        Cte::Identity
                    }
                }),
            attachment: headers
                .iter()
                .find(|h| h.name == HeaderName::ContentDisposition)
                .and_then(|h| h.value.as_content_type())
                .is_some_and(|cd| cd.is_attachment()),
        }
    }
}

/// `Content-Type` classification, mirroring upstream `mime_type()`:
/// multipart without a boundary attribute is a binary leaf; a CT-less
/// entity inside `multipart/digest` is a nested message; CT-less
/// elsewhere is `text/plain`.
enum Entity {
    Text { html: bool },
    Binary,
    Multipart { boundary: Vec<u8>, digest: bool },
    NestedMessage,
}

fn classify(head: &Head, parent_digest: bool) -> Entity {
    match head.ctype.as_deref() {
        Some(t) if t.eq_ignore_ascii_case(b"multipart") => match head.boundary.as_ref() {
            Some(boundary) => Entity::Multipart {
                boundary: boundary.clone(),
                digest: head
                    .csubtype
                    .as_deref()
                    .is_some_and(|s| s.eq_ignore_ascii_case(b"digest")),
            },
            None => Entity::Binary,
        },
        Some(t) if t.eq_ignore_ascii_case(b"text") => Entity::Text {
            html: head
                .csubtype
                .as_deref()
                .is_some_and(|s| s.eq_ignore_ascii_case(b"html")),
        },
        Some(t) if t.eq_ignore_ascii_case(b"message") => match head.csubtype.as_deref() {
            Some(s) if s.eq_ignore_ascii_case(b"rfc822") || s.eq_ignore_ascii_case(b"global") => {
                Entity::NestedMessage
            }
            _ => Entity::Binary,
        },
        Some(_) => Entity::Binary,
        None if parent_digest => Entity::NestedMessage,
        None => Entity::Text { html: false },
    }
}

/// Frame step within a region.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    /// An entity header block starts at `pos`.
    Headers,
    /// Scan for this frame's next `--boundary` (a multipart's epilogue
    /// after a nested container closed).
    ScanBoundary,
    /// Sitting right after a `--boundary` match: `--` closes the frame,
    /// anything else is the next entity's boundary line.
    AtBoundaryMatch,
}

/// How the parent frame resumes when a pushed frame pops.
#[derive(Clone, Copy)]
enum Resume {
    /// Parent scans on for its own boundary from the child's final
    /// offset (a multipart ends at its own closing boundary).
    Scan,
    /// The child's body already consumed the parent's `--boundary`
    /// match (nested messages inherit the parent's boundary); parent
    /// resumes right after it, at this offset.
    AtMatch(usize),
}

struct Frame {
    region: usize,
    /// Region-coordinate end of the frame's area.
    end: usize,
    boundary: Option<Vec<u8>>,
    pos: usize,
    step: Step,
    depth: u32,
    digest: bool,
    enc_depth: u32,
    resume: Resume,
}

/// Header parse for classification only: content type, disposition and
/// transfer encoding; every other field ignored.
fn walk_parser() -> MessageParser {
    MessageParser::new()
        .header_content_type(HeaderName::ContentType)
        .header_content_type(HeaderName::ContentDisposition)
        .header_text(HeaderName::ContentTransferEncoding)
        .default_header_ignore()
}

/// Locate the header block at the start of `data` exactly as upstream
/// `parse_headers` does: leading whitespace is skipped, a blank or
/// whitespace-only line ends the block, every other line is a field and
/// runs to its newline (folds included in the byte count). Returns
/// `(block_len, fields, terminated)`.
fn header_block(data: &[u8]) -> (usize, usize, bool) {
    let mut pos = 0;
    let mut fields = 0usize;
    loop {
        loop {
            match data.get(pos) {
                None => return (pos, fields, false),
                Some(b'\n') => return (pos + 1, fields, true),
                Some(ch) if ch.is_ascii_whitespace() => pos += 1,
                Some(_) => break,
            }
        }
        fields += 1;
        while let Some(&ch) = data.get(pos) {
            pos += 1;
            if ch == b'\n' {
                break;
            }
        }
    }
}

/// Upper bound on the bytes upstream would materialize for `extent`
/// under `cte`. Mirrors `decode_base64_mime` failure semantics: any
/// byte outside the base64 alphabet, `=` and ASCII whitespace makes
/// upstream keep the raw extent, so the bound is then the raw length.
fn decoded_bound(extent: &[u8], cte: Cte) -> u64 {
    match cte {
        Cte::Identity | Cte::QuotedPrintable => extent.len() as u64,
        Cte::Base64 => {
            let mut digits = 0u64;
            for &b in extent {
                match b {
                    b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'+' | b'/' => digits += 1,
                    b'=' | b' ' | b'\t' | b'\r' | b'\n' => (),
                    // Raw-extent fallback: the bound is the raw length.
                    _ => return extent.len() as u64,
                }
            }
            digits.div_ceil(4) * 3
        }
    }
}

/// Bound on the bytes upstream would materialize for this leaf: the
/// decoded extent for binary parts and nested messages, times
/// `TEXT_EXPANSION` for text. The factor-1 exception is unencoded text
/// that is already valid UTF-8 with no charset decoder upstream.
fn materialized_bound(extent: &[u8], cte: Cte, kind: &Leaf) -> u64 {
    let decoded = decoded_bound(extent, cte);
    let Leaf::Text { charset, .. } = kind else {
        return decoded;
    };
    if cte == Cte::Identity
        && charset
            .as_deref()
            .and_then(mail_parser::decoders::charsets::map::charset_decoder)
            .is_none()
        && std::str::from_utf8(extent).is_ok()
    {
        decoded
    } else {
        decoded * TEXT_EXPANSION
    }
}

/// Scan an entity body to its extent end, mirroring upstream
/// `seek_part_end`: returns `(extent_len, post_match_offset, found)`
/// where `post_match_offset` is just past the `--boundary` match (or
/// `data.len()` when `found` is false).
fn scan_extent(data: &[u8], boundary: Option<&[u8]>) -> (usize, usize, bool) {
    let mut stream = MessageStream::new(data);
    let (end_rel, found) = stream.seek_part_end(boundary);
    (end_rel, stream.offset(), found)
}

/// Run the admission walk over `data`. The caller has already clamped
/// `data` to `max_message_bytes` and pre-seeded `warnings`.
pub(crate) fn walk<'x>(data: &'x [u8], limits: &Limits, warnings: Vec<ParseWarning>) -> Walked<'x> {
    let parser = walk_parser();
    let mut w = Walker {
        limits,
        parser: &parser,
        regions: vec![Region::Borrowed(data)],
        stack: vec![Frame {
            region: 0,
            end: data.len(),
            boundary: None,
            pos: 0,
            step: Step::Headers,
            depth: 1,
            digest: false,
            enc_depth: 0,
            resume: Resume::Scan,
        }],
        parts: 0,
        malformed: 0,
        warnings,
        violated: false,
        root_block: None,
        first_text: None,
        saw_attachment: false,
    };
    w.run();
    if w.malformed > 0 {
        w.warnings
            .push(ParseWarning::Malformed { parts: w.malformed });
    }
    Walked {
        admitted: !w.violated,
        warnings: w.warnings,
        root_block: w.root_block,
        first_text: w.first_text,
        saw_attachment: w.saw_attachment,
        regions: w.regions,
    }
}

struct Walker<'x, 'p, 'l> {
    limits: &'l Limits,
    parser: &'p MessageParser,
    regions: Vec<Region<'x>>,
    stack: Vec<Frame>,
    parts: u64,
    malformed: u64,
    warnings: Vec<ParseWarning>,
    violated: bool,
    root_block: Option<Range<usize>>,
    first_text: Option<TextExtent>,
    saw_attachment: bool,
}

impl Walker<'_, '_, '_> {
    fn run(&mut self) {
        while !self.stack.is_empty() && !self.violated {
            match self.stack.last().expect("non-empty").step {
                Step::Headers => self.step_headers(),
                Step::ScanBoundary => self.step_scan_boundary(),
                Step::AtBoundaryMatch => self.step_at_boundary_match(),
            }
        }
    }

    /// Pop the top frame and arm its resume on the parent.
    fn pop_frame(&mut self) {
        let frame = self.stack.pop().expect("non-empty");
        if let Some(parent) = self.stack.last_mut() {
            match frame.resume {
                Resume::Scan => {
                    parent.pos = frame.pos.min(parent.end);
                    parent.step = Step::ScanBoundary;
                }
                Resume::AtMatch(pos) => {
                    parent.pos = pos.min(parent.end);
                    parent.step = Step::AtBoundaryMatch;
                }
            }
        }
    }

    /// Parse the entity header block at the frame cursor, apply the
    /// header budgets, then dispatch on the entity's class.
    fn step_headers(&mut self) {
        let (region, pos, end, depth, digest, enc_depth) = {
            let f = self.stack.last().expect("non-empty");
            (f.region, f.pos, f.end, f.depth, f.digest, f.enc_depth)
        };
        if pos >= end {
            // Region exhausted: nothing to parse, no phantom entity —
            // matching upstream's EOF stop in `parse_headers`.
            self.pop_frame();
            return;
        }
        let data = &self.regions[region].bytes()[pos..end];
        let (block_len, fields, terminated) = header_block(data);
        if region == 0 && pos == 0 {
            self.root_block = Some(0..block_len);
        }
        if !terminated {
            // Headers ran into the region end: truncated input.
            // Recoverable; upstream drops such an entity.
            self.malformed += 1;
        }
        if block_len > self.limits.max_header_bytes() {
            self.warnings.push(ParseWarning::HeaderBlockTooLarge {
                limit: self.limits.max_header_bytes() as u64,
            });
            self.violated = true;
            return;
        }
        if fields > self.limits.max_header_fields() {
            self.warnings.push(ParseWarning::TooManyHeaderFields {
                limit: self.limits.max_header_fields() as u64,
            });
            self.violated = true;
            return;
        }

        let head = {
            let mut stream = MessageStream::new(&data[..block_len]);
            let mut headers = Vec::new();
            stream.parse_headers(self.parser, &mut headers);
            Head::parse(&headers)
        };
        let body = pos + block_len;

        self.parts += 1;
        if self.parts > self.limits.max_parts() {
            self.warnings.push(ParseWarning::TooManyParts {
                limit: self.limits.max_parts(),
            });
            self.violated = true;
            return;
        }
        crate::tripwire(&data[..block_len]);

        // A `Content-Disposition: attachment` entity is an attachment
        // whatever its content type.
        self.saw_attachment |= head.attachment;

        match classify(&head, digest) {
            Entity::Multipart { boundary, digest } => {
                if !self.child_depth(depth) {
                    return;
                }
                let child_pos = {
                    let mut stream = MessageStream::new(&self.regions[region].bytes()[body..end]);
                    stream.seek_next_part(&boundary).then(|| {
                        stream.skip_crlf();
                        body + stream.offset()
                    })
                };
                match child_pos {
                    Some(child_pos) => self.stack.push(Frame {
                        region,
                        end,
                        boundary: Some(boundary),
                        pos: child_pos,
                        step: Step::Headers,
                        depth: depth + 1,
                        digest,
                        enc_depth,
                        resume: Resume::Scan,
                    }),
                    // Boundary declared but absent: upstream demotes to
                    // a text leaf covering the rest of the frame. That
                    // demotion is recoverable damage — count it.
                    None => {
                        self.malformed += 1;
                        self.leaf(
                            region,
                            body,
                            head.cte,
                            Leaf::Text {
                                charset: head.charset,
                                html: false,
                            },
                        );
                    }
                }
            }
            Entity::NestedMessage => {
                self.nested_message(region, body, end, depth, enc_depth, &head)
            }
            Entity::Text { html } => self.leaf(
                region,
                body,
                head.cte,
                Leaf::Text {
                    charset: head.charset,
                    html,
                },
            ),
            Entity::Binary => self.leaf(region, body, head.cte, Leaf::Binary),
        }
    }

    /// Depth check shared by the two descents; pushes the `TooDeep`
    /// warning and sets `violated` when `depth + 1` exceeds the budget.
    fn child_depth(&mut self, depth: u32) -> bool {
        if depth + 1 > self.limits.max_depth() {
            self.warnings.push(ParseWarning::TooDeep {
                limit: self.limits.max_depth(),
            });
            self.violated = true;
            return false;
        }
        true
    }

    /// `message/rfc822` / `message/global` body: bound the raw or
    /// decoded extent, then open a frame over it. Encoded bodies decode
    /// here — the only allocation the walk performs — bounded by the
    /// same per-part budget before the decoder runs.
    fn nested_message(
        &mut self,
        region: usize,
        body: usize,
        end: usize,
        depth: u32,
        enc_depth: u32,
        head: &Head,
    ) {
        if !self.child_depth(depth) {
            return;
        }
        let (extent_len, after_match, found) = {
            let f = self.stack.last().expect("non-empty");
            scan_extent(
                &self.regions[region].bytes()[body..end],
                f.boundary.as_deref(),
            )
        };
        if !found {
            self.malformed += 1;
        }
        let extent_end = body + extent_len;
        let resume_pos = body + after_match;
        self.saw_attachment = true;
        match head.cte {
            Cte::Identity => {
                if extent_len as u64 > self.limits.max_decoded_part_bytes() {
                    return self.part_too_large();
                }
                self.stack.push(Frame {
                    region,
                    end: extent_end,
                    boundary: None,
                    pos: body,
                    step: Step::Headers,
                    depth: depth + 1,
                    digest: false,
                    enc_depth,
                    resume: Resume::AtMatch(resume_pos),
                });
            }
            Cte::Base64 | Cte::QuotedPrintable => {
                let bound =
                    decoded_bound(&self.regions[region].bytes()[body..extent_end], head.cte);
                if bound > self.limits.max_decoded_part_bytes() {
                    return self.part_too_large();
                }
                if enc_depth + 1 > MAX_ENCODED_NESTING {
                    // Upstream's depth-0 path keeps the part as an
                    // opaque binary attachment.
                    self.warnings.push(ParseWarning::NestedTooDeep {
                        limit: MAX_ENCODED_NESTING,
                    });
                    return self.resume_at(resume_pos);
                }
                let decoded = match head.cte {
                    Cte::Base64 => mail_parser::decoders::base64::base64_decode(
                        &self.regions[region].bytes()[body..extent_end],
                    ),
                    _ => mail_parser::decoders::quoted_printable::quoted_printable_decode(
                        &self.regions[region].bytes()[body..extent_end],
                    ),
                };
                match decoded {
                    Some(vec) => {
                        let idx = self.regions.len();
                        let len = vec.len();
                        self.regions.push(Region::Owned(vec));
                        self.stack.push(Frame {
                            region: idx,
                            end: len,
                            pos: 0,
                            step: Step::Headers,
                            boundary: None,
                            depth: depth + 1,
                            digest: false,
                            enc_depth: enc_depth + 1,
                            resume: Resume::AtMatch(resume_pos),
                        });
                    }
                    None => {
                        // Decode failed: upstream keeps the raw extent
                        // as a flagged attachment.
                        self.malformed += 1;
                        self.resume_at(resume_pos);
                    }
                }
            }
        }
    }

    /// Leaf body: scan its extent, apply the decoded-size budget,
    /// capture the first text extent, and move to `AtBoundaryMatch` —
    /// or end the frame when no boundary exists.
    fn leaf(&mut self, region: usize, body: usize, cte: Cte, kind: Leaf) {
        let (extent_len, after_match, found) = {
            let f = self.stack.last().expect("non-empty");
            scan_extent(
                &self.regions[region].bytes()[body..f.end],
                f.boundary.as_deref(),
            )
        };
        if !found {
            self.malformed += 1;
        }
        let extent_end = body + extent_len;
        let bound = materialized_bound(&self.regions[region].bytes()[body..extent_end], cte, &kind);
        if bound > self.limits.max_decoded_part_bytes() {
            return self.part_too_large();
        }
        match kind {
            Leaf::Text { charset, html } => {
                if self.first_text.is_none() {
                    self.first_text = Some(TextExtent {
                        region,
                        range: body..extent_end,
                        cte,
                        charset,
                        html,
                    });
                } else {
                    self.saw_attachment = true;
                }
            }
            Leaf::Binary => self.saw_attachment = true,
        }
        if found {
            let f = self.stack.last_mut().expect("non-empty");
            f.pos = body + after_match;
            f.step = Step::AtBoundaryMatch;
        } else {
            // Region exhausted: this frame is done.
            self.stack.last_mut().expect("non-empty").pos =
                self.stack.last().expect("non-empty").end;
            self.pop_frame();
        }
    }

    fn part_too_large(&mut self) {
        self.warnings.push(ParseWarning::PartTooLarge {
            limit: self.limits.max_decoded_part_bytes(),
        });
        self.violated = true;
    }

    fn resume_at(&mut self, pos: usize) {
        let f = self.stack.last_mut().expect("non-empty");
        f.pos = pos.min(f.end);
        f.step = Step::AtBoundaryMatch;
    }

    /// A multipart's epilogue after a nested container popped: scan to
    /// the next `--boundary` of this frame.
    fn step_scan_boundary(&mut self) {
        let (region, pos, end, boundary) = {
            let f = self.stack.last().expect("non-empty");
            (f.region, f.pos, f.end, f.boundary.clone())
        };
        let Some(boundary) = boundary else {
            self.pop_frame();
            return;
        };
        let mut stream = MessageStream::new(&self.regions[region].bytes()[pos..end]);
        if stream.seek_next_part(&boundary) {
            let f = self.stack.last_mut().expect("non-empty");
            f.pos = pos + stream.offset();
            f.step = Step::AtBoundaryMatch;
        } else {
            self.resume_at(end);
            self.pop_frame();
        }
    }

    /// Right after a `--boundary` match: `--` closes this frame,
    /// anything else is the next entity's boundary line.
    fn step_at_boundary_match(&mut self) {
        let (region, pos, end, has_boundary) = {
            let f = self.stack.last().expect("non-empty");
            (f.region, f.pos, f.end, f.boundary.is_some())
        };
        if !has_boundary || pos >= end {
            self.pop_frame();
            return;
        }
        let mut stream = MessageStream::new(&self.regions[region].bytes()[pos..end]);
        let ended = stream.is_multipart_end();
        self.stack.last_mut().expect("non-empty").pos = pos + stream.offset();
        if ended {
            self.pop_frame();
        } else {
            self.stack.last_mut().expect("non-empty").step = Step::Headers;
        }
    }
}
