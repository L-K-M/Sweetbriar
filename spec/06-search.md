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
  | `subject` | decoded root Subject header |
  | `from` | address and display name |
  | `to_text` | decoded root To mailboxes: display names and addresses |
  | `cc_text` | decoded root Cc mailboxes: display names and addresses |
  | `body_authored` | text outside positively detected quotes and signatures |
  | `body_quoted` | quoted text |
  | `body_signature` | signature |
  | `attachment_names` | decoded MIME attachment filenames, in part order |
  | `attachment_text` | later; PDF via PDFKit on macOS |

- **When quote detection is unsure,** the text goes into `body_authored`.
  Nothing extractable is dropped.
- **Sources:** the `text/plain` part when present, and text extracted
  from HTML parts by stripping markup (`html5ever`). If both exist, both
  are indexed, deduplicated by normalized paragraph.
- **Limits:**
  - at most 1,000,000 UTF-8 bytes per message and 64,000 per attachment
    name list (decimal units);
  - parsing runs with depth and size caps (10-security).
  - A message hitting a cap is marked `partial` in coverage.
- **Versioning:** the `extractor_version` is stored. A version bump
  re-queues every document.

### Extraction v1 details (M1.6a)

- `from` maps to `from_text`; flatten all root From mailboxes, including
  groups, with names and addresses separated by newlines. These fields
  come from the acquisition, not the display snippet or the sanitized
  reader. Missing fields are empty, not extraction failures.
- Traverse inline plain and HTML body candidates in MIME part order,
  including both alternatives; exclude attachment bodies and nested
  `message/rfc822` contents. Attachment names include attached messages,
  but no file contents. `attachment_text` has no pilot column or predicate.
- Decode transfer encodings and charsets with the MIME parser's limits
  (10); decode format=flowed before partitioning plain text. For HTML,
  `html5ever` emits text nodes with block boundaries; script/style contents
  are omitted, entities decoded and no URLs fetched. Parse with scripting
  disabled so `noscript` fallback markup contributes text, not raw tags.
  No reader cache is read.
- Bound HTML tree building independently of the MIME byte limits. Feed at
  most 1,024 UTF-8 bytes at a time. Stop between chunks after 10,000 arena
  nodes, 4,000,000 work units, or 16,384 input bytes without a tree mutation.
  Charge input/text bytes, node/name comparisons, per-tag attribute counts
  squared (n² for a tag carrying n attributes),
  and the length of sibling lists scanned or shifted. At the tokenizer boundary,
  also charge attribute scans using a conservative tag-span bound (including
  ignored tags), and potential formatting-list comparisons before tree building.
  This bounds tree repair
  and unfinished tokens without approximating HTML context. On exhaustion,
  omit that candidate's body and mark all three body fields partial; keep
  decoded headers, filenames and other candidates. No time-based cutoff.
- Normalize line endings to LF. For paragraph deduplication, a blank line
  separates paragraphs; collapse Unicode whitespace to one ASCII space
  and trim for comparison, case preserved. Keep the first equal paragraph
  across plain/HTML candidates, with its first classification and text.
- V1 detects only explicit quotes (plain lines whose first non-space
  character is `>`, HTML `blockquote`) and signatures (a plain `-- ` line
  starts a signature through that candidate's end, excluding quoted text).
  Everything else is authored. Delimiters stay with their classified text;
  uncertain reply headings and signatures stay authored. No stemming.
- Caps count UTF-8 bytes: 1,000,000 total across the eight columns, of which
  at most 64,000 are attachment names. Fill in schema column order, cutting
  at a character boundary. `truncated_fields` is a sorted, unique JSON
  array of column names whose text may be missing, including later fields
  starved of budget. A MIME limit marks every potentially affected field;
  use all eight when the affected set is unknown. `partial` requires a
  nonempty mask. A parser failure is `failed`, with empty columns; an I/O
  failure is reported and never published as a successful empty document.

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
  tokenize = "unicode61 remove_diacritics 2",
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
- **Keeping FTS5 in step:** `d_search_fts` is an external-content table,
  so SQLite doesn't index `d_search_docs` changes by itself. In one writer
  transaction per batch, the index job:
  1. when replacing an indexed/partial row, issues FTS5's `'delete'`
     command with the old values;
  2. writes the `d_search_docs` row;
  3. inserts the new values into `d_search_fts` unless extraction failed;
  4. deletes the `d_index_queue` entry.
  - A crash before commit leaves the queue entry and the batch reruns;
    after commit the complete batch is already visible.
- **Tokenizer:** plain `unicode61`, so `alice@example.com` indexes as
  `alice`, `example`, `com` and `A-1234` as `a`, `1234`. Searching for
  `example` or `1234` finds them.
  - **Exact identifiers:** the query parser turns any term containing
    punctuation (`alice@example.com`, `A-1234`, `2026-001`) into an FTS5
    **phrase** of its tokens, so it matches them adjacent and in order.
    (Tested: `"A-1234"` and `"alice@example.com"` match;
    `"paid invoice"` matches "Paid invoice.".)
  - An earlier draft used `tokenchars`; it was dropped because it glued
    sentence punctuation into tokens and broke substring matches.
  - **Exact address equality** (`from.is:…`) uses `message_headers`
    for synced mail, else `d_message_summary`, never FTS.
  - A second, trigram-tokenized table for substring and CJK search is
    *later*, behind the Tantivy-or-trigram decision.
  - **Pilot limitation:** CJK text is indexed by `unicode61`, which
    doesn't segment words, so CJK search is weak. This is stated in the
    coverage UI.
- **Ranking:** `bm25(d_search_fts, 4.0, 2.0, 1.0, 1.0, 1.0, 0.25, 0.1, 0.75)`
  weights subject, from, to, cc, authored, quoted, signature and
  attachment names. These are starting values, validated against the
  judged task set.
- **Rebuild:** derived search state is reproducible from acquisitions
  and the target extractor version. Clearing it and re-queueing every
  acquisition must reproduce the index. The visible, resumable rebuild
  protocol below preserves the table layout and queue revision counter.

### Queue, versions and recovery (M1.6a)

The SQL block above is the implemented schema, used by the schema-drift
test. M1.6d adds `revision INTEGER NOT NULL CHECK (revision >= 1)` to
`d_index_queue` in a forward migration, then updates that SQL block with
the migration. It also adds `d_index_queue_oldest`, an index on
`d_index_queue(enqueued_at, doc_id)`. These requirements describe that
target schema. M1.6d2 separately migrates the durable rebuild/worker record
specified in 07, initializing the monotonic epoch to 0 with no rebuild job
or pause. These future fields stay in prose until their migration lands.

- The tables above already exist in schema v1. Do not edit that migration.
  An additive migration adds queue revisions, initializes the metadata
  below, assigns distinct revisions to existing pending entries and queues
  every acquisition lacking current extraction. Set `index_inputs` to the
  largest assigned revision (0 for no work). Enqueue new acquisitions
  even if summary parsing failed; summary insertion is not the enqueue
  condition. The queue key is an acquisition, never an occurrence or a
  logical message.
- `store_meta.index_inputs` is a monotonically increasing integer, initially
  0. Every enqueue/upsert increments it and copies it into `revision`.
  `enqueued_at` is the oldest still-pending request time, for progress,
  not a correctness watermark. Requeue replaces the reason from current
  source state: `delete` removes a derived document, never an acquisition.
  Restored published bytes enqueue `reextract` and supersede `delete`.
- The queue is the durable watermark: there is no "largest processed id"
  that could skip older requeued work. Workers read `(doc_id, revision)`
  ordered by `enqueued_at ASC, doc_id ASC` using that index, and extract
  outside write transactions. In the writer they publish only if that
  revision, rebuild epoch and target extractor version still match, and
  indexing is not paused. A stale result writes nothing, including no queue
  deletion. Accepted publication deletes only that revision. This avoids
  acknowledging newer work that arrived during extraction.
- `store_meta.search_extractor_version` holds the target version (v1 starts
  at 1). An app unable to read that target version never downgrades it
  or runs indexing. Coverage reports `BlockedNewerExtractor` with the
  target version (07); Search opens return `Unavailable` explaining that
  an app update is required. Other list windows remain available. This is
  an index capability state, not a schema migration error.
- Runtime version changes and explicit rebuilds use the same durable,
  bounded protocol. A short start transaction sets the target version,
  increments a monotonic rebuild epoch, creates a singleton rebuild job
  (07), and records phase `reset` with empty cursors (or restarts the active
  job on a version change, preserving its user pause). It does not scan,
  clear or enqueue the corpus. Every worker captures the epoch as well as
  revision/version; publication rechecks all three in the writer. Starting
  a reset therefore rejects already-extracted work, even at the same version.
- During `reset` and `enqueue`, extraction/publication is suspended.
  Search reads/opens return `Unavailable` explaining the rebuild phase;
  existing handles invalidate. Coverage remains readable, with all scoped
  messages pending throughout reset/enqueue; it never reports old documents
  as current. Ordinary lists remain usable. No old FTS corpus participates
  in ranking during reset. Starting a version change also makes old-version
  documents unknown immediately.
- Reset keyset-scans derived docs by doc id. Each low-priority transaction
  removes at most 200 documents, issuing FTS's external-content `'delete'`
  with each old indexed/partial row's values before deleting its docs row
  (failed rows have no FTS entry). It commits its cursor and progress with
  those changes. No runtime `delete-all` or corpus-wide DELETE is allowed.
  After docs drain, persist phase `enqueue` and an acquisition cursor.
- Enqueue keyset-scans acquisitions in batches of at most 200. In each
  transaction, queue/upsert each acquisition from its current availability
  (`reextract` for published bytes, `delete` otherwise), assign fresh queue
  revisions, and commit the cursor/progress. Preserve the revision counter
  and existing pending entries. New acquisitions and availability changes
  still enqueue in their own publication transactions, including behind
  the scan cursor; they cannot be missed. End-of-scan switches atomically
  to phase `index`, enabling ordinary queue processing. Queue membership,
  not a scan cursor, remains the extraction acknowledgement watermark.
- Each phase transition, batch and completion checks the active epoch/job
  and pause state in the writer; stale or paused work cannot move cursors,
  publish documents or acknowledge requests.
  Cursors and phase changes commit with their effects, so crash/open resumes
  exactly that phase. Yield to foreground writes between every batch; all
  reset/enqueue/publication transactions obey 03's bounds. Completion is a
  writer transaction observing an empty queue in phase `index`. Progress
  exposes phase, committed phase document count and global live queue length,
  including increments of the index-phase count with accepted publication,
  not an unbounded pre-count or a misleading fixed-total percentage.
  Cancellation and explicit resume use 07's durable job controls; cancelled
  work does not restart on open. An uncancelled job resumes on open.
- Batch publication is all-or-nothing, including FTS delete/insert, docs,
  queue acknowledgements and search generation (07). A delete removes the
  old FTS values before its docs row; failed documents have no FTS row.
  Replacing a failed row therefore needs no FTS delete. SQL/I/O failure
  leaves work pending and reports an error, with retry backoff; a permanent
  parse failure publishes `failed` and acknowledges only that revision.
  Failed/partial documents retry after a version change, restored bytes
  or an explicit rebuild, not in a tight loop. Startup after any crash
  sees either the old batch and queue or the complete new batch.
- Missing or quarantined acquisitions cannot use stale indexed text:
  search treats them as unknown immediately, and the state-change
  transaction queues `delete`. Re-publication queues extraction. Rebuilds
  and retries never touch authoritative rows or `seen_addresses` history.

## Query AST v1

One typed representation, serialized as JSON with a version. It is
shared by the search field, saved views and, later, rules.

```json
{"v": 1, "q": {"and": [
  {"field": "from", "op": "contains", "value": "alice@example.com"},
  {"text": "invoice"},
  {"not": {"flag": "seen"}},
  {"date": {"after": "2026-01-01", "tz": "Europe/Zurich"}},
  {"account": "12345678-1234-4234-8234-123456789abc"}, {"mailbox": 42}
]}}
```

**Node types:**

| Node | Meaning |
|---|---|
| `text` | Full-text query (FTS5) |
| `field` | `from`, `to` and `cc` take `contains`, `is` or `domain`; `subject`, `body` and `attachment` take `contains` only. `contains` uses FTS5 on the field's columns (`body`: `body_authored`, `body_quoted` and `body_signature`). `is` and `domain` match parsed address lists exactly, never the tokenized text: from `message_headers` for synced mail, else `d_message_summary` |
| `flag` | `seen`, `flagged`, `answered`, `draft` |
| `tag`, `has` | `has` takes `attachment` or `note` |
| `date` | `before`, `after`, `on`, `within_days`; always with an explicit IANA timezone; relative dates also carry the evaluation clock |
| `size` | exact byte length comparison (`lt`, `eq`, `gt`) |
| `account`, `mailbox` | account or present-mailbox membership predicate; outer scope is in 07 |
| `and`, `or`, `not` | Combinators |

**Rules:**

- **Unknown under negation.** A message whose body isn't downloaded or
  indexed is `unknown` for every FTS predicate (`text`, and `contains`
  on any field), and for `is`/`domain` when neither `message_headers`
  nor a complete summary exists (partial summaries follow the conservative
  rule below). So is a document
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

### V1 wire types and text grammar (M1.6a)

The JSON envelope is exactly `{ "v": 1, "q": Node }`. Reject unknown
versions, duplicate/unknown keys, enum values and mixed node forms.
Canonical JSON follows 02 (sorted keys, no insignificant whitespace).
Nodes have these shapes:

| Shape | Operands |
|---|---|
| `{"text": string}` | literal term or phrase, never raw MATCH syntax |
| `{"field": field, "op": op, "value": string}` | fields/operators from the table above |
| `{"flag": flag}` | `seen`, `flagged`, `answered`, `draft` |
| `{"tag": uuid}`, `{"has": kind}` | tag id; `attachment` or `note` |
| `{"date": {op: value, "tz": zone}}` | exactly one of `before`, `after`, `on` (ISO civil date) or `within_days` (positive u32); the last also requires `clock` (UTC i64 milliseconds) |
| `{"size": {"op": op, "bytes": n}}` | `lt`, `eq`, `gt`; nonnegative i64 bytes |
| `{"account": uuid}`, `{"mailbox": id}` | canonical UUID; positive i64 mailbox id |
| `{"and": [Node, ...]}`, `{"or": [Node, ...]}` | empty AND is true (empty search); OR is nonempty |
| `{"not": Node}` | one operand |

`from`/`to`/`cc` `is` compares a parsed addr-spec's local part exactly and
domain case-insensitively; `domain` compares the entire domain,
not its suffix. Domain keys use Unicode lowercase, without IDNA or other
rewriting in v1; local parts are never case-folded or Unicode-normalized.
Reject invalid addresses/domains. For imports the summary
contains only the first From mailbox; that is v1's exact-From source,
whereas FTS includes all From mailboxes. Empty known lists give false;
missing headers and absent/failed summaries give unknown. A partial summary
has no field-level completeness information in v1: all summary-backed
address `is`/`domain` predicates are conservatively unknown, including when
some addresses were retained. A complete `message_headers` source may still
supply a known result. Never infer a negative from an incomplete list.
`size` uses acquired byte length, else the chosen occurrence's RFC822.SIZE;
absent size is unknown.
Attachment presence uses a complete summary, else unknown. In particular,
a partial MIME walk can stop before an attachment, so a retained
`has_attachments=false` cannot prove absence. V1 treats attachment presence
from any partial summary as unknown even when its retained value is true;
future field-level completeness must be explicit before relaxing this rule.
Thus `NOT has:attachment` and negated address equality/domain stay unknown
for partial summaries unless a complete header source supplies the address
result. Candidates whose final expression is unknown contribute to
excluded-unknown counts. Note/tag predicates read authoritative local
metadata. Flags use the chosen occurrence, like its display row.

```text
query   := or_expr | empty
or_expr := and_expr ("OR" and_expr)*
and_expr:= unary (("AND")? unary)*
unary   := ("-" | "NOT") unary | "(" query ")" | atom
atom    := value | key ":" value
key     := [a-z]+ ("." [a-z]+)?
value   := bare | '"' quoted '"'
```

- Whitespace is Unicode whitespace. Uppercase `AND`, `OR`, `NOT` are
  reserved whole tokens; precedence is unary, AND, OR. Keys are lowercase
  and case-sensitive. Bare values exclude whitespace, parentheses, quotes,
  backslashes and colons. A colon after a key introduces its value with no
  surrounding whitespace; an unknown well-formed key is `unknown_key` at
  that key. A recognized field with an unsupported operator suffix is
  `invalid_operator` at the suffix. A malformed key prefix is
  `unexpected_token` at that prefix (`12:30` points at `12`); an extra
  colon in a value is `unexpected_token` at the colon. Quote literal colons, reserved words or a leading minus.
  `()` is an explicit empty AND (true), so even nested empty AND nodes
  round-trip. Inside quotes only `\"` and `\\` are escapes; no other
  escapes: `"\q"` is `invalid_value` at the two-byte escape, not literal
  text. A missing value points at the next token or EOF (`from.is:` uses
  `[8,8)`). A value must be nonempty and full-text values must contain a
  tokenizer token. JSON validation rejects the same invalid field/operator
  pairs as text validation.
- Keys: `from`, `to`, `cc`, `subject`, `body`, `attachment` mean `contains`;
  `from.is`, `to.is`, `cc.is` mean `is`; `.domain` selects `domain`.
  Invalid pairs such as `body.is:x` fail at the operator. `has` accepts
  `attachment`/`note`; `is` accepts `unread` (NOT seen), `read`, `flagged`,
  `answered`, `draft`. `tag` and `account` take UUIDs; `in` takes a mailbox
  id, never an ambiguous display name. Chips show resolved names but
  serialize ids. `size.lt`, `size.eq`, `size.gt` take decimal bytes.
- `before`, `after`, `on` take `YYYY-MM-DD@IANA-zone`; `within` takes
  `days@IANA-zone@clock-ms`. The parser also accepts a date without `@zone`
  or days without context using its required `ParseContext { tz, clock }`.
  The canonical formatter always emits explicit context, so reopening never
  changes relative dates. Validate zone names and dates; never use the
  machine's timezone implicitly. `before` is strictly before local day
  start, `after` is at or after next local day start, `on` is that local
  day, and `within_days` is `[clock - days*86,400,000, clock]`.
  Day start is the earliest instant on that civil date; a skipped civil
  date or arithmetic overflow is invalid input. Predicates use sort_date.
- Each text value is one literal token sequence; multiple tokens in one
  value are a phrase. Thus punctuation identifiers and quoted phrases use
  the same unicode61 tokenization as the index. No implicit prefix, fuzzy
  match or raw FTS operators in v1; `*` is punctuation, not an operator.
  The prefix index remains reserved for a later explicit prefix contract.
- Canonical formatting preserves tree order and groups expressions as
  needed; parsing it reconstructs the same tree modulo flattened AND/OR
  groups, singleton AND/OR and `is:unread`'s NOT spelling. Chips edit this
  tree, not SQL.
- Limits: 16,384 UTF-8 input bytes (text or JSON), 256 nodes, depth 32.
  Canonical text and JSON must also fit the byte limit; reject a conversion
  that would exceed it with `limit`, rather than emit an unreadable query.
  Return `SearchParseError { start, end, code }`: zero-based, half-open
  UTF-8 byte offsets at character boundaries, EOF `[len,len)`. Codes are
  `unexpected_token`, `missing_value`, `unclosed_quote`, `unclosed_group`,
  `unknown_key`, `invalid_operator`, `invalid_value`, `limit`. Point at the
  offending token; an unclosed construct points at its opening delimiter.
  JSON errors use the offending value span. Never fall back to text on an
  error. Service/FFI preserve the span; shells convert to native indices.

## Execution

- Filters (account, mailbox, flags, dates) are applied **inside** the SQL
  query before ranking and limit, so a small account's hits can't vanish
  under a large one's.
- **Grain:** one row per logical message, its representative occurrence
  (02, `d_list_rows`). With a `mailbox` scope, rows are that mailbox's
  occurrences instead (one chosen per message; detailed below). Counts
  are messages. A message with no present occurrence isn't a hit; it
  appears in the Recovery list (04).
- **Results stream:** the first 50 hits are ranked or date-sorted; more
  are fetched on scroll with keyset paging.
  - Date-sorted results page by `(sort_date, occurrence_id)`.
  - Relevance results page by `(bm25 score, occurrence_id)`, within the
    same query generation. A new generation restarts paging.
  - The cursor is opaque to the shell (07).
- **Freshness:** a newly acquired message is searchable within 2 s at
  p95 while the index job keeps up. Flag and mailbox predicates read live
  authoritative tables, so they are always current.
- **Server search** (IMAP `SEARCH` on Bridge) is *later*. Bridge
  searches Proton's encrypted store with limitations (04).

### SQL and ordering details (M1.6a)

- Compile node results as true/false/unknown (SQL NULL), with Kleene
  AND/OR/NOT: false AND unknown is false, true OR unknown is true. Only
  true is a hit; count final unknown separately. A truncated-field FTS
  leaf is unknown even if its retained prefix contains the term, retaining
  v1's conservative rule above. Unqualified text depends on all eight
  columns; `body` depends on all three body columns. No summary fallback
  for an unindexed FTS predicate. Queueing a same-version re-extraction
  keeps the last committed document usable until replacement, unless its
  object is unavailable. Target-version changes make old versions unknown.
- Bind all operands, including quoted/escaped MATCH literals. Never
  interpolate user FTS or SQL. Apply the full Boolean predicate and scope
  before score, keyset boundary and limit. Use EXISTS for membership/tag
  filters rather than multiplying hits through joins. Well-formed AST
  account/mailbox/tag ids with no stored entity yield false; they are not
  syntax errors or unknown data. Unknown outer scope ids are errors (07).
- Search scope (07) selects candidates first. All/Account choose the
  ordinary representative. Mailbox chooses the lowest-id present
  occurrence of each message in that mailbox. An AST `mailbox` node tests
  whether that message has a present occurrence there; it does not change
  the display occurrence, including under OR/NOT. This explicitly refines
  "that mailbox's occurrences" above to one hit per message even when
  a mailbox holds several copies of the same provider message. Different
  logical messages sharing one acquisition still give separate hits.
- Date order is `(sort_date DESC, occurrence_id DESC)`. Relevance is
  `(score ASC, occurrence_id DESC)`: SQLite BM25's smaller score is better.
  For a Boolean tree, sum the weighted BM25 values of matching FTS leaves
  in positive polarity (under an even number of NOTs), once per distinct
  `(column set, token sequence)`. Negated/unknown/nonmatching leaves add
  zero. No positive leaves means score 0. Filter-only hits in a mixed OR
  also score 0; no recency boost or score rounding. Corpus statistics are
  those of the whole current FTS table; scope filters precede result limit,
  not BM25's corpus statistics. Rebuild with unchanged input/version must
  reproduce scores, order and coverage as well as the matching ids.

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
