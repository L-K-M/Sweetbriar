<!-- provenance: helper=codex model=gpt-6.1-sol started=2026-10-12T07:11:32+00:00 exit=124 brief=research/briefs/ -->

# Sweetbriar: first-milestone crates and public-domain licensing

**Research cutoff: 2026-10-12, UTC.** This report revisits `research/01-architecture.md`, `02-rust-ecosystem.md`, `05-search.md`, `13-database-engine.md` and decision D13. Recommendations are judgments; registry entries, published source and licence terms are cited as facts. Publication dates below are crates.io dates, which can differ from upstream tag dates.

I fetched live crates.io metadata, downloaded the published crate archives and verified their registry SHA-256 checksums, read versioned docs.rs documentation/source, and compared relevant upstream files. Captures and provenance are in `evidence/`. “MSRV undeclared” means the published manifest supplies no `rust-version`; the actual minimum is **unverified**, rather than whatever compiler docs.rs happened to use. Rust’s documentation describes the distinction between a declared requirement and dependency compatibility. No Sweetbriar dependency graph, macOS build or live account was available for integration testing. [Cargo MSRV reference](https://doc.rust-lang.org/cargo/reference/rust-version.html).

## 1. TL;DR

- **Use `rusqlite` 0.40.2 with `bundled`.** Current `libsqlite3-sys` 0.38.2 contains **SQLite 3.53.2**, satisfying the 3.51.3 WAL-fix floor and avoiding withdrawn 3.52.0. It does **not** contain latest SQLite 3.54.0, released 2026-10-09. FTS5 is already compiled into the bundle. [Published wrapper](https://docs.rs/crate/rusqlite/0.40.2), [build script](https://docs.rs/crate/libsqlite3-sys/0.38.2/source/build.rs), [SQLite release](https://www.sqlite.org/releaselog/3_54_0.html).
- **Use `mail-parser` 0.11.9 and `mail-builder` 1.0.0**, with bounded parsing and preserved originals. The snapshot conflates published `mail-parser` with its unpublished 1.0 branch: Rust 1.98 is declared on that branch, not 0.11.9. [Published manifest](https://docs.rs/crate/mail-parser/0.11.9/source/Cargo.toml), [upstream manifest](https://raw.githubusercontent.com/stalwartlabs/mail-parser/main/Cargo.toml).
- **Correct the mbox dependency.** `mbox` 0.7.1 is a malloc-pointer wrapper. `mail-parser` already supplies an mbox iterator; wrap it with Sweetbriar’s import accounting and format detection. [mbox](https://docs.rs/crate/mbox/0.7.1), [mailbox parser](https://docs.rs/crate/mail-parser/0.11.9/source/src/mailbox/mbox.rs).
- **Prefer a bounded `io-imap` 0.7.1 evaluation for the first backend**, using its Tokio transport example and typed UID receipts. Its FETCH wrapper drops `VANISHED`, so complete resync needs a fix or a qualified alternative path. `async-imap` 0.12.0 is the strongest bootstrap alternative, but its public APPEND API returns no APPENDUID and it lacks a complete QRESYNC API. [io-imap](https://docs.rs/crate/io-imap/0.7.1), [async-imap session](https://docs.rs/async-imap/0.12.0/async_imap/struct.Session.html).
- **Use rustls with platform verification and account-scoped Bridge trust.** Proton documents a self-signed certificate on loopback; certificate exceptions must not disable verification for other accounts. [Proton](https://proton.me/support/bridge-ssl-connection-issue), [platform verifier](https://docs.rs/crate/rustls-platform-verifier/0.7.1).
- **UniFFI 0.32.2 remains current and MPL-2.0.** The Swift 6 async callback fix merged on 2026-07-15 is absent from its published templates. Compile the actual Swift exports before choosing a patch or avoiding that callback shape. [PR #2943](https://github.com/mozilla/uniffi-rs/pull/2943), [published template](https://docs.rs/crate/uniffi_bindgen/0.32.2/source/src/bindings/swift/templates/Async.swift).
- **Select the macOS credential store explicitly.** `keyring` 4.2.0 defaults to the legacy keychain feature. Its separate Apple store has a `protected` module for the data-protection keychain. [keyring manifest](https://docs.rs/crate/keyring/4.2.0/source/Cargo.toml), [Apple store](https://docs.rs/crate/apple-native-keyring-store/1.0.2).
- **Enforce adjacent layers inside crates with a project-owned `syn` checker and a restricted source policy.** Cargo graph checks cannot do this. Semantic alternatives now exist, but re-export ownership and macro coverage require explicit qualification. [cargo-modules](https://docs.rs/crate/cargo-modules/0.27.0), [Cargo Bylaw analysis](https://raw.githubusercontent.com/danielgerlag/cargo-bylaw/main/docs/analysis.md).
- **Recommendation: offer the owner’s code as `Unlicense OR 0BSD`.** This combines an explicit public-domain intention with an unconditional, OSI-approved software licence. CC0 has the stronger waiver/fallback drafting, but expressly excludes patents and has downstream policy obstacles. [Unlicense](https://opensource.org/license/unlicense), [0BSD](https://opensource.org/license/0bsd), [CC0 legal code](https://creativecommons.org/publicdomain/zero/1.0/legalcode.en).
- **The distributed app will still have third-party obligations.** Generate notices, make MPL-covered source available, preserve Linux LGPL replacement/relinking rights, and exclude GPL/AGPL from linked runtime dependencies. Public-domain status of the owner’s files does not erase those obligations. [MPL §3](https://www.mozilla.org/en-US/MPL/2.0/), [LGPL §6](https://www.gnu.org/licenses/old-licenses/lgpl-2.1.html).

## 2. Findings

### Part A — crates for the first milestones

### A1. SQLite, FTS5, flags and migrations

| Component | Current publication | Release date | Licence | MSRV |
|---|---|---|---|---|
| [rusqlite](https://docs.rs/crate/rusqlite/0.40.2) | 0.40.2 | 2026-08-08 | MIT | Undeclared; documented policy: latest stable Rust at release |
| [libsqlite3-sys](https://docs.rs/crate/libsqlite3-sys/0.38.2) | 0.38.2 | 2026-08-08 | MIT wrapper; bundled SQLite public domain | Undeclared |
| [rusqlite_migration](https://docs.rs/crate/rusqlite_migration/2.6.0) | 2.6.0 | 2026-05-28 | Apache-2.0 | 1.95 |
| [sqlx](https://docs.rs/crate/sqlx/0.9.0) | 0.9.0 | 2026-05-21 | MIT OR Apache-2.0 | 1.94.0 |

**Verified bundle:** `sqlite3.h` in the checksum-verified `libsqlite3-sys` 0.38.2 archive defines `SQLITE_VERSION "3.53.2"`; rusqlite’s published documentation independently identifies the same bundle. The snapshot’s 0.38.1 reference should become **0.38.2**, although the embedded SQLite version remains 3.53.2. [Published bundle documentation](https://docs.rs/crate/rusqlite/0.40.2).

SQLite identifies the WAL-reset fix in **3.51.3, released 2026-03-13**, with additional backports for some older branches. **3.52.0 is explicitly withdrawn.** Latest upstream is **3.54.0, 2026-10-09**; later 3.53 patch releases also contain fixes absent from the current Rust bundle. Meeting the WAL-fix floor therefore establishes one necessary safety condition, not blanket currency or safety. [WAL-reset explanation](https://www.sqlite.org/wal.html#walreset), [withdrawal](https://www.sqlite.org/releaselog/3_52_0.html), [3.53.4 fixes](https://www.sqlite.org/releaselog/3_53_4.html), [3.54.0](https://www.sqlite.org/releaselog/3_54_0.html).

**FTS5 needs no additional Cargo feature.** The bundled build script already sets `SQLITE_ENABLE_FTS5`, `SQLITE_DEFAULT_FOREIGN_KEYS=1` and `SQLITE_THREADSAFE=1`. Rusqlite’s `vtab` feature exposes Rust virtual-table facilities; it is not the switch that enables built-in FTS5. [Build script](https://docs.rs/crate/libsqlite3-sys/0.38.2/source/build.rs), [rusqlite features](https://docs.rs/crate/rusqlite/0.40.2).

Suggested reproducible configuration:

```toml
# Cargo.toml
[dependencies]
rusqlite = { version = "=0.40.2", features = ["bundled", "backup", "limits"] }
rusqlite_migration = "=2.6.0"

# .cargo/config.toml
[env]
LIBSQLITE3_FLAGS = { value = "SQLITE_DQS=0 SQLITE_DEFAULT_WAL_SYNCHRONOUS=2 SQLITE_STRICT_SUBTYPE=1", force = true }
```

`LIBSQLITE3_FLAGS` is whitespace-separated and accepts `-D`, `-U`, or bare `SQLITE_...` definitions. It affects a bundled compilation; it neither changes the bundled version nor configures an external system SQLite. The build script tracks changes to this variable. Keep the selected `libsqlite3-sys` version/checksum in the lockfile and record the actual compile options. [Flag handling](https://docs.rs/crate/libsqlite3-sys/0.38.2/source/build.rs), [SQLite compile options](https://www.sqlite.org/compile.html).

**Recommendation:** keep the dedicated store worker from the architecture snapshot. At open, assert `sqlite_version()`, `sqlite_source_id()`, foreign keys and synchronous settings; in CI, actually create, populate and query an FTS5 table. Set WAL and durability pragmas explicitly, including `synchronous=FULL` and the macOS flush/checkpoint settings selected by the database study. [SQLite pragmas](https://www.sqlite.org/pragma.html). Review changes after 3.53.2 before the real-data milestone; if a newer amalgamation is required, use an audited local package patch with recorded upstream hashes and matching bindings. Do not silently inherit the host’s SQLite.

Rusqlite’s moving MSRV policy implies the **1.97 release series** for its August publication; the official Rust release history places 1.97.1 on 2026-07-16. This is a support-policy inference, not a tested numerical minimum. The current stable compiler on the research date is **Rust 1.99.0, released 2026-10-01**. Pin that toolchain initially, then establish Sweetbriar’s own MSRV by building its actual locked feature graph. [Rust release history](https://blog.rust-lang.org/releases/), [1.99.0 announcement](https://blog.rust-lang.org/2026/10/01/Rust-1.99.0/).

**Migrations:** `rusqlite_migration` uses `PRAGMA user_version`, transactional upgrades, optional down migrations and Rust hooks; its current dependency line accommodates rusqlite 0.40. It is a small, recently released, single-project dependency rather than a backup system. Use it for database-only steps, with immutable numbered SQL files, a Sweetbriar checksum ledger and pre-upgrade recovery snapshots. External blob moves need a separately journalled, restartable operation; a SQLite transaction cannot make filesystem changes atomic. [Published migration source](https://docs.rs/crate/rusqlite_migration/2.6.0/source/src/lib.rs).

**Strongest alternatives:** SQLx offers an async-facing database abstraction and migration system, attractive if several database backends become necessary. For the chosen single SQLite writer, it adds an abstraction Sweetbriar currently does not need. An owned migration runner is stronger when upgrades become multi-stage store transformations, but then Sweetbriar owns ordering, rollback and compatibility correctness for decades.

### A2. MIME, composition, EML and mbox

| Component | Current publication | Release date | Licence | MSRV |
|---|---|---|---|---|
| [mail-parser](https://docs.rs/crate/mail-parser/0.11.9) | 0.11.9 | 2026-09-09 | Apache-2.0 OR MIT | Undeclared |
| [mail-builder](https://docs.rs/crate/mail-builder/1.0.0) | 1.0.0 | 2026-09-12 | Apache-2.0 OR MIT | Undeclared |
| [mailparse](https://docs.rs/crate/mailparse/0.18.0) | 0.18.0 | 2026-10-05 | 0BSD | 1.85.0 |
| [mbox-reader](https://docs.rs/crate/mbox-reader/0.2.0) | 0.2.0 | 2019-09-27 | MIT OR Apache-2.0 | Undeclared |
| [mbox](https://docs.rs/crate/mbox/0.7.1) | 0.7.1 | 2024-03-08 | MIT | 1.36.0; unrelated to mail |

Published `mail-parser` 0.11.9 has edition 2024 but no declared MSRV. Upstream `main` identifies an **unpublished 1.0.0**, declares Rust **1.98**, and changes dependencies and parsing internals. Consequently, the snapshot’s “0.11.9 requires 1.98” and SIMD wording must not be imported into a milestone manifest as verified facts about the published release. [Published manifest](https://docs.rs/crate/mail-parser/0.11.9/source/Cargo.toml), [development manifest](https://raw.githubusercontent.com/stalwartlabs/mail-parser/main/Cargo.toml).

The published parser documents RFC 5322/MIME parsing, a JMAP-oriented body representation, and 41 character sets, with `full_encoding` enabling additional multibyte encodings through `encoding_rs`. Its fuzzing, Miri and production-use claims are upstream claims, not an independent Sweetbriar benchmark. `mail-builder` provides the separate MIME composition layer. Both have September publications; the ongoing 1.0 parser work demonstrates development while also increasing upgrade risk. [Parser README](https://docs.rs/crate/mail-parser/0.11.9), [builder](https://docs.rs/crate/mail-builder/1.0.0).

**Security qualification:** Stalwart’s advisory, published **2026-02-14**, describes cyclic MIME references from `mail-parser` and a consumer traversal that exhausted memory. The stated fixed version is **Stalwart server 0.15.5**, not a specific mail-parser crate release. A “safe mail-parser version” inferred from that server number remains **unverified**. Bound bytes, MIME depth/part count and traversal work; maintain a visited-part set when following references. [Upstream advisory](https://github.com/stalwartlabs/stalwart/security/advisories/GHSA-jm95-876q-c9gw).

**EML:** treat an individual message as raw RFC message bytes, not necessarily UTF-8 text. [Internet Message Format](https://www.rfc-editor.org/rfc/rfc5322.html). Preserve the original byte stream before parsing and retain recoverable failures in the import inventory. Use `mail-parser` for interpretation and `mail-builder` for newly composed messages, rather than parsing and rebuilding imported mail as an archival operation.

**mbox:** `mail-parser::mailbox::mbox::MessageIterator` already reads a `BufRead` stream and implements qmail-style `>From ` unquoting. Source inspection shows a new message begins at any line starting `From `, and each message is accumulated in a `Vec<u8>`. It therefore needs byte limits and format-specific reconciliation; it does not establish universal mboxcl/mboxcl2 support or retain raw source offsets for Sweetbriar. [Published iterator](https://docs.rs/crate/mail-parser/0.11.9/source/src/mailbox/mbox.rs). RFC 4155 documents substantial mbox variation rather than one universally reliable delimiter convention. [RFC 4155](https://www.rfc-editor.org/rfc/rfc4155.html).

Apple’s current manual says export produces **`.mbox` packages**, not a promise that the selected path is a flat file. Historical first-hand export tests describe `Mailbox.mbox/mbox` plus a table-of-contents index; they distinguish the export from native mailbox directories containing `Messages`, attachments and `Info.plist`. Apple does not document those internals in its current export manual; the exact macOS 27 layout and escaping variant remain **unverified** pending an export fixture. Implement both flat files and package directories containing `mbox`, treating Apple indexes as optional metadata. Do not require `Info.plist` or infer message completeness from an index. [Apple export manual](https://support.apple.com/guide/mail/import-or-export-mailboxes-mlhlp1030/mac), [first-hand archive test, 2009-07-24, consulted through search cache](https://discussions.apple.com/thread/2092094).

**Recommendation:** reuse the parser’s iterator behind a Sweetbriar import wrapper initially. Preserve the input archive, record message counts/failures and validate escaping against Apple and classic-client fixtures. Build an owned splitter only when a required variant or preservation requirement is missing. **Strongest alternatives:** `mailparse` offers a simpler parser and 0BSD licence; `mbox-reader` is an actual mail-format crate but its latest publication is seven years old, making it a poor default dependency without a source/fixture audit.

### A3. IMAP for Proton Bridge, then generic accounts

| Component | Latest non-yanked publication | Release date | Licence | MSRV |
|---|---|---|---|---|
| [async-imap](https://docs.rs/crate/async-imap/0.12.0) | 0.12.0 | 2026-09-30 | MIT OR Apache-2.0 | 1.88 |
| [io-imap](https://docs.rs/crate/io-imap/0.7.1) | 0.7.1 | 2026-10-01 | MIT OR Apache-2.0 | 1.88 |
| [imap-next](https://docs.rs/crate/imap-next/0.3.4) | 0.3.4 | 2026-02-18 | MIT OR Apache-2.0 | 1.85 |
| [imap-codec](https://docs.rs/crate/imap-codec/2.0.0-alpha.9) | 2.0.0-alpha.9 | 2026-07-19 | MIT OR Apache-2.0 | 1.85 |
| [imap-types](https://docs.rs/crate/imap-types/2.0.0-alpha.7) | 2.0.0-alpha.7 | 2026-07-19 | MIT OR Apache-2.0 | 1.85 |
| [imap](https://docs.rs/crate/imap/3.0.0-alpha.15) | 3.0.0-alpha.15 | 2025-02-08 | Apache-2.0 OR MIT | Undeclared |

The codec/types latest **stable** publication is **1.0.0, 2023-08-22**; current work uses the 2.0 alpha line. The synchronous imap crate’s latest stable publication is **2.4.1, 2021-01-13**, distinct from its newer alpha API. Its README explicitly solicits maintainers. Those dates and alpha status are substantial adoption risks, not proof that all those projects are abandoned. [Codec versions](https://crates.io/api/v1/crates/imap-codec), [types versions](https://crates.io/api/v1/crates/imap-types), [imap versions](https://crates.io/api/v1/crates/imap), [imap README](https://docs.rs/crate/imap/3.0.0-alpha.15).

`async-imap` is a ready client with selectable runtimes. Its default feature is `runtime-async-std`; for Sweetbriar use `default-features = false` and `runtime-tokio`. Its generic stream boundary can accept a TLS stream constructed by Sweetbriar. It has SELECT CONDSTORE, MOVE, UID EXPUNGE, ID and IDLE APIs, but those do not equal a complete durable-sync implementation. [Manifest](https://docs.rs/crate/async-imap/0.12.0/source/Cargo.toml), [session API](https://docs.rs/async-imap/0.12.0/async_imap/struct.Session.html).

`imap-next` handles protocol flows—literals, AUTHENTICATE and IDLE—over `imap-codec`, rather than providing the sync engine. Its optional/default `stream` adapter uses Tokio; the state machine itself is sans-I/O. Enable `ext_condstore_qresync`, `ext_id` and `starttls` when required; these capabilities are not all enabled by default. [Published manifest and README](https://docs.rs/crate/imap-next/0.3.4).

`io-imap` supplies higher-level command coroutines, CONDSTORE/QRESYNC selection data, APPENDUID/COPYUID results and streaming operations. Its SELECT/EXAMINE result captures vanished UIDs, but the published FETCH wrapper keeps only `Data::Fetch`, discarding `VANISHED` even though FETCH modifiers accept it. Complete QRESYNC support is therefore **partial**, not established by the README’s RFC list. [Published FETCH wrapper](https://docs.rs/crate/io-imap/0.7.1/source/src/rfc3501/fetch.rs). Its published Tokio example implements `ImapClientAsync` over caller-owned sockets/TLS; this is an adapter integration, not an assurance that every example is a production-ready backend. It exposes returned UID information that the high-level async-imap APPEND method discards. [Tokio example](https://docs.rs/crate/io-imap/0.7.1/source/examples/tokio_session.rs), [APPEND](https://docs.rs/crate/io-imap/0.7.1/source/src/rfc3501/append.rs), [SELECT](https://docs.rs/crate/io-imap/0.7.1/source/src/rfc3501/select.rs).

The extension matrix is in section 3. **Recommendation:** evaluate `io-imap` 0.7.1 behind `AccountBackend` first, preserving Sweetbriar’s own account, UIDVALIDITY, cursor and intent semantics. Its recent publication and broader typed command coverage justify evaluation. Fix or avoid the lossy FETCH path, and prove deletion reconciliation before adoption. **Strongest counter-argument:** this is a young stack with an alpha codec and a transport adapter Sweetbriar must qualify. `async-imap` gets a narrower Bridge backend running sooner; choose it if the io-imap compatibility spike fails, with explicit reconciliation and no promise of complete QRESYNC.

A direct `imap-next` client is the strongest control-oriented alternative, but it makes Sweetbriar implement more command correlation, extension response handling and reconnect behavior. A synchronous imap client can live on a dedicated thread, but blocking IDLE and older maintenance make it a weaker primary choice.

All shortlisted crates are permissively licensed. Proton Bridge’s separate GPL program is discussed under B2; using IMAP/SMTP does not require importing its implementation. The GPL/AGPL and other copyleft traps identified by D13 remain outside the default linked dependency policy.

### A4. SMTP: lettre versus mail-send

| Component | Current publication | Release date | Licence | MSRV |
|---|---|---|---|---|
| [lettre](https://docs.rs/crate/lettre/0.11.23) | 0.11.23 | 2026-08-03 | MIT | 1.85 |
| [mail-send](https://docs.rs/crate/mail-send/0.6.2) | 0.6.2 | 2026-08-18 | Apache-2.0 OR MIT | Undeclared |

Both have recent publications and maintained upstream transport code. Lettre supports runtime/TLS feature selection, SMTP transport and raw-message submission, allowing `mail-builder` output to remain the canonical queued message. Its TLS builder supports custom roots and platform verification. Select Tokio/rustls features explicitly rather than accepting its default native-TLS stack. [lettre](https://docs.rs/crate/lettre/0.11.23), [TLS implementation](https://docs.rs/crate/lettre/0.11.23/source/src/transport/smtp/client/tls.rs).

`mail-send` uses Tokio, integrates optional MIME/DKIM facilities, supports XOAUTH2 and OAUTHBEARER, and currently uses the platform verifier. Its default features also include DKIM and older digest authentication mechanisms; disable facilities that Sweetbriar does not need. Its `allow_invalid_certs()` implementation bypasses certificate and handshake-signature checks and is unsuitable for Bridge trust. [Published crate/features](https://docs.rs/crate/mail-send/0.6.2), [TLS implementation](https://docs.rs/crate/mail-send/0.6.2/source/src/smtp/tls.rs).

**Recommendation:** lettre for submission, with `mail-builder` handling MIME. **Strongest alternative/counter-argument:** `mail-send` is a more cohesive Stalwart stack and may reduce integration work, especially if DKIM becomes a client requirement. Either transport must expose enough progress information to retain an **OutcomeUnknown** state when the connection dies around the final SMTP acceptance reply. Retrying blindly can duplicate mail; neither library is the durable outbox. SMTP’s acceptance transaction is specified by RFC 5321. [RFC 5321 §4.1.1.4](https://www.rfc-editor.org/rfc/rfc5321.html#section-4.1.1.4).

### A5. TLS and Proton Bridge trust

| Component | Current publication | Release date | Licence | MSRV |
|---|---|---|---|---|
| [rustls](https://docs.rs/crate/rustls/0.23.45) | 0.23.45 | 2026-09-14 | Apache-2.0 OR ISC OR MIT | 1.71 |
| [tokio-rustls](https://docs.rs/crate/tokio-rustls/0.26.6) | 0.26.6 | 2026-09-28 | MIT OR Apache-2.0 | 1.71 |
| [rustls-platform-verifier](https://docs.rs/crate/rustls-platform-verifier/0.7.1) | 0.7.1 | 2026-09-24 | MIT OR Apache-2.0 | 1.85 |
| [native-tls](https://docs.rs/crate/native-tls/0.2.18) | 0.2.18 | 2026-02-18 | MIT OR Apache-2.0 | 1.80 |

All three rustls components have September publications. The platform verifier uses Security.framework and macOS trust settings on Apple platforms; Linux uses discovered CA bundles with webpki, not an identical OS verification service. Its documented Linux fallback lacks the macOS revocation behavior. [Platform behavior](https://docs.rs/crate/rustls-platform-verifier/0.7.1).

Proton documents loopback **127.0.0.1**, STARTTLS/SSL options and a self-signed certificate generated by Bridge. [Bridge TLS documentation](https://proton.me/support/bridge-ssl-connection-issue).

**Recommendation:** two explicit trust policies. Ordinary accounts use platform verification. A Bridge account uses an imported/confirmed certificate scoped to that account and loopback endpoint. Prefer normal validation with an additional trusted certificate; verify the actual Bridge certificate’s names, validity and constraints before assuming that suffices. If a pin-aware custom verifier is needed, retain cryptographic handshake verification and reject an unexpected certificate. Treat certificate rotation as a visible trust change.

Do not elevate the Bridge certificate into a global account trust exception or use a verifier that accepts everything. Cover a wrong pin, wrong endpoint, missing STARTTLS and certificate rotation in integration tests. **Strongest alternative:** `native-tls` follows platform TLS facilities directly and can simplify particular enterprise integrations, but Linux OpenSSL and platform differences increase build/licence variation.

The crypto provider also has its own licences. Current `ring` **0.17.14, 2025-03-11**, declares **Apache-2.0 AND ISC**, MSRV **1.66.0**; its published licence says the previously sourced OpenSSL code is now Apache-2.0. Thus old instructions to allow `OpenSSL` automatically for current ring are stale. Rustls’s default AWS-LC provider has a broader native-source inventory that must also appear in notices. [ring metadata](https://crates.io/api/v1/crates/ring), [published licence](https://docs.rs/crate/ring/0.17.14/source/LICENSE), [AWS-LC native metadata](https://crates.io/api/v1/crates/aws-lc-sys).

### A6. Tokio and UniFFI build workflow

| Component | Current publication | Release date | Licence | MSRV |
|---|---|---|---|---|
| [Tokio](https://docs.rs/crate/tokio/1.53.2) | 1.53.2 | 2026-10-03 | MIT | 1.71 |
| [uniffi](https://docs.rs/crate/uniffi/0.32.2) | 0.32.2 | 2026-09-23 | MPL-2.0 | Undeclared |
| [uniffi_bindgen](https://docs.rs/crate/uniffi_bindgen/0.32.2) | 0.32.2 | 2026-09-23 | MPL-2.0 | Undeclared |

Tokio has a current October release; UniFFI has a September release and ongoing upstream development. For both, compatibility of the complete selected dependency graph still needs a build. **Recommendation:** one engine-owned Tokio runtime, bounded jobs, and a dedicated blocking SQLite worker. FFI methods dispatch to that runtime instead of creating a runtime per call or blocking the UI thread.

**Swift 6 correction confirmed:** PR #2943 merged on **2026-07-15**, adding `@Sendable` closures and special handling of captured RustBuffer values for async foreign callbacks. Published **0.32.2** still has the earlier closure signatures in `Async.swift` and `CallbackInterfaceImpl.swift`. Merge date precedes publication date but does not establish release inclusion. [PR/diff](https://github.com/mozilla/uniffi-rs/pull/2943), [published async template](https://docs.rs/crate/uniffi_bindgen/0.32.2/source/src/bindings/swift/templates/Async.swift), [callback template](https://docs.rs/crate/uniffi_bindgen/0.32.2/source/src/bindings/swift/templates/CallbackInterfaceImpl.swift).

**Workflow:**

1. Put proc-macro exports in the small `sb-ffi` façade; call `uniffi::setup_scaffolding!()`. This route needs no UDL scaffolding `build.rs`.
2. Build a host `cdylib` for metadata extraction and the macOS `staticlib` for packaging. A separate local bindgen executable calls `uniffi::uniffi_bindgen_main()` with the `cli` feature and the exact same UniFFI version.
3. Run bindgen against the built library, then compile generated Swift, C header/module map and native adapter together. Package the static library and headers as an XCFramework.
4. If UDL is chosen instead, use `uniffi`’s `build` feature to generate scaffolding in `build.rs`, then `include_scaffolding!()`; do not combine that with `setup_scaffolding!()` in the same crate.

For a proposed `sb-bindgen` helper on macOS, the generation command is:

```sh
cargo run -p sb-bindgen -- generate --library target/release/libsb_ffi.dylib --language swift --out-dir generated/swift
```

These are the documented scaffolding/bindgen mechanisms; XCFramework packaging is Sweetbriar’s build recommendation. Use the **0.32.2 source documentation** when implementing, because the live manual contains newer CLI guidance. [Scaffolding](https://mozilla.github.io/uniffi-rs/latest/tutorial/Rust_scaffolding.html), [version-pinned binding instructions](https://raw.githubusercontent.com/mozilla/uniffi-rs/7bc621ee9d0d15371d792612707ba5ed4111f950/docs/manual/src/tutorial/foreign_language_bindings.md).

Prefer avoiding async foreign callback traits in the first façade if commands plus pull-based subscriptions suffice. Otherwise backport the reviewed template fix and record the MPL patch/source. UniFFI’s guide calls for an explicit application cancellation channel; cancelling a Swift waiter must not revoke an already accepted durable command. [Async/cancellation guide](https://mozilla.github.io/uniffi-rs/latest/futures.html).

**Strongest alternative/counter-argument:** a small C ABI gives complete control and avoids the UniFFI runtime dependency, but shifts lifetime, error, async and Swift glue correctness to Sweetbriar. Use it if the qualified UniFFI façade requires substantial recurring patches, not merely because its licence is MPL.

### A7. Secrets: keyring, platform code and secrecy

| Component | Current publication | Release date | Licence | MSRV |
|---|---|---|---|---|
| [keyring](https://docs.rs/crate/keyring/4.2.0) | 4.2.0 | 2026-08-29 | MIT OR Apache-2.0 | 1.88.0 |
| [keyring-core](https://docs.rs/crate/keyring-core/1.0.0) | 1.0.0 | 2026-04-21 | MIT OR Apache-2.0 | 1.85 |
| [apple-native-keyring-store](https://docs.rs/crate/apple-native-keyring-store/1.0.2) | 1.0.2 | 2026-08-06 | MIT OR Apache-2.0 | 1.85 |
| [zbus-secret-service-keyring-store](https://docs.rs/crate/zbus-secret-service-keyring-store/1.0.1) | 1.0.1 | 2026-08-15 | MIT OR Apache-2.0 | 1.88 |
| [secrecy](https://docs.rs/crate/secrecy/0.10.3) | 0.10.3 | 2024-10-09 | Apache-2.0 OR MIT | 1.60 |

The keyring ecosystem now separates API and stores. `keyring` 4.2.0’s default `v1` feature selects `apple-native-keyring-store/keychain`, rather than the `protected` store. The maintainer recommends depending on `keyring-core` plus the specific stores for advanced applications; enabling its broad `cli` feature is not the application integration path. [Published manifest](https://docs.rs/crate/keyring/4.2.0/source/Cargo.toml), [maintainer guidance](https://docs.rs/crate/keyring/4.2.0).

Apple TN3137, updated **2026-09-24**, distinguishes file-based and data-protection keychains. On macOS, SecItem defaults to the file-based implementation unless directed otherwise; data-protection selection uses `kSecUseDataProtectionKeychain`. Apple recommends SecItem and generally the data-protection implementation. It describes the old implementation as moving toward deprecation, not universally already deprecated. [TN3137](https://developer.apple.com/documentation/technotes/tn3137-on-mac-keychains).

**Recommendation:** a Sweetbriar `SecretStore` interface backed by `keyring-core` and Apple’s explicit `protected` store on macOS, with a Secret Service store on Linux. Qualify code signing, provisioning/access groups and item accessibility in an actual app bundle; unsigned CLI tests do not prove this works. The Apple store documents protected-store entitlement requirements and error **-34018** when they are missing. [Apple store documentation](https://docs.rs/crate/apple-native-keyring-store/1.0.2).

Linux Secret Service is a D-Bus service supplied by the desktop/session, with locked collections and prompts; it is not guaranteed to exist in a headless test environment. Preserve `Locked`, `Unavailable` and `NotFound` separately and provide a fake store for ordinary tests. [Secret Service specification](https://specifications.freedesktop.org/secret-service-spec/latest/).

Use `secrecy` for in-memory tokens/passwords, explicit exposure and zeroization-oriented handling. Its last publication is older, but small stable functionality is not evidence of abandonment. It is not persistent storage and cannot stop every copy made by FFI or native frameworks. [secrecy](https://docs.rs/crate/secrecy/0.10.3).

**Strongest alternative:** a small Swift SecItem adapter and direct Linux Secret Service integration give tighter platform control and native prompts, at the cost of additional FFI/error paths. Use that if the protected-store crate fails the signing/accessibility requirements; do not silently fall back to plaintext settings.

### A8. Testing, real servers and fault injection

| Component | Current publication | Release date | Licence | MSRV |
|---|---|---|---|---|
| [proptest](https://docs.rs/crate/proptest/1.12.0) | 1.12.0 | 2026-10-10 | MIT OR Apache-2.0 | 1.88 |
| [insta](https://docs.rs/crate/insta/1.49.0) | 1.49.0 | 2026-10-03 | Apache-2.0 | 1.66.0 |
| [fail](https://docs.rs/crate/fail/0.5.1) | 0.5.1 | 2022-10-08 | Apache-2.0 | Undeclared |

Proptest and insta have current October releases. Use proptest for import splitting, UID set handling, cursor monotonicity and operation-state invariants; it supplies generated cases and shrinking. Use insta for stable, normalized MIME/search/import projections, not snapshots containing live secrets or unstable timestamps. [Proptest guide](https://proptest-rs.github.io/proptest/intro.html), [insta guide](https://insta.rs/docs/).

**Recommendation:** three complementary server layers. An in-process scripted peer exercises fragmented input, interleaved untagged responses, EOF and malformed literals deterministically. Dovecot in Docker exercises real IMAP extension semantics. A controlled Bridge integration lane verifies the actual target, certificate and capability behavior. Pin server image tags **and digests**; record CAPABILITY, rather than assuming an extension merely because Dovecot supports it.

Dovecot’s latest upstream release is **2.4.5, published 2026-08-28**; the core licence inventory includes LGPL-2.1 and MIT. Keep this server as a separate CI service. [Upstream release metadata](https://api.github.com/repos/dovecot/core/releases/latest), [licence inventory](https://raw.githubusercontent.com/dovecot/core/main/COPYING). Its current Docker documentation uses rootless images, documents nonstandard IMAP/IMAPS ports **31143/31993**, and requires explicit authentication configuration for test images. Copying old `143:143` recipes may therefore test the wrong endpoint. [Dovecot Docker documentation](https://doc.dovecot.org/main/installation/docker.html).

**GreenMail** is Apache-2.0 and its site lists **2.1.14** as current stable on 2026-10-12, with the upstream release published **2026-09-19**; [release metadata](https://api.github.com/repos/greenmail-mail-test/greenmail/releases/latest). It supplies SMTP/POP3/IMAP test services, but that does not establish complete CONDSTORE/QRESYNC coverage. It is the strongest convenient alternative for basic protocol tests, especially SMTP, while adding a Java service/runtime. [GreenMail](https://greenmail-mail-test.github.io/greenmail/).

`fail` provides named failpoints but has had no publication since 2022. Prefer project-owned injectable file/store/clock interfaces; optionally use `fail` behind a test-only feature for named interruption points. It cannot itself simulate torn storage writes or prove power-loss safety. [fail-rs](https://raw.githubusercontent.com/tikv/fail-rs/master/README.md).

The milestone fault suite should inject ENOSPC, short writes, failed flush/rename, process death between publication/commit steps, interrupted migration, and lost SMTP acknowledgement. Assert that every durably acknowledged local intent survives restart. **Strongest counter-argument:** fakes can encode the same mistaken assumptions as the implementation; they require the real-server and recovery lanes. The precise power-loss model remains an acceptance question, not a property a testing crate guarantees.

### A9. Enforcing the strict adjacent-layer rule

| Tool | Current publication/date | Licence / MSRV | What it can establish |
|---|---|---|---|
| [cargo-modules](https://docs.rs/crate/cargo-modules/0.27.0) | 0.27.0 / 2026-08-03 | MPL-2.0 / 1.95.0 | Module/dependency visualization; cycles and orphan checks |
| [syn](https://docs.rs/crate/syn/3.0.8) | 3.0.8 / 2026-10-11 | MIT OR Apache-2.0 / 1.71 | Syntax infrastructure for an owned checker |
| [cargo-bylaw](https://docs.rs/crate/cargo-bylaw/0.1.0) | 0.1.0 / 2026-08-14 | MIT OR Apache-2.0 / 1.95 | Semantic dependency rules using embedded rust-analyzer |
| [archunit](https://docs.rs/crate/archunit/0.0.2) | 0.0.2 / 2026-09-30 | MIT / 1.85 | File-level architecture rules with syntax extraction |
| [rust_arkitect](https://docs.rs/crate/rust_arkitect/0.3.7) | 0.3.7 / 2025-01-25 | MIT / undeclared | Rust architecture-rule library; needs coverage qualification |

The latest publications show activity in cargo-modules/syn and very young releases in Bylaw/ArchUnitRust; maturity of the latter tools remains unproven. **`cargo-deny` checks crate graphs, not intra-crate layers.** An acyclic module graph also permits a prohibited L2→L5 edge. [cargo-deny checks](https://embarkstudios.github.io/cargo-deny/checks/bans/cfg.html), [cargo-modules behavior](https://docs.rs/crate/cargo-modules/0.27.0).

Tach is written in Rust but enforces **Python** imports. It is a useful policy model, not a Rust-code checker. [Tach’s own README](https://github.com/tach-org/tach/blob/main/README.md).

Bylaw resolves aliases, re-exports, qualified paths and macros, fails closed for incomplete analysis, and supports explicit dependency allowlists. However, inspection of published `bylaw-analyzer` shows that a resolved definition is assigned to its **defining module**. Sweetbriar explicitly permits DTOs re-exported upward through adjacent façades. A rule using only definition-origin edges can therefore flag a valid `resources::Dto` import as a direct leaf dependency. The valid access path and original definition are different policy facts. [Semantic design](https://raw.githubusercontent.com/danielgerlag/cargo-bylaw/main/docs/analysis.md), [published resolution mapping](https://docs.rs/crate/bylaw-analyzer/0.1.0/source/src/analyzer.rs).

ArchUnitRust documents syntax-only extraction, no macro expansion or build-generated source, and conservative union of `cfg` branches. Its layer policy ignores edges within a layer and outside declared layers, so default layer ordering does not enforce independence between same-level leaves. [Published limitations/policy](https://docs.rs/crate/archunit/0.0.2).

**Recommendation:** a small owned `syn`-based source-policy checker in `xtask`, with the allowed graph checked into the repository. The checker must:

- Map every handwritten module/file to an owner and layer, including inline modules, `#[path]` files and literal `include!()` targets; reject unclassified code and unresolved/non-literal includes.
- Check `use` trees, `pub use`, absolute/relative qualified paths, type/trait paths and aliases. For first-party references, permit only the same component and the immediately lower layer; separately prohibit cross-leaf calls. Give external crates per-owner allowlists, with standard-library paths explicitly permitted.
- Track permitted façade re-export chains explicitly. Reject direct deep paths, root re-export shortcuts and wildcard internal imports.
- Treat `#[macro_export]` hoisting to the crate root as an export subject to the same owner/facade rules; reject unauthorized exports and inspect `$crate` references in macro bodies.
- Inspect expression-bearing macro arguments or reject unsupported cross-boundary macro forms. Generated code and procedural macros need a separate approved boundary and check; `syn` does not supply name resolution or macro expansion automatically.
- Run against the supported macOS/Linux feature configurations and fail on unsupported syntax rather than silently omitting it.

The parser’s visitor/path APIs are building blocks, not an existing enforcement guarantee. [syn visitor](https://docs.rs/syn/3.0.8/syn/visit/trait.Visit.html), [paths](https://docs.rs/syn/3.0.8/syn/struct.Path.html). Rust documents `include!` expansion and crate-root macro-export scope separately. [include!](https://doc.rust-lang.org/std/macro.include.html), [macro scope](https://doc.rust-lang.org/reference/macros-by-example.html).

The first commit should include negative fixtures for forbidden imports, direct calls, aliases, re-exports, glob attempts, includes, exported macros and macro-contained paths, plus a **positive fixture for a legal adjacent DTO re-export**. Unsupported generated code must fail. This makes the owner’s actual rule executable without pretending to solve arbitrary Rust semantics.

**Strongest alternative/counter-argument:** an owned checker is maintenance work and can miss cases unless the restricted source grammar is enforced. Bylaw is the strongest semantic alternative; adopt it if its qualified fixture suite and custom re-export policy satisfy these requirements. Cargo-modules remains useful for inspection, but is not the complete policy gate.

### A10. cargo-deny: advisories and licences

Current **cargo-deny 0.20.2, released 2026-07-09**, is **MIT OR Apache-2.0**, MSRV **1.88.0**. Current **cargo-about 0.9.2, released 2026-08-18**, has the same licence choice/MSRV. Both have recent releases. [cargo-deny](https://docs.rs/crate/cargo-deny/0.20.2), [cargo-about](https://docs.rs/crate/cargo-about/0.9.2).

Use an SPDX allowlist, rather than old configuration examples containing `copyleft = "deny"`, `[licenses].deny`, or `allow-osi-fsf-free`. Current cargo-deny denies licences not accepted by the allowlist; `OR` needs a satisfiable branch, while `AND` requires all applicable terms. Scoped exceptions support audited components without globally authorizing a copyleft family. In 0.20.2, use `crate = "uniffi@0.32.2"` for an exact package version; a separate `version` field belongs to the older `name` form. [Published package-spec parser](https://docs.rs/crate/cargo-deny/0.20.2/source/src/cfg/package_spec.rs). [Current licence schema](https://embarkstudios.github.io/cargo-deny/checks/licenses/cfg.html).

Proposed starting policy:

```toml
[advisories]
yanked = "deny"
unmaintained = "all"
unsound = "all"
ignore = []
maximum-db-staleness = "P1D"
unused-ignored-advisory = "warn"

[licenses]
confidence-threshold = 0.95
allow = [
  "MIT", "MIT-0", "Apache-2.0", "BSD-2-Clause", "BSD-3-Clause",
  "ISC", "0BSD", "Unlicense", "CC0-1.0", "Zlib", "Unicode-3.0"
]
exceptions = [
  { crate = "uniffi@0.32.2", allow = ["MPL-2.0"] },
  { crate = "uniffi_core@0.32.2", allow = ["MPL-2.0"] },
  { crate = "uniffi_macros@0.32.2", allow = ["MPL-2.0"] },
  { crate = "uniffi_meta@0.32.2", allow = ["MPL-2.0"] },
  { crate = "uniffi_pipeline@0.32.2", allow = ["MPL-2.0"] },
  { crate = "uniffi_bindgen@0.32.2", allow = ["MPL-2.0"] }
]

[licenses.private]
ignore = false

[bans]
multiple-versions = "warn"
wildcards = "deny"

[sources]
unknown-registry = "deny"
unknown-git = "deny"
```

This is a **starting template, not a passed check against a nonexistent Sweetbriar lockfile**. Enumerate all additional MPL UniFFI/build-tool packages in the resolved graph, retaining per-package versions. Add audited permissive identifiers only when encountered; add future Linux LGPL exceptions by package and distribution arrangement. `CC0-1.0` is suitable for metadata/test content acceptance, but its presence here does not mean Fedora will accept newly written CC0 application code.

In 0.20.2, `unmaintained` and `unsound` are scopes (`all`, `workspace`, etc.), not severity strings. Vulnerability advisories fail by default. Inspect the versioned source when online docs mix deprecated and current wording. `maximum-db-staleness` is checked when database fetching is disabled, not a general network freshness guarantee. [Advisory schema](https://embarkstudios.github.io/cargo-deny/checks/advisories/cfg.html), [published parser](https://docs.rs/crate/cargo-deny/0.20.2/source/src/advisories/cfg.rs).

Run checks on the actual shipped feature graph for `aarch64-apple-darwin` and Linux. Use `cargo deny --locked --target aarch64-apple-darwin check advisories licenses bans sources`; schedule a wider dependency audit separately. RustSec databases are versioned Git repositories: that future CI workflow may fetch them, but no git command was used for this research assignment.

**Strongest alternative:** cargo-audit for vulnerabilities plus a separate licence process. Cargo-deny combines more gates, but neither it nor cargo-about proves compliance for bundled C code, Swift dependencies, fonts, artwork or distribution terms. Those need the release inventory described below.

### Part B — licence

### B1. Public-domain instruments and Switzerland

**Public domain is a copyright status, not an exemption from other people’s rights.** A dedication can relinquish only rights its author owns, and cannot ensure identical legal effects worldwide. A permissive fallback is therefore relevant even for a sole-author project. Creative Commons explicitly describes this jurisdictional limitation. [CC0 FAQ](https://wiki.creativecommons.org/wiki/CC0_FAQ).

**CC0-1.0** is the most elaborate candidate: §2 waives copyright and related rights to the fullest lawful extent; §3 grants an unconditional fallback licence and contains a non-assertion commitment for remaining rights. It addresses moral and database rights more explicitly than the shorter alternatives. **§4(a) expressly excludes patent and trademark rights.** Creative Commons says CC0 can be applied to software, but OSI has not approved it and both OSI and FSF raise software-specific patent concerns. [Legal code](https://creativecommons.org/publicdomain/zero/1.0/legalcode.en), [CC software guidance](https://wiki.creativecommons.org/wiki/CC0_FAQ), [OSI FAQ](https://opensource.org/faq), [FSF classifications](https://www.gnu.org/licenses/license-list.html).

**Unlicense** expressly dedicates software to the public domain and includes a broad permission grant and warranty disclaimer. FSF interprets that permission grant as covering cases where the dedication is inadequate. OSI approved it on **2020-06-12**. It is silent on patents; that is not an explicit patent grant. [OSI text/status](https://opensource.org/license/unlicense), [FSF classification](https://www.gnu.org/licenses/license-list.html#Unlicense).

**0BSD** is an unconditional software licence without attribution or notice-retention conditions; it is **not itself a copyright waiver**. OSI approved it on **2015-10-14**. FSF regards it as GPL-compatible but criticizes the practical uncertainty created when the unrequired licence notice disappears. Like MIT and the Unlicense, it has no express patent grant. [OSI text/status](https://opensource.org/license/0bsd), [FSF discussion](https://www.gnu.org/licenses/license-list.html#ZeroClauseBSD).

**Dual offering** means a recipient chooses a licence route. `Unlicense OR MIT` is a real software precedent—ripgrep offers that choice—and gives organizations a conventional MIT path. A recipient choosing MIT must retain its notice; a recipient validly choosing the Unlicense does not acquire MIT’s notice condition merely because MIT is also offered. My preferred variation for Sweetbriar is **`Unlicense OR 0BSD`**, so the fallback also lacks attribution conditions. This combination is a recommendation assembled from standard instruments, not a newly OSI-approved composite licence. [ripgrep’s offering](https://raw.githubusercontent.com/BurntSushi/ripgrep/master/COPYING), [MIT text](https://opensource.org/license/mit), [0BSD text](https://opensource.org/license/0bsd).

**Switzerland:** the consulted official English Copyright Act is the consolidation **as at 2025-07-01**. Article 9 addresses authorship, Article 11 protects integrity even where alteration is permitted, and Article 16 addresses assignment of copyright. These provisions do not establish that every personal right can be extinguished by a blanket public-domain declaration. The exact effect of Sweetbriar’s dedication, including non-waivable residual rights, remains **unverified**; assignment and abandonment are different questions. The implementation decision should supply affirmative reuse permissions instead of depending entirely on successful abandonment. [Official Swiss Act, Articles 9, 11 and 16](https://www.fedlex.admin.ch/filestore/fedlex.data.admin.ch/eli/cc/1993/1798_1798_1798/20250701/en/html/fedlex-data-admin-ch-eli-cc-1993-1798_1798_1798-20250701-en-html.html).

**Recognition, checked 2026-10-12:** all three identifiers—`CC0-1.0`, `Unlicense`, `0BSD`—are in SPDX. Cargo accepts SPDX expressions such as `Unlicense OR 0BSD`; cargo-deny recognizes them as licence identifiers and can allow or reject them by policy. GitHub’s licence detection supports known texts but warns that multiple licences/extra text can impair its displayed badge. Machine recognition does not establish waiver enforceability. [SPDX CC0](https://spdx.org/licenses/CC0-1.0.html), [Unlicense](https://spdx.org/licenses/Unlicense.html), [0BSD](https://spdx.org/licenses/0BSD.html), [Cargo licence field](https://doc.rust-lang.org/cargo/reference/manifest.html#the-license-and-license-file-fields), [cargo-deny](https://embarkstudios.github.io/cargo-deny/checks/licenses/cfg.html), [GitHub detection](https://docs.github.com/en/repositories/managing-your-repositorys-settings-and-features/customizing-your-repository/licensing-a-repository).

A significant downstream distinction: Fedora’s current policy documents grandfathering of CC0 code present before **2022-08-01**, plus limited exceptions. A new Sweetbriar application cannot rely on that grandfathering; this is a reason against **CC0-only** software, independently of Flathub metadata acceptance. [Fedora licence policy](https://docs.fedoraproject.org/en-US/legal/allowed-licenses/).

### B2. Dependencies and distributed binaries

**Yes:** the owner’s public-domain code can use MIT, Apache-2.0, BSD, MPL and suitably integrated LGPL dependencies. The resulting distribution contains components under different terms. The owner cannot replace third-party licences with the project’s dedication. Mozilla explicitly permits an MPL-covered component in a larger work under other terms. [MPL §3.3](https://www.mozilla.org/en-US/MPL/2.0/).

| Dependency licence | Obligations when distributing Sweetbriar binaries |
|---|---|
| [MIT](https://opensource.org/license/mit) | Retain the copyright and permission notice for the included software/substantial portions. Bundle the actual notices, not just the word “MIT”. |
| [BSD-2/3-Clause](https://opensource.org/license/bsd-3-clause) | Reproduce copyright, conditions and disclaimer in accompanying documentation/materials. Three-clause BSD also restricts endorsement using upstream names. |
| [Apache-2.0 §4](https://www.apache.org/licenses/LICENSE-2.0) | Supply the licence, retain applicable notices, identify modifications as required, and reproduce relevant NOTICE material when supplied. Its express patent grant and termination clause remain applicable to that component. |
| [MPL-2.0 §3](https://www.mozilla.org/en-US/MPL/2.0/) | Make **covered source available even when unmodified**, tell recipients how to obtain it, retain notices and keep modifications to covered files under MPL. Separate Sweetbriar files can use the owner’s offering. Copying MPL implementation into a new file can make that file covered. |
| [LGPL-2.1 §6](https://www.gnu.org/licenses/old-licenses/lgpl-2.1.html) | Preserve notices/licence and modification/reverse-engineering rights for debugging changes to the library. Use a suitable replaceable shared-library mechanism, or supply the material needed to relink with a modified library. Distributed library copies also have their own source obligations. Static linking needs a real relinking plan. |
| [LGPL-3.0 §4](https://www.gnu.org/licenses/lgpl-3.0.html) | Similar combined-work accommodation, with version-specific requirements including Installation Information where applicable. Do not assume every LGPL version has identical terms. |
| [Historical OpenSSL/SSLeay](https://spdx.org/licenses/OpenSSL.html) | Retain its required notices and advertising acknowledgements where those clauses apply. Audit the actual bundled source/version. OpenSSL **3.0 and later uses Apache-2.0**, so “OpenSSL-style” is not a single current licence category. [Upstream licensing](https://openssl-library.org/source/license/). |

GTK’s current documentation identifies **LGPL-2.1-or-later**. A Linux GTK app can retain the owner’s code offering while dynamically using GTK, provided the relevant distribution complies. Rust bindings and native GTK/GLib libraries are separate inventory entries; auditing the Cargo metadata alone misses that native-library layer. [GTK documentation](https://docs.gtk.org/gtk4/).

For UniFFI, archive the exact MPL runtime sources and any patches, and put the source URL/instructions in the shipped notices. **A modified template remains MPL-covered.** I did not verify a blanket exception permitting all generated Swift helper implementation to be relicensed as public domain. Conservatively keep generated helper code separate and make it available under MPL where it incorporates covered implementation, pending clarification; the owner’s handwritten adapter and domain files retain their own offering. [UniFFI licence](https://docs.rs/crate/uniffi/0.32.2), [MPL definition and distribution provisions](https://www.mozilla.org/en-US/MPL/2.0/).

**GPL/AGPL:** exclude them from linked runtime dependencies unless a separately audited exception permits the intended use. Ordinary GPL linking generally requires the combined distributed program to meet GPL terms. The owner’s original public-domain files do not retroactively become privately owned, but the distributed combination is no longer offered solely on the desired unrestricted basis. AGPL adds source-offer requirements for qualifying remote interaction with a modified covered program; it is not merely a “server-only” restriction. Tool-only use and genuine separate-program aggregation are different cases. [GPL FAQ](https://www.gnu.org/licenses/gpl-faq.html), [AGPL §13](https://www.gnu.org/licenses/agpl-3.0.html).

**Proton Bridge is compatible with this approach:** its implementation is GPL, but Sweetbriar can speak ordinary IMAP/SMTP to a separately installed Bridge. The FSF’s distinction turns on the nature of the combination and communication, not merely the existence of a socket. Standard mail protocols between independent programs support treating them as separate works; copying/linking Bridge code would be a different decision. Recommend user-installed Bridge for the first milestone. [Bridge repository/licence](https://github.com/ProtonMail/proton-bridge), [GPL aggregation guidance](https://www.gnu.org/licenses/gpl-faq.html#MereAggregation).

### B3. Files, contribution policy, notices and distribution

**Proposed licence layout:** root `LICENSE` states that owner-authored code is available under `Unlicense OR 0BSD`, explains the public-domain intention to the lawful extent, and excludes third-party material from that statement. Keep unmodified standard texts in `LICENSES/Unlicense.txt` and `LICENSES/0BSD.txt`, identify ownership clearly, and use matching SPDX headers/manifest fields. The scope statement should not alter the standard permissions. Do not apply these terms to users’ mail, third-party assets, vendored dependencies or covered generated helpers.

```toml
[package]
license = "Unlicense OR 0BSD"
```

**Proposed `CONTRIBUTING.md`:**

> Sweetbriar does not accept external code, documentation, translations,
> artwork or patches. Please do not submit pull requests or replacement code.
> Bug reports and discussion are welcome; describe the behavior without
> submitting patches. You may fork and maintain your own version under the
> published licence terms. This contribution policy does not limit those rights.

The policy belongs in CONTRIBUTING, not as a restriction on licence-granted use. Avoid copying unsolicited patches into the owner-authored codebase. GitHub surfaces contribution guidelines for potential contributors. [GitHub guidelines](https://docs.github.com/en/communities/setting-up-your-project-for-healthy-contributions/setting-guidelines-for-repository-contributors).

**GitHub setting, verified 2026-10-12:** Settings → Features → deselect **Pull requests** disables PRs; an alternative restricts them to collaborators. This capability exists now, so advice that public repositories cannot disable PRs is stale. Issues may remain enabled for reports. Public-repository forking remains part of GitHub’s terms and is compatible with the owner’s policy. [Disable pull requests](https://docs.github.com/en/repositories/managing-your-repositorys-settings-and-features/enabling-features-for-your-repository/disabling-pull-requests), [repository licensing/forking](https://docs.github.com/en/repositories/managing-your-repositorys-settings-and-features/customizing-your-repository/licensing-a-repository).

**Notices in CI:** use cargo-about with a checked-in template and acceptance/clarification configuration. Generate an HTML or text notice artifact from the locked, release-relevant graph, then add native libraries, applicable NOTICE files, Swift packages/assets and source-availability links. For example:

```sh
cargo about generate --locked notices.hbs -o THIRD_PARTY_NOTICES.html
```

Its configuration supports target selection and dependency inclusion controls; its generator supports `--locked` and an output file. Keep a full dependency audit as well as the shipped-component inventory. A generated list of standard licence names/texts alone is insufficient when author-specific notices or source/relinking duties apply. Make the final file accessible in the app and package it with the release. [Generator documentation](https://embarkstudios.github.io/cargo-about/cli/generate/index.html), [published configuration example](https://docs.rs/crate/cargo-about/0.9.2/source/about.toml).

**Apple distribution:** the App Review rules require rights to included material; they do not prescribe one open-source licence for the application. A permissive/public-domain source offering is therefore not inherently excluded. App Store EULA/distribution terms remain distinct from source permissions, and actual approval is **unverified** until submission. For direct macOS distribution, use Developer ID signing/notarization and ship notices/source links. This research does not establish compatibility of every hypothetical copyleft component with App Store terms. [Review guideline 5.2](https://developer.apple.com/app-store/review/guidelines/#intellectual-property), [Apple EULA](https://www.apple.com/legal/internet-services/itunes/dev/stdeula/), [notarization](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution).

**Flathub/AppStream, verified 2026-10-12:** Flathub requires valid SPDX project-licence expressions and supports `OR`. App metadata has its **own** licence: use `CC0-1.0` for `metadata_license`, and `Unlicense OR 0BSD` for the application’s `project_license`. AppStream’s metadata whitelist includes CC0/0BSD; do not assume Unlicense is also a metadata-licence choice. Flathub’s requirements explicitly suggest MIT, 0BSD or Unlicense for manifests. This is a viable declared licensing arrangement, not advance acceptance of the eventual app or its dependency bundle. [Flathub MetaInfo rules](https://docs.flathub.org/docs/for-app-authors/metainfo-guidelines), [requirements](https://docs.flathub.org/docs/for-app-authors/requirements), [AppStream specification](https://www.freedesktop.org/software/appstream/docs/chap-Metadata.html).

### B4. Recommendation and strongest counter-argument

**Recommendation: `Unlicense OR 0BSD` for the owner’s own code.** The Unlicense expresses the requested dedication; 0BSD supplies affirmative reuse permission without attribution conditions when dedication is ineffective or unacceptable to a recipient’s policy. Preserve both standard texts and describe the project as “public domain to the extent permitted by law, otherwise available under 0BSD.” Audit third-party components independently.

**Strongest counter-argument:** CC0 has better explicit worldwide waiver, fallback and non-assertion drafting, especially for residual moral/database rights. `Unlicense OR 0BSD` is not a guarantee of complete abandonment, and neither operand grants patents expressly. If literal waiver drafting outweighs software-policy convenience, **`CC0-1.0 OR 0BSD`** is the strongest alternative; the 0BSD option addresses recipients who reject CC0-only code. `Unlicense OR MIT` is the stronger established dual-offering precedent, with optional MIT attribution obligations. If an express patent grant becomes essential, choose an instrument that supplies one rather than pretending any of these three does.

## 3. Options compared

### IMAP extension support

“Typed” means relevant published client operations/results exist; “grammar” means encoding/decoding and flow support, with orchestration left to Sweetbriar. A raw-command escape hatch does not constitute complete extension support. Ratings refer to the versions above, checked 2026-10-12.

| Stack | UIDPLUS | MOVE | CONDSTORE / QRESYNC | IDLE | LITERAL+ | ID |
|---|---|---|---|---|---|---|
| async-imap 0.12.0 | Partial: UID EXPUNGE; APPEND returns `()` | Typed | CONDSTORE selection/MODSEQ; incomplete QRESYNC | Typed | Standard APPEND uses synchronizing `{N}` | Typed |
| io-imap 0.7.1 | Typed UID receipts/UID EXPUNGE | Typed | SELECT captures vanished UIDs; FETCH drops `VANISHED` | Coroutine/adapter | Explicit non-sync APPEND option; caller checks capability | Typed |
| imap-next + 2.0 alpha codec | Grammar/flows | Grammar | Grammar with feature enabled; engine owns cursors | Flow state machine | Literal mode/flow support | Grammar with feature enabled |
| imap 3.0.0-alpha.15 | Partial high-level support | Typed | No complete typed resync API verified | Blocking handle | Standard APPEND waits for continuation | Raw command; no typed API verified |

Sources: [async-imap API/source](https://docs.rs/async-imap/0.12.0/async_imap/struct.Session.html), [io-imap APPEND](https://docs.rs/crate/io-imap/0.7.1/source/src/rfc3501/append.rs), [SELECT](https://docs.rs/crate/io-imap/0.7.1/source/src/rfc3501/select.rs), [imap-next](https://docs.rs/crate/imap-next/0.3.4), [codec/types](https://docs.rs/crate/imap-types/2.0.0-alpha.7), [sync imap session](https://docs.rs/imap/3.0.0-alpha.15/imap/struct.Session.html).

Relevant protocol contracts are **UIDPLUS RFC 4315**, **MOVE RFC 6851**, **CONDSTORE/QRESYNC RFC 7162**, and **non-synchronizing literals RFC 7888**. Capability negotiation, UIDVALIDITY changes, persistent MODSEQ state and reconciling expunges remain application responsibilities. [RFC 4315](https://www.rfc-editor.org/rfc/rfc4315.html), [RFC 6851](https://www.rfc-editor.org/rfc/rfc6851.html), [RFC 7162](https://www.rfc-editor.org/rfc/rfc7162.html), [RFC 7888](https://www.rfc-editor.org/rfc/rfc7888.html).

### Licence instruments

| Offering | Public-domain dedication | Positive permission fallback | Express patent grant | Main trade-off |
|---|---|---|---|---|
| CC0-1.0 | Yes, detailed | Explicit | No; patents excluded | Strong waiver drafting; software-policy friction |
| Unlicense | Yes | Broad grant, interpreted as fallback | No | Software dedication; less elaborate residual-right drafting |
| 0BSD | No | It is the licence | No | Simple unrestricted reuse; copyright remains where it exists |
| Unlicense OR MIT | Yes via Unlicense | Conventional MIT alternative | No | Familiar precedent; MIT users retain notices |
| **Unlicense OR 0BSD** | Yes via Unlicense | Unconditional 0BSD alternative | No | Recommended fit; dual metadata and residual-right uncertainty |

Texts/status: [CC0](https://creativecommons.org/publicdomain/zero/1.0/legalcode.en), [Unlicense](https://opensource.org/license/unlicense), [0BSD](https://opensource.org/license/0bsd), [MIT](https://opensource.org/license/mit). The proposed dual choices are recommendations; no claim is made that these composites have separate OSI approval.

## 4. Recommendations for Sweetbriar

1. **Local archive milestone:** pin rusqlite 0.40.2 and its actual libsqlite3-sys lockfile entry; use bundled FTS5 and rusqlite_migration 2.6.0. Adopt mail-parser 0.11.9 with `full_encoding`, mail-builder 1.0.0 and the bounded mbox wrapper. Review post-3.53.2 SQLite fixes before trusting real archives. Acceptance is preserved originals, reconciled import counts, migration/restart recovery and a clean restore—not a successful parser demo.
2. **First live backend:** qualify io-imap 0.7.1 against Bridge and Dovecot. Stop the evaluation once greeting/TLS/auth, UID receipts, fetch/flags, reconnect, IDLE and required incremental resync cases—including preservation of FETCH-time `VANISHED`—pass. If it fails, bootstrap with async-imap 0.12.0 and explicitly narrow the supported sync behavior. Keep the engine independent of either crate.
3. **Submission:** use lettre 0.11.23 to send persisted mail-builder bytes, rustls/platform verification and account-scoped Bridge trust. Acceptance includes ambiguous SMTP completion and duplicate prevention/reconciliation behavior.
4. **Native shell:** use UniFFI 0.32.2 only after the actual Swift façade compiles in strict Swift 6 mode. Prefer a façade avoiding async foreign callback traits initially; backport only if required. Select the Apple protected credential store and test it in the signed app.
5. **Repository gates from the first commit:** owned module-policy checker with deliberate failing fixtures; cargo-deny on release graphs; proptest/insta for domain invariants; scripted peers plus real-server tests. Keep snapshots and failpoints subordinate to assertions about preservation and recovery.
6. **Licensing:** publish the owner’s code under `Unlicense OR 0BSD`, disable external PRs, and document forks as welcome. Ship third-party notices plus MPL source and any LGPL compliance material with every release. This research recommends the arrangement; it does not apply a licence to existing files.

The strongest overall counter-argument is maintenance concentration: young IMAP code, custom boundary enforcement and native FFI qualification all land on one owner. The remedy is a narrow first release with explicit replacement boundaries and acceptance fixtures. Selecting more crates or writing a larger framework does not remove those obligations.

## 5. Risks and open questions

- **Exact integration compatibility:** current publication metadata is verified; the combined Cargo feature graph, native linking, Swift output and signed-app behavior have not been built. Declare Sweetbriar’s MSRV only after those builds pass.
- **SQLite freshness:** bundled 3.53.2 meets the WAL floor but trails both later patches and 3.54.0. Review their fixes and decide whether to patch before the real-data gate. Mac/APFS power-loss behavior remains a separate validation requirement.
- **MIME and archives:** the safe mail-parser version for the cited consumer vulnerability is unverified. Obtain actual macOS 27 Apple exports and classic-client mbox variants; preserve ambiguous imports for reconciliation.
- **Protocol qualification:** no live Bridge capability transcript was captured. QRESYNC grammar and convenience APIs do not prove reliable reconnect/reconciliation semantics; io-imap’s lossy FETCH wrapper is a verified gap requiring a fix or qualified alternative. Large literals and cancellation need explicit resource/progress bounds.
- **Boundary semantics:** decide which DTO re-exports are permitted, and prohibit behavior-bearing leaf APIs escaping through an indiscriminate façade. Macros, generated code and unclassified modules must not form unchecked paths.
- **Licence scope:** the owner’s precise Swiss waiver effect and any UniFFI generated-output exception are unverified. Preserve permissions/source availability without claiming all app components are public domain.
- **Release inventory:** optional TLS providers, native Linux libraries and non-Rust assets can introduce terms absent from the top-level crate list. The first real lockfile and packaged binaries must be audited.
- **Contribution scope:** the suggested policy excludes code and non-code material; future employment, commissioned assets or accepted external snippets would require an ownership decision. Store submission and Flathub acceptance remain unverified until the actual app exists.

## 6. Sources

All sources below were consulted on **2026-10-12**. Versioned documentation and registry URLs in the tables above are also part of this catalogue; registry JSON and checksum-verified archives are retained under `evidence/registry/` and `evidence/crates/`. Provenance JSON records additional fetched upstream files and unsuccessful fetches. Failed endpoints were retried over HTTP; where dynamic pages or bot protection intervened, alternate primary representations/search retrieval were used. Unverified conclusions are labelled in the report.

- **Version/MSRV authority:** [crates.io API example: rusqlite](https://crates.io/api/v1/crates/rusqlite), [Cargo MSRV](https://doc.rust-lang.org/cargo/reference/rust-version.html), [Cargo licence expressions](https://doc.rust-lang.org/cargo/reference/manifest.html), [Rust releases](https://blog.rust-lang.org/releases/).
- **SQLite:** [WAL-reset fix](https://www.sqlite.org/wal.html), [withdrawn 3.52.0](https://www.sqlite.org/releaselog/3_52_0.html), [3.53.4](https://www.sqlite.org/releaselog/3_53_4.html), [3.54.0](https://www.sqlite.org/releaselog/3_54_0.html), [compile options](https://www.sqlite.org/compile.html), [FTS5](https://www.sqlite.org/fts5.html), [copyright](https://www.sqlite.org/copyright.html).
- **Mail formats/security:** [mail-parser development manifest](https://raw.githubusercontent.com/stalwartlabs/mail-parser/main/Cargo.toml), [Stalwart advisory](https://github.com/stalwartlabs/stalwart/security/advisories/GHSA-jm95-876q-c9gw), [RFC 4155](https://www.rfc-editor.org/rfc/rfc4155.html), [Apple mailbox export](https://support.apple.com/guide/mail/import-or-export-mailboxes-mlhlp1030/mac), [historical first-hand package test](https://discussions.apple.com/thread/2092094).
- **Native integration:** [Bridge TLS](https://proton.me/support/bridge-ssl-connection-issue), [UniFFI PR #2943](https://github.com/mozilla/uniffi-rs/pull/2943), [UniFFI async guide](https://mozilla.github.io/uniffi-rs/latest/futures.html), [Apple TN3137](https://developer.apple.com/documentation/technotes/tn3137-on-mac-keychains), [Secret Service](https://specifications.freedesktop.org/secret-service-spec/latest/).
- **Tests/boundaries:** [Dovecot Docker](https://doc.dovecot.org/main/installation/docker.html), [GreenMail](https://greenmail-mail-test.github.io/greenmail/), [Tach](https://github.com/tach-org/tach/blob/main/README.md), [Bylaw semantic design](https://raw.githubusercontent.com/danielgerlag/cargo-bylaw/main/docs/analysis.md), [published analyzer](https://docs.rs/crate/bylaw-analyzer/0.1.0/source/src/analyzer.rs).
- **Licence instruments/status:** [CC0 legal code](https://creativecommons.org/publicdomain/zero/1.0/legalcode.en), [CC0 FAQ](https://wiki.creativecommons.org/wiki/CC0_FAQ), [OSI FAQ](https://opensource.org/faq), [OSI Unlicense](https://opensource.org/license/unlicense), [OSI 0BSD](https://opensource.org/license/0bsd), [FSF list](https://www.gnu.org/licenses/license-list.html), [SPDX list entries](https://spdx.org/licenses/Unlicense.html), [ripgrep precedent](https://raw.githubusercontent.com/BurntSushi/ripgrep/master/COPYING), [Fedora policy](https://docs.fedoraproject.org/en-US/legal/allowed-licenses/).
- **Swiss law:** [official English consolidation, status 2025-07-01](https://www.fedlex.admin.ch/filestore/fedlex.data.admin.ch/eli/cc/1993/1798_1798_1798/20250701/en/html/fedlex-data-admin-ch-eli-cc-1993-1798_1798_1798-20250701-en-html.html).
- **Distribution obligations:** [MIT](https://opensource.org/license/mit), [BSD](https://opensource.org/license/bsd-3-clause), [Apache-2.0](https://www.apache.org/licenses/LICENSE-2.0), [MPL text](https://www.mozilla.org/en-US/MPL/2.0/), [MPL FAQ](https://www.mozilla.org/en-US/MPL/2.0/FAQ/), [LGPL-2.1](https://www.gnu.org/licenses/old-licenses/lgpl-2.1.html), [LGPL-3.0](https://www.gnu.org/licenses/lgpl-3.0.html), [GPL FAQ](https://www.gnu.org/licenses/gpl-faq.html), [AGPL](https://www.gnu.org/licenses/agpl-3.0.html), [historical OpenSSL](https://spdx.org/licenses/OpenSSL.html), [current OpenSSL policy](https://openssl-library.org/source/license/).
- **Automation/channels:** [cargo-deny licences](https://embarkstudios.github.io/cargo-deny/checks/licenses/cfg.html), [advisories](https://embarkstudios.github.io/cargo-deny/checks/advisories/cfg.html), [cargo-about generation](https://embarkstudios.github.io/cargo-about/cli/generate/index.html), [GitHub PR settings](https://docs.github.com/en/repositories/managing-your-repositorys-settings-and-features/enabling-features-for-your-repository/disabling-pull-requests), [Apple review](https://developer.apple.com/app-store/review/guidelines/), [Apple EULA](https://www.apple.com/legal/internet-services/itunes/dev/stdeula/), [Flathub MetaInfo](https://docs.flathub.org/docs/for-app-authors/metainfo-guidelines), [Flathub requirements](https://docs.flathub.org/docs/for-app-authors/requirements), [AppStream](https://www.freedesktop.org/software/appstream/docs/chap-Metadata.html).
