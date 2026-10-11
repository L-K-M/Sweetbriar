# Topic: Best-in-class email search

Questions:

1. Define "best in class" for mail search: as-you-type latency, ranking
   quality, operators, typo tolerance, attachment contents, multilingual
   text (CJK segmentation, diacritics, stemming), natural-language queries,
   semantic search, saved searches / smart mailboxes, search across
   accounts, mail not yet downloaded (server-side search) and merging local
   with server results. What do users praise and complain about in Gmail,
   Apple Mail, Outlook, Fastmail, Thunderbird, MailMate, notmuch/mu,
   Superhuman, Mimestream?
2. Engines, with numbers where available (index size per 100k messages,
   indexing throughput, query latency, memory): Tantivy (incremental
   updates, segment merging, multi-process access, tokenizers, fuzzy and
   phrase queries), SQLite FTS5 (unicode61, trigram, custom tokenizers,
   contentless/external-content tables), Xapian (notmuch, mu, Fastmail;
   GPL), Meilisearch's milli/charabia (tokenization incl. CJK), Lucene
   (reference), Apple Core Spotlight and Search Kit (SKIndex), Linux
   localsearch/Tracker. Which can be embedded in a Rust core on both
   macOS and Linux?
3. How existing clients implement search internally: Apple Mail (Spotlight
   and its Envelope Index), Thunderbird (Gloda, SQLite FTS), notmuch and mu
   (Xapian), Fastmail (Xapian in Cyrus), Gmail, Superhuman, Mimestream,
   MailMate, Spark.
4. Query language: Gmail operators (from:, to:, subject:, has:attachment,
   filename:, larger:, older_than:, label:, is:unread), notmuch syntax,
   MailMate's, Apple Mail's tokens. Design a query language for Sweetbriar
   that is easy for novices and powerful for experts (structured tokens in
   the search field + a text syntax), and natural-language-to-query with an
   on-device model.
5. Semantic search on device: embedding models (Apple NLContextualEmbedding,
   Apple Foundation Models framework on macOS 26, small open models such as
   bge-small, gte, nomic-embed, EmbeddingGemma run via candle/ort/MLX),
   vector indexes (sqlite-vec, usearch, hnsw_rs, LanceDB, Tantivy vector
   support if any), hybrid ranking (BM25 + vectors, reciprocal rank fusion),
   cost (indexing time for 1M messages, disk, battery), privacy.
6. Attachment text extraction: PDFKit, Vision OCR, Spotlight importers on
   macOS; Rust crates for PDF/Office text on Linux.
7. Server-side search: IMAP SEARCH with ESEARCH, FUZZY (RFC 6203), Gmail
   X-GM-RAW and Gmail API q=, Microsoft Graph $search, JMAP Email/query.
8. Recommend a search architecture for Sweetbriar: engine(s), indexed
   fields, update pipeline and consistency with the message store,
   rebuildability, ranking, and how the UI presents results (instant
   results, facets, highlighting, scoped search).
