//! Literal plain-text blocks.
//!
//! Never Markdown: only `>` quote runs and unambiguous bullet lines
//! become structure. Ordered lists are not inferred (the MailMate
//! 2025 incident had dates become list numbers), and plain-text bullets
//! stay flat; nested lists come from HTML. Line breaks inside a
//! paragraph are preserved as explicit breaks.

use crate::PartId;

use super::html::MAX_DEPTH;
use super::{Block, Inline, LineBreak, List, ListItem, Paragraph, Quote, Text};

/// Literal blocks, or `None` once quote or bullet nesting passes the model's
/// maximum depth (sender layout, not personal mail).
pub(super) fn blocks(text: &str, part: &PartId) -> Option<Vec<Block>> {
    // A CRLF pair is one line break; a lone CR is another. Collapsing both
    // at once would turn every CRLF into a blank line.
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = normalized.split('\n').collect();
    blocks_from_lines(&lines, part, 0)
}

fn blocks_from_lines(lines: &[&str], part: &PartId, depth: usize) -> Option<Vec<Block>> {
    let mut blocks = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        if lines[index].trim().is_empty() {
            index += 1;
            continue;
        }
        if quote_depth(lines[index]).is_some() {
            if depth >= MAX_DEPTH {
                return None;
            }
            let mut quoted = Vec::new();
            while index < lines.len() && quote_depth(lines[index]).is_some() {
                quoted.push(lines[index]);
                index += 1;
            }
            let inner = quoted
                .iter()
                .map(|line| strip_quote(line))
                .collect::<Vec<_>>()
                .join("\n");
            let inner_lines: Vec<&str> = inner.split('\n').collect();
            blocks.push(Block::Quote(Quote {
                part: part.clone(),
                blocks: blocks_from_lines(&inner_lines, part, depth + 1)?,
            }));
            continue;
        }
        if bullet(lines[index]).is_some() {
            let mut items = Vec::new();
            while index < lines.len() {
                let Some(text) = bullet(lines[index]) else {
                    break;
                };
                let mut item_lines = vec![text];
                index += 1;
                // An indented, non-blank continuation line belongs to the
                // item; a nested bullet starts a new (flat) item.
                while index < lines.len()
                    && !lines[index].trim().is_empty()
                    && bullet(lines[index]).is_none()
                    && lines[index].starts_with([' ', '\t'])
                {
                    item_lines.push(lines[index]);
                    index += 1;
                }
                if depth >= MAX_DEPTH {
                    return None;
                }
                items.push(ListItem {
                    part: part.clone(),
                    number: None,
                    blocks: blocks_from_lines(&item_lines, part, depth + 1)?,
                });
            }
            blocks.push(Block::List(List {
                part: part.clone(),
                ordered: false,
                start: 1,
                reversed: false,
                items,
            }));
            continue;
        }
        let mut paragraph = Vec::new();
        while index < lines.len()
            && !lines[index].trim().is_empty()
            && quote_depth(lines[index]).is_none()
            && bullet(lines[index]).is_none()
        {
            paragraph.push(lines[index]);
            index += 1;
        }
        blocks.push(paragraph_block(&paragraph, part));
    }
    Some(blocks)
}

/// Depth of a `>` quote run: `> > text` is two levels deep.
fn quote_depth(line: &str) -> Option<usize> {
    let mut depth = 0;
    for c in line.chars() {
        match c {
            '>' => depth += 1,
            ' ' | '\t' => {}
            _ => break,
        }
    }
    (depth > 0).then_some(depth)
}

fn strip_quote(line: &str) -> &str {
    let trimmed = line.trim_start();
    let Some(after) = trimmed.strip_prefix('>') else {
        return line;
    };
    after
        .strip_prefix(' ')
        .or_else(|| after.strip_prefix('\t'))
        .unwrap_or(after)
}

/// The text of a `- `, `* ` or `+ ` bullet line. Ordered markers are
/// deliberately not recognized.
fn bullet(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = trimmed.strip_prefix(marker) {
            return Some(rest);
        }
    }
    None
}

fn paragraph_block(lines: &[&str], part: &PartId) -> Block {
    let mut inlines = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            inlines.push(Inline::LineBreak(LineBreak { part: part.clone() }));
        }
        inlines.push(Inline::Text(Text {
            part: part.clone(),
            text: (*line).to_owned(),
        }));
    }
    Block::Paragraph(Paragraph {
        part: part.clone(),
        inlines,
    })
}
