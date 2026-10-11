# Topic: Guaranteeing data safety in an email client

"Data safety" means: never lose or corrupt mail, drafts, outgoing messages,
settings or credentials; never delete on the server by mistake; never send
twice or not at all without telling the user; and protect users from
malicious mail. Questions:

1. Real incidents. Find documented cases where email clients lost or
   corrupted data or deleted mail on servers (e.g. Thunderbird folder
   compaction bugs, Apple Mail index rebuilds or vanished messages, Outlook
   PST/OST corruption, Eudora mbox damage, Airmail/Spark/Mailspring bugs,
   Proton Bridge cache issues, Gmail IMAP delete semantics, sync loops that
   propagated deletions). What was the root cause, and what design would
   have prevented it?
2. Sync engine correctness: a local operation log (intent queue) replayed
   idempotently; MOVE vs COPY + STORE \Deleted + UID EXPUNGE (UIDPLUS);
   never a bare EXPUNGE; UIDVALIDITY changes; tombstones; conflict
   resolution; a circuit breaker when the server suddenly reports most
   messages gone; Gmail label semantics; offline operations; crash in the
   middle of an operation.
3. Local durability on macOS and Linux: SQLite WAL and synchronous
   settings, F_FULLFSYNC and SQLite's fullfsync pragmas, atomic file writes
   (temp file + rename + fsync of directory), APFS behaviour, crash and
   power-loss testing, safe schema migrations (backup first), detecting
   corruption (checksums), self-healing (rebuilding indexes from raw
   messages), Time Machine friendliness (exclude rebuildable caches),
   pitfalls of cloud-synced folders.
4. Storage format the user owns: should raw messages be stored as files
   (Maildir/.eml) so they stay readable without the app? Export/import
   (mbox, Maildir, EML, Apple Mail .emlx, Thunderbird, Outlook PST).
5. Drafts and outbox: autosave strategy, undo send, a durable outbox with
   retries, preventing duplicate sends (Message-ID, Sent-folder APPEND
   de-duplication, SMTP failure after DATA), large attachments.
6. Security: credentials and OAuth tokens in the Keychain / Secret Service,
   encryption at rest (FileVault vs app-level, SQLCipher), HTML mail
   (WKWebView with JavaScript disabled, content rules, remote content
   blocking and proxying, tracking pixels, CSS-based exfiltration, EFAIL
   lessons), attachment quarantine (com.apple.quarantine), link spoofing
   warnings, phishing signals (DKIM/DMARC/BIMI), App Sandbox and hardened
   runtime, parser robustness (fuzzing), Rust memory-safety benefits and
   limits.
7. Testing for correctness: deterministic simulation testing (FoundationDB,
   TigerBeetle, Antithesis) applied to a sync engine; model-based and
   property testing (proptest) against an in-memory IMAP model; conformance
   tests against real servers (Dovecot, Cyrus, Stalwart, Gmail, Microsoft
   365, iCloud, Fastmail); fault injection (connection drop mid-APPEND,
   disk full, kill -9).
8. Recommend a concrete data-safety design for Sweetbriar: invariants it
   must guarantee, the mechanisms that enforce each, and how to test each.
