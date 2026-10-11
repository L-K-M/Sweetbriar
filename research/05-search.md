# 05 — Best-in-class search

Synthesis of [raw/search/codex.md](raw/search/codex.md) and
[raw/search/glm.md](raw/search/glm.md), with input from the performance,
data-safety and classic-client reports. Checked 2026-10-11.

## Summary

- **Trust first, then speed, then cleverness.** Exact queries,
  understandable scope, visible coverage ("bodies cached through March;
  300 attachments pending"), and recovery from index failure. Users
  forgive a missing feature; they don't forgive missing results.
  - Apple Mail's recurring search breakage and Thunderbird's unreliable
    Gloda are the cautionary tales.
  - macOS 27 Mail now ranks results by relevance and highlights terms, so
    relevance ranking is table stakes.
- **One query tree, many front ends.** The same AST drives the search
  field (chips and text syntax), saved searches (smart mailboxes), rules
  and scripting. Mailsmith's shared criteria engine and Musashi's "load
  rule into search" prove the value.
- **Engine: SQLite FTS5 is the declared starting choice** (revised after
  the debate). Tantivy must clear pre-registered bars in a bounded
  (about two-week) spike over the same extraction pipeline: judged
  relevance, identifier and CJK tasks, latency under sync, freshness,
  recovery and resources (see [debate/04-resolution](debate/04-resolution.md)).
  GLM and codex/search leaned Tantivy (fuzzy matching, tokenizers, field
  boosts, facets); codex/performance and devin/data-safety leaned FTS5
  (one transaction, one durability story). Index placement (inside the
  authoritative database or a separate derived FTS5 database) is tested
  separately from engine choice.
- **Federated search** reaches mail that isn't downloaded, via Gmail
  `X-GM-RAW`, Graph `$search`, JMAP `Email/query` and IMAP
  ESEARCH/FUZZY. Remote search is capability-qualified: each clause is
  exact, broader (verified locally) or unsupported, and a capped result
  set never proves "no matches". Results merge by account- or
  mailbox-scoped provider identity with "on server" labels.
- **Semantic search is opt-in and comes after lexical quality is proven.**
  Query parsing, vector retrieval and answer generation are separate
  concerns.

## What "best in class" means (targets)

| Axis | Target |
|---|---|
| Type-to-results | p95 ≤ 100 ms warm, local (incl. ~30 ms debounce); the last token is treated as a prefix |
| Exactness | Literal phrases, identifiers (order numbers, tracking codes, addresses) never silently rewritten; Apple Mail's macOS 26.2 short-code tokenization bug is the anti-example |
| Ranking | BM25 with field boosts; strict chronological sort always available and kept strict |
| Operators | Gmail-compatible set plus a notmuch-style boolean core and regex (expert mode) |
| Typo tolerance | Offered as labelled broader search or suggestion, never silent |
| Languages | CJK segmentation, accent folding, per-language stemming in separate fields |
| Attachments | Filenames and MIME always; extracted text (PDF, Office, OCR opt-in) with page or slide location |
| Scope | All accounts, a selection, a folder, or the current results (search within results) |
| Coverage | Local body/attachment coverage and server-only hits shown explicitly |
| Saved | Any query becomes a named smart mailbox or a rule predicate |

## Engines

| Engine | Fit for a macOS + Linux Rust core | Strength | Cost | Role |
|---|---|---|---|---|
| **SQLite FTS5** | In the bundled SQLite | Same transaction as metadata; SQL joins for filters and facets; `bm25` with column weights, custom tokenizers, auxiliary functions, `snippet`, `highlight` | No fuzzy matching; trigram has a 3-character floor; CJK needs custom tokenizers | **Starting choice** |
| **Tantivy** 0.26.2 (MIT; 0.27 unreleased) | Native Rust | BM25, phrases with slop, `FuzzyTermQuery`, regex, facets, custom tokenizers, mmap segments, commit payloads | Separate store with its own commits (a second crash-consistency surface); immutable docs (delete + reinsert); format churn between minors; no vectors | Challenger behind registered bars |
| Xapian | C++ bridge | Mature mail search (notmuch, mu, Fastmail's Cyrus fork with CJK) | **GPLv2+**; MPL relicensing planned but not done | Only if Sweetbriar goes GPL |
| Meilisearch (`milli`) | Unpublished internal crate | Typo tolerance, bucket ranking | API coupling | Use its tokenizer `charabia`, not the engine |
| Lucene | JVM | Reference feature set | Runtime | Design reference |
| Core Spotlight / LocalSearch | One OS each | System integration | No ranking control | Optional export adapters, never the source |

No comparable, verified numbers (index bytes per 100k messages, indexing
throughput, p95 latency, RSS) exist for mail workloads. Measure them.

**Decision gate** (codex):

- **Setup:** identical extraction and filters, a fixed relevance judgment
  set, 100k and 1M messages with mixed languages, long quoted threads,
  large attachments, duplicates and mutation bursts.
- **Machines:** an 8 GiB Apple-silicon Mac and a 16 GiB Ubuntu box.
- **Record:**
  - warm/cold p50/p95/p99 latency and first-result UI latency
  - exact-hit recall and ranking judgments
  - indexing throughput and RSS
  - bytes per 100k messages, merge disk space and energy
  - crash recovery at every publication step

**FTS5 unless Tantivy clears the registered bars** (for example nDCG@10
+0.05 with a paired 95% interval above zero and no exact or filter
regression, or FTS5 missing latency, freshness or identifier/CJK
requirements after bounded tuning). The full registration is in
[debate/04-resolution](debate/04-resolution.md).

## Index design

- **Fields** (codex):
  - **Identity:** local id, account, provider id, locator, Message-ID,
    thread, revision.
  - **People:** From, To, Cc and Bcc separately; full address, display
    name and address components in distinct fields.
  - **Text:** subject, **newly authored body**, **quoted body**,
    **signature**. Each comes in exact and normalized forms, plus optional
    stemmed per-language variants, with positions and offsets.
  - **Attachments:** filename, MIME type, hash, extracted text with
    location, extraction status and version.
  - **Filters and sort:** dates, size, folders/labels/tags, flags,
    language, coverage.
- **Separating quotes and signatures** stops copied correspondence from
  dominating ranking. When the quote parser is unsure, the text stays
  searchable.
- **Analyzers:** custom ones for addresses, URLs, filenames, long
  identifiers. Versioned Unicode normalization with accent-folded
  alternates. `charabia` (jieba, lindera) or `lindera-tantivy` for CJK,
  with an n-gram fallback. Test single-character CJK queries; Japanese and
  Korean segmentation runs at about 2–3 MiB/s, so budget indexing time.
- **Starting boosts** (to be validated): subject 4, authored body 1,
  attachments 0.75, quotes 0.25, plus person matching. Optional recency
  and contact signals only if judged queries improve.
- **Filters go inside retrieval,** before top-k truncation. Otherwise a
  small account's results vanish under a large one's.
- **One logical cross-account index with account filters** (codex) vs
  per-account indexes (GLM). Start with one. Per-account indexes make
  deleting an account atomic, but complicate global ranking.

## Keeping the index consistent with the store

The SQLite store and a Tantivy commit can't be atomic together, so publish
through a replayable outbox (codex). FTS5 would get the first step inside
the same transaction.

1. In the canonical transaction, bump the message revision and append a
   `search_outbox` row. Sync, drafts, labels, extraction and deletes all
   go through this path.
2. One broker owns the index writer. It reads a bounded batch from a
   consistent snapshot and deletes and reinserts by stable id and
   revision, so replay is idempotent.
3. It commits the index with a payload: generation, analyzer and schema
   version, and the applied outbox watermark.
4. It reopens and validates the generation, then acknowledges the
   watermark in SQLite. A crash between the two steps just replays.
5. Readers reload. Stale extraction or vector jobs are rejected by
   revision. Tombstones suppress deleted mail immediately, before index
   cleanup.
6. A rebuild writes a new generation from originals, replays intervening
   changes, verifies, and publishes atomically. The old generation keeps
   serving until then.
7. For instant feedback, pending deltas from the store are overlaid on
   results, with the same predicates run on the small delta.

Rebuild is a visible, resumable, first-class operation (MailMate has
"Rebuild Database"). An index-format version bump triggers a rebuild,
never a corrupt read.

## Query language

Mail searches cover:

- **Fields:** `from: to: cc: bcc: subject: body: attachment:/filename:
  mimetype: list: header:<name>: account: mailbox:/folder: label:/tag:
  thread: id:`
- **Dates:** `before: after: older_than: newer_than:`, plus relative dates
  like notmuch's
- **Status:** `is:unread|read|flagged|answered|draft|muted`, `has:attachment`
- **Size:** `larger: smaller:`
- **Combinators:** `AND OR NOT`, `-term`, `"phrase"`, `( )`, `NEAR(a b, N)`
- **Expert:** regex as `from:/…/`; `query:name` composes saved searches

Bare terms search subject, people, authored body and attachment text.

**One typed representation, different allowed subsets** (revised after
the debate). Search, saved views, rules and remote queries share the
AST, but each use has its own evaluator and allowed subset:

- Ranked, fuzzy and semantic predicates are search-only, never rule
  predicates.
- Missing body or attachment data evaluates to **unknown**, including
  under negation: `NOT body:invoice` must not file every uncached
  message. Rule previews use the same completeness semantics as
  execution.
- Relative dates carry an explicit evaluation clock and timezone.
- Canonical query and date semantics are stored separately from the
  localized display.
- Searchable content is extracted independently from the reading view:
  folded quotes, signatures and text the clean view drops stay
  searchable, with source-part provenance and explicit extraction
  coverage.

Front ends over the AST (launch with the first two):

1. Token chips: novices never need syntax, and the text and chips stay in
   sync.
2. The text syntax.
3. Later, with D15: natural language → AST. Rules handle dates and entities first; the
   on-device LLM (Foundation Models guided generation into the AST schema;
   a constrained local model on Linux) handles the rest. The result is
   always shown as editable chips, never executed blindly.

Results stream the lexical page first; obsolete queries are cancelled and
keyboard selection survives as more results arrive. Each row shows why it
matched (highlighted excerpt or "semantic match"), and threads collapse to
their best hit, with matching messages distinguished from context when
expanded. Facets cover sender, date, attachment type, folder and account,
marked approximate when coverage is incomplete. Remote-only hits and
account errors go in a supplemental section that doesn't displace the
selection. "Save as smart mailbox" and search-within-results are always
available.

## Server-side search

| Backend | Mechanism | Caveats |
|---|---|---|
| Gmail | IMAP `X-GM-RAW` / API `q=`: the full web grammar | Conversation-scoped negation |
| Microsoft Graph | `$search` (KQL) | **Date-sorted, max 1,000 results**: the client ranks, and local cache depth is the real search radius |
| JMAP | `Email/query` + `SearchSnippet/get` (server highlighting) | Capabilities vary |
| IMAP | SEARCH, ESEARCH (counts), PARTIAL for paging (RFC 9394), FUZZY with RELEVANCY (RFC 6203) where advertised | Server-dependent; CHARSET handling |

- If an account is fully cached: search locally, and refresh from the
  server asynchronously.
- If it isn't: query the server first and merge by account- or
  mailbox-scoped authoritative ids (Gmail `X-GM-MSGID`, Graph immutable
  id, JMAP Email id) or established locators. Message-ID and Graph
  `internetMessageId` are correspondence clues only.
- Per-account deadlines and cancellation on every keystroke. Never
  download bodies just to show snippets.

## Attachment text

- An extraction pipeline stage, versioned, with caps (first N KB), per-account
  policy and status.
- **macOS:** PDFKit; Vision OCR (opt-in).
- **Cross-platform:** pdfium-render or pure-Rust PDF; spreadsheets via
  `calamine`; DOCX and PPTX via their own bounded `zip` + `quick-xml`
  extractors (`calamine` reads spreadsheets only); Tesseract on Linux.
- Legacy `.doc`/`.xls`: filename-only, or an optional helper.
- Encrypted mail: index decrypted text only under an explicit local
  retention policy.

## Semantic search (later, opt-in)

- **Models:**
  - EmbeddingGemma (300M, 768 dimensions with Matryoshka truncation to
    128, 100+ languages; check the Gemma license terms; "EmbeddingGemma 2"
    is now a candidate)
  - bge-small-en (MIT, 384 dimensions, English only)
  - bge-m3 (dense, sparse and multi-vector)

  Run through `ort` (CoreML execution provider) or candle.
- **Storage per 1M vectors, before index overhead:**
  - 768 dimensions: float32 3.07 GB, int8 0.77 GB
  - Matryoshka 128 dimensions: float32 0.51 GB, int8 0.13 GB

  Attachment chunking multiplies these. (An earlier version mixed
  precisions; Devin's review caught it.)
  Use sqlite-vec (pre-v1, brute force is fine at ≤1M) or USearch (HNSW,
  mmap views). Tantivy has no vectors.
- **Ranking:** hybrid BM25 plus vectors via reciprocal rank fusion (k≈60).
  Compare ANN recall against exact search under restrictive filters before
  adopting ANN.
- **Cost:** about 1–8 CPU-core-hours to embed 1M messages (estimate). Run
  on AC power and at idle, resumable, with a storage estimate shown first.
- **Separate concerns:** natural-language parsing (on device), retrieval
  and answer generation. No cloud models see bodies by default.

## Disagreements and open questions

- **Tantivy vs FTS5:** settled as FTS5 first with a competitive gate
  (debate rounds 1–2). Claude's earlier prior favoured Tantivy; both
  reviewers argued that a second store is a second durability surface and
  that the bars, not the prior, should decide.
- **One index vs per-account indexes.**
- **Privacy:** index files, snippets and vectors need the same protection
  as blobs. Database encryption alone doesn't cover separate index files.
- **Search after a server expunges uncached mail:** local tombstones vs
  keeping the cached copy (product policy).

## Key sources

- [Tantivy](https://github.com/quickwit-oss/tantivy), [IndexWriter commit payloads](https://docs.rs/tantivy/latest/tantivy/indexer/struct.IndexWriter.html), [lindera-tantivy](https://docs.rs/lindera-tantivy/latest/lindera_tantivy/)
- [SQLite FTS5](https://www.sqlite.org/fts5.html)
- [Xapian licensing](https://trac.xapian.org/wiki/Licensing), [charabia](https://github.com/meilisearch/charabia)
- [Gmail operators](https://support.google.com/mail/answer/7190), [Gmail IMAP extensions](https://developers.google.com/gmail/imap/imap-extensions), [Graph $search](https://learn.microsoft.com/en-us/graph/search-query-parameter), [RFC 8621](https://www.rfc-editor.org/rfc/rfc8621.html), [RFC 6203](https://www.rfc-editor.org/rfc/rfc6203.html)
- [notmuch search terms](https://manpages.debian.org/notmuch-search-terms.7.en.html), [MailMate search](https://manual.mailmate-app.com/view)
- [Foundation Models](https://developer.apple.com/documentation/foundationmodels), [EmbeddingGemma](https://ai.google.dev/gemma/docs/embeddinggemma), [sqlite-vec](https://github.com/asg017/sqlite-vec), [USearch](https://github.com/unum-cloud/usearch)
