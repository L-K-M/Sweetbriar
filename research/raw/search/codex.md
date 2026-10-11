<!-- provenance: helper=codex model=gpt-6.1-sol started=2026-10-11T18:14:20+00:00 exit=0 brief=research/briefs/ -->

Saved to [report.md](codex-artifacts/report.md).

# Sweetbriar: best-in-class email search

Research date: **2026-10-11**. Mutable facts below describe documentation/source code checked on that date unless a historical date is explicitly given. Recommendations are labeled; targets and calculations are not product benchmarks. Public sources establish some implementations much better than others.

## 1. TL;DR

- **Choose Tantivy provisionally for portable lexical search**, with SQLite FTS5 as the mandatory simpler comparison. The decision should follow a mail-specific benchmark, not a web-search benchmark (§2.2, §4).
- **Trust and coverage come first:** exact queries, understandable scope, visible indexing/download status, and recovery from index failure (§2.1).
- Target **p95 ≤100 ms** for warm local type-to-results interaction; this is a proposed Sweetbriar target, not a measured competitor result (§2.1).
- Use **one query tree behind native tokens and text operators**, with saved searches/smart mailboxes and identical cross-account semantics (§2.4).
- Keep original mail, draft revisions and settings in durable canonical storage. Publish derived search indexes through a replayable outbox; never recover mail from an index (§4).
- **CJK, accents, identifiers and quoted threads require deliberate analysis/ranking.** Default tokenizers alone do not meet the proposed requirements (§2.2, §4).
- Local-first search must still reach undownloaded mail through IMAP/Gmail/Graph/JMAP adapters, while labeling unsupported filters and capped remote results (§2.7).
- Add **optional on-device hybrid semantics after lexical quality is proven**. Separate query parsing, vector retrieval and answer generation (§2.5).
- One million 768-dimensional float32 vectors need **3.072 GB before graph/metadata**; attachment chunking multiplies this. Benchmark disk, throughput and energy (§2.5).
- As checked **2026-10-11**, Xapian remains GPLv2+, EWS retirement is phased, and EmbeddingGemma 2 is a current candidate. [Licensing](https://trac.xapian.org/wiki/Licensing), [EWS plan](https://techcommunity.microsoft.com/blog/exchange/exchange-online-ews-your-time-is-almost-up/4492361), [models](https://ai.google.dev/gemma/docs/embeddinggemma)

## 2. Findings

### 2.1. Definition of best in class and lessons from existing clients

**Recommendation:** define excellence as reliably finding the intended message, quickly, with understandable scope. A search that feels instant but silently omits an account, an old message or an attachment is inadequate.

Use the following **proposed acceptance targets**, not claims about competing products:

| Dimension | Sweetbriar acceptance condition |
|---|---|
| As-you-type speed | Warm local lexical search: p95 ≤100 ms from keystroke to useful rendered results, including debounce; p95 ≤50 ms inside the engine. Cold first query: ≤500 ms target. Measure p99 and typing during merges too. |
| Exact retrieval | All expected hits for identifiers, address filters, date boundaries and Boolean queries in a curated downloaded corpus. Never silently replace an exact query with fuzzy or semantic matching. |
| Ranking | Measure known-message top-1/top-5 success separately from exploratory nDCG@10. Evaluate old mail, frequent contacts, uncommon names and long quoted threads independently. |
| Coverage | Show accounts searched, locally indexed date/body coverage, remote status and attachment extraction failures. “Zero results” must distinguish complete and partial searches. |
| Language | Test mixed-language messages, CJK words without spaces, one-character queries, accents, composed/decomposed Unicode and language-appropriate stemming. Preserve literal identifiers. |
| Interaction | Native tokens and plain-text operators; keyboard navigation; clear relevance/date sorting; facets; stable selection; saved queries/smart mailboxes across accounts. |
| Freshness | Show a saved draft immediately through an overlay; target searchable downloaded text within two seconds of a durable store update outside initial-index backlogs. |
| Recovery/privacy | Search indexes are disposable and rebuildable. Search remains usable without semantic models or connectivity; local mail is not uploaded for local search. |

Build separate test sets for “find the invoice number,” “messages from this person,” “something about cancelling the lease,” and “all unread project mail.” These are different retrieval tasks. Typo tolerance should help a misspelled human word; it must not corrupt a tracking number, email address or quoted phrase. Natural-language filter construction and semantic discovery also need separate controls and evaluation.

**What users say:** the following are dated, individual reports, selected to reveal failure modes. They are not surveys, controlled benchmarks or current product rankings.

| Client | Praise and complaint evidence |
|---|---|
| Gmail | A **2022-05-03** user stayed with Gmail because it found mail that Spark/Apple Mail missed. In a **2026-08-01** discussion, a mobile user reported quick-result hits disappearing after submitting the query. [2022 report](https://news.ycombinator.com/item?id=31252863), [2026 discussion](https://news.ycombinator.com/item?id=49136990) |
| Apple Mail | A **2024-05-08** user found a tracking number immediately when Proton search failed; a **2026-01-20** user described Mail search as broken. [Success](https://news.ycombinator.com/item?id=40300691), [complaint](https://news.ycombinator.com/item?id=46689438) |
| Outlook | A **2025-11-13** user reported seven self-addressed results where hundreds were expected in new Outlook/web, while classic Outlook worked. This is a useful version distinction, not a general benchmark. [Report](https://news.ycombinator.com/item?id=45914332) |
| Fastmail | A **2026-08-02** user praised precise search over 131,000+ messages; a **2026-08-18** user complained about search and preferred an agent-assisted workflow. [Praise](https://news.ycombinator.com/item?id=49136990), [complaint](https://news.ycombinator.com/item?id=49344752) |
| Thunderbird | A **2023-07-10** discussion includes both preference for Thunderbird's search and complaints about its search experience. Its several search interfaces complicate comparisons. [Discussion](https://news.ycombinator.com/item?id=36664113) |
| MailMate | A **2015-03-01** user praised search over a 500,000-message archive. A **2018-10-17** reproducible ticket documents body misses, acknowledged by its developer. Neither establishes current latency/reliability. [Praise](https://news.ycombinator.com/item?id=9125729), [historical bug](https://freron.lighthouseapp.com/projects/58672/tickets/2140-body-text-search-broken-in-r-6123) |
| notmuch/mu | A **2019-11-20** mu user praised fast local search over Gmail. A **2022-11-12** discussion praises notmuch's power while describing extra setup/synchronization work around tags and accounts. [mu report](https://news.ycombinator.com/item?id=21580302), [notmuch discussion](https://news.ycombinator.com/item?id=33568388) |
| Superhuman | A **2020-06-23** discussion contains both speed praise and a user finding old mail more slowly than in Gmail. A **2025-07** user reported persistent search bugs. [Mixed reports](https://news.ycombinator.com/item?id=23614167), [later complaint](https://news.ycombinator.com/item?id=44435231) |
| Mimestream | A **2026-06-03** user praised its Gmail API experience; this is broader product praise. Its founder documented limited offline search/cache at launch on **2020-09-10**; current offline coverage is **(unverified)** here. [Praise](https://news.ycombinator.com/item?id=48379770), [launch explanation](https://news.ycombinator.com/item?id=24422432) |

The design lesson is to provide an exact, explainable path alongside ranked discovery, and expose incomplete coverage. No evidence here supports declaring one client universally fastest or attributing complaints solely to its index engine.

### 2.2. Search engines and embedding constraints

**Tantivy is the leading portable candidate.** It is an MIT-licensed Rust library supporting macOS and Linux, BM25, incremental indexing, positional phrase queries, facets and custom tokenizers. Its documents are immutable: an edit requires deletion and reinsertion, followed by a commit and reader reload. [Tantivy](https://github.com/quickwit-oss/tantivy)

The documentation inspected identifies **Tantivy 0.26.2**. `IndexWriter` supports explicit commits, rollback, segment merging and commit payloads. Commits and a full indexing queue can block, so all indexing belongs off the UI thread. [IndexWriter](https://docs.rs/tantivy/latest/tantivy/indexer/struct.IndexWriter.html) The default tokenizer splits on punctuation/whitespace, lowercases and drops tokens longer than 40 characters; it does not supply adequate email-address, identifier or CJK behavior by itself. [Tokenizer documentation](https://docs.rs/tantivy/latest/tantivy/tokenizer/index.html)

Tantivy's writer acquires an exclusive index lock; multiple readers are possible, but separate processes need a deliberate publication/reload protocol rather than multiple writers. [Index API](https://docs.rs/tantivy/latest/tantivy/index/struct.Index.html) Fuzzy terms use Levenshtein automata; positional phrase search is a separate query type. [FuzzyTermQuery](https://docs.rs/tantivy/latest/tantivy/query/struct.FuzzyTermQuery.html), [PhraseQuery](https://docs.rs/tantivy/latest/tantivy/query/struct.PhraseQuery.html)

**SQLite FTS5 offers the strongest simplicity argument.** It has BM25, phrase/prefix/NEAR queries and custom tokenizers. `unicode61` uses Unicode 6.1 classifications and Latin diacritic folding; its contiguous-letter tokenization is insufficient for CJK word segmentation. Porter stemming is English-specific. Trigrams support substrings, with no full-text match for queries shorter than three Unicode characters; they are not edit-distance fuzzy search. External-content tables avoid duplicating text but require maintained consistency; ordinary contentless tables have restrictive updates. Contentless-delete tables, introduced in SQLite 3.43.0, support deletion/replacement with update restrictions. [FTS5 documentation](https://www.sqlite.org/fts5.html)

**Xapian requires a licensing decision.** Its licensing page, modified **2026-03-24** and checked **2026-10-11**, still identifies GPLv2+, describes an intended future MPL transition, and explicitly says the code has not yet been relicensed. Sweetbriar must not treat that plan as permission to embed it in a proprietary distributed client. [Xapian licensing](https://trac.xapian.org/wiki/Licensing)

Xapian is a C++ library with probabilistic/BM25 ranking and Boolean queries; its databases support one writer and multiple readers. A Rust integration would need a maintained binding or a small C/C++ bridge. [Xapian theory](https://xapian.org/docs/intro_ir.html), [API overview](https://xapian.org/docs/overview.html)

**Meilisearch's tokenizer is more attractive than its engine as a first dependency.** `charabia` is a multilingual Rust segmentation/normalization library. Its published microbenchmarks report approximately **9 MiB/s Latin** and **5 MiB/s Chinese** tokenization on a Xeon E5-1650 server with 64 GB RAM; these measure tokenization, not email indexing. Chinese uses Jieba; Japanese/Korean use Lindera dictionaries. Compile-time features and dictionary behavior need testing. [Charabia README](https://raw.githubusercontent.com/meilisearch/charabia/main/charabia/README.md) The current `milli` manifest sets `publish = false` and depends on other workspace crates: adopting it means owning an internal integration rather than assuming a stable, independently published embedding API. [Milli manifest](https://raw.githubusercontent.com/meilisearch/meilisearch/main/crates/milli/Cargo.toml) Meilisearch applies ordered ranking rules through bucket sorting, including words, typos and proximity, rather than a conventional BM25 score. Community libraries are MIT; enterprise features have separate BSL licensing. [Ranking rules](https://www.meilisearch.com/docs/capabilities/full_text_search/relevancy/ranking_rules), [edition licensing](https://www.meilisearch.com/open-source)

**Lucene is the reference implementation, with a Java runtime cost.** Its current download page identifies **10.5.2**, released **2026-10-04**, as the latest release checked on **2026-10-11**. [Release status](https://lucene.apache.org/core/downloads.html), [release news](https://lucene.apache.org/core/corenews.html) It has Apache-2.0 licensing, language analyzers and a documented `KnnFloatVectorField` API. It can run on both operating systems, but using it from Sweetbriar's Rust core would introduce a JVM/bridge or sidecar. [Lucene](https://lucene.apache.org/), [analysis APIs](https://lucene.apache.org/core/10_5_2/analysis/common/allclasses-index.html), [vector API](https://lucene.apache.org/core/10_5_2/core/org/apache/lucene/document/KnnFloatVectorField.html)

**Platform services are integration options.** Core Spotlight can index app-managed content locally; `CSUserQuery` supports ranked lexical/semantic queries and suggestions. This is a useful macOS capability, but not a portable Rust search implementation. [Core Spotlight](https://developer.apple.com/documentation/corespotlight), [CSUserQuery](https://developer.apple.com/documentation/corespotlight/csuserquery.md) Search Kit's `SKIndex` is an older macOS indexing API. Legacy synchronous functions such as `SKSearchGroupCreate` are deprecated; blanket deprecation of the entire API collection was not established. Its historical “vector index” maps documents to terms and should not be confused with an embedding ANN index. [Search Kit](https://developer.apple.com/documentation/coreservices/search_kit?language=objc), [Search Kit concepts](https://developer.apple.com/library/archive/documentation/UserExperience/Conceptual/SearchKitConcepts/searchKit_basics/searchKit_basics.html)

The Tracker SPARQL/Miners projects were renamed **TinySPARQL/LocalSearch** in a **2024-07-14** maintainer announcement. [Rename](https://blogs.gnome.org/carlosg/2024/07/14/goodbye-tracker-hello-tinysparql-and-localsearch/) GNOME's current documentation describes LocalSearch as a filesystem indexer with a TinySPARQL store and a D-Bus endpoint. It is a Linux desktop service rather than a macOS/Linux embedded mail engine; optional desktop integration should not determine Sweetbriar's search correctness. [LocalSearch](https://gnome.pages.gitlab.gnome.org/localsearch/), [endpoint](https://gnome.pages.gitlab.gnome.org/localsearch/endpoint.html)

**Performance evidence has limits.** Tantivy's README claims English Wikipedia indexing in under three minutes on the author's desktop, without enough detail there to project a mail workload. Its comparison benchmark uses Wikipedia, warmed caches and the best of repeated runs; that is not a cold-start or p95 mail benchmark. [Tantivy README](https://github.com/quickwit-oss/tantivy), [benchmark methodology](https://github.com/quickwit-oss/search-benchmark-game)

No comparable, verified **index bytes per 100,000 messages, peak resident memory, sustained email indexing throughput and query latency** across these engines has been established in this research. Such numbers remain **(unverified)** until measured on a defined mail corpus. Report raw MIME storage, extracted text, lexical index, vectors, temporary merge space and operating-system page cache separately.

### 2.3. How existing clients implement search

These are documentation/source findings **checked 2026-10-11**. A dated engineering post establishes that historical implementation, not necessarily today's production deployment.

| Product | What is established; what remains unknown |
|---|---|
| Gmail | Server implementation is proprietary; its engine identity is **(unverified)**. Google's **2025-03-20** ranking announcement names recency, most-clicked mail and frequent contacts. A **2026-09-15** announcement expands English AI Overviews with paid-plan/account/region eligibility restrictions. [Ranking](https://blog.google/products-and-platforms/products/gmail/gmail-search-update-relevant-emails/), [AI rollout](https://workspaceupdates.googleblog.com/2026/09/gmail-searchs-ai-overviews-now-available-globally.html) |
| Apple Mail | Apple's macOS 26 guide documents suggestions, filter tokens, Boolean search and Top Results using recent reading/replies, VIPs and contacts. Independent **2026-09-02** analysis describes SQLite Envelope Index metadata and separate `.emlx` files. Community reports describe Spotlight involvement, but the exact current division among Mail's indexes and Spotlight is **(unverified)** here. [Apple UI](https://support.apple.com/guide/mail/search-for-emails-mlhlp1003/16.0/mac/26), [independent schema analysis](https://dev.to/paris_moschovakos_5f8f1e0/whats-actually-inside-apple-mails-envelope-index-2loh), [2026 discussion](https://news.ycombinator.com/item?id=48377864) |
| Outlook | Classic Windows Instant Search uses the Windows search catalog; Microsoft documents service-assisted search for classic Outlook too. A known issue updated **2026-09-29** describes stale results after moves/category/flag changes and a Windows Search fallback. Do not transfer this architecture to Outlook for Mac or assume “classic = only local.” Current Mac internals are **(unverified)**. [Instant Search](https://support.microsoft.com/en-us/outlook/fix-search-issues-by-rebuilding-your-instant-search-catalog), [current service issue](https://support.microsoft.com/en-us/support/known-issues/classic-outlook-issues-with-service-assisted-search-for-shared-mailboxes) |
| Fastmail | Its **2014-12-01** engineering post describes Cyrus with per-user Xapian and multiple index tiers. Current Cyrus source still contains the Xapian backend; Fastmail's exact 2026 production topology is **(unverified)**. [Engineering history](https://www.fastmail.com/blog/email-search-system/), [Cyrus source](https://raw.githubusercontent.com/cyrusimap/cyrus-imapd/master/imap/search_xapian.c) |
| Thunderbird | Current comm-central Gloda source creates SQLite **FTS3**, with the custom `mozporter` tokenizer—not FTS5. Gloda global search and its result facets are distinct from folder Quick Filter/other search interfaces. [Datastore source](https://searchfox.org/comm-central/source/mailnews/db/gloda/modules/GlodaDatastore.sys.mjs), [global-search guide](https://support.mozilla.org/en-US/kb/global-search) |
| notmuch / mu | Both use Xapian. notmuch operates on local message files and has no network code; mu indexes Maildir. mu's query documentation explicitly supports case/accent-insensitive matching and query inspection. [notmuch](https://notmuchmail.org/), [mu](https://github.com/djcb/mu), [mu query examples](https://www.djcbsoftware.nl/code/mu/mu4e/Queries.html) |
| MailMate | Its developer explicitly separates its internal indexes from its optional Spotlight importer. Index files can be rebuilt from cached messages; their exact proprietary format is **(unverified)**. Its smart-mailbox/search editor is a useful model for expert functionality. [Developer, 2025-01-08](https://blog.freron.com/2025/busycontacts/), [rebuilding](https://manual.mailmate-app.com/rebuild), [search UI](https://manual.mailmate-app.com/view) |
| Superhuman | Its **2017-05-05** post describes cached browser search, a WebSQL full-text table and a query compiler, with offline use and server-result merging. Current local engine is **(unverified)**. A later supplier case study identifies turbopuffer for server-side Ask AI retrieval; that does not establish on-device semantics. [Historical implementation](https://blog.superhuman.com/delightful-search-more-than-meets-the-eye/), [vector supplier case study](https://turbopuffer.com/customers/superhuman) |
| Mimestream | Current help documents Gmail operators and tokenized All Mail/current-folder scope. Its founder's **2020-09-10** explanation describes merging server results with basic local From/Subject search over a limited recent cache. Do not assume full offline body search today without testing. [Current help](https://mimestream.com/help/user-guide/searching), [dated implementation](https://news.ycombinator.com/item?id=24422432) |
| Spark | Vendor documentation says semantic indexing runs locally. Its AI Assistant nevertheless sends the query and selected relevant emails to an AI provider to generate answers. The actual keyword/vector engines are **(unverified)**. [CLI search](https://sparkmailapp.com/help/spark-cli/getting-started-with-spark-cli), [AI data flow](https://sparkmailapp.com/features/ai-assistant) |

Fastmail's current help makes useful distinctions: punctuation is not indexed, stemming can be disabled with quotes, saved searches are supported, and attachment-content search is unavailable offline. Spark's Smart Search help describes different default folder coverage for Gmail and other accounts. These documented differences reinforce the need to show scope and attachment coverage explicitly. [Fastmail search help](https://www.fastmail.help/hc/en-us/articles/360060591213-Searching-your-mail), [Spark Smart Search](https://support.readdle.com/spark/manage-your-inbox/use-smart-search)

### 2.4. Query language and natural-language queries

Gmail supplies the familiar vocabulary: `from:`, `to:`, `subject:`, `has:attachment`, `filename:`, `larger:`, `older_than:`, `label:` and `is:unread`, plus quoted phrases, OR and exclusions. Its own documentation warns that conversation display can include a conversation with both excluded and matching messages. [Gmail operators](https://support.google.com/mail/answer/7190?hl=en)

notmuch adds tags, folder/path scope, thread subqueries, date ranges and selected regular-expression fields. Its `to:` includes To, Cc and Bcc, illustrating why apparently identical operator names need defined semantics. [notmuch search terms](https://notmuchmail.org/doc/latest/man7/notmuch-search-terms.html) MailMate distinguishes unquoted body from quoted text, supports Boolean conditions and provides an advanced search editor. [MailMate search](https://manual.mailmate-app.com/view)

**Proposed Sweetbriar language:** one versioned query tree, editable as either readable text or native search tokens. Support implicit AND, explicit OR, parentheses, unary `-`/NOT, phrases and field values. Recognize `account:`, `in:`, `label:`/`tag:`, `from:`, `to:`, `cc:`, `bcc:`, `recipient:`, `subject:`, `body:`, `quoted:`, `filename:`, `attachment:`, `has:attachment`, `is:unread`, `before:`, `after:`, `older_than:` and `larger:`. Keep filename matching and attachment-content matching distinct. Provide expert `header:`, `message-id:` and thread expansion later.

```text
from:alice@example.org has:attachment filename:pdf after:2026-01-01
(from:alice@example.org OR from:bob@example.org) "budget review" -in:trash
account:work attachment:"termination clause" larger:5MB
```

Define precedence as NOT > AND > OR; `to:` means To only and `recipient:` means To/Cc/Bcc. Define `larger:` against raw message bytes, with explicit decimal MB and binary MiB units. Interpret calendar dates in the selected timezone, with inclusive `after:` and exclusive `before:` boundaries. Relative periods are evaluated from the current clock using that timezone. Preserve quoted phrases as literal token sequences; addresses/IDs bypass stemming and typo expansion. Offer explicit stemming/linguistic matching and substring operators separately from exact mode. Unknown operators should produce a repair suggestion, not silently search a different query. Save the tree, original text, timezone policy, search scope and sorting preference for smart mailboxes; relative dates must reevaluate as time passes.

Unscoped literal text searches headers/body and enabled attachment text; `body:` includes authored and quoted body, while `quoted:` isolates quotations. In an explicit semantic mode, unscoped text becomes a meaning-based retrieval request, while account/date/flag filters and literal clauses remain mandatory. Keep default spam/trash exclusion visible and provide `in:anywhere` to remove it. A saved query should record its matching mode.

For novices, selecting a suggested person should create an editable From/To token, and a filter builder should expose dates, accounts and attachments without memorizing syntax. Preserve OR groups and parentheses when switching between text and tokens; never flatten a complex query into an AND-only token list.

Natural language should produce a validated query tree whose interpretation is visible and editable. For “PDFs from Alice last month,” resolve the person and display the exact date interval. The model proposes filters; the deterministic executor decides what matches. Semantic similarity is a separate retrieval feature.

### 2.5. On-device semantic search

**Apple APIs serve different purposes.** `NLContextualEmbedding`, available from **macOS 14**, returns contextual sequences of vectors. Its current documentation emphasizes classification/tagging and points elsewhere for semantic similarity; it does not establish a retrieval-trained, interoperable document embedding. Assets and language/model availability must be checked, and English/Chinese can require different models. Averaging token vectors needs evaluation rather than an assumption of good mail retrieval. [Apple documentation](https://developer.apple.com/documentation/naturallanguage/nlcontextualembedding.md)

`SystemLanguageModel` is an on-device generation API available from **macOS 26**, subject to device/region/model availability. Use it as an optional natural-language-to-query parser, with constrained output and deterministic validation. It is not a documented general embedding endpoint; Apple describes integrating separate embedding models for retrieval. Current Foundation Models documentation also includes Private Cloud Compute, so selecting this framework alone does not guarantee on-device processing: explicitly choose the local model. [SystemLanguageModel](https://developer.apple.com/documentation/foundationmodels/systemlanguagemodel.md), [Apple retrieval guidance](https://developer.apple.com/documentation/technotes/tn3193-managing-the-on-device-foundation-model-s-context-window.md), [current framework](https://developer.apple.com/documentation/foundationmodels.md)

**Portable model candidates, checked 2026-10-11:** names below identify specific checkpoints, not every model in each family.

| Model | Language; output/context | License and engineering implication |
|---|---|---|
| `BAAI/bge-small-en-v1.5` | English; 384 dimensions, 512 tokens | MIT; useful small English baseline. Follow its retrieval-query instruction and pooling recipe. [Card](https://huggingface.co/BAAI/bge-small-en-v1.5) |
| `thenlper/gte-small` | English; 384 dimensions, 512 tokens | MIT; comparable baseline, requiring chunks for long messages. [Card](https://huggingface.co/thenlper/gte-small) |
| `nomic-ai/nomic-embed-text-v1.5` | English; 768 dimensions, 8,192 tokens; trained dimension reduction | Apache-2.0; use distinct `search_query:`/`search_document:` prefixes and the documented normalization. [Card](https://huggingface.co/nomic-ai/nomic-embed-text-v1.5) |
| Original `embeddinggemma-300m` | 308M parameters; 100+ languages, 768→128 dimensions, 2K context | Its checkpoint metadata identifies the Gemma license; review those terms rather than inferring the newer release's license. [Overview](https://ai.google.dev/gemma/docs/embeddinggemma), [checkpoint metadata](https://huggingface.co/api/models/google/embeddinggemma-300m), [terms](https://ai.google.dev/gemma/terms) |
| **EmbeddingGemma 2** | 270M text-only parameters; 100+ languages, 8,192 context, 768/512/256/128 dimensions | Apache-2.0 in current official documentation. Full multimodal model is 740M; text-only deployment can omit vision/audio encoders. Attractive multilingual candidate, but exact Rust-runtime compatibility remains **(unverified)**. [Current card](https://huggingface.co/google/embeddinggemma-2/raw/main/README.md) |

Google's overview was updated **2026-10-06**: treating the original EmbeddingGemma as the newest choice would already be stale. The new card requires renormalization after dimension truncation and matching query/document dimensions. Neither multilingual benchmark scores nor advertised mobile latency prove quality or throughput on private email. [Current overview](https://ai.google.dev/gemma/docs/embeddinggemma), [model requirements](https://huggingface.co/google/embeddinggemma-2/raw/main/README.md)

For deployment, **Candle** offers Rust-native inference with CPU/GPU backends; **`ort`** wraps ONNX Runtime and can reuse exported ONNX models. Model architecture, tokenizer, pooling, quantization and supported execution-provider operations must all match the checkpoint. [Candle](https://github.com/huggingface/candle), [ort](https://github.com/pykeio/ort) **MLX** is another candidate for native integration through C/C++/Swift. Its current README also documents Linux CPU/CUDA packages, so “MLX only runs on macOS” is now too broad; verify the specific model/backend combination. Keep an accelerated adapter behind a portable embedding interface. [MLX](https://github.com/ml-explore/mlx)

| Vector option | Embedding fit and trade-off |
|---|---|
| `sqlite-vec` | Small C extension usable from Rust on both platforms; SQL-managed `vec0` vectors and manual distance functions. Good exact-search baseline for filtered cohorts; the project warns it is pre-v1. Do not assume a mature ANN path merely from “vector search.” [Project](https://github.com/asg017/sqlite-vec), [KNN methods](https://alexgarcia.xyz/sqlite-vec/features/knn.html) |
| `usearch` | C++ HNSW with Rust bindings, saved indexes/mapped views and reduced-precision vectors. Leading ANN prototype if embedding is justified. Extra native dependency and separate recovery protocol. [USearch](https://github.com/unum-cloud/usearch) |
| `hnsw_rs` | Rust HNSW with graph dump/reload, mapped vector data and filtering APIs. Inspected docs identify **0.3.5**. Application must own message revisions, deletions and consistent publication. [Documentation](https://docs.rs/hnsw_rs/latest/hnsw_rs/) |
| LanceDB | Embedded Rust-oriented database with vector and hybrid search. Potentially useful for large chunk collections, but adds another storage lifecycle. [Project](https://github.com/lancedb/lancedb), [vector search](https://docs.lancedb.com/search/vector-search), [hybrid search](https://docs.lancedb.com/search/hybrid-search) |
| Tantivy itself | The inspected **0.26.2** `FieldType` API contains no ANN vector field. Plan a separate vector index; storing bytes is not nearest-neighbor indexing. [Schema](https://docs.rs/tantivy/latest/tantivy/schema/enum.FieldType.html) |

**Proposed hybrid ranking:** independently retrieve lexical BM25 and vector candidates, enforce the same hard filters, collapse chunk hits to message IDs, then apply reciprocal rank fusion: `score(d) = Σ 1/(k + rank_i(d))`. The original **2009** paper combines ranks without depending on arbitrary engine scores. Start experiments with `k=60` and 200 candidates per branch, then tune against mail judgments; this is not a guaranteed optimal configuration. Keep literal-only search available. [RRF paper](https://cormack.uwaterloo.ca/cormacksigir09-rrf.pdf)

**Storage arithmetic, not benchmarks:** for one million vectors, payload alone is:

| Dimensions | float32 | float16 | int8 |
|---|---:|---:|---:|
| 384 | 1.536 GB | 0.768 GB | 0.384 GB |
| 768 | 3.072 GB | 1.536 GB | 0.768 GB |

These are decimal GB, calculated as `count × dimensions × bytes`. They exclude graph edges, IDs, metadata, alignment, model weights and text. Four vectors per message multiply them by four; one million messages with four 768-dimensional float32 chunks require **12.288 GB** of vector payload. A trained 256-dimensional representation stored as int8 would require **0.256 GB per million vectors**, if quantization preserves acceptable quality and the chosen engine supports it.

Indexing time is workload-dependent **(unverified)**. An explicit planning scenario—one million messages, two 256-token chunks each—means **512 million input tokens**. At an assumed sustained 1,000 tokens/s that is **142 hours**; at 10,000 tokens/s, **14.2 hours**. At an assumed additional 10 W, the latter consumes approximately **142 Wh**. These are sensitivity calculations, not laptop measurements or promised speeds; token padding, attachments and OCR can add substantial work.

**Recommendation:** semantic search should be optional and resumable, scheduled during charging/idle time with thermal and battery limits. Start with subject plus newly authored body chunks, then opt-in attachment chunks; avoid repeatedly embedding quoted history. Pin model hashes, tokenizer/pooling/prefix rules and dimension versions. Store only local vectors, treat them as sensitive derived mail, purge them with removed accounts, and allow disabling/deleting the index. A local semantic index followed by cloud answer generation is still a cloud disclosure, as Spark's documented flow illustrates. [Spark data flow](https://sparkmailapp.com/features/ai-assistant)

### 2.6. Attachment extraction

**macOS:** `PDFDocument.string` exposes a PDF's textual content; for navigation/highlights, retain page-level provenance rather than just concatenating the document. For image-only pages or attached scans, render safely and run Vision `VNRecognizeTextRequest`; the API exposes accuracy/speed choices and revision-dependent supported-language discovery. OCR output should be labeled and retain page/box references. [PDFKit](https://developer.apple.com/documentation/pdfkit/pdfdocument/string.md), [Vision OCR](https://developer.apple.com/documentation/vision/vnrecognizetextrequest.md)

Spotlight importers are an OS extraction/indexing extension mechanism, with installed-type coverage and platform constraints. Use them for optional integration or additional extraction, not the only path to reproducible cross-platform search. An importer is not a portable promise that every attachment format yields text. [Apple importer guide](https://developer.apple.com/library/archive/documentation/Carbon/Conceptual/MDImporters/MDImporters.html)

**Linux/shared Rust candidates:** `pdf-extract` provides PDF text extraction; `lopdf` provides lower-level PDF manipulation/extraction. For difficult PDFs or rendering before OCR, `pdfium-render` wraps PDFium but **does not include PDFium itself**—shipping the native library is a separate dependency. Tesseract is a native OCR option with language data and C++ APIs. These choices need a representative PDF corpus; extraction quality was not benchmarked here. [pdf-extract](https://github.com/jrmuizel/pdf-extract), [lopdf](https://github.com/J-F-Liu/lopdf), [pdfium-render](https://github.com/ajrcarey/pdfium-render), [Tesseract](https://github.com/tesseract-ocr/tesseract)

For Office attachments, `calamine` reads Excel formats including XLS/XLSX/XLSB and ODS; `docx-rs` now documents reading DOCX packages. [Calamine](https://github.com/tafia/calamine), [DOCX reader](https://github.com/bokuweb/docx-rs) A bounded OOXML extractor can also use `zip` plus `quick-xml` to read document/slide text and related notes, following package relationships. This is proposed implementation work, not a claim that a few XML tags cover every Office document. Legacy binary DOC, complex layout, encrypted files and unsupported types need explicit coverage states or additional converters. [ZIP crate](https://github.com/zip-rs/zip2), [quick-xml](https://github.com/tafia/quick-xml), [OOXML packaging](https://learn.microsoft.com/en-us/office/open-xml/general/how-to-create-a-package)

**Proposed extraction contract:** original attachment hash + extractor/version → UTF-8 text, language hints, page/slide/sheet offsets and status. Run parsers/OCR in a constrained worker process with time, memory, uncompressed-size and archive-depth budgets. Do not execute macros or fetch linked resources. Publish only if the message/attachment revision still matches. Preserve originals; “encrypted,” “unsupported,” “failed” and “not downloaded” must remain distinguishable from “contains no matching text.”

### 2.7. Server-side search and federation

**IMAP:** Prefer UID results. Basic SEARCH covers headers, body/text, dates, sizes and flags; mailbox UIDs must be interpreted with mailbox identity and UIDVALIDITY. [IMAP4rev2, RFC 9051](https://www.rfc-editor.org/rfc/rfc9051.txt) ESEARCH adds compact ALL sets and COUNT/MIN/MAX, not an inverted index or a latency guarantee. [RFC 4731](https://www.rfc-editor.org/rfc/rfc4731) SEARCHRES can retain result sets; MULTISEARCH adds cross-mailbox searching; PARTIAL adds paged search results. Negotiate capabilities rather than assuming a server supports these standards. [RFC 5182](https://www.rfc-editor.org/rfc/rfc5182.txt), [RFC 7377](https://www.rfc-editor.org/rfc/rfc7377.txt), [RFC 9394](https://www.rfc-editor.org/rfc/rfc9394.txt)

`SEARCH=FUZZY` allows server-defined approximate matching and, with ESEARCH, RELEVANCY scores from 1 to 100. SEARCH results are not automatically relevance-sorted. It does not promise a particular edit distance or consistent matching across providers. [RFC 6203](https://www.rfc-editor.org/rfc/rfc6203.txt)

**Gmail:** `X-GM-RAW` exposes Gmail's search language over IMAP, while `X-GM-MSGID` identifies the same Gmail message across label folders. IMAP results still belong to the selected mailbox, so a unified query needs deliberate mailbox scope. [Gmail IMAP extensions](https://developers.google.com/workspace/gmail/imap/imap-extensions) Gmail API `messages.list(q=...)` supports most Gmail syntax, but does not reproduce UI alias expansion or thread-wide matching. Date strings use midnight PST; send epoch seconds for precise boundaries. Google's guide was updated **2026-09-10**, checked **2026-10-11**. [Search/filter guide](https://developers.google.com/workspace/gmail/api/guides/filtering) The list endpoint paginates at up to 500 messages per call; `q` is unavailable with the `gmail.metadata` scope, and spam/trash inclusion has an explicit parameter. [messages.list](https://developers.google.com/workspace/gmail/api/reference/rest/v1/users.messages/list)

**Microsoft:** Graph message-collection `$search` defaults to from/subject/body, sorts by sent time and returns at most **1,000 results**. [Graph `$search`](https://learn.microsoft.com/en-us/graph/search-query-parameter) Separately, `POST /search/query` with `entityTypes:["message"]` searches bodies and supported attachments. It serves the signed-in user's own work/school mailbox, excludes delegated mailboxes, and supports relevance ordering with `enableTopResults`; its reported `total` is the page count, not a corpus-wide match count. [Microsoft Search for messages](https://learn.microsoft.com/en-us/graph/search-concept-messages)

For Microsoft 365, build on Graph. Microsoft's **2026-02-05** announcement specifies phased, administrator-controllable EWS disablement starting **October 2026**, with complete shutdown in **2027**. This is not a claim that every tenant was disabled on October 1. The retirement excludes on-premises Exchange Server, and Graph does not support on-premises Exchange. [Current EWS plan](https://techcommunity.microsoft.com/blog/exchange/exchange-online-ews-your-time-is-almost-up/4492361), [migration overview](https://learn.microsoft.com/en-us/graph/migrate-exchange-web-services-overview)

**JMAP:** `Email/query` takes structured filters, sorting and pagination, with per-account IDs and optional thread collapse. `text` must cover address/subject headers and should cover convertible body text; `body` may omit non-text MIME parts. `hasAttachment` does not guarantee attachment-content indexing. Query changes are useful only when the server says it can calculate them. [JMAP Mail, RFC 8621](https://www.rfc-editor.org/rfc/rfc8621.txt)

**Proton:** Its documentation explicitly describes local content indexes and downloading/decrypting messages through Bridge for a third-party client's own content search. A Proton-hosted plaintext body-search API is **(unverified)**. Treat Bridge as a local IMAP source and expose body-download coverage. [Proton search](https://proton.me/support/search)

**Proposed federation policy:** show local results immediately, then remote results with an account-specific coverage indicator. Compile a conservative candidate query using supported server clauses. Dropping an unsupported term inside OR/NOT must not narrow the candidate set; use a safe superset or mark that query unsupported. Validate residual predicates locally after downloading; keep unvalidated candidates separate. Distinguish “no matches” from “not searched,” “offline,” “capped” and “attachments still indexing.”

Deduplicate with provider identities mapped to a local stable message ID. Use account + mailbox + UIDVALIDITY + UID for ordinary IMAP, account + Gmail message ID for Gmail, and account + JMAP ID for JMAP. For canonical Graph identities, request `Prefer: IdType="ImmutableId"` on message operations and normalize search hits to that format. Ordinary IDs can change on moves; immutable IDs persist within a mailbox, with archive-mailbox/export exceptions. [Graph identity rules](https://learn.microsoft.com/en-us/graph/outlook-immutable-id) Preserve separate copies across accounts. Never merge raw BM25 scores with provider relevance scores. Initially show server-only hits separately; hydrate them, apply the same local query semantics, and rerank when enough content is available. Do not make completeness claims about truncated server results.

## 3. Options compared

| Option | macOS + Linux Rust-core fit | Main advantage | Main cost | Proposed role |
|---|---|---|---|---|
| [Tantivy](https://github.com/quickwit-oss/tantivy) | Direct Rust embedding | Ranking, phrases, fuzziness, facets | Separate commit/recovery pipeline; immutable document updates | Leading lexical candidate |
| [SQLite FTS5](https://www.sqlite.org/fts5.html) | C library through Rust bindings | Store/index transactions; SQL joins | Custom linguistic and fuzzy work | Mandatory benchmark alternative |
| [Xapian](https://trac.xapian.org/wiki/Licensing) | C++ bridge | Mature mail-search deployments | GPLv2+ and native bridge | Reconsider if project licensing fits |
| [Meilisearch libraries](https://www.meilisearch.com/docs/capabilities/full_text_search/relevancy/ranking_rules) | Rust, but internal `milli` integration | Typo tolerance and multilingual analysis | API coupling; different ranking model | Evaluate `charabia` independently |
| [Lucene](https://lucene.apache.org/) | JVM bridge or service | Broad reference feature set, vectors | Runtime/package complexity | Benchmark/design reference |
| [Core Spotlight](https://developer.apple.com/documentation/corespotlight) / [Search Kit](https://developer.apple.com/documentation/coreservices/search_kit?language=objc) | macOS only | OS search integration | Portability and ranking control | Optional macOS adapter |
| [LocalSearch](https://gnome.pages.gitlab.gnome.org/localsearch/) / TinySPARQL | Linux desktop integration | Desktop discovery | Service/ontology dependencies | Optional Linux adapter |

Facts behind this comparison are sourced in §2.2; suitability judgments are recommendations.

## 4. Recommendations for Sweetbriar

### 4.1. Portable core and durable ownership

**Recommended initial stack:** canonical SQLite through `rusqlite` with a pinned bundled SQLite build; immutable original MIME/attachment blobs; Tantivy for lexical retrieval; shared Rust query parsing, normalization, ranking, federation and extraction-job management. Native Swift UI and macOS extraction/model adapters consume cancellable search streams. Linux should reuse those semantics with its own native UI/OCR adapter. `rusqlite` documents bundled builds that avoid depending on the host's SQLite version. [Rust bindings](https://github.com/rusqlite/rusqlite)

Keep platform search services optional. Start with one logical cross-account lexical index and explicit account filters; do not ship both Tantivy and FTS5 as redundant lexical engines without evidence that this earns its complexity.

**Durability is a store contract.** Persist a blob, complete its durable rename/write protocol, then commit the metadata reference; clean orphaned blobs after recovery, never missing references by deleting mail. Store draft revisions and settings transactionally. Use WAL with `synchronous=FULL` for acknowledged canonical writes, and verify the platform's stronger sync options where applicable. SQLite documents that WAL `NORMAL` can lose recently committed transactions after power loss; `FULL` adds the commit sync. Filesystem/controller assumptions still matter. [SQLite WAL](https://www.sqlite.org/wal.html), [sync settings](https://www.sqlite.org/pragma.html#pragma_synchronous)

Index files are derived data. A corrupt index triggers reconstruction, preserving mail/drafts/settings. Backups must cover a consistent database snapshot and referenced blobs; index backups are optional. “Never lose mail” needs a defined failure model and backups against device loss, not just an indexing choice.

### 4.2. Indexed fields and analysis

Proposed schema:

| Group | Fields and treatment |
|---|---|
| Identity | Stable local message ID, account ID, provider identity, mailbox UIDVALIDITY/UID, exact RFC Message-ID, thread ID, document revision |
| People | Parsed From/To/Cc/Bcc separately; complete addresses, display names and address components in distinct fields; preserve original values |
| Text | Subject; newly authored body; quoted body; signature; raw/normalized/exact and optional language-stemmed variants; token positions and source offsets |
| Attachments | Filename, MIME type, hash, extracted text, page/slide/sheet location, extraction status/version; distinguish inline decorations from user attachments |
| Filters/sort | Sent/received times, raw byte size, folders/labels/tags, read/star/draft flags, language hints, download/index coverage |

Use custom analyzers for addresses, URLs, filenames, invoice/tracking identifiers and long tokens. Preserve original Unicode; use versioned normalization and accent-folded alternatives for discovery. Stem language-specific text in separate fields, leaving literal phrases and identifiers untouched. Evaluate `charabia` dictionary segmentation for Chinese/Japanese/Korean and n-gram fallback for unknown words; test single-character queries rather than assuming trigrams cover them. Its documented segmenters make it a concrete candidate, not proof of mail-specific correctness. [Charabia](https://github.com/meilisearch/charabia)

Separate quote/signature fields so copied correspondence does not dominate ranking. A quote parser must be fallible: retain searchable text when classification is uncertain. Index body structure and attachment provenance once; avoid embedding entire growing threads repeatedly.

### 4.3. Update pipeline and consistency

**Proposed publication protocol:**

1. In the canonical SQLite transaction, update the message revision and append an ordered `search_outbox` change. Sync, drafts, labels, attachment extraction and deletions all use this path.
2. One search broker owns each index writer. Read a bounded batch against a consistent store snapshot; delete/reinsert by stable message ID and current revision. Repeated replay must produce one current document, not duplicates.
3. Commit the index with payload containing index generation, analyzer/schema version and applied outbox watermark. Tantivy's documented commit payloads provide a mechanism for this metadata. [Commit API](https://docs.rs/tantivy/latest/tantivy/indexer/struct.IndexWriter.html)
4. Reopen/validate that committed generation, then acknowledge the watermark in SQLite. A crash after index commit but before acknowledgement causes safe replay. Never acknowledge ahead of durable publication.
5. Reload readers and emit progress. Reject stale extractor/vector jobs by message revision; authoritative tombstones immediately suppress deleted messages, even before index cleanup.
6. During rebuild, create a separate generation from stored originals/cached extraction output, replay intervening changes, verify it, and publish its manifest durably. Retain change records until active rebuilds have passed them. Keep the old readable generation until publication succeeds.

This intentionally avoids pretending a SQLite transaction and a Tantivy commit are atomic together. For immediate feedback, overlay pending changed/new documents from the store, exclude their older index versions, and execute the same query predicates on that small delta. Bound work and expose backlog status. Sorting/flags come from current state; exact result counts require a consistent generation.

Hard account/security/date filters belong inside retrieval, before top-k truncation. Simply retrieving the top 100 globally and filtering afterward can hide all results from a smaller account. Stale-result validation should refill candidates within a work budget. Vector generations follow the same revision discipline and remain optional.

### 4.4. Ranking and native result presentation

Start with BM25 and **experimental** field boosts: subject 4, authored body 1, attachments 0.75, quotes 0.25, plus explicit address/person matching. Validate these values; do not equate frequent clicking with universal relevance. Add bounded recency/contact signals only if they improve judged queries without burying old mail. Offer strict chronological sorting and keep it strict.

Treat the final unfinished token as a prefix while typing; cap expansions. Offer spelling alternatives or a visibly labeled broader search when exact retrieval is poor. Do not silently rewrite operators, identifiers or phrases. Saved smart mailboxes should use deterministic predicates, with semantic discovery offered separately.

The native UI should stream lexical results first, cancel obsolete queries and preserve keyboard selection as more results arrive. Optional semantic retrieval may arrive later and should not delay that first page. Show account/scope tokens and facets for sender, dates, attachment types and folders. Mark facet counts approximate when coverage is incomplete. Each row should show why it matched, a literal highlighted excerpt or a semantic-match label, and the matching message within a collapsed thread. Open attachment hits at their page/slide where possible. Expanding a thread should distinguish matching messages from context.

Show remote-only hits and account errors in a clear supplemental section until hydrated/revalidated. A user should be able to inspect “local body coverage through this date; 300 attachments pending” without opening developer diagnostics. Server response arrival should not displace the selected result unpredictably.

### 4.5. Decision gate and strongest counter-argument

Before committing permanently to Tantivy, run it against FTS5 with identical extraction, filters and a fixed relevance set. Test **100,000 and one million messages**, mixed languages, long quoted threads, large attachments, duplicates and mutation bursts. Suggested test classes are an 8 GiB Apple-silicon machine and a 16 GiB Ubuntu machine with SSDs; these are chosen test budgets, not performance claims.

Record warm/cold p50/p95/p99 latency, first-result UI latency, exact-hit recall, ranking judgments, initial and incremental documents/s, peak RSS, cache state, bytes per 100,000 messages, temporary merge disk, write amplification and measured energy. Test concurrent indexing/OCR, flags/moves/deletes, crashes between publication steps, and disk-full recovery. Compare ANN recall against exact vector search under restrictive filters before adopting it.

**Strongest argument against Tantivy:** SQLite FTS5 can keep store and lexical index updates in one transaction, join metadata naturally and eliminate a substantial recovery/overlay/compaction subsystem. If it meets the same multilingual and interactive targets, its simplicity may be more valuable than Tantivy's richer retrieval surface. FTS5's custom-tokenizer and external-content APIs make that a serious alternative. [FTS5](https://www.sqlite.org/fts5.html)

**Decision:** prototype Tantivy first for retrieval flexibility, but choose FTS5 if measurements show no material quality/latency advantage. First release should prioritize durable storage, lexical search, saved searches and transparent coverage; then attachment extraction/federation, followed by optional semantics once quality and resource budgets are demonstrated.

## 5. Risks and open questions

| Risk/question | Evidence gap or required resolution |
|---|---|
| Missing results | Distinguish unsupported predicates, undownloaded body, stale revision, tokenizer mismatch, account scope and server cap. Add inspectable coverage and reproducible query traces without logging private message content. |
| Engine performance | Comparable mail benchmarks remain **(unverified)**. Corpus size alone does not determine index size: extracted attachment text, languages and positions must be specified. |
| Index recovery | Prove every crash boundary, idempotent deletion/reinsertion and safe manifest publication. Rebuild must never modify canonical mail. |
| Model/runtime drift | Test checkpoint exports and quantized inference on each supported architecture; evaluate multilingual mail directly. Model changes require a new vector generation. |
| Power and disk | Measure initial semantic/OCR indexing on target hardware. Reserve merge/rebuild space; provide resumable limits and a storage estimate before optional bulk processing. |
| Privacy | Apply the same protection policy to blobs, snippets, lexical indexes, vectors and backups. Database encryption alone cannot protect separate index files. Decide OS-search export scope per account; exclude drafts/sensitive accounts by policy if requested. |
| Encrypted/special sources | How much history should Proton Bridge or partial-sync accounts cache? What happens to local search after a server expunges mail before download? Decrypt searchable S/MIME/PGP only under an explicit local retention policy. |
| Attachment coverage | Which PDF fonts/layouts, Office variants, languages and password-protected documents pass the extraction corpus? A format extension is not a reliability guarantee. |
| Licensing | Decide Sweetbriar's distribution license before choosing GPL dependencies; verify exact model, tokenizer/dictionary and native-library licenses at packaging time. Xapian's announced future relicensing is insufficient (§2.2). |
| User research | Validate these priorities with archive-heavy, multilingual, legal/project and novice users. The cited anecdotes identify problems; they do not quantify prevalence. |

## 6. Sources

Cited sources consulted on **2026-10-11**. Historical dates are identified in the report. Forum comments and timestamps were checked directly and/or through Hacker News Algolia API snapshots. These are individual experiences, not representative surveys.

**Engines and desktop search**

- [Xapian licensing](https://trac.xapian.org/wiki/Licensing)
- [Tantivy](https://github.com/quickwit-oss/tantivy)
- [IndexWriter](https://docs.rs/tantivy/latest/tantivy/indexer/struct.IndexWriter.html)
- [Tokenizer documentation](https://docs.rs/tantivy/latest/tantivy/tokenizer/index.html)
- [Index API](https://docs.rs/tantivy/latest/tantivy/index/struct.Index.html)
- [FuzzyTermQuery](https://docs.rs/tantivy/latest/tantivy/query/struct.FuzzyTermQuery.html)
- [PhraseQuery](https://docs.rs/tantivy/latest/tantivy/query/struct.PhraseQuery.html)
- [FTS5 documentation](https://www.sqlite.org/fts5.html)
- [Xapian theory](https://xapian.org/docs/intro_ir.html)
- [API overview](https://xapian.org/docs/overview.html)
- [Charabia README](https://raw.githubusercontent.com/meilisearch/charabia/main/charabia/README.md)
- [Milli manifest](https://raw.githubusercontent.com/meilisearch/meilisearch/main/crates/milli/Cargo.toml)
- [Ranking rules](https://www.meilisearch.com/docs/capabilities/full_text_search/relevancy/ranking_rules)
- [edition licensing](https://www.meilisearch.com/open-source)
- [Lucene current releases](https://lucene.apache.org/core/downloads.html)
- [Lucene release announcements](https://lucene.apache.org/core/corenews.html)
- [Lucene](https://lucene.apache.org/)
- [analysis APIs](https://lucene.apache.org/core/10_5_2/analysis/common/allclasses-index.html)
- [vector API](https://lucene.apache.org/core/10_5_2/core/org/apache/lucene/document/KnnFloatVectorField.html)
- [Core Spotlight](https://developer.apple.com/documentation/corespotlight)
- [CSUserQuery](https://developer.apple.com/documentation/corespotlight/csuserquery.md)
- [Search Kit](https://developer.apple.com/documentation/coreservices/search_kit?language=objc)
- [Search Kit concepts](https://developer.apple.com/library/archive/documentation/UserExperience/Conceptual/SearchKitConcepts/searchKit_basics/searchKit_basics.html)
- [Tracker/TinySPARQL/LocalSearch rename](https://blogs.gnome.org/carlosg/2024/07/14/goodbye-tracker-hello-tinysparql-and-localsearch/)
- [LocalSearch](https://gnome.pages.gitlab.gnome.org/localsearch/)
- [endpoint](https://gnome.pages.gitlab.gnome.org/localsearch/endpoint.html)
- [Cyrus source](https://raw.githubusercontent.com/cyrusimap/cyrus-imapd/master/imap/search_xapian.c)
- [Schema](https://docs.rs/tantivy/latest/tantivy/schema/enum.FieldType.html)
- [Charabia](https://github.com/meilisearch/charabia)

**Client implementations and query interfaces**

- [historical bug](https://freron.lighthouseapp.com/projects/58672/tickets/2140-body-text-search-broken-in-r-6123)
- [benchmark methodology](https://github.com/quickwit-oss/search-benchmark-game)
- [Gmail relevance-ranking announcement](https://blog.google/products-and-platforms/products/gmail/gmail-search-update-relevant-emails/)
- [AI rollout](https://workspaceupdates.googleblog.com/2026/09/gmail-searchs-ai-overviews-now-available-globally.html)
- [Apple Mail search guide for macOS 26](https://support.apple.com/guide/mail/search-for-emails-mlhlp1003/16.0/mac/26)
- [independent schema analysis](https://dev.to/paris_moschovakos_5f8f1e0/whats-actually-inside-apple-mails-envelope-index-2loh)
- [Instant Search](https://support.microsoft.com/en-us/outlook/fix-search-issues-by-rebuilding-your-instant-search-catalog)
- [current service issue](https://support.microsoft.com/en-us/support/known-issues/classic-outlook-issues-with-service-assisted-search-for-shared-mailboxes)
- [Fastmail's 2014 search architecture](https://www.fastmail.com/blog/email-search-system/)
- [Datastore source](https://searchfox.org/comm-central/source/mailnews/db/gloda/modules/GlodaDatastore.sys.mjs)
- [global-search guide](https://support.mozilla.org/en-US/kb/global-search)
- [notmuch architecture and scope](https://notmuchmail.org/)
- [mu project](https://github.com/djcb/mu)
- [mu query examples](https://www.djcbsoftware.nl/code/mu/mu4e/Queries.html)
- [MailMate internal versus Spotlight indexing](https://blog.freron.com/2025/busycontacts/)
- [rebuilding](https://manual.mailmate-app.com/rebuild)
- [search UI](https://manual.mailmate-app.com/view)
- [Historical implementation](https://blog.superhuman.com/delightful-search-more-than-meets-the-eye/)
- [vector supplier case study](https://turbopuffer.com/customers/superhuman)
- [Mimestream search guide](https://mimestream.com/help/user-guide/searching)
- [CLI search](https://sparkmailapp.com/help/spark-cli/getting-started-with-spark-cli)
- [AI data flow](https://sparkmailapp.com/features/ai-assistant)
- [Fastmail search help](https://www.fastmail.help/hc/en-us/articles/360060591213-Searching-your-mail)
- [Spark Smart Search](https://support.readdle.com/spark/manage-your-inbox/use-smart-search)
- [Gmail operators](https://support.google.com/mail/answer/7190?hl=en)
- [notmuch search terms](https://notmuchmail.org/doc/latest/man7/notmuch-search-terms.html)

**Dated user and developer discussions**

- [Gmail versus Spark/Apple Mail, 2022-05-03](https://news.ycombinator.com/item?id=31252863)
- [Exact/fuzzy search discussion; Gmail and Fastmail, 2026-08](https://news.ycombinator.com/item?id=49136990)
- [Apple Mail/Proton tracking-number search, 2024-05-08](https://news.ycombinator.com/item?id=40300691)
- [Apple Mail search complaint, 2026-01-20](https://news.ycombinator.com/item?id=46689438)
- [New versus classic Outlook search, 2025-11-13](https://news.ycombinator.com/item?id=45914332)
- [Fastmail search complaint, 2026-08-18](https://news.ycombinator.com/item?id=49344752)
- [Thunderbird search discussion, 2023-07-10](https://news.ycombinator.com/item?id=36664113)
- [MailMate/Apple Mail archive search, 2015-03-01](https://news.ycombinator.com/item?id=9125729)
- [mu search experience, 2019-11-20](https://news.ycombinator.com/item?id=21580302)
- [notmuch workflows and setup, 2022-11](https://news.ycombinator.com/item?id=33568388)
- [Superhuman search experiences, 2020-06-23](https://news.ycombinator.com/item?id=23614167)
- [Superhuman search complaint, 2025-07-01](https://news.ycombinator.com/item?id=44435231)
- [Mimestream experience, 2026-06-03](https://news.ycombinator.com/item?id=48379770)
- [Mimestream founder's implementation discussion, 2020-09](https://news.ycombinator.com/item?id=24422432)
- [Apple Mail/Spotlight discussion, 2026-06-03](https://news.ycombinator.com/item?id=48377864)

**Models, runtimes and vector retrieval**

- [Google EmbeddingGemma overview and current versions](https://ai.google.dev/gemma/docs/embeddinggemma)
- [Apple documentation](https://developer.apple.com/documentation/naturallanguage/nlcontextualembedding.md)
- [SystemLanguageModel](https://developer.apple.com/documentation/foundationmodels/systemlanguagemodel.md)
- [Apple retrieval guidance](https://developer.apple.com/documentation/technotes/tn3193-managing-the-on-device-foundation-model-s-context-window.md)
- [current framework](https://developer.apple.com/documentation/foundationmodels.md)
- [BGE-small English model card](https://huggingface.co/BAAI/bge-small-en-v1.5)
- [GTE-small model card](https://huggingface.co/thenlper/gte-small)
- [Nomic Embed Text v1.5 model card](https://huggingface.co/nomic-ai/nomic-embed-text-v1.5)
- [checkpoint metadata](https://huggingface.co/api/models/google/embeddinggemma-300m)
- [terms](https://ai.google.dev/gemma/terms)
- [EmbeddingGemma 2 model card](https://huggingface.co/google/embeddinggemma-2/raw/main/README.md)
- [Candle runtime](https://github.com/huggingface/candle)
- [Rust ort runtime](https://github.com/pykeio/ort)
- [MLX runtime and platform support](https://github.com/ml-explore/mlx)
- [sqlite-vec project and stability notice](https://github.com/asg017/sqlite-vec)
- [KNN methods](https://alexgarcia.xyz/sqlite-vec/features/knn.html)
- [USearch](https://github.com/unum-cloud/usearch)
- [Documentation](https://docs.rs/hnsw_rs/latest/hnsw_rs/)
- [LanceDB project](https://github.com/lancedb/lancedb)
- [vector search](https://docs.lancedb.com/search/vector-search)
- [hybrid search](https://docs.lancedb.com/search/hybrid-search)
- [RRF paper](https://cormack.uwaterloo.ca/cormacksigir09-rrf.pdf)

**Attachment extraction**

- [PDFKit](https://developer.apple.com/documentation/pdfkit/pdfdocument/string.md)
- [Vision OCR](https://developer.apple.com/documentation/vision/vnrecognizetextrequest.md)
- [Apple importer guide](https://developer.apple.com/library/archive/documentation/Carbon/Conceptual/MDImporters/MDImporters.html)
- [pdf-extract](https://github.com/jrmuizel/pdf-extract)
- [lopdf](https://github.com/J-F-Liu/lopdf)
- [pdfium-render](https://github.com/ajrcarey/pdfium-render)
- [Tesseract](https://github.com/tesseract-ocr/tesseract)
- [Calamine](https://github.com/tafia/calamine)
- [DOCX reader](https://github.com/bokuweb/docx-rs)
- [ZIP crate](https://github.com/zip-rs/zip2)
- [quick-xml](https://github.com/tafia/quick-xml)
- [OOXML packaging](https://learn.microsoft.com/en-us/office/open-xml/general/how-to-create-a-package)

**Server protocols and provider APIs**

- [Exchange Online EWS retirement plan](https://techcommunity.microsoft.com/blog/exchange/exchange-online-ews-your-time-is-almost-up/4492361)
- [IMAP4rev2, RFC 9051](https://www.rfc-editor.org/rfc/rfc9051.txt)
- [RFC 4731](https://www.rfc-editor.org/rfc/rfc4731)
- [RFC 5182](https://www.rfc-editor.org/rfc/rfc5182.txt)
- [RFC 7377](https://www.rfc-editor.org/rfc/rfc7377.txt)
- [RFC 9394](https://www.rfc-editor.org/rfc/rfc9394.txt)
- [RFC 6203](https://www.rfc-editor.org/rfc/rfc6203.txt)
- [Gmail IMAP extensions](https://developers.google.com/workspace/gmail/imap/imap-extensions)
- [Search/filter guide](https://developers.google.com/workspace/gmail/api/guides/filtering)
- [messages.list](https://developers.google.com/workspace/gmail/api/reference/rest/v1/users.messages/list)
- [Graph `$search`](https://learn.microsoft.com/en-us/graph/search-query-parameter)
- [Microsoft Search for messages](https://learn.microsoft.com/en-us/graph/search-concept-messages)
- [migration overview](https://learn.microsoft.com/en-us/graph/migrate-exchange-web-services-overview)
- [JMAP Mail, RFC 8621](https://www.rfc-editor.org/rfc/rfc8621.txt)
- [Proton search](https://proton.me/support/search)
- [Graph identity rules](https://learn.microsoft.com/en-us/graph/outlook-immutable-id)

**Canonical storage and durability**

- [Rust bindings](https://github.com/rusqlite/rusqlite)
- [SQLite WAL](https://www.sqlite.org/wal.html)
- [sync settings](https://www.sqlite.org/pragma.html#pragma_synchronous)