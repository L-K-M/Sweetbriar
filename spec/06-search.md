# 06 — Search

Rests on research [05-search](../research/05-search.md) and D7 in
[debate/04-resolution](../research/debate/04-resolution.md). The engine
is **SQLite FTS5**. Tantivy may replace it only by passing the bars
registered in the resolution.

## Extraction

- **Inputs:** a background index job extracts searchable text from each
  acquisition's parsed MIME tree, **independently of how the reader
  renders it**.
- **Fields extracted:**

  | Field | Contents |
  |---|---|
  | `subject` | |
  | `from` | address and display name |
  | `to_text` | |
  | `cc_text` | |
  | `body_authored` | text before the detected quote or signature boundary |
  | `body_quoted` | quoted text |
  | `body_signature` | signature |
  | `attachment_names` | |
  | `attachment_text` | later; PDF via PDFKit on macOS |

- **When quote detection is unsure,** the text goes into `body_authored`.
  Nothing extractable is dropped.
- **Sources:** the `text/plain` part when present, and text extracted
  from HTML parts by stripping markup (`html5ever`). If both exist, both
  are indexed, deduplicated by normalized paragraph.
- **Limits:**
  - at most 1 MB of text per message and 64 KB per attachment name list;
  - parsing runs with depth and size caps (10-security).
  - A message hitting a cap is marked `partial` in coverage.
- **Versioning:** the `extractor_version` is stored. A version bump
  re-queues every document.

## Schema

```sql
CREATE TABLE d_search_docs (             -- one row per acquisition with extracted text
  doc_id            INTEGER PRIMARY KEY, -- = acquisitions.acquisition_id
  extractor_version INTEGER NOT NULL,
  state             TEXT NOT NULL CHECK (state IN ('indexed','partial','failed')),
  truncated_fields  TEXT NOT NULL DEFAULT '[]', -- JSON array of field names cut by a cap
  subject           TEXT,
  from_text         TEXT,
  to_text           TEXT,
  cc_text           TEXT,
  body_authored     TEXT,
  body_quoted       TEXT,
  body_signature    TEXT,
  attachment_names  TEXT
) STRICT;

CREATE VIRTUAL TABLE d_search_fts USING fts5(
  subject, from_text, to_text, cc_text, body_authored, body_quoted, body_signature, attachment_names,
  content='d_search_docs', content_rowid='doc_id',
  tokenize = "unicode61 remove_diacritics 2 tokenchars '@.-_+'",
  prefix = '2 3'
);

CREATE TABLE d_index_queue (              -- work for the index job; durable watermark
  doc_id      INTEGER PRIMARY KEY,
  reason      TEXT NOT NULL CHECK (reason IN ('new','reextract','delete')),
  enqueued_at INTEGER NOT NULL
) STRICT;
```

- **Placement (pilot):** FTS5 lives in the same database file as derived
  tables. Index writes go through the writer task at low priority, in
  batches of at most 200 documents. Moving FTS5 to a separate derived
  database is a measured option (research D7). The queue table makes
  that move mechanical.
- **Enqueueing:** acquisitions are queued in the same transaction that
  inserts them, so the index can never silently miss one.
- **Tokenizer:** `tokenchars '@.-_+'` keeps email addresses, order
  numbers and identifiers searchable as whole tokens.
  - A second, trigram-tokenized table for substring and CJK search is
    *later*, behind the Tantivy-or-trigram decision.
  - **Pilot limitation:** CJK text is indexed by `unicode61`, which
    doesn't segment words, so CJK search is weak. This is stated in the
    coverage UI.
- **Ranking:** `bm25(d_search_fts, 4.0, 2.0, 1.0, 1.0, 1.0, 0.25, 0.1, 0.75)`
  weights subject, from, to, cc, authored, quoted, signature and
  attachment names. These are starting values, validated against the
  judged task set.
- **Rebuild:** dropping all `d_search_*` tables and re-queueing every
  acquisition must reproduce the index. Rebuild is a visible, resumable
  operation.

## Query AST v1

One typed representation, serialized as JSON with a version. It is
shared by the search field, saved views and, later, rules.

```json
{"v": 1, "q": {"and": [
  {"field": "from", "op": "contains", "value": "alice@example.com"},
  {"text": "invoice"},
  {"not": {"flag": "seen"}},
  {"date": {"after": "2026-01-01", "tz": "Europe/Zurich"}},
  {"account": "uuid"}, {"mailbox": 42}
]}}
```

**Node types:**

| Node | Meaning |
|---|---|
| `text` | Full-text query (FTS5) |
| `field` | `from`, `to`, `cc`, `subject`, `body`, `attachment`, each with `contains`, `is` or `domain`. `contains` uses FTS5 on the field's column. `is` and `domain` match the parsed address lists and header values in `d_message_summary` exactly, never the tokenized text |
| `flag` | `seen`, `flagged`, `answered`, `draft` |
| `tag`, `has` | `has` takes `attachment` or `note` |
| `date` | `before`, `after`, `on`, `within_days`; always with an explicit IANA timezone; relative dates also carry the evaluation clock |
| `size` | |
| `account`, `mailbox` | Scope |
| `and`, `or`, `not` | Combinators |

**Rules:**

- **Unknown under negation.** A message whose body isn't downloaded or
  indexed is `unknown` for body and text predicates. So is a document
  indexed as `partial` (an extraction cap was hit), for the fields that
  were truncated: a term beyond the cap may still be in the message.
  `not unknown` stays `unknown`, so it never matches. Results show a count of messages
  excluded as unknown ("312 messages not yet searchable").
- **Per-use subsets:** search may use every node. Saved views may use
  every node. Rules (*later*) may not use ranked or fuzzy constructs.
- **Exactness:** `field … is` and quoted phrases are never stemmed or
  rewritten.
- **Front ends (pilot):**
  - a text syntax: `from:`, `to:`, `subject:`, `body:`, `has:attachment`,
    `is:unread`, `is:flagged`, `before:`, `after:`, `in:`, `tag:`,
    quoted phrases, `-term`, `OR`, parentheses;
  - chips that round-trip with that syntax.
  - Natural-language queries are *later* (D15).
- **Parser:** a hand-written parser in the core, used by both shells.
  Invalid syntax returns an error with a position, never a silent
  reinterpretation.

## Execution

- Filters (account, mailbox, flags, dates) are applied **inside** the SQL
  query before ranking and limit, so a small account's hits can't vanish
  under a large one's.
- **Results stream:** the first 50 hits are ranked or date-sorted; more
  are fetched on scroll with keyset paging over `(rank or date, id)`.
- **Freshness:** a newly acquired message is searchable within 2 s at
  p95 while the index job keeps up. Flag and mailbox predicates read live
  authoritative tables, so they are always current.
- **Server search** (IMAP `SEARCH` on Bridge) is *later*. Bridge
  searches Proton's encrypted store with limitations (04).

## Coverage

The core exposes, per account and per mailbox:

- headers synced;
- bodies acquired;
- documents indexed, partial or failed;
- queue length.

The search UI shows coverage whenever it is below 100% for the current
scope.

## Acceptance targets (pilot)

On the owner's Mac with 100k messages:

- cached exact search, p95 ≤ 100 ms and p99 ≤ 250 ms during background
  sync;
- new mail searchable within 2 s at p95;
- index rebuild ≤ 15 min.
