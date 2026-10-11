# Topic: Making an email client fast and responsive

Questions:

1. Latency budgets. What response times feel instant (e.g. Superhuman's
   100 ms rule, RAIL, Apple HIG guidance)? Propose concrete budgets for
   Sweetbriar: cold launch, switching mailbox, opening a message, opening a
   50-message thread, archiving/deleting, search-as-you-type, compose window
   open, scrolling a 500k-message mailbox.
2. Why email clients feel slow. Catalog real causes with examples (sync
   blocking UI, fetching bodies on open, HTML layout, huge mailboxes,
   threading recomputation, search, attachment previews, main-thread
   database access, IMAP round trips, Gmail label explosion).
3. Sync protocols and extensions that matter for speed, with server
   support (Gmail, iCloud, Fastmail, Microsoft 365, Yahoo, Dovecot, Cyrus,
   Stalwart): IMAP4rev2 (RFC 9051), CONDSTORE/QRESYNC (RFC 7162), IDLE,
   NOTIFY (RFC 5465), COMPRESS=DEFLATE (RFC 4978), MOVE, UIDPLUS, ESEARCH,
   LIST-STATUS, SPECIAL-USE, OBJECTID (RFC 8474), BINARY, PREVIEW
   (RFC 8970), PARTIAL (RFC 9394), LITERAL+, pipelining and multiple
   connections. JMAP (RFC 8620/8621) with push. Gmail API history.list.
   Microsoft Graph delta queries.
4. Sync strategy: headers first and bodies on demand vs full offline copy;
   prefetch heuristics; priority queues so user-visible work runs first;
   background scheduling, energy use, macOS QoS classes and App Nap.
5. Local store design for millions of messages: SQLite schema and indexes
   (covering indexes, WITHOUT ROWID, denormalized thread/summary tables),
   WAL, mmap, page size, one writer plus reader pool; raw RFC 5322 blobs on
   disk vs in the database; compression (zstd); deduplicating a message that
   appears in several folders/labels. Cite real numbers.
6. Threading at scale: JWZ algorithm, incremental threading, Gmail
   X-GM-THRID, JMAP threadId, Graph conversationId.
7. UI performance on macOS 26: NSTableView with cell reuse vs SwiftUI
   List/Table/LazyVStack with very large data sets (cite measurements and
   known problems), keeping the main thread free, applying diffs, preloading
   the next messages, WKWebView cost (process launch, prewarming, reuse
   pools), a plain-text fast path, image decoding off the main thread,
   launch time, memory.
8. How fast clients achieve speed: Superhuman, Mimestream, Apple Mail,
   MailMate, Spark, Mailspring, mutt/notmuch. Cite engineering blogs and
   talks.
9. Measurement: os_signpost and Instruments, performance tests in CI,
   synthetic and public mail corpora (e.g. Enron) to test 1M+ messages.
10. Give a prioritized list of design decisions that most affect perceived
    speed, and what must be decided on day one because it is hard to change
    later.
