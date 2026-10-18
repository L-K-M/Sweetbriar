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
//! Containers live in an arena and every walk is iterative, so a
//! hostile chain of references costs time, never stack.

use std::collections::{BTreeMap, HashMap};

/// How many of a message's last References ids count (spec/02).
const MAX_REFERENCES: usize = 128;

/// How deep a parent may lie for a link to it to count (spec/02): each
/// cycle check walks at most this far.
const MAX_DEPTH: usize = 1_024;

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
#[derive(Debug, Default)]
struct Container {
    /// Index into the sorted inputs.
    message: Option<usize>,
    parent: Option<usize>,
    children: Vec<usize>,
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
    /// `parent` or above it, nor if `parent` lies deeper than
    /// [`MAX_DEPTH`].
    fn may_link(&self, parent: usize, child: usize) -> bool {
        let mut current = Some(parent);
        for _ in 0..=MAX_DEPTH {
            let Some(at) = current else {
                return true;
            };
            if at == child {
                return false;
            }
            current = self.containers[at].parent;
        }
        false
    }

    fn unlink(&mut self, child: usize) {
        let Some(parent) = self.containers[child].parent.take() else {
            return;
        };
        self.containers[parent].children.retain(|&c| c != child);
    }

    /// Make `parent` the parent of `child`, unless [`Graph::may_link`]
    /// refuses.
    fn link(&mut self, parent: usize, child: usize) {
        if !self.may_link(parent, child) {
            return;
        }
        self.unlink(child);
        self.containers[child].parent = Some(parent);
        self.containers[parent].children.push(child);
    }

    /// Every message under `root`, in pre-order with children in the
    /// order they were linked.
    fn messages_under(&self, root: usize) -> Vec<usize> {
        let mut found = Vec::new();
        let mut stack = vec![root];
        while let Some(at) = stack.pop() {
            found.extend(self.containers[at].message);
            stack.extend(self.containers[at].children.iter().rev());
        }
        found
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
        let references: Vec<&str> = if input.references.is_empty() {
            input.in_reply_to.iter().map(String::as_str).collect()
        } else {
            let skip = input.references.len().saturating_sub(MAX_REFERENCES);
            input.references[skip..]
                .iter()
                .map(String::as_str)
                .collect()
        };
        let chain: Vec<usize> = references
            .into_iter()
            .filter(|id| !id.is_empty())
            .map(|id| graph.named(id))
            .collect();
        // Earlier references only fill in parents not yet known.
        for pair in chain.windows(2) {
            if graph.containers[pair[1]].parent.is_none() {
                graph.link(pair[0], pair[1]);
            }
        }
        // The message's own last reference overrules any guess.
        let container = own[index];
        match chain.last() {
            Some(&parent) => graph.link(parent, container),
            None => graph.unlink(container),
        }
    }

    let roots: Vec<usize> = (0..graph.containers.len())
        .filter(|&at| graph.containers[at].parent.is_none())
        .collect();
    let mut merged = UnionFind::new(roots.len());
    let mut by_subject: HashMap<String, usize> = HashMap::new();
    let mut members = Vec::with_capacity(roots.len());
    for (slot, &root) in roots.iter().enumerate() {
        let under = graph.messages_under(root);
        let subject = under
            .first()
            .and_then(|&first| sorted[first].subject.as_deref())
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

/// RFC 5256's base subject, lowercased (spec/02): whitespace collapsed,
/// then trailing `(fwd)`, leading `Re:`/`Fw:`/`Fwd:` and `[…]` blobs, and
/// a `[Fwd: …]` wrapper removed until nothing changes.
///
/// `"Re: [list] Fwd: Lunch (fwd)"` → `"lunch"`.
pub(crate) fn base_subject(subject: &str) -> String {
    let mut text = subject.split_whitespace().collect::<Vec<_>>().join(" ");
    loop {
        // RFC 5256 step 2: trailers.
        loop {
            let trimmed = text.trim_end();
            match strip_suffix_ignoring_case(trimmed, "(fwd)") {
                Some(rest) => text = rest.to_owned(),
                None => {
                    text = trimmed.to_owned();
                    break;
                }
            }
        }
        // Steps 3–5: leaders and leading blobs.
        loop {
            let before = text.len();
            text = strip_leader(&text);
            if let Some(rest) = strip_blob(&text).filter(|rest| !rest.is_empty()) {
                text = rest.to_owned();
            }
            if text.len() == before {
                break;
            }
        }
        // Step 6: a `[Fwd: …]` wrapper starts the cycle again.
        if let Some(inner) =
            strip_prefix_ignoring_case(&text, "[fwd:").and_then(|rest| rest.strip_suffix(']'))
        {
            text = inner.trim().to_owned();
            continue;
        }
        return text.to_lowercase();
    }
}

/// Remove one `subj-leader`: blobs then `re`, `fw` or `fwd`, an
/// optional blob and `:` — or leading whitespace.
fn strip_leader(text: &str) -> String {
    let trimmed = text.trim_start();
    if trimmed.len() != text.len() {
        return trimmed.to_owned();
    }
    let mut rest = text;
    while let Some(after) = strip_blob(rest) {
        rest = after;
    }
    // `fwd` before `fw`: the longer leader wins.
    let Some(mut after) = ["re", "fwd", "fw"]
        .into_iter()
        .find_map(|word| strip_prefix_ignoring_case(rest, word))
    else {
        return text.to_owned();
    };
    after = after.trim_start();
    if let Some(unblobbed) = strip_blob(after) {
        after = unblobbed;
    }
    match after.strip_prefix(':') {
        Some(subject) => subject.trim_start().to_owned(),
        None => text.to_owned(),
    }
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

/// `text` without a leading `[…]` blob and its trailing whitespace, if
/// it starts with one.
fn strip_blob(text: &str) -> Option<&str> {
    let inner = text.strip_prefix('[')?;
    let close = inner.find(']')?;
    if inner[..close].contains('[') {
        return None;
    }
    Some(inner[close + 1..].trim_start())
}

#[cfg(test)]
mod tests;
