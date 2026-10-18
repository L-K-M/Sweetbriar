//! Threading (spec/02 "Threads"): JWZ over parsed header fields,
//! returning which logical messages share a thread. Pure: the caller
//! loads the inputs and persists the groups (M1.3d).
//!
//! ```text
//!   message ──References──► [a] ─► [b] ─► [c] ─► message      (containers;
//!   message' ─In-Reply-To─► [c]                                  [x] may be
//!                                                                empty)
//!   roots: containers without a parent; roots with one base subject merge
//!   thread = every message under one merged root
//! ```
//!
//! Containers live in an arena that only records parents while links
//! change, so moving a container costs O(1); children are gathered once
//! at the end. Every walk is iterative and bounded, so hostile input
//! costs bounded time and no stack.

use std::collections::{BTreeMap, HashMap};

/// How many of a message's last References ids count (spec/02).
const MAX_REFERENCES: usize = 64;

/// How deep a parent may lie for a link to it to count (spec/02); a
/// root is at depth 0. Each cycle check walks at most this far.
const MAX_DEPTH: usize = 256;

/// What threading reads from one logical message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ThreadInput {
    pub(crate) message_id: i64,
    /// The Message-ID header, without angle brackets.
    pub(crate) header_id: Option<String>,
    /// The References ids, oldest first.
    pub(crate) references: Vec<String>,
    /// The first In-Reply-To id.
    pub(crate) in_reply_to: Option<String>,
    pub(crate) subject: Option<String>,
}

/// One node of the reference graph: a message, or a placeholder for an
/// id only seen in references.
#[derive(Debug, Default, Clone, Copy)]
struct Container {
    /// Index into the sorted inputs.
    message: Option<usize>,
    parent: Option<usize>,
}

/// The containers and the ids that name them.
#[derive(Default)]
struct Graph<'a> {
    containers: Vec<Container>,
    ids: HashMap<&'a str, usize>,
}

impl<'a> Graph<'a> {
    fn add(&mut self) -> usize {
        self.containers.push(Container::default());
        self.containers.len() - 1
    }

    /// The container named `id`, created empty when first seen.
    fn named(&mut self, id: &'a str) -> usize {
        if let Some(&found) = self.ids.get(id) {
            return found;
        }
        let added = self.add();
        self.ids.insert(id, added);
        added
    }

    /// Whether `child` may hang under `parent`: not if `child` is
    /// `parent` or above it (a cycle), nor if `parent` lies deeper than
    /// [`MAX_DEPTH`].
    fn may_link(&self, parent: usize, child: usize) -> bool {
        let mut at = parent;
        for _ in 0..=MAX_DEPTH {
            if at == child {
                return false;
            }
            match self.containers[at].parent {
                Some(up) => at = up,
                None => return true,
            }
        }
        false
    }

    /// Make `parent` the parent of `child`, unless [`Graph::may_link`]
    /// refuses.
    fn link(&mut self, parent: usize, child: usize) {
        if self.may_link(parent, child) {
            self.containers[child].parent = Some(parent);
        }
    }
}

/// Group `inputs` into threads: each group is the message ids of one
/// thread, ascending, and the groups are ordered by their first id.
/// The result does not depend on the inputs' order.
pub(crate) fn threads(inputs: &[ThreadInput]) -> Vec<Vec<i64>> {
    let mut sorted: Vec<&ThreadInput> = inputs.iter().collect();
    sorted.sort_by_key(|input| input.message_id);

    let mut graph = Graph::default();
    let mut own = Vec::with_capacity(sorted.len());
    for (index, input) in sorted.iter().enumerate() {
        // A repeated or missing Message-ID gets a container of its own.
        let container = match input.header_id.as_deref().filter(|id| !id.is_empty()) {
            Some(id) => {
                let named = graph.named(id);
                if graph.containers[named].message.is_some() {
                    graph.add()
                } else {
                    named
                }
            }
            None => graph.add(),
        };
        graph.containers[container].message = Some(index);
        own.push(container);
    }

    for (index, input) in sorted.iter().enumerate() {
        let references: &[String] = if input.references.is_empty() {
            input.in_reply_to.as_slice()
        } else {
            let skip = input.references.len().saturating_sub(MAX_REFERENCES);
            &input.references[skip..]
        };
        let chain: Vec<usize> = references
            .iter()
            .filter(|id| !id.is_empty())
            .map(|id| graph.named(id))
            .collect();
        // Earlier references only fill in parents not yet known.
        for pair in chain.windows(2) {
            if graph.containers[pair[1]].parent.is_none() {
                graph.link(pair[0], pair[1]);
            }
        }
        // The message's own last reference overrules any guess, which
        // goes first even when that link is refused (RFC 5256 §3 1.B).
        let container = own[index];
        graph.containers[container].parent = None;
        if let Some(&parent) = chain.last() {
            graph.link(parent, container);
        }
    }

    // Children in container order: messages first, by message id.
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); graph.containers.len()];
    let mut roots = Vec::new();
    for (at, container) in graph.containers.iter().enumerate() {
        match container.parent {
            Some(parent) => children[parent].push(at),
            None => roots.push(at),
        }
    }

    let mut merged = UnionFind::new(roots.len());
    let mut by_subject: HashMap<String, usize> = HashMap::new();
    let mut members = Vec::with_capacity(roots.len());
    for (slot, &root) in roots.iter().enumerate() {
        let under = messages_under(&graph, &children, root);
        // A placeholder root takes its earliest message's subject.
        let first = graph.containers[root]
            .message
            .or(under.iter().copied().min());
        let subject = first
            .and_then(|first| sorted[first].subject.as_deref())
            .map(base_subject)
            .filter(|subject| !subject.is_empty());
        if let Some(subject) = subject {
            match by_subject.get(&subject) {
                Some(&other) => merged.union(slot, other),
                None => {
                    by_subject.insert(subject, slot);
                }
            }
        }
        members.push(under);
    }

    let mut groups: BTreeMap<usize, Vec<i64>> = BTreeMap::new();
    for (slot, under) in members.into_iter().enumerate() {
        let group = groups.entry(merged.find(slot)).or_default();
        group.extend(under.into_iter().map(|index| sorted[index].message_id));
    }
    let mut groups: Vec<Vec<i64>> = groups
        .into_values()
        .filter(|group| !group.is_empty())
        .map(|mut group| {
            group.sort_unstable();
            group
        })
        .collect();
    groups.sort_unstable_by_key(|group| group[0]);
    groups
}

/// Every message under `root`, the root's own included.
fn messages_under(graph: &Graph<'_>, children: &[Vec<usize>], root: usize) -> Vec<usize> {
    let mut found = Vec::new();
    let mut stack = vec![root];
    while let Some(at) = stack.pop() {
        found.extend(graph.containers[at].message);
        stack.extend(children[at].iter().copied());
    }
    found
}

/// Disjoint sets over root slots, for subject merging.
struct UnionFind {
    parent: Vec<usize>,
}

impl UnionFind {
    fn new(len: usize) -> Self {
        Self {
            parent: (0..len).collect(),
        }
    }

    fn find(&mut self, mut at: usize) -> usize {
        while self.parent[at] != at {
            self.parent[at] = self.parent[self.parent[at]];
            at = self.parent[at];
        }
        at
    }

    fn union(&mut self, a: usize, b: usize) {
        let (a, b) = (self.find(a), self.find(b));
        self.parent[a.max(b)] = a.min(b);
    }
}

/// RFC 5256's base subject, lowercased (spec/02): runs of spaces, tabs
/// and line breaks become one space, then trailing `(fwd)`, leading
/// `Re:`/`Fw:`/`Fwd:` and `[…]` blobs, and a `[Fwd: …]` wrapper are
/// removed until nothing changes. Every step only narrows a slice, so
/// the work is linear in the subject's length.
///
/// `"Re: [list] Fwd: Lunch (fwd)"` → `"lunch"`.
pub(crate) fn base_subject(subject: &str) -> String {
    // Step 1: only RFC whitespace, not Unicode separators.
    let normalized = subject
        .split([' ', '\t', '\r', '\n'])
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let mut text = normalized.as_str();
    loop {
        // Step 2: trailers.
        loop {
            text = text.trim_end_matches(' ');
            match strip_suffix_ignoring_case(text, "(fwd)") {
                Some(rest) => text = rest,
                None => break,
            }
        }
        // Steps 3–5: leaders and leading blobs.
        text = strip_leaders(text);
        // Step 6: a `[Fwd: …]` wrapper starts the cycle again.
        match strip_prefix_ignoring_case(text, "[fwd:").and_then(|rest| rest.strip_suffix(']')) {
            Some(inner) => text = inner,
            None => return text.to_lowercase(),
        }
    }
}

/// RFC 5256 steps 3–5: drop every `subj-leader` — blobs then `re`, `fw`
/// or `fwd`, an optional blob and `:` — and then the leading blobs
/// whose removal leaves text. Each blob is scanned once: blobs that no
/// leader follows are removed together, which is where repeating steps
/// 3 and 4 one blob at a time ends.
fn strip_leaders(mut text: &str) -> &str {
    loop {
        text = text.trim_start_matches(' ');
        let mut after_blobs = text;
        let mut last_blob = None;
        while let Some(rest) = strip_blob(after_blobs) {
            last_blob = Some(after_blobs);
            after_blobs = rest;
        }
        if let Some(rest) = strip_reply_or_forward(after_blobs) {
            text = rest;
            continue;
        }
        // Step 4: the last blob stays when nothing follows it.
        return match last_blob {
            Some(last) if after_blobs.is_empty() => last,
            _ => after_blobs,
        };
    }
}

/// `text` after a leading `re`, `fw` or `fwd`, an optional blob and
/// `:`, or `None` when it doesn't start with one.
fn strip_reply_or_forward(text: &str) -> Option<&str> {
    // `fwd` before `fw`: the longer leader wins.
    let after = ["re", "fwd", "fw"]
        .into_iter()
        .find_map(|word| strip_prefix_ignoring_case(text, word))?
        .trim_start_matches(' ');
    let after = strip_blob(after).unwrap_or(after);
    after.strip_prefix(':')
}

/// `text` without `prefix`, compared ignoring ASCII case.
fn strip_prefix_ignoring_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let head = text.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &text[prefix.len()..])
}

/// `text` without `suffix`, compared ignoring ASCII case.
fn strip_suffix_ignoring_case<'a>(text: &'a str, suffix: &str) -> Option<&'a str> {
    let start = text.len().checked_sub(suffix.len())?;
    let tail = text.get(start..)?;
    tail.eq_ignore_ascii_case(suffix).then(|| &text[..start])
}

/// `text` without a leading `[…]` blob and the spaces after it, if it
/// starts with one. The scan stops at the first bracket, so a run of
/// nested `[` costs no more than its length.
fn strip_blob(text: &str) -> Option<&str> {
    let inner = text.strip_prefix('[')?;
    let close = inner.find(['[', ']'])?;
    if inner.as_bytes()[close] != b']' {
        return None;
    }
    Some(inner[close + 1..].trim_start_matches(' '))
}

#[cfg(test)]
mod tests;
