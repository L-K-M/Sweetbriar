<!-- provenance: helper=glm model=zai-coding-plan/glm-5.3 started=2026-10-11T18:38:36+00:00 exit=0 brief=research/briefs/ -->

# Sweetbriar Research Report — Best-in-Class Email Search

**Topic:** Search architecture for the Sweetbriar email client (Rust core, native macOS-first UI, Linux later).
**Date of research:** 2026-10-11. Time-sensitive facts were verified against the web on this date where possible; the access date is noted per fact. Facts that could not be verified are marked **(unverified)**.

> **OS-version note (verified).** The assignment correction checks out: [apple.com/macos](https://www.apple.com/macos/) (fetched 2026-10-11) presents **macOS 27 "Golden Gate"** as the current release, lists Apple-silicon-only device support (MacBook Neo 2026; M1 and later), and — directly relevant to this topic — advertises **"More relevant search in Mail"** ("a new ranking system brings the most relevant results to the top… even if it's buried deep in your inbox", with search-term highlighting in results), **"Better Spotlight search suggestions"**, and Siri AI personal-context mail search. Apple's [Foundation Models documentation](https://developer.apple.com/documentation/foundationmodels.md) (fetched 2026-10-11) references WWDC2026 sessions and an "AFM 3 Cloud" model generation. Details below cite these where used.

---

## 1. TL;DR

- **macOS 27 ships a relevance-ranked Mail search with term highlighting** (verified on [apple.com/macos](https://www.apple.com/macos/), 2026-10-11). Relevance ranking in a native Mac mail client is now table stakes, not a nice-to-have.
- **Recommended core engine: Tantivy** (MIT, pure Rust, Lucene-inspired, incremental indexing, BM25, phrase + fuzzy queries, mmap, aggregations) for full-text, **plus SQLite as the system of record** (with FTS5 as an optional fallback path). Both embed cleanly in a Rust core on macOS and Linux ([Tantivy README](https://github.com/quickwit-oss/tantivy); [SQLite FTS5 docs](https://www.sqlite.org/fts5.html)).
- **Xapian is disqualified for a closed-source core**: it is GPL-licensed ([Xapian features page](https://xapian.org/features.php)), which is exactly why notmuch and mu are GPLv3.
- **Upstream Tantivy has no vector support through 0.27.0** (verified against its [CHANGELOG](https://raw.githubusercontent.com/quickwit-oss/tantivy/main/CHANGELOG.md), 2026-10-11). For on-device semantic search, pair Tantivy with **sqlite-vec** (pure-C SQLite extension, Apache-2.0/MIT, pre-v1) or **USearch** (Apache-2.0 HNSW with Rust bindings) ([sqlite-vec](https://github.com/asg017/sqlite-vec); [USearch](https://github.com/unum-cloud/usearch)).
- **Multilingual must be designed in**: SQLite's default unicode61 and Tantivy's default tokenizer do not segment CJK usefully; use **charabia** (MIT; jieba for Chinese, lindera for Japanese/Korean, Thai/Khmer dictionaries, diacritics folding — with published per-script throughput) or tantivy-jieba/lindera ([charabia](https://github.com/meilisearch/charabia); [Tantivy README](https://github.com/quickwit-oss/tantivy)).
- **Query language**: a Gmail-compatible token set (`from:`, `has:attachment`, `older_than:`, …) over a notmuch-style boolean core (AND/OR/NOT, phrases, NEAR, regex, saved queries), with an always-visible structured-token UI so novices never need the syntax. Natural-language → query via rules first, then on-device LLM with constrained output (Apple Foundation Models guided generation on macOS 26/27).
- **Ranking**: BM25F with field boosts + recency decay + thread collapsing, fused with optional vector results via reciprocal rank fusion. macOS 27, Gmail-style relevance, and RFC 6203's RELEVANCY scores all point the same direction.
- **Server-side search per account type**: Gmail `X-GM-RAW` / Gmail API `q=` (full web-Gmail grammar over IMAP/API), Microsoft Graph `$search` (KQL, date-sorted, ≤1,000 results), JMAP `Email/query` + `SearchSnippet/get`, and plain IMAP SEARCH/ESEARCH with FUZZY (RFC 6203) where offered; merge into local results by Message-ID with provenance badges.
- **Data safety**: the search index is 100% derived, rebuildable state next to an append-only message store; never the source of truth. Indexing runs as a crash-safe pipeline with a durable change log.
- **Semantic search is opt-in and cheap-ish**: EmbeddingGemma (300M params, 768-dim Matryoshka down to 128, 100+ languages, Gemma license) or bge-small-en-v1.5 (33M params, 384-dim, MIT) run on-device; at 768-dim int8, 1M messages ≈ 0.8 GB of vectors (estimate, see §2.5).

---

## 2. Findings

### 2.1 What "best in class" means for mail search (Q1)

**Finding — the axes that matter.** "Best in class" decomposes into ten measurable axes. Where I cite a capability below it is sourced; the target numbers are my synthesis (opinion) of what users of the reference clients have come to expect:

1. **As-you-type latency.** Local results must appear per keystroke. Superhuman markets itself as letting you "fly through your inbox twice as fast" ([superhuman.com](https://superhuman.com/), fetched 2026-10-11 — a marketing claim, not a measurement). A reasonable engineering target (opinion): first results in <50 ms per keystroke against a 100k-message local index, <150 ms end-to-end including rendering.
2. **Ranking quality.** Gmail-style relevance ordering — and now Apple's: macOS 27 explicitly advertises "a new ranking system [that] brings the most relevant results to the top… even if it's buried deep in your inbox," with matched terms highlighted in results ([apple.com/macos](https://www.apple.com/macos/)). RFC 6203 formalized the same idea server-side with 1–100 RELEVANCY scores ([RFC 6203 §4](https://www.rfc-editor.org/rfc/rfc6203.html)).
3. **Operators.** Gmail's operator set is the de-facto vocabulary: `from: to: cc: bcc: subject: after:/before:/older:/newer:/older_than:/newer_than: label: category: has:attachment has:drive has:youtube… list: filename: in:anywhere/in:archive/in:snoozed is:muted/important/starred/unread/read has:yellow-star… deliveredto: size:/larger:/smaller: +word rfc822msgid: has:userlabels header:` plus `AND OR - "…" ( ) AROUND N` ([Gmail Help](https://support.google.com/mail/answer/7190?hl=en)). notmuch adds `body: from:/regex/ attachment: mimetype: tag:/is: id:/mid: thread:{…} path: folder: date:…​..…​ lastmod: query: property: sexp:` with `and/or/not/xor`, `NEAR/N`, `ADJ/N`, wildcards, and rich relative dates ([notmuch-search-terms(7)](https://manpages.debian.org/notmuch-search-terms.7.en.html)). MailMate compresses the same idea into one-letter modifiers (`f s t a d b q m M T K A`) with `!` negation and `and/or` ([MailMate manual](https://manual.mailmate-app.com/view)).
4. **Typo tolerance.** Xapian "can suggest spelling corrections for user supplied queries… based on words which occur in the data being indexed" ([Xapian](https://xapian.org/features.php)); Tantivy has `FuzzyTermQuery` (Levenshtein, incl. JSON fields) ([CHANGELOG](https://raw.githubusercontent.com/quickwit-oss/tantivy/main/CHANGELOG.md)); RFC 6203 makes fuzziness a server-side concept too.
5. **Attachment contents.** Users expect `filename:`/`has:attachment` (Gmail) and `attachment:`/`mimetype:` (notmuch) at minimum; best-in-class indexes extracted attachment *text* (mu does: search on "attachment (name, mime-type, text)" and even language, per [mu's feature list](https://www.djcbsoftware.nl/code/mu/)).
6. **Multilingual text.** CJK segmentation, diacritic folding, stemming. mu advertises "accent/case normalization — so *angstrom* matches *Ångström*" ([mu](https://www.djcbsoftware.nl/code/mu/)); notmuch's stemming is English-only today (verified: "Stemming is currently only supported for English", [man page](https://manpages.debian.org/notmuch-search-terms.7.en.html)); charabia gives per-script pipelines with measured throughputs ([charabia](https://github.com/meilisearch/charabia)).
7. **Natural-language queries and semantic search.** Gmail's help documents `AROUND` and free-text; macOS 27's Siri AI "can find relevant answers… easily locate an email buried in your inbox" ([apple.com/macos](https://www.apple.com/macos/)); Apple's Foundation Models framework provides on-device LLM inference with **guided generation** (`@Generable`) and tool calling ([Foundation Models docs](https://developer.apple.com/documentation/foundationmodels.md)) — the building blocks for NL→query on device.
8. **Saved searches / smart mailboxes.** MailMate ships smart mailboxes editable as boolean condition trees and lets any search be saved as one; notmuch has named saved queries (`query:` prefix, [man page](https://manpages.debian.org/notmuch-search-terms.7.en.html)).
9. **Search across accounts.** MailMate's "universal" mailboxes aggregate standard mailboxes across all IMAP accounts ([MailMate manual](https://manual.mailmate-app.com/)). Local index unification plus per-account server fan-out is the natural extension.
10. **Mail not yet downloaded (server-side) + merged results.** See §2.7; merging by `Message-ID`/`X-GM-MSGID` with provenance ("on server only") is the pattern Gmail-over-IMAP clients and Mimestream live with daily.

**Finding — what users praise and complain about (press/community sentiment; specifics unverified unless sourced).**

- **Gmail** — praised for operator breadth, speed, and server-side scope; criticized for tokenization quirks (no substring matching; exact-phrase behavior), and conversation-level semantics: Google's own help notes that `-is:starred` can still surface a conversation containing an unstarred message, because "Gmail identifies matching messages first" ([Gmail Help](https://support.google.com/mail/answer/7190?hl=en)). Gemini-in-Gmail search features announced in 2025 could not be verified from here **(unverified)**.
- **Apple Mail** — praised for Spotlight integration and simplicity; long-standing complaints about missed results/reindexing cycles are widely reported but **(unverified)**. What *is* verified: Apple now markets a new relevance-ranking system and result highlighting in macOS 27 Mail ([apple.com/macos](https://www.apple.com/macos/)).
- **Outlook (Microsoft 365)** — Graph `$search` results are "sorted by the date and time the message was sent" and capped at 1,000 results ([Microsoft Learn](https://learn.microsoft.com/en-us/graph/search-query-parameter)); users who want relevance-first or >1k results are out of luck on the API side. New-Outlook UI complaints are community sentiment **(unverified)**.
- **Fastmail** — server-side search backed by Xapian inside Cyrus: Cyrus's own README states "If you intend to use Xapian for search…" and that their custom Xapian fork adds "word boundary analysis for Chinese, Japanese and Korean" ([Cyrus README](https://raw.githubusercontent.com/cyrusimap/cyrus-imapd/master/README.md)); Fastmail staff are the main Cyrus contributors ([Wikipedia](https://en.wikipedia.org/wiki/Cyrus_IMAP_server)).
- **Thunderbird** — Quick Filter + Gloda global search; liked for substring-ish matching, criticized for index staleness and rebuild pain. Gloda's SQLite-backed fulltext details **(unverified — source fetch blocked)**.
- **notmuch / mu** — loved for speed and scriptability (tags, saved queries, emacs integration); limitations are documented by the tools themselves: notmuch's English-only stemming; mu is terminal/emacs-centric. Both are GPLv3 via Xapian ([mu](https://www.djcbsoftware.nl/code/mu/)).
- **Superhuman** — famous for perceived instant search and command palette; internals are proprietary; public site makes only general speed claims ([superhuman.com](https://superhuman.com/)).
- **Mimestream** — a native macOS Gmail-API client, actively developed (v1.10, July 2026; iOS TestFlight opened 2026-07; future builds require macOS 14+) ([Mimestream blog](https://mimestream.com/blog/)); its Gmail API dependence is visible even in incident notes ("the Gmail API began returning unexpected data"). Search quality ≈ Gmail's server search; offline/local search is limited by design **(internals unverified)**.
- **MailMate** — power users praise its search language, smart mailboxes, and Thread Arcs; complaints center on the spartan UI and plain-text-only composition ([MailMate manual](https://manual.mailmate-app.com/)).
- **Spark** — server-assisted client; search internals not public **(unverified)**.

### 2.2 Search engines and their numbers (Q2)

**Finding — SQLite FTS5** ([sqlite.org/fts5.html](https://www.sqlite.org/fts5.html), fetched 2026-10-11):
- Query syntax: phrases (`"a b"`), prefix (`term*`), initial-token (`^`), `NEAR(g1 g2, N)`, column filters (`col:` / `{c1 c2}:` including negated filters), `AND/OR/NOT` with implicit AND; tokens capped at 32,768 bytes.
- Tokenizers: `unicode61` (default; Unicode 6.1 categories; configurable `remove_diacritics 0/1/2`, `categories`, `tokenchars`, `separators`), `ascii`, `porter` (stemming wrapper), and `trigram` (general substring matching; `case_sensitive`, `remove_diacritics` options; also accelerates `LIKE`/`GLOB`; sub-3-character substrings don't match). Custom tokenizers via the C API; the `locale` and `tokendata` options exist for advanced use.
- Storage shapes: plain, `content=''` (contentless), `contentless_delete=1` (since 3.43.0, enables DELETE/UPDATE), and external-content tables kept in sync with triggers; `columnsize`, `detail=full/column/none`, `prefix='2 3'` options trade size for capability; `bm25()`, `highlight()`, `snippet()` auxiliary functions; segment `automerge/crisismerge/deletemerge` controls; `rebuild`/`integrity-check`/`optimize` commands.
- Fit: part of the SQLite amalgamation you already ship; zero extra moving parts; the natural place for *facets* and metadata filters via ordinary SQL joins.

**Finding — Tantivy** ([README](https://github.com/quickwit-oss/tantivy); [CHANGELOG](https://raw.githubusercontent.com/quickwit-oss/tantivy/main/CHANGELOG.md); fetched 2026-10-11): current release **0.27.0**; MIT; 16.2k stars; pure Rust (Linux/macOS/Windows).
- Feature list (from README): BM25 scoring "the same as Lucene," natural query language, phrase queries, **incremental indexing**, multithreaded indexing ("English Wikipedia takes < 3 minutes on my desktop" — maintainer claim), mmap directory, SIMD compression, fast fields (u64/i64/f64/bytes), text/date/ip/bool/facet/JSON fields, compressed doc store (LZ4/Zstd), range queries, faceted search, aggregation collectors (histogram, range, stats), configurable indexing (term frequencies/positions), **LogMergePolicy with deletes**.
- Mutation model: "Data in tantivy is immutable. To edit a document, the document needs to be deleted and reindexed" (README FAQ) — deletes are tombstoned and purged on merge; commits gate visibility to newly acquired searchers.
- Queries: fuzzy (`FuzzyTermQuery`, also on JSON fields), `RegexQuery` and (0.26) regex in the query grammar, `RegexPhraseQuery` ("b.* b.* wolf", with slop), phrase slop/prefix, `IN […]` lists, lenient parser.
- Tokenizers: stemming "for 17 Latin languages" built in; third-party Chinese (`tantivy-jieba`, `cang-jie`), Japanese (`lindera-tantivy`, `vaporetto_tantivy`, `tiny-segmenter`), Korean (`lindera` + `lindera-ko-dic-builder`) are linked from the README.
- Performance: project claims "approximately 2x faster than Lucene" on its [search-latency benchmark](https://tantivy-search.github.io/bench/) (self-reported).
- **Vectors: none.** The 0.27.0 CHANGELOG (all versions back to 0.10) contains no vector/HNSW feature; vector search must come from a companion index (§2.5).
- Multi-process: one writer process at a time (directory lock); readers in other processes can open the index read-only — the classic split for a mail daemon + UI.

**Finding — Xapian** ([xapian.org/features.php](https://xapian.org/features.php), fetched 2026-10-11): C++, **GPL** ("Free Software/Open Source - licensed under the GPL"). Probabilistic/DFR/LM weighting families, relevance feedback, phrase + proximity, boolean filters, Snowball stemmers for ~22 languages, wildcards, synonyms (`~cash`), snippets with highlighting, **spelling correction**, facets, >2 GB databases, "simultaneous update and searching; new documents become searchable right away." CJK: upstream is weak here — Cyrus maintains a *fork* specifically to get "word boundary analysis for Chinese, Japanese and Korean" ([Cyrus README](https://raw.githubusercontent.com/cyrusimap/cyrus-imapd/master/README.md)). Embedding a GPL C++ library would force GPL on Sweetbriar's core (unless Sweetbriar chooses GPL — an open question for the project).

**Finding — Meilisearch's charabia** ([GitHub](https://github.com/meilisearch/charabia), fetched 2026-10-11): MIT Rust tokenizer library, usable independently of Meilisearch. Script/language detection selects a pipeline; measured segmentation/tokenization throughput (their bench, Xeon E5 1650): Latin ~23/~9 MiB/s, Greek ~27/~8, Hebrew ~33/~11, Arabic ~36/~11, **Chinese (jieba) ~10/~5**, Thai ~22/~11, Khmer ~7/~5, **Japanese (lindera IPA) ~3/~3**, **Korean (lindera KO) ~2/~2** MiB/s. Normalizations: NFKC compatibility decomposition, lowercase, nonspacing-mark removal, `Đ/Ð` spoof fixes, kvariant conversion for Chinese. This is the strongest turnkey multilingual tokenizer story available to a Rust core.

**Finding — Lucene**: the reference architecture (segments, BM25, doc values) that Tantivy explicitly mirrors; Java, server-oriented, not embeddable in a Rust core. Tantivy's 2×-latency claim above is the useful comparison point.

**Finding — Apple Search Kit (SKIndex) and Core Spotlight**: SKIndex is the legacy C API of the system search stack (macOS-only, not embeddable in a cross-platform Rust core; current deprecation status **(unverified)**). Core Spotlight lets apps contribute `CSSearchableItem`s to the *system* index — good for "find my mail from Spotlight", irrelevant for in-app ranking control. macOS 27's "Better Spotlight search suggestions" ([apple.com/macos](https://www.apple.com/macos/)) improves the OS surface, not your in-app engine.

**Finding — GNOME localsearch (ex-Tracker)** ([docs](https://gnome.pages.gitlab.gnome.org/localsearch/), fetched 2026-10-11): "a filesystem indexer suitable for desktop and mobile environments… low memory footprint… fast queries across file metadata (type-to-search)." A D-Bus service for the GNOME desktop, not an app-embedded library; not usable as Sweetbriar's engine on either platform.

**Embeddability in a Rust core on macOS + Linux (fact-based):** Tantivy ✅ (pure Rust); FTS5 ✅ (bundle SQLite; `rusqlite` + bundled feature); charabia ✅ (Rust, MIT); Xapian ⚠️ (C++ bindings exist but GPL); Lucene ❌ (JVM); SKIndex/Core Spotlight ❌ (macOS-only, Obj-C); localsearch ❌ (GNOME service).

**Numbers for planning (estimates, not benchmarks I could run here):** For a 100k-message corpus (bodies + subject + participants + attachment text, positions enabled), expect a positional inverted index on the order of 0.5–2 GB for both Tantivy and FTS5 — roughly 20–60 % of raw text size after compression, before `detail`-style reductions; FTS5 `detail=column` and Tantivy without positions shrink this substantially at the cost of phrase queries. notmuch/mu users commonly report Xapian DBs in the same band **(unverified community figures)**. Query latency for interactive, field-scoped BM25 queries at this scale is single-digit milliseconds in both engines **(estimate)**.

### 2.3 How existing clients implement search (Q3)

| Client | Engine / backend (verified where linked) | Notes |
| --- | --- | --- |
| **Apple Mail** | Own index ("Envelope Index", a SQLite DB — **widely reported, internals unverified**) + system Spotlight; **macOS 27 adds a new relevance-ranking system with highlighted results** ([apple.com/macos](https://www.apple.com/macos/)) | Historical search breakage and "rebuild mailbox" folklore **(unverified)**; Apple's macOS 27 page confirms relevance ranking + highlighting is now a headline feature |
| **Thunderbird** | Gloda ("global database") over SQLite; its own full-text table — FTS3 historically, current version **(unverified; source fetch returned HTTP 406)** | Quick Filter does per-field substring; Gloda does global search + facets |
| **notmuch** | **Xapian**; maildir store; tags live in the Xapian DB | Prefixes, boolean/probabilistic split, `NEAR/ADJ`, regex on some fields, saved queries (`query:`), English-only stemming ([man page](https://manpages.debian.org/notmuch-search-terms.7.en.html)) |
| **mu / mu4e** | **Xapian**; GPLv3 ([mu site](https://www.djcbsoftware.nl/code/mu/)) | Searches sender/receiver/subject/date/size/priority/flags/message-id/maildir/labels/attachment name+mimetype+**text**/language; accent+case normalization |
| **Fastmail** | **Xapian inside Cyrus** — Cyrus README instructs builders to use their custom Xapian fork for search incl. CJK word-boundary analysis ([Cyrus README](https://raw.githubusercontent.com/cyrusimap/cyrus-imapd/master/README.md)); Fastmail drives Cyrus development ([Wikipedia](https://en.wikipedia.org/wiki/Cyrus_IMAP_server)) | Server-side; also the primary author of JMAP Mail, incl. `Email/query` filtering and `SearchSnippet/get` ([RFC 8621](https://datatracker.ietf.org/doc/html/rfc8621)) |
| **Gmail** | Proprietary server-side | Exposes full web grammar to clients via IMAP `X-GM-RAW` and the API's `q=` ([Gmail IMAP extensions](https://developers.google.com/gmail/imap/imap-extensions)); operator table in [Gmail Help](https://support.google.com/mail/answer/7190?hl=en) |
| **Superhuman** | Proprietary; local caching + own backend | Public claims are marketing-level ("twice as fast") ([superhuman.com](https://superhuman.com/)); internals **(unverified)** |
| **Mimestream** | Gmail API on macOS; server-side search | Actively developed (1.10 July 2026; iOS beta; macOS 14+ floor) ([blog](https://mimestream.com/blog/)); Gmail API usage confirmed by their incident notes |
| **MailMate** | Own index + query engine (internals **(unverified)**); "Rebuild Database" command exists (manual ToC) | Search language with one-letter modifiers, explicit header search (`delivered-to:`, `x-mailer.name:`), smart mailboxes, Thread/Correspondence expansion, search history ([manual](https://manual.mailmate-app.com/view)) |
| **Spark** | Server-assisted (Readdle cloud) **(unverified)** | Multi-account unified inbox; search largely delegated |

**Takeaway (opinion):** every serious *local* index converges on an inverted index next to a structured store (SQLite or Xapian's flint), with boolean+probabilistic query languages; every serious *hosted* backend (Gmail, Fastmail/Cyrus, Microsoft) adds relevance ranking server-side. Sweetbriar should do both: local engine with ranking, server-side delegation with merge.

### 2.4 Query languages: Gmail, notmuch, MailMate, Apple Mail (Q4)

**Verified inventories.**
- **Gmail**: `from: to: cc: bcc: subject: after: before: older: newer: older_than: newer_than: OR/{…} AND - AROUND N label: category: has:attachment|youtube|drive|document|spreadsheet|presentation list: filename: "…" ( ) in:anywhere|archive|snoozed is:muted|important|starred|unread|read has:<star-color> deliveredto: size: larger: smaller: + rfc822msgid: has:userlabels|nouserlabels label:encryptedmail header:X:Y` ([Gmail Help](https://support.google.com/mail/answer/7190?hl=en)). Numbers split on space/dash, not dot. Negation is conversation-scoped.
- **notmuch**: prefixes `body: from:[regex] to: subject:[regex] attachment: mimetype: tag:/is:[regex] id:/mid:[regex] thread:{query} path:[/**] folder:[regex] date:<since>..<until> lastmod:a..b query: property:k=v sexp:`; operators `and or not xor`, `-term`, `NEAR/N`, `ADJ/N`; wildcards; implicit AND (OR within a common prefix); capitalized terms and phrases skip stemming ([notmuch-search-terms(7)](https://manpages.debian.org/notmuch-search-terms.7.en.html)).
- **MailMate**: `foo f !smith t (smith or joe)` — modifiers `f`(From) `s`(Subject) `t`(Recipients) `a`(any address header) `d`(date) `b`(body) `q`(quoted) `m`(common headers+body) `M`(+quoted) `T`(tags) `K`(IMAP keywords) `A`(attachment filenames); `!` negation; rich relative dates (`d 5y !2y`, `d yesterday`, `d 2005-04`); arbitrary headers via `delivered-to:joe` ([manual](https://manual.mailmate-app.com/view)).
- **Apple Mail**: search-field token suggestions (People, Subjects, Attachments…) — behavior documented in the Mail User Guide **(not re-verified this session)**; macOS 27 adds relevance ranking + highlighting ([apple.com/macos](https://www.apple.com/macos/)).
- **Microsoft Graph `$search` on messages** (KQL): searchable properties `attachment bcc body cc from hasAttachment importance kind participants received recipients sent size subject to`; default search hits `from subject body`; results date-sorted; ≤1,000 results ([Microsoft Learn](https://learn.microsoft.com/en-us/graph/search-query-parameter)).
- **JMAP `Email/query` filter** (RFC 8621 §4.4.1, verified from the [RFC text](https://datatracker.ietf.org/doc/html/rfc8621)): `from to cc bcc subject body hasAttachment header hasKeyword notKeyword inMailbox inMailboxOtherThan before after minSize maxSize text`, operator objects (`operator: "and"|"or"|"not"`), `collapseThreads`, `anchor`/`position` pagination, and a companion `SearchSnippet/get` returning per-message highlighted fragments.

**Design — the Sweetbriar Query Language (SBQL).** My recommendation (opinion, grounded in the above):

1. **One canonical AST**, three front-ends: (a) structured *token chips* in the search field (novices), (b) SBQL text syntax (experts), (c) NL input translated into the same AST. Chips and text stay bidirectionally in sync — editing text re-parses to chips when unambiguous.
2. **Text syntax** — deliberately Gmail-superset, notmuch-core:
   - Fields: `from: to: cc: bcc: subject: body: id: thread: folder:/mailbox: tag:/is: has:attachment attachment:/filename: mimetype: list: before:/after:/older_than:/newer_than: larger:/smaller: is:unread|read|flagged|answered|draft|pinned label: header:<name>: account:`;
   - Combinators: `AND OR NOT` (uppercase), `-term`, `"exact phrase"`, `( … )`, `NEAR(a b, N)`;
   - Values: quoted strings, `*` wildcard, `value OR value` alternates, bare terms = any indexed field, `mailto:`-style addresses as first-class values with contact resolution ("from:Bob" matches any Bob in the address book, disambiguated in the chip UI);
   - Regex (`from:/…/`) as an explicitly *expert* escape hatch, matching notmuch;
   - Everything typed without a field defaults to subject+participants+body+attachment text (like Gmail/Apple), *not* raw any-field.
3. **Saved searches are first-class** ("smart mailboxes"): any query AST can be named, persisted, and appear in the mailbox tree (MailMate/notmuch precedent). Saved queries compose: `query:team-plus-vendor`.
4. **NL → query, on device, with a deterministic fallback**: a rules layer first (dates like "last week", "from Bob", "with attachments" → AST; notmuch's relative-date grammar is a good model of what users actually type). Anything unresolved goes to the on-device LLM with **constrained/guided decoding to the query AST** — Apple's Foundation Models `@Generable` + schema-guided generation makes this a first-class pattern on macOS 26/27 ([Foundation Models docs](https://developer.apple.com/documentation/foundationmodels.md)); on Linux, an equivalent small local model with a JSON-schema-constrained decoder (e.g. via `ort`/`candle`-served model, or llama.cpp-style grammar constraints) — but always show the interpreted query as editable chips, never execute prose blindly. **(Opinion.)**

### 2.5 On-device semantic search (Q5)

**Finding — Apple's on-device building blocks (verified from developer docs, fetched 2026-10-11).**
- **NLContextualEmbedding** (NaturalLanguage framework; availability per docs: macOS 14.0+/iOS 17.0+): "dense vector representation… using models trained with contextualized language understanding"; per-language/script models, `dimension`, `maximumSequenceLength`, `requestAssets` download, `embeddingResult(for:language:)` ([docs](https://developer.apple.com/documentation/naturallanguage/nlcontextualembedding.md)). Apple's own note steers *semantic similarity* use cases to `NLEmbedding`, and ML classification to CreateML integration — i.e., these are Apple's embeddings for on-device NLP, but Apple does not market NLContextualEmbedding as a general-purpose retrieval embedding.
- **Foundation Models framework**: `SystemLanguageModel` (on-device, Apple Intelligence devices), `LanguageModelSession`, **guided generation** (`@Generable`, `GenerationSchema`, "strong guarantees that the model generates instances of your type"), **tool calling** (`Tool` protocol — a search-tool pattern for agent-ish queries), image attachments, Private Cloud Compute variant, and (newer) running Core-AI-exported open models in a session ([docs](https://developer.apple.com/documentation/foundationmodels.md)). The on-device model's parameter count is not stated in the docs **(press-reported ≈3B in 2025: unverified)**. The macOS 27 page confirms Siri AI personal-context retrieval of mail and "AFM 3 Cloud" model naming ([apple.com/macos](https://www.apple.com/macos/)). Usage limits apply to server-backed features per Apple's footnote.

**Finding — small open embedding models (verified model cards, fetched 2026-10-11).**

| Model | Params | Dim | Ctx | Languages | License | Notes |
| --- | --- | --- | --- | --- | --- | --- |
| [google/embeddinggemma-300m](https://huggingface.co/google/embeddinggemma-300m) | 300M | **768 (MRL: 512/256/128)** | 2048 | 100+ | **Gemma terms** | MTEB Multilingual v2 61.15 (768d); QAT Q4/Q8 checkpoints within ~0.5 pt; task-prefixed prompts (`task: search result \| query:`); no float16 activations |
| [BAAI/bge-small-en-v1.5](https://huggingface.co/BAAI/bge-small-en-v1.5) | 33.4M | 384 | 512 | English | **MIT** | MTEB avg 62.17; ONNX export available |
| [BAAI/bge-m3](https://huggingface.co/BAAI/bge-small-en-v1.5) (per its README) | ~568M **(unverified)** | 1024 | 8192 | 100+ | MIT | dense + sparse + multi-vector (ColBERT) in one model — attractive for hybrid search |
| gte-small (via BGE README table) | ~33M | 384 | 512 | English | Apache-2.0 **(unverified license)** | MTEB 61.36 |
| nomic-embed-text-v1.5 | 137M **(unverified)** | 64–768 MRL | 8192 | EN + multilingual-ish | Apache-2.0 **(unverified)** | not re-verified this session |

**Finding — vector indexes (verified repos, fetched 2026-10-11).**
- **[sqlite-vec](https://github.com/asg017/sqlite-vec)**: "extremely small, 'fast enough' vector search SQLite extension"; pure C, zero deps, runs anywhere SQLite runs; `vec0` virtual tables store float/int8/**binary** vectors; metadata/auxiliary/partition-key columns; KNN `MATCH` syntax; **explicitly pre-v1 ("expect breaking changes")**; first-party Rust crate (`cargo add sqlite-vec`); ANN (IVF, DiskANN experiments) present as in-tree sources (`sqlite-vec-ivf.c`, `sqlite-vec-diskann.c`). Today's stable path is brute-force scan, which at mail scale (≤1M rows) is usually fast enough **(estimate)**.
- **[USearch](https://github.com/unum-cloud/usearch)**: Apache-2.0; single-header C++ with **Rust/Swift/Obj-C bindings**; HNSW; claims up to ~10× faster indexing than FAISS at equal recall (their benchmarks: 100M×96d i8 in ~0.2 h vs 2.6 h); `bf16/f16/e5m2/i8/b1` downcasting; **memory-mapped read-only "view" of a serialized index** (disk-serving); filtered search with predicates (incl. Rust); on-the-fly deletions; used by ClickHouse, DuckDB, Ente's Rust photo DB.
- **Tantivy vectors**: *not available* upstream through 0.27.0 (verified absence in the [CHANGELOG](https://raw.githubusercontent.com/quickwit-oss/tantivy/main/CHANGELOG.md)).
- **LanceDB / hnsw_rs**: Rust-native embedded vector store / pure-Rust HNSW; not re-verified this session **(unverified)**.

**Finding — hybrid ranking.** BM25 + vector with **reciprocal rank fusion** is the standard cheap hybrid: `RRF(d) = Σ 1/(k + rank_i(d))`, `k≈60`; robust because it needs only rankings, not score calibration **(method is common knowledge; treat the k value as convention, unverified attribution)**. EmbeddingGemma's MRL dimensions (768→128) let you trade recall for disk/CPU at a fixed model.

**Cost model (estimates, clearly labeled).** For **1M messages** at ~2 KB indexed text each:
- Embedding throughput: bge-small-33M on one modern CPU core ≈ hundreds of messages/s; EmbeddingGemma-300M ≈ 50–150 msgs/s/core (Q4/Q8); on Apple-silicon GPU via Metal/MLX, several× faster **(estimates — no benchmark could be run in this environment; EmbeddingGemma's model card does not publish per-device throughput)**. Net: 1M messages ≈ 1–8 CPU-core-hours, i.e., an overnight background job on first enablement.
- Disk: vectors only — 384-d int8 ≈ 0.4 GB; 768-d int8 ≈ 0.8 GB; 768-d fp32 ≈ 3 GB (arithmetic from verified dims × 1M; MRL-128 ≈ 130 MB). Plus the HNSW/brute-force index overhead (USearch HNSW graph typically adds tens of % **(estimate)**).
- Battery/thermal: do initial embedding on AC power or idle, rate-limited; incremental embeddings for new mail are trivial (<1 s/min of new mail).
- Privacy: everything above stays on device. NLContextualEmbedding assets download on demand (Apple); open models ship in the app bundle. Cloud LLMs must never see message bodies for query parsing — Foundation Models' on-device session or bust; note Apple's macOS 27 footnote that *server-backed* Apple Intelligence features carry daily limits.

**Recommendation (opinion):** ship lexical-first; make semantic an explicit per-account (or global) opt-in with a visible progress indicator and an "index only subject+sender+snippet" cheap mode (subject/participants only → ~1/20 the text). Use bge-small (MIT, English) or EmbeddingGemma (multilingual, Gemma license — check redistribution terms!) via `ort` (ONNX Runtime, with CoreML EP on macOS) or pure-Rust `candle`; store vectors in sqlite-vec alongside the metadata DB; upgrade path to USearch if brute force ever becomes the bottleneck.

### 2.6 Attachment text extraction (Q6)

**macOS (fact, framework availability; version specifics unverified this session):**
- **PDFKit** (`PDFDocument.string`) — text-layer PDFs, AppKit, no extra process.
- **Vision** (`VNRecognizeTextRequest`) — OCR for image PDFs/scans; on-device; language support varies by OS version **(unverified here)**.
- **Spotlight importers**: the system already indexes many attachment types system-wide; *Query the system index or duplicate it?* — Core Spotlight is for contributing items, and reading Spotlight's DB is unsupported/fragile; treat macOS's index as ambient (Finder-level) coverage, not Sweetbriar's source.
- Quartz/`mdimport`-based sidecar approaches are possible but fragile **(unverified)**.

**Rust core, cross-platform (crate names verified to exist on the ecosystem; individual licenses/versions not re-verified this session — mark as candidates, not commitments):**
- PDF: `pdf-extract` (pure Rust), `pdfium-render` (Google's PDFium via bindings — robust, ships a C binary), `lopdf` (low-level). PDFium's text extraction quality generally beats pure-Rust parsers **(widely held opinion)**.
- Office: OOXML is ZIP+XML — `calamine` (xlsx/xls/ods, mature), docx/pptx via `quick-xml` over unzipped `document.xml`; Microsoft publishes an official Rust `ooxml` crate **(existence noted in the ecosystem; unverified this session)**. Legacy `.doc/.xls` are hard in pure Rust; options are filename-only indexing or an optional helper (e.g., bundled `libreoffice --headless` convert, or Apache Tika sidecar — heavy; Tika's format coverage is the reference, and notably Xapian's Omega indexer already covers Word/Excel/PowerPoint/Publisher/Visio/RTF/iWork/etc. via external filters ([Xapian features](https://xapian.org/features.php))).
- HTML mail → text: `html5ever`/`scraper` (Servo lineage) or the HTML-to-text step inside a MIME parser.
- OCR (Linux): Tesseract via `tesseract` crate bindings or CLI sidecar; PaddleOCR via `ort` **(unverified)**.
- Architecture: extraction is a **pipeline stage**, not inline: attachment → extractor → capped plain text (e.g., first N KB; store the cap in the doc) → tokenizer → index; per-account policy knobs (max size, skip encrypted, OCR on/off).

### 2.7 Server-side search (Q7)

**IMAP baseline.** RFC 3501 `SEARCH` (and `UID SEARCH`) with `HEADER`, `TEXT`, `BODY`, `SUBJECT`, `FROM/TO/CC/BCC`, `SINCE/BEFORE/ON`, `LARGER/SMALLER`, `UNANSWERED` etc.; `CHARSET` handling is mandatory for non-ASCII. **ESEARCH** (RFC 4731, referenced from RFC 6203's normative refs) adds `RETURN (MIN MAX COUNT ALL UPDATE)` so clients can page and count without fetching all hits. **SEARCH=FUZZY (RFC 6203, verified in full)**: `FUZZY <search-key>` lets the server match inexact, assign **RELEVANCY 1–100** via ESEARCH return option, `SORT (RELEVANCY)` with `PARTIAL 1:10` windowing via CONTEXT — explicitly designed so "the server is allowed to perform all matching in an implementation-defined manner", including fuzzy dates ("from Dave about a week ago") and sizes. Adoption is server-dependent (Dovecot/Cyrus support varies) — **capability-discover at runtime** ([RFC 6203](https://www.rfc-editor.org/rfc/rfc6203.html)).

**Gmail.** `X-GM-EXT-1` capability; `SEARCH X-GM-RAW "has:attachment in:unread"` is "interpreted in the same manner as in the Gmail web interface" — i.e., the *entire verified operator table* in §2.4 is available over IMAP; the Gmail API `q=` carries the same grammar ([Gmail IMAP extensions](https://developers.google.com/gmail/imap/imap-extensions); [Gmail Help](https://support.google.com/mail/answer/7190?hl=en)). `X-GM-MSGID`/`X-GM-THRID` give stable dedupe/thread keys across folders and protocols; labels map via `X-GM-LABELS`.

**Microsoft Graph.** `GET /me/messages?$search="…"`: default searches `from subject body`; KCL property clauses for `attachment bcc body cc from hasAttachment importance kind participants received recipients sent size subject to`; `AND/OR` outside quotes, property required inside clauses; **results sorted by sent date (not relevance), max 1,000 results**; `$filter`+`$orderby` rules and `InefficientFilter` error documented on the messages endpoint ([Microsoft Learn: search-query-parameter](https://learn.microsoft.com/en-us/graph/search-query-parameter); [user-list-messages](https://learn.microsoft.com/en-us/graph/api/user-list-messages?view=graph-rest-1.0)). Practical consequence: for Graph accounts the *client* must do relevance ranking and cannot exhaustively search very large mailboxes.

**JMAP.** `Email/query` with the filter object (§2.4), `sort` (per-account `emailQuerySortOptions`), `collapseThreads: true`, `anchor`/`position` for stable pagination around a message, and **`SearchSnippet/get`** returning per-email highlighted snippet text — the only mainstream protocol with server-side highlighting designed in ([RFC 8621 §4.4, §5](https://datatracker.ietf.org/doc/html/rfc8621)).

**Merging local + server results (design, opinion).** Model every hit as `(account, folder, Message-ID | server-key, snippet?, provenance)`. Query plan per account: if local index says "account fully cached" → local only, still fire server search asynchronously for freshness; if not cached → server-first with local merge. Dedupe on `Message-ID` (fallback `X-GM-MSGID`, Graph `internetMessageId`); badge results "on server" and stream them in below local hits; surface server snippets (JMAP SearchSnippet / Graph preview / IMAP BODYSTRUCTURE+fetch) only after user scrolls, to avoid full-body downloads. Cap fan-out (parallel per-account queries, deadlines, cancellation on keystroke).

---

## 3. Options compared

**Table 1 — Local full-text engines for the Rust core** (facts sourced in §2.2; "fit" column is my judgment).

| | Tantivy | SQLite FTS5 | Xapian | charabia (tokenizer only) | Lucene | SKIndex / Spotlight | localsearch |
| --- | --- | --- | --- | --- | --- | --- | --- |
| License | MIT | Public domain | **GPL** | MIT | Apache-2.0 | proprietary | GPL-ish GNOME **(unverified)** |
| Language | Rust | C (in SQLite) | C++ | Rust | Java | C/Obj-C | C |
| Incremental updates | ✅ segments + delete-terms + merges | ✅ (contentless-delete since 3.43.0) | ✅ live | n/a | ✅ | n/a | n/a |
| Ranking | BM25 (+custom score/tweak) | bm25() (+custom aux fns) | rich (BM25/DFR/LM, feedback) | n/a | BM25 + more | opaque | n/a |
| Fuzzy/typo | FuzzyTermQuery (Levenshtein) | ❌ (trigram substring only) | spelling correction + wildcards | n/a | fuzzy | ❌ | ❌ |
| CJK | via jieba/lindera add-ons | weak (trigram workaround) | via Cyrus-style fork | **first-class** | analyzers | system | n/a |
| Phrase/proximity | ✅ (+ slop, regex-phrase) | ✅ NEAR | ✅ (+ NEAR/ADJ via notmuch) | n/a | ✅ | limited | n/a |
| Vectors | ❌ (≤0.27.0) | via sqlite-vec | ❌ | n/a | ❌ (needs complement) | ❌ | ❌ |
| Embeds in Rust core, macOS+Linux | ✅ | ✅ | ⚠️ GPL | ✅ | ❌ | ❌ | ❌ |
| Notable users | Etsy, ParadeDB, Element, Quickwit | everyone | notmuch, mu, **Cyrus/Fastmail** | Meilisearch | reference | Apple Mail (system) | GNOME |

**Table 2 — Query-language coverage of the verified operator sets.**

| Capability | Gmail | notmuch | MailMate | Graph `$search` | JMAP `Email/query` |
| --- | --- | --- | --- | --- | --- |
| from/to/cc/bcc | ✅ | ✅ (+regex) | ✅ (f/t/a) | ✅ | ✅ |
| subject | ✅ | ✅ (+regex) | ✅ (s) | ✅ | ✅ |
| body-only | ❌ (implicit) | ✅ `body:` | ✅ `b` | ✅ `body:` | ✅ |
| attachments | `has:attachment filename:` | `attachment:` `mimetype:` | `A` (filenames) | `attachment:` `hasAttachments` | `hasAttachment` only |
| dates | `after:/before:/older_than:` | `date:a..b` (rich) | `d …` (rich) | `received:` `sent:` | `before/after` |
| size | `size:/larger:/smaller:` | via mu (`size:`) | ❌ | `size:1..500000` | `minSize/maxSize` |
| flags/labels | `is:/label:/has:userlabels` | `tag:/is:` | `K` (keywords) `T` (tags) | `importance:` `kind:` | `hasKeyword/notKeyword` |
| boolean | `AND OR - ()` | `and or not xor - NEAR/N ADJ/N` | `and or !` | `"a" AND/OR "b"` | operator objects |
| arbitrary headers | `header:X:Y` | (any header) | `delivered-to:` etc. | ❌ | `header: [name, value]` |
| saved queries | labels-as-saved-search workaround | ✅ `query:` | ✅ smart mailboxes | ❌ | client-side |
| server snippets | ❌ (client) | n/a local | n/a local | ❌ | ✅ `SearchSnippet/get` |

**Table 3 — On-device semantic options** (sources in §2.5).

| | NLContextualEmbedding | Foundation Models (SysLM) | EmbeddingGemma-300M | bge-small-en-v1.5 | bge-m3 |
| --- | --- | --- | --- | --- | --- |
| Kind | Apple embedding | Apple on-device LLM | open embedding | open embedding | open embedding |
| Platforms | Apple only | Apple, AI-capable devices | any (ort/candle) | any | any |
| Dims/size | per-language, `dimension` API | n/a (LLM) | 768 (MRL→128), 300M | 384, 33.4M | 1024, ~568M |
| Multilingual | per-language assets | system languages | 100+ | EN | 100+ |
| License | Apple OS | Apple OS | Gemma terms | MIT | MIT |
| Best use | Apple-ecosystem embeddings | **NL→SBQL parsing**, snippet gen | default semantic index | cheap English index | hybrid dense+sparse |

**Table 4 — Vector index choices.**

| | sqlite-vec | USearch | Tantivy (built-in) | LanceDB / hnsw_rs |
| --- | --- | --- | --- | --- |
| License | Apache-2.0/MIT | Apache-2.0 | — | Apache-2.0/MIT **(unverified)** |
| Algorithm today | brute-force KNN (ANN in progress, pre-v1) | HNSW (claims ≈10× FAISS indexing) | **none ≤0.27.0** | HNSW / Lance |
| Rust story | first-party crate | first-class bindings | — | Rust-native / pure Rust |
| Storage | inside your SQLite DB | own file; **mmap read-only view** | — | own files |
| Deletions | ✅ (rows) | ✅ on-the-fly | — | ✅ |
| Sweetbriar fit | best default (one DB, trivial backup) | best at 1M+ scale / memory pressure | n/a | optional later |

## 4. Recommendations for Sweetbriar

**R1 — Architecture: Tantivy for text, SQLite for truth, sqlite-vec for optional vectors.**
- **Message store (system of record):** SQLite (WAL) — accounts, folders, messages, parts, flags, tags, contacts, sync state. Mail bodies on disk (per-message files) referenced from the DB; everything append-only where possible. Data-safety requirement: no user data ever lives *only* in the index.
- **Index (derived, disposable):** one Tantivy index per account (or per profile) with fields: `subject^4`, `from_names^3`, `from_addrs^3`, `to^2`, `cc/bcc^2`, `body^1`, `attachment_names^2`, `attachment_text^1`, `list_id`, plus fast fields (`date`, `size`, `folder_id`, `account_id`, `flag_bits`, `thread_id`, `lang`). Doc-address = stable internal message id; deletes by term on message id; commits batched (every N messages or T ms); `IndexReader` reload on commit for NRT-ish visibility (README: changes visible to newly acquired searchers after commit+reload).
- **Why per-account indexes:** server-search fan-out maps 1:1; an account can be reindexed/deleted atomically; smaller merge churn. Cross-account search = parallel sub-searchers + merged top-K.
- **Vector side (opt-in):** sqlite-vec `vec0` table in the same SQLite DB, keyed by message id; embed subject+participants+first-KB (cheap mode) or full text.

**R2 — Tokenizer plan.** charabia (or tantivy-jieba + lindera + a Latin pipeline) with per-script pipelines; store detected language per message (fast field) to pick stemming; `LowerCaser` + NFKD-style diacritic folding default-on (mu-style "angstrom matches Ångström"); phrase queries position-aware. Test corpus must include CJK + Thai + Arabic + Hebrew + mixed-script subjects from day one — charabia's own throughput table shows Japanese/Korean are the expensive ones (~2–3 MiB/s), so budget indexing time accordingly.

**R3 — Query pipeline.** Keystroke → debounce(~30 ms) → SBQL parse to AST (lenient parser, never error to user) → plan:
1. **Instant local pass** (limit 50, BM25F + recency prior `exp(-λ·age)` with λ≈1/year (tunable), thread collapse to best-hit-per-thread, facets computed in the same pass via Tantivy aggregation/facet collectors).
2. **Async server pass** per uncached/large account (Gmail X-GM-RAW/q=; Graph `$search`; JMAP Email/query; IMAP ESEARCH; FUZZY if advertised), merged by Message-ID with provenance badges.
3. **Semantic pass** (if enabled): embed query (asymmetric prefix if the model wants it — EmbeddingGemma's `task: search result | query:`), ANN top-K, then **RRF fusion** with lexical ranking.
4. UI: token-chip editor over the field; result list with highlighted snippets (Tantivy `Snippet`/FTS5 `snippet()`/JMAP SearchSnippet); facet sidebar (People, Folders, Has attachment, Date ranges, Tags, Account); "Save as Smart Mailbox" always visible; search history like MailMate.

**R4 — NL input.** Rules-first parser for dates/entities; Foundation Models on macOS 26/27 with `@Generable` to a `QueryAST` schema (tool-calling to a `search` tool for "show me…" interactions); plain model + JSON-schema-constrained decoding on Linux. Always render the resulting AST as chips for confirmation. Latency budget: <300 ms for the rules path; the LLM path may take longer but must be non-blocking and cancellable.

**R5 — Attachment indexing.** Stage 1: filenames + MIME always. Stage 2: PDF text (pdfium-render) and OOXML (calamine/quick-xml) in the background extractor service. Stage 3 (opt-in): OCR — Vision on macOS, Tesseract sidecar on Linux. Capped text sizes; per-account policies (e.g., skip attachment text for giant shared mailboxes).

**R6 — Rebuildability & consistency.** The Tantivy index and vector tables are pure derivatives; a change-log table (monotonic seq) drives (re)indexing after crashes; "Rebuild index…" is a first-class, progress-reporting operation (MailMate precedent); integrity checks (FTS5 `integrity-check`; Tantivy `validate` tooling) on demand; index version stamped so upgrades trigger automatic rebuilds rather than corrupt reads.

**R7 — Ranking defaults (opinion).** subject > participants > body boosts; recency decay; small boosts for read/star/direct-to-me; penalize mailing-list bulk mildly; per-thread collapse with "show all in thread". Expose a relevance/date toggle (Gmail and macOS 27 Mail both chose relevance-first; Graph forces date-first).

**Trade-offs accepted:** two index systems (Tantivy + SQLite) instead of one; Rust-native but young vector path (sqlite-vec pre-v1); extra process boundary decisions (single daemon writer, UI reader) — mitigated because both stores are file-based and crash-safe.

**Strongest counter-argument (against R1, and my answer):** *"SQLite FTS5 alone would do everything in one battle-tested database — why add a second engine, a second on-disk format, and Tantivy's faster-moving release cadence (0.24 removed index sorting; formats break across minors)?"* This is a real argument: FTS5 gives you BM25, snippets, external-content tables, and one backup story, and it is what Apple Mail's Envelope Index reportedly is. The honest answer: (a) FTS5's tokenizer story for CJK and typo tolerance is materially worse (no fuzzy query; trigram bloat and the 3-char floor); (b) ranking control (field boosts, custom scorers, aggregations for facets) is far more limited inside SQL; (c) Tantivy's mmap + segment design gives headroom for 1M+ message archives where FTS5's merge behavior inside a transactional DB gets awkward. If Sweetbriar wants the minimal-risk path, FTS5-only is viable for v1 — but I would then still keep the SBQL AST and the extractor pipeline engine-agnostic, so Tantivy can be introduced per-account later. Conversely, if Sweetbriar goes GPL, Xapian+notmuch-style is the proven alternative — at the cost of C++ binding surface and weaker Rust ergonomics.

---

## 5. Risks and open questions

**Risks.**
1. **sqlite-vec is pre-v1** ("expect breaking changes" — [README](https://github.com/asg017/sqlite-vec)). Mitigation: wrap behind a trait; USearch is the drop-in escalation path (Apache-2.0, Rust bindings, mmap views).
2. **Tantivy format churn**: index formats have broken across minor versions historically (e.g., 0.24 notes on reindex-compatibility boundaries). Mitigation: engine-agnostic design, stamped index versions, automatic rebuild (R6).
3. **CJK/Korean indexing cost**: charabia measures Korean/Japanese at ~2–3 MiB/s — a large Japanese archive will take hours to index on one core. Mitigation: parallelize by account/folder; make attachment-text and semantic stages separately skippable.
4. **Graph `$search` limits**: date-sorted, ≤1,000 results, no arbitrary-header clauses — Exchange/M365 users with big mailboxes will hit the ceiling; local caching depth becomes the real search radius. Decide the caching policy for Graph accounts early.
5. **EmbeddingGemma license (Gemma terms, not OSI)** — redistribution/commercial terms must be reviewed before bundling; bge-small (MIT) or gte are the safe fallbacks.
6. **Privacy and cloud temptation**: NL parsing must stay on-device (Foundation Models session or local model); any server-assisted parsing would be a trust failure for this product's positioning.
7. **Apple-platform drift**: macOS 27 raises expectations (relevance-ranked Mail search, highlighted results, Siri personal context). Sweetbriar's Rust core can't use Foundation Models directly — the Swift UI layer must bridge it (guided generation there, AST across FFI), duplicating some logic on Linux.
8. **Multi-process discipline**: one writer (daemon) + reader (UI) must be enforced; a stray second writer corrupts or lock-blocks. Document the lifecycle; consider a single-process embedded mode for tests.
9. **Encrypted mail**: mu handles signed/encrypted search via its own path; Sweetbriar must decide whether to index decrypted bodies (key availability) and how to mark such index entries.

**Open questions (to research next).**
- Verified numbers we still lack: index sizes/latencies **on Sweetbriar's own corpora** (build the 100k/1M benchmark fixture); Foundation Models on-device model size and token throughput on M-series **(unverified)**; Gmail AI search results rollout status **(unverified)**; Thunderbird Gloda's current FTS version **(unverified — searchfox fetch blocked, HTTP 406)**; Dovecot/Cyrus FUZZY deployment breadth **(unverified)**; SKIndex deprecation status **(unverified)**; nomic/gte licenses **(unverified)**.
- Should SBQL ever be user-extensible (MailMate hints at user-definable shorthand languages long-term)? 
- Do we index server-side-only accounts (headers-only) so at least `from:/subject:` work offline?
- Facet interaction model: click-to-filter chips vs. query rewriting in the text field (usability testing needed).
- Linux UI: does the chip/token search field translate to GTK4/Adwaita patterns, or does Linux get a different search surface with the same AST?

## 6. Sources

Consulted 2026-10-11 (HTTP GET via web fetch; noted where access failed):

1. SQLite FTS5 documentation — https://www.sqlite.org/fts5.html
2. Tantivy README — https://github.com/quickwit-oss/tantivy
3. Tantivy CHANGELOG (main; through 0.27.0) — https://raw.githubusercontent.com/quickwit-oss/tantivy/main/CHANGELOG.md
4. Xapian features — https://xapian.org/features.php
5. charabia README — https://github.com/meilisearch/charabia
6. notmuch-search-terms(7) (Debian manpages, notmuch 0.39) — https://manpages.debian.org/notmuch-search-terms.7.en.html
7. mu homepage — https://www.djcbsoftware.nl/code/mu/
8. Gmail Help: search operators — https://support.google.com/mail/answer/7190?hl=en
9. Gmail IMAP extensions (X-GM-EXT-1, X-GM-RAW, X-GM-MSGID/THRID/LABELS) — https://developers.google.com/gmail/imap/imap-extensions
10. Microsoft Graph: `$search` query parameter — https://learn.microsoft.com/en-us/graph/search-query-parameter
11. Microsoft Graph: List messages (v1.0) — https://learn.microsoft.com/en-us/graph/api/user-list-messages?view=graph-rest-1.0
12. RFC 6203 (IMAP FUZZY search) — https://www.rfc-editor.org/rfc/rfc6203.html
13. RFC 8621 (JMAP for Mail; Email/query, SearchSnippet) — https://datatracker.ietf.org/doc/html/rfc8621
14. MailMate manual (Introduction; View/Search) — https://manual.mailmate-app.com/ ; https://manual.mailmate-app.com/view
15. Apple Developer: Foundation Models framework — https://developer.apple.com/documentation/foundationmodels.md
16. Apple Developer: NLContextualEmbedding — https://developer.apple.com/documentation/naturallanguage/nlcontextualembedding.md
17. EmbeddingGemma model card — https://huggingface.co/google/embeddinggemma-300m
18. BAAI/bge-small-en-v1.5 model card (incl. BGE family table) — https://huggingface.co/BAAI/bge-small-en-v1.5
19. sqlite-vec README — https://github.com/asg017/sqlite-vec
20. USearch README — https://github.com/unum-cloud/usearch
21. Cyrus IMAP server README (Xapian/CJK fork note) — https://raw.githubusercontent.com/cyrusimap/cyrus-imapd/master/README.md
22. Cyrus IMAP server (Wikipedia; Fastmail contribution, 3.12.4 release) — https://en.wikipedia.org/wiki/Cyrus_IMAP_server
23. Mimestream blog index — https://mimestream.com/blog/
24. Superhuman homepage (speed claims) — https://superhuman.com/
25. Apple: macOS 27 Golden Gate page (Mail search ranking, Spotlight suggestions, Siri AI) — https://www.apple.com/macos/
26. GNOME LocalSearch documentation — https://gnome.pages.gitlab.gnome.org/localsearch/

Failed/limited access attempts (for transparency): DuckDuckGo HTML search (bot challenge); Bing web results (unusable localization); `notmuchmail.org` manpage path (404; used Debian mirror); `jmap.io/spec-mail.html` (404; used datatracker RFC); searchfox comm-central raw source (HTTP 406); `xapian.org` license subpage and cyrusimap.org search page (404 variants); slusarz/ and dovecot/ `fts-flatcurve` repos (404).
