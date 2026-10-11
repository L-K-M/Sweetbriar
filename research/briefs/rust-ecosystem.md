# Topic: Existing email engines and the Rust email ecosystem

Survey what can be reused and what Sweetbriar must build. Questions:

1. Rust crates, per layer. For each crate give: purpose, maintainer,
   license, latest release and activity as of 2026, RFC coverage,
   async model, fuzzing/test status, notable users, known problems.
   - IMAP: imap-codec / imap-types / imap-next (duesee), async-imap, imap
     (jonhoo), melib's IMAP, any others. Which support IMAP4rev2 (RFC 9051),
     CONDSTORE/QRESYNC (RFC 7162), IDLE, NOTIFY, COMPRESS, MOVE, UIDPLUS,
     OBJECTID, BINARY, UTF8=ACCEPT, ID, XOAUTH2/OAUTHBEARER?
   - SMTP: lettre, mail-send.
   - MIME parsing: mail-parser (Stalwart), mailparse, melib, others. Charset
     handling (encoding_rs), malformed-mail robustness, TNEF (winmail.dat).
   - MIME building: mail-builder, lettre.
   - Authentication results: mail-auth (DKIM, ARC, SPF, DMARC, BIMI?).
   - JMAP: jmap-client (Stalwart) and others.
   - OAuth2 (oauth2 crate), TLS (rustls vs platform TLS such as
     Security.framework via native-tls; certificate pinning, client certs),
     DNS (hickory-dns) for SRV/MX/autoconfig.
   - HTML: ammonia, html5ever, lol_html for sanitizing/rewriting mail HTML;
     html2text.
   - Sieve (sieve-rs), iCalendar, vCard, OpenPGP (rpgp / Sequoia), S/MIME.
   - Storage/search: rusqlite, sqlx, Diesel, Tantivy, Maildir crates.
2. Complete engines and clients, and what can be learned or reused:
   Delta Chat / chatmail core (Rust), meli/melib (Rust), pimalaya
   (email-lib, himalaya), Stalwart (server, but its client-side crates),
   Thunderbird's Rust components (EWS client and XML crates), Mailspring's
   Mailsync (C++, open source), Geary's engine (Vala), Evolution Data
   Server, notmuch, mu, aerc (Go), K-9 Mail / Thunderbird for Android,
   Apple Mail's local store (Envelope Index SQLite + .emlx). For each: data
   model, sync strategy, storage, strengths, weaknesses, license.
3. Licensing: Sweetbriar may be sold commercially or open-sourced; which
   choices (GPL-3 melib, Proton Bridge GPL-3, Xapian GPL, permissive
   Stalwart crates, etc.) constrain that? Flag every copyleft dependency.
4. Recommend a crate per layer, and list the gaps Sweetbriar must build
   itself (e.g. a robust sync engine, local store, threading).
