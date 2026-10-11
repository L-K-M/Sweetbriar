<!-- provenance: helper=devin model=swe-2-max started=2026-10-11T18:14:20+00:00 exit=0 brief=research/briefs/ -->

# Existing email engines and the Rust email ecosystem

*Sweetbriar research — topic "rust-ecosystem". Researched and written
2026-10-11. All crate versions, release dates and repository activity were
checked against the crates.io API, repository pages and project READMEs on
2026-10-11 unless noted otherwise.*

## 1. TL;DR

- The Rust email stack in late 2026 is **good enough to build a serious mail
  client on, but no complete engine exists to reuse**. Protocol codecs and
  MIME handling are essentially solved; the *sync engine, local store,
  threading index, and unified search* are all things Sweetbriar must build
  itself — which is also where its value will live.
- **IMAP**: the best protocol substrate is Damian Poddebniak's
  `imap-types` → `imap-codec` → `imap-next` stack (MIT/Apache-2.0, fuzzed,
  zero-copy, misuse-resistant). Two usable client layers exist on top:
  pimalaya's `io-imap` 0.7.1 (CONDSTORE+QRESYNC, IDLE, MOVE, UIDPLUS,
  SORT/THREAD, OAUTHBEARER/XOAUTH2, SCRAM-SHA-256 — the widest extension
  coverage in the ecosystem) and Delta Chat's `async-imap` 0.12.0 (CONDSTORE,
  MOVE, UIDPLUS, IDLE, COMPRESS, METADATA — narrower but battle-proven at
  scale). `imap` (jonhoo) is unmaintained; melib's IMAP is the most complete
  but is copyleft.
- **IMAP4rev2 (RFC 9051) is not fully implemented anywhere yet** — it is
  being assembled in imap-codec 2.x (issues
  [#702](https://github.com/duesee/imap-codec/issues/702),
  [#704](https://github.com/duesee/imap-codec/issues/704), open Oct 2026).
  NOTIFY (RFC 5465) and OBJECTID (RFC 8474) have no Rust implementation.
- **MIME**: `mail-parser` 0.11.9 (Stalwart, Apache-2.0/MIT) is the clear
  choice — SIMD-accelerated, lazy, zero-copy, 41 charsets, fuzzed and
  Miri-tested, ~3x faster than `mailparse` per its own benchmarks. Build
  outgoing mail with `mail-builder` 1.0. **TNEF (winmail.dat) is a gap** —
  the only `tnef` crate is abandonware from 2019.
- **Auth/JMAP**: `mail-auth` covers DKIM1+DKIM2(draft), ARC, SPF, DMARC
  (incl. new RFC 9989–9991), ARF, TLS-RPT; `jmap-client` covers JMAP core,
  mail, WebSocket and EventSource push. Both Apache/MIT.
- **License traps to plan around**: `sieve-rs` is AGPL-3.0 (the only copyleft
  Stalwart crate — commercial license sold separately), `sequoia-openpgp` is
  LGPL-2.0+, `melib` is EUPL-1.2/GPL-3.0, notmuch/mu/Mailsync/Proton
  Bridge/Xapian are all GPL. Everything recommended below is
  MIT/Apache/BSD/MPL-2.0.
- **Engines worth studying**: Delta Chat core (MPL-2.0 — the only proven
  "Rust core + native UI" mail engine, SQLite+sqlcipher), meli/melib (best
  client-side IMAP logic, unreadable license), **pimalaya's neverest**
  (MIT/Apache sync engine in progress: IMAP + MS Graph + CardDAV/CalDAV +
  a local "pimdir" store — the closest thing to what Sweetbriar needs to
  write), Thunderbird's `ews` crate (MPL-2.0, a reusable EWS client in Rust
  shipping to users), and Mailspring's Mailsync (GPL-3 C++ but superbly
  documented sync architecture).
- Recommendation: **imap-codec/imap-next + own sync engine** (vendor/contribute
  to `io-imap` rather than depend on it), `mail-parser`/`mail-builder`/`mail-send`,
  `jmap-client`, `rusqlite` (+optional sqlcipher) for the store, `tantivy`
  or SQLite FTS5 for search, `pgp` (rpgp) for OpenPGP, `ammonia`+`lol_html`
  for HTML. Biggest custom work: sync engine, store schema+threading,
  Exchange/M365 and Proton backends, Sieve-equivalent rules.

## 2. Findings — Rust crates, per layer

For each crate: purpose, maintainer, license, latest release/activity,
coverage, async model, testing, notable users, known problems.

### 2.1 IMAP

The ecosystem looks like this (arrows = depends on):

```
imap-proto (djc) ──────────────► async-imap ──► Delta Chat core
imap-types → imap-codec ─► imap-next ─► io-imap (pimalaya)
                     │        └─► imap-proxy (duesee)
                     └─► melib::imap (meli's own client logic)
imap (jonhoo/rust-imap) ──── sync, seeking maintainers
io-smtp / async-smtp / lettre / mail-send ──── submission side
```

| Crate | Latest (as of 2026-10-11) | License | Maintainer | Async | Fuzz/test status |
|---|---|---|---|---|---|
| `imap-types` | 2.0.0-alpha.7 (stable 1.0.0, 2023) | MIT OR Apache-2.0 | duesee (NLnet-funded) | n/a | property tests, `arbitrary` |
| `imap-codec` | 2.0.0-alpha.9 (2026-07-19) | MIT OR Apache-2.0 | duesee | n/a | cargo-fuzz + property tests; coveralls badge |
| `imap-next` | 0.3.4 (2026-02-18) | MIT OR Apache-2.0 | duesee | sans-I/O | tested via imap-codec |
| `io-imap` | 0.7.1 (2026-10-01) | MIT OR Apache-2.0 | soywod/pimalaya (NLnet-funded) | I/O-free coroutines → blocking → full | used by himalaya, neverest, mirador, sirup |
| `imap-client` (renamed → io-imap) | 0.3.1 (2026-05-17) | MIT | pimalaya | — | superseded |
| `async-imap` | 0.12.0 (2026-09-30) | MIT OR Apache-2.0 | async-email org (chatmail/Delta Chat orbit) | async-std or tokio features | GreenMail integration tests; production via Delta Chat |
| `imap-proto` | 0.17.0 (2026-09-28) | MIT OR Apache-2.0 | djc | n/a | parser under async-imap |
| `imap` | 3.0.0-alpha.15 (2025-02-08); stable 2.4.1 | MIT OR Apache-2.0 | jonhoo — "looking for maintainers" | sync only | GreenMail tests |
| `melib` | 0.8.13 (2026-01-05) | **EUPL-1.2 OR GPL-3.0-or-later** | Manos Pitsidianakis (epilys) | async internally | full client: IMAP/Maildir/mbox/notmuch/JMAP/NNTP/Sieve |

Sources: [imap-codec repo](https://github.com/duesee/imap-codec),
[imap-next](https://github.com/duesee/imap-next),
[io-imap](https://github.com/pimalaya/io-imap),
[async-imap](https://github.com/async-email/async-imap),
[rust-imap](https://github.com/jonhoo/rust-imap),
[melib](https://docs.rs/melib).

#### Extension matrix

Verified by reading feature lists, READMEs and (for async-imap) the docs.rs
source of `client.rs`/`extensions/` on 2026-10-11. "core" = folded into
IMAP4rev2/2.x baseline; "?" = not verifiable from docs.

| Capability (RFC) | imap-codec 2.x | io-imap | async-imap | melib::imap |
|---|---|---|---|---|
| IMAP4rev1 (3501) | full | full | full | full |
| IMAP4rev2 (9051) | being assembled ([#702](https://github.com/duesee/imap-codec/issues/702)) | no | no | no |
| IDLE (2177) | yes (flow in imap-next) | yes | yes | yes |
| ID (2971) | `ext_id` | yes | yes | ? |
| CONDSTORE (7162) | `ext_condstore_qresync` | yes | `select_condstore` | yes |
| QRESYNC (7162) | `ext_condstore_qresync` | yes | **no** | ? |
| MOVE (6851) | yes (2.x core) | yes | `mv`, `uid_mv` | ? |
| UIDPLUS (4315) | yes (2.x core) | yes | `uid_expunge` | yes |
| COMPRESS=DEFLATE (4978) | 1.x `ext_compress` (2.x: via types) | ? | yes (`compress` feature) | yes (deflate) |
| NOTIFY (5465) | no | no | no | no |
| OBJECTID (8474) | no | no | no | no |
| BINARY (3516) | yes (2.x core) | ? | no | ? |
| UTF8=ACCEPT (6855) | `ext_utf8` | ? | no | ? |
| SORT/THREAD (5256) | yes (2.x changelog) | yes (+client fallback) | no | threading client-side |
| ENABLE (5161) | yes (2.x core) | yes | ? | yes |
| UNSELECT (3691) | yes (2.x changelog) | yes | no | ? |
| METADATA (5464) | `ext_metadata` | ? | `get_metadata` | ? |
| QUOTA (2087) | 1.x ext_quota | ? | `get_quota` | ? |
| NAMESPACE (2342) | `ext_namespace` | ? | ? | yes |
| WITHIN (5032) | `ext_within` | ? | ? | ? |
| OAUTHBEARER (7628) | mechanism type | yes | via `authenticate()` | via password-cmd |
| XOAUTH2 (proprietary) | mechanism type | yes | via `authenticate()` | yes |
| SASL-IR (4959) | yes | yes | ? | ? |
| SCRAM (5802/7677) | `ScramSha3_512{,Plus}` | SCRAM-SHA-256 | ? | ? |
| STATUS=SIZE (8438) | `ext_status_size` ([#704](https://github.com/duesee/imap-codec/issues/704)) | ? | ? | ? |

Facts: io-imap's RFC table is documented in its
[README](https://github.com/pimalaya/io-imap) (2177, 2971, 3501, 3691, 4315,
4959, 5161, 5256, 6851, 7162, 7628, 7677 + XOAUTH2). async-imap's surface is
from its source: `append authenticate capabilities check close copy create
delete examine expunge fetch get_metadata get_quota get_quota_root id idle
list login lsub mv noop rename search select select_condstore status store
subscribe uid_copy uid_expunge uid_fetch uid_mv uid_search uid_store
unsubscribe` — i.e. no QRESYNC, NOTIFY, UNSELECT, SORT, OBJECTID,
UTF8=ACCEPT. imap-codec's 2.x extension features (from `Cargo.toml`):
`starttls, ext_condstore_qresync, ext_id, ext_login_referrals,
ext_mailbox_referrals, ext_metadata, ext_namespace, ext_status_size,
ext_utf8, ext_within`; its CHANGELOG records UNSELECT/SORT/THREAD/BINARY/
METADATA added in the 2.0 line, and a `quirk_*` family
(`quirk_missing_text`, `quirk_rectify_numbers`, `quirk_crlf_relaxed`, …) for
broken real-world servers.

#### Per-crate notes

- **imap-types / imap-codec** (duesee, [repo](https://github.com/duesee/imap-codec),
  pushed 2026-10-09): nom-based, streaming (`Incomplete` on short input),
  zero-copy with owned conversion, misuse-resistant (can't construct invalid
  messages), both client and server halves, `Fragmentizer` two-stage parsing
  for malformed streams, fuzz + property tests ("never produce a message it
  can't parse"). NLnet NGI Assure funded. 2.x is alpha since 2024-06 — the
  big caveat. `imap-sec` is duesee's companion IMAP server probe tool.
- **imap-next** ([repo](https://github.com/duesee/imap-next), pushed
  2026-04-24): thin sans-I/O abstraction over IMAP's awkward flows (literal
  handling, AUTHENTICATE, IDLE). Async-model-agnostic — works with any
  runtime, which suits a "Rust core driving native UIs" architecture.
- **io-imap** (pimalaya, [repo](https://github.com/pimalaya/io-imap), crate
  0.7.1 on 2026-10-01; formerly `imap-client`, moved/renamed mid-2026):
  three layers — I/O-free `no_std` coroutines, a blocking "light client" over
  your stream, a "full client" adding TCP+TLS (rustls ring/aws-lc or
  native-tls). SASL: ANONYMOUS, LOGIN, PLAIN, XOAUTH2, OAUTHBEARER,
  SCRAM-SHA-256. Used by himalaya, neverest, mirador, sirup. Young (<1.0).
- **async-imap** ([repo](https://github.com/async-email/async-imap), 0.12.0
  2026-09-30, on `imap-proto` 0.17): the most deployed client (Delta Chat
  uses it with `runtime-tokio`+`compress`; async-std also supported).
  Changelog shows steady maintenance: COMPRESS (0.10.2), METADATA, QUOTA,
  CONDSTORE select, MOVE, Gmail label exposure. API is command/response —
  you still write the sync engine.
- **imap** (jonhoo, [repo](https://github.com/jonhoo/rust-imap)): README
  states "This crate is looking for maintainers"; sync-only API; native-tls
  or rustls options. 3.0.0-alpha line last touched 2025-02. Skip for new code.
- **melib** ([repo](https://git.meli-email.org/meli/meli), crates.io
  0.8.13): embeds a complete async IMAP client built on `imap-codec` with
  `SUPPORTED_CAPABILITIES` = COMPRESS=DEFLATE, CONDSTORE, ENABLE, IDLE,
  IMAP4REV1 (+ManageSieve connections, OAuth2 via password command), a
  UIDStore + offline cache, watchers. License is the problem: **EUPL-1.2 OR
  GPL-3.0-or-later** — read it for design, do not link it into anything
  non-GPL.

### 2.2 SMTP / message submission

| Crate | Ver | License | Notes |
|---|---|---|---|
| [`lettre`](https://github.com/lettre/lettre) | 0.11.23 (2026-08-03) | MIT | The standard mailer: builders, SMTP transport, STARTTLS/implicit TLS, Unicode, async (tokio/async-std/smol). Explicitly *no* parsing. 20M+ downloads — most-used email crate in Rust. |
| [`mail-send`](https://github.com/stalwartlabs/mail-send) | 0.6.2 (2026-08-18) | MIT OR Apache-2.0 | Build+sign+send: DKIM1 (RSA/Ed25519) **and DKIM2 draft**, SMTP+STARTTLS, auth CRAM-MD5/DIGEST-MD5/XOAUTH2/LOGIN/PLAIN. Tokio-only. Miri-tested. |
| [`async-smtp`](https://github.com/async-email/async-smtp) | 0.10.2 (2025-05-13) | MIT OR Apache-2.0 | lettre lineage; Delta Chat's SMTP layer. |
| [`io-smtp`](https://github.com/pimalaya/io-smtp) | 0.6.0 (2026-10-09) | MIT OR Apache-2.0 | pimalaya; same layered design as io-imap (coroutines→light→full). New. |

mail-send notably lacks OAUTHBEARER and SCRAM in its mechanism list — for
Microsoft 365/Gmail submission, XOAUTH2 covers the need, but plan auth per
backend. lettre is the safe default; mail-send adds DKIM signing for free.

### 2.3 MIME parsing

| Crate | Ver | License | Notes |
|---|---|---|---|
| [`mail-parser`](https://github.com/stalwartlabs/mail-parser) | 0.11.9 (2026-09-09) | MIT OR Apache-2.0 | See below. |
| [`mailparse`](https://github.com/staktrace/mailparse) | 0.18.0 (2026-10-05) | 0BSD | Simple `parse_mail(&[u8])`; robust real-world parser; Delta Chat uses it; MSRV 1.85. |
| `tnef` | 0.1.1 (2019) | MIT/Apache | Dead. TNEF is a build-it-yourself gap. |
| `email`, `email-format`, `eml`, `dkim` | stale/minimal | — | Do not use. |

`mail-parser` details (from its [README](https://github.com/stalwartlabs/mail-parser)):
- RFC 5322 + MIME 2045–2049 + many more (full RFC list in README); Postel's
  law robustness for malformed input.
- 41 charsets incl. UTF-7; multi-byte CJK via optional `full_encoding` →
  `encoding_rs` (Shift_JIS, EUC-JP, ISO-2022-JP, Big5, GBK, GB18030, EUC-KR,
  windows-874, IBM866, x-mac-cyrillic, x-user-defined).
- SIMD: boundary/header scanning via NEON/AVX2/SSE2 kernels; `encodify` for
  base64/QP; `simdutf8` validation; `hashify` perfect-hash lookups.
- Lazy: nothing is transfer-decoded or charset-converted until read — a
  client that renders only headers pays almost nothing per message.
- Zero-copy; `parse_owned`/`into_owned` for `'static` messages; mbox
  `MessageIterator`; configurable header forms; hard limits (depth 64, 10k
  parts, 3 levels of nested decode).
- Emits the RFC 8621 §4.1.4 three-list model: text bodies, HTML bodies,
  attachments — plus HTML→text and text→HTML conversion for missing
  alternatives. This is exactly the model an email client wants.
- Fuzzed (targets: parse, limits, fields, charsets, kernels, mbox) + Miri
  tested; benchmark claims ~3x `mailparse`, 5–31x the C libraries (GMime,
  VMime, libEtPan, Dovecot lib-mail) — self-reported on an Apple M4.
- Caveat: aggressive MSRV (1.98 in 0.11.9); Stalwart moves fast and bumps it.

### 2.4 MIME building

- [`mail-builder`](https://github.com/stalwartlabs/mail-builder) 1.0.0
  (2026-09-12, MIT/Apache): RFC 5322 + MIME; auto-picks optimal
  transfer encoding; nested structures via `MimePart`; only dep `encodify`.
  Delta Chat builds messages with it.
- lettre's `Message::builder()` — widely used alternative; fine but less
  precise control over structure.

### 2.5 Authentication results

[`mail-auth`](https://github.com/stalwartlabs/mail-auth) 0.13.3
(2026-09-18, MIT/Apache):
- DKIM1: RFC 6376 sign/verify (RSA-SHA256, RSA-SHA1, Ed25519), ATPS (6541),
  failure reports (6651).
- **DKIM2** (draft-ietf-dkim-dkim2-spec + draft-chuang-dkim2-dns): chained
  per-hop signatures with envelope binding — cutting edge.
- ARC (8617): now opt-in (`arc` feature) since ARC is being reclassified
  Historic; DKIM2 is the intended successor.
- SPF (7208), DMARC incl. new DMARCbis RFCs 9989/9990/9991, ARF reports,
  TLS-RPT (8460), Authentication-Results (8601) build/parse, EAI auth (8616).
- Crypto backends: aws-lc-rs (default)/ring/rust-crypto (wasm); DNS:
  hickory (default) or DoH via reqwest; runs on wasm32.
- **No BIMI** (checked docs.rs item index). Client-side value: parsing
  `Authentication-Results` headers for display, plus verifying on imported
  archives.

### 2.6 JMAP

[`jmap-client`](https://github.com/stalwartlabs/jmap-client) 0.4.3
(2026-09-28, MIT/Apache): RFC 8620 (core), 8621 (mail), 8887 (WebSocket
subprotocol), draft-ietf-jmap-sieve-14; async + `blocking` feature;
EventSource and WebSocket push streams; request-builder helpers. The only
maintained Rust JMAP client — alternatives are the 2016 `jmap` crate
(abandonware), melib's internal "functional" JMAP backend, and Go's
`go-jmap` (used by aerc). pimalaya lists JMAP as "configure but no backend
yet" in neverest.

### 2.7 Supporting libraries

- **OAuth2**: [`oauth2`](https://github.com/ramosbugs/oauth2-rs) 5.0.0
  (2025-01-21, MIT/Apache) — standard, runtime-agnostic; pair with platform
  credential storage (macOS Keychain via `security-framework`, Secret
  Service on Linux via `oo7`/`keyring`) and a loopback-redirect or
  device-flow helper for Gmail/MS app registrations.
- **TLS**: `rustls` 0.23.45 (Apache/ISC/MIT) + `rustls-platform-verifier`
  for OS trust stores vs `native-tls` 0.2.18 → `security-framework` 3.7.0 on
  macOS. Native TLS gives you system trust anchors *and* client certs/smart
  cards (relevant to corporate/Exchange users); rustls is uniform across
  platforms and required for certificate pinning control. Plan to support
  both behind a feature, as pimalaya does (`rustls-ring`/`rustls-aws`/
  `native-tls` features).
- **DNS**: `hickory-resolver`/`hickory-proto` 0.26.3 (MIT/Apache) — SRV/MX/
  TXT lookups for autoconfig (RFC 6186), DoH/DoT/DoQ support; renamed from
  trust-dns (trust-dns-resolver 0.23.2 is frozen).
- **HTML**: `ammonia` 4.x (MIT/Apache, html5ever-based allowlist sanitizer)
  for sanitizing; `lol_html` 3.0.1 (BSD-3, Cloudflare streaming rewriter)
  for rewriting cid:/remote-resource links, link-target rewriting, CSS
  stripping; `html5ever` 0.40.1 under both; `html2text` 0.17.2 (MIT) or
  `htmd` 0.5.5 (Apache) for text alternatives. mail-parser also does a basic
  HTML→text conversion internally.
- **Charset**: `encoding_rs` 0.8.42 (+`chardetng` 1.0.0 detection) — the
  WHATWG set; mail-parser's `full_encoding` uses it.
- **Sieve**: [`sieve-rs`](https://github.com/stalwartlabs/sieve) 1.0.2 —
  **AGPL-3.0-only**; implements every registered Sieve extension (RFC list
  in README incl. MIME part tests 5703, include 6609, enotify 5435,
  vacation, regex draft); bytecode-compiled, fuzzed. Commercial license via
  licensing@stalw.art. Flag: do not casually depend on this in a product
  that may be sold closed-source.
- **iCalendar/vCard**: [`calcard`](https://github.com/stalwartlabs/calcard)
  0.3.14 (MIT/Apache) — iCal + vCard + JSCalendar + JSContact with lossless-ish
  conversions, RRULE expansion, IANA timezone resolution, fuzzed+Miri;
  alternative `icalendar` 0.17.14 (MIT/Apache, hoodie) + `vparser` 1.2.1 (ISC).
- **OpenPGP**: [`pgp`](https://github.com/rpgp/rpgp) (rpgp) 0.21.0
  (2026-10-03, MIT/Apache) — RFC 9580 incl. v6 keys, AEAD, `pqc` feature
  (RFC 9980); used by Delta Chat and himalaya; low-level API — semantics
  (trust/expiration) are on you. [`sequoia-openpgp`](https://gitlab.com/sequoia-pgp/sequoia)
  2.4.1 — **LGPL-2.0-or-later**, the reference implementation with much
  higher-level semantics (cert store, policy engine); `sequoia-sop`/`rsop`
  CLIs exist. `gpgme` 0.11.0 bindings exist but need system libgpgme —
  poor fit for a shipped app.
- **S/MIME**: no production crate. RustCrypto's `cms`/`x509-cert`/`pkcs7`
  are building blocks; `openssl` crate is the pragmatic escape hatch.
  **Gap.**
- **SQL**: `rusqlite` 0.40.2 (MIT; `bundled`, `bundled-sqlcipher` —
  Delta Chat uses sqlcipher for at-rest encryption); `sqlx` 0.9.0
  (MIT/Apache, async, compile-checked queries, MSRV 1.94); `diesel` 2.3.14
  (MIT/Apache, sync ORM).
- **Search**: `tantivy` 0.26.2 (MIT) — embedded Lucene-class index, BM25,
  facets, columnar store; alternative: SQLite FTS5 built into rusqlite
  `bundled`. Xapian is GPL-2.0 — out for a commercial product.
- **Local formats**: `maildir` 0.6.6 (0BSD, staktrace), `maildirs` 0.2.2
  (MIT, pimalaya), `mbox` 0.7.1 (MIT, stale but stable), mail-parser's own
  mbox iterator; `notmuch` bindings 0.8.1 — **GPL-3.0+**.

### 2.8 Confirmed gaps (things Sweetbriar must write or fund)

1. **Sync engine** — incremental UID/MODSEQ sync, UIDVALIDITY handling,
   deleted-message reconciliation, offline outbox/draft queue, partial-body
   fetch policy. Neverest (below) is the only in-progress reusable one.
2. **Local store** — schema, blob vs DB split, threading index
   (jwz/RFC 5256-based), dedup by Message-ID.
3. **IMAP NOTIFY/OBJECTID, full IMAP4rev2** — upstreamable contributions.
4. **TNEF** — port ytnef or write new.
5. **S/MIME** — build on RustCrypto `cms` or OpenSSL.
6. **Exchange/M365** — `ews` (MPL-2.0) is a start; Graph mail coverage is
   thin everywhere (Thunderbird's `ms_graph_tb` is in-tree; neverest's
   `msgraph` feature is young).
7. **Proton** — Bridge is GPL-3.0; needs own bridge or API-level access.
8. **Gmail API backend** — label↔mailbox mapping is design work, not a crate.
9. **Autoconfig** — Thunderbird autoconfig/ISPDB + DNS SRV (hickory) +
   Microsoft autodiscover + PACC; neverest implements several — reuse or
   port that logic (MIT).
10. **CardDAV/CalDAV** — no mature client crate; neverest has DAV backends
    in progress.
11. **Sieve-compatible rules** — AGPL blocker; write your own or license it.

## 3. Findings — complete engines and clients

### 3.1 Delta Chat core — `chatmail/core` (ex deltachat-core-rust)

- **License**: MPL-2.0 ([LICENSE](https://github.com/chatmail/core/blob/main/LICENSE)).
- **Stack** (from its Cargo.toml): `async-imap` 0.12 (tokio, `compress`),
  `async-smtp` 0.10, `mail-builder` 1.0, `mailparse`, `pgp` 0.21 (`pqc`
  feature), `rusqlite` + **sqlcipher**, `async-native-tls`.
- **Architecture**: tokio scheduler with per-folder IMAP loops (INBOX
  IDLE-based watcher, MVBOX/"DeltaChat" folder watcher, sentbox) plus an SMTP
  send loop and a connectivity store — one account context driving multiple
  connections; pushes events to UIs over FFI (used by the desktop
  Electron app, Android and iOS native apps — the exact Rust-core/native-UI
  shape Sweetbriar wants).
- **Data model**: SQLite per account (sqlcipher-encrypted); chats/messages/
  contacts/blobs; IMAP UIDs mapped to local message ids; multi-device aware.
- **Sync strategy**: IMAP UID + UIDVALIDITY tracking, IDLE push with
  fallback polling, download-on-demand bodies.
- **Strengths**: production-hardened at user scale; encrypted store;
  handles weird servers; good reference for per-connection task layout.
- **Weaknesses for reuse**: chat-first data model (no folder fidelity, no
  general-purpose flags/labels UI semantics), Autocrypt-centric crypto,
  protocol coverage limited to what Delta Chat needs.
- **Reuse verdict**: design reference and proof-of-concept; melib-style
  linking is not needed (MPL is friendly) but its model is too specific to
  lift wholesale.

### 3.2 meli / melib

- **License**: EUPL-1.2 OR GPL-3.0-or-later ([melib README](https://github.com/meli/meli)).
- **Versions**: meli/melib 0.8.13 (2026-01-05).
- **Backends**: IMAP (via imap-codec — full), Maildir (full), notmuch
  (full), mbox (read-only), JMAP (functional), NNTP (functional),
  ManageSieve, integrated SMTP; rusqlite caches; GPGME via libgpgme;
  native-tls or vendored OpenSSL; isahc/curl HTTP.
- **IMAP**: CONDSTORE, ENABLE, IDLE, COMPRESS=DEFLATE, OAuth2
  (password-command hook), UIDStore + watch loops + offline cache — the
  most complete client-side IMAP implementation in Rust.
- **Strengths**: real "engine" structure (MailBackend trait, watchers,
  extension negotiation, per-account UID store); mature MIME `Envelope`
  model with threading.
- **Weaknesses**: copyleft; TUI-shaped APIs; single-maintainer.
- **Reuse verdict**: study `uid_store`, `watch`, `cache` internals for the
  sync-engine design; cannot be shipped in a non-copyleft product. (EUPL is
  also an unusual pick — OSI-approved copyleft, effectively GPL-equivalent.)

### 3.3 pimalaya (soywod) — io-imap, io-smtp, email-lib, himalaya, neverest

- **License**: MIT (io-* crates MIT/Apache-2.0).
- **Crates**: `io-imap` 0.7.1, `io-smtp` 0.6.0, `email-lib` 0.27.0
  (umbrella config/backends/templates), `maildirs` 0.2.2, `pgp-lib` 1.0.0,
  `himalaya` CLI 2.2.1 (2026-10-02).
- **neverest** ([repo](https://github.com/pimalaya/neverest), v0.x,
  unreleased): a PIM *sync engine* CLI/lib — mail via IMAP **and Microsoft
  Graph**, contacts via CardDAV/Graph/Google People, calendar via
  CalDAV/Graph/Google; local **pimdir** store (SQLite, `vendored` feature);
  retention (deleted items kept, reclaimed on schedule); server-to-server
  relay streaming; queued submission; auth matrix
  anonymous/login/plain/oauthbearer/xoauth2/scram-sha-256 + DAV bearer;
  discovery via provider rules, PACC, Thunderbird autoconfig, RFC 6186 SRV,
  RFC 6764 DAV. JMAP configured but no backend yet.
- **Strengths**: the most permissively licensed usable stack; sans-I/O
  layering maps cleanly onto "core + native UI"; neverest is literally the
  problem Sweetbriar must solve, attacked the same way (MIT code to study
  or collaborate on); funded through 2027 by NGI Zero Commons Fund.
- **Weaknesses**: everything <1.0 and churning (imap-client→io-imap rename,
  breaking changes); essentially one author; no rev2 yet.
- **Reuse verdict**: highest-value reuse candidates in the whole survey —
  io-imap for the client layer, neverest's design (and possibly code) for
  sync; a collaboration/contribution relationship is plausible given the
  NLnet funding model.

### 3.4 Stalwart Labs (server AGPL/SEL; crates mostly Apache-2.0/MIT)

- The `stalwart` server repo carries `LICENSES/AGPL-3.0-only.txt` +
  `LicenseRef-SEL.txt` (Stalwart Enterprise License).
- Standalone client-relevant crates are **all Apache-2.0/MIT except
  sieve-rs (AGPL-3.0-only)**: mail-parser, mail-auth, mail-builder,
  mail-send, jmap-client, calcard, encodify, hashify.
- Battle-tested at server scale; adopts new specs first (DKIM2 draft,
  DMARCbis RFC 9989–9991, JSCalendar/JSContact).
- **Reuse verdict**: use the permissive crates freely; treat sieve-rs as
  either a buy-out or a reimplementation trigger; note these are
  server-shaped (e.g., mail-auth wants DNS access a client may not have).

### 3.5 Mozilla Thunderbird (Rust components, MPL-2.0)

- `comm/rust` workspace (mirror: [thunderbird/thunderbird-desktop](https://github.com/thunderbird/thunderbird-desktop),
  `rust/` at root; pushed 2026-10-11): members `ews_xpcom`, `graph_xpcom`,
  `ms_graph_tb`, `moz_http`, `protocol_shared`, `mailnews_string_glue`,
  `mailnews_ui_glue`, `xpcom_async_glue`, `sys_tray`, `gkrust`, `gtest`.
  Workspace license MPL-2.0, Rust 1.90, edition 2024.
- **`ews`** = [thunderbird/ews-rs](https://github.com/thunderbird/ews-rs):
  "A Rust crate for interacting with the Exchange Web Services API", MPL-2.0,
  on crates.io (0.1.2), pushed 2026-10-06 — the only reusable Rust EWS
  client, shipping in Thunderbird (EWS support landed around TB ~145/146;
  see [bug 1993555](https://bugzilla.mozilla.org/show_bug.cgi?id=1993555)
  and the [Rust-in-Thunderbird docs](https://source-docs.thunderbird.net/en/latest/rust/index.html)).
- `ms_graph_tb`: in-tree typed Microsoft Graph client (serde/serde_with/
  thiserror) — not on crates.io, MPL-2.0.
- Thunderbird's IMAP/POP3/SMTP remain C++; the Rust effort went where the
  standards gap was (Exchange/Graph) — same priority logic applies to
  Sweetbriar.
- Storage context: Thunderbird still uses mbox + .msf + gloda global SQLite
  FTS — architecture to *avoid* copying (decades of compromises), while its
  protocol crates are the opposite: fresh, typed, reusable.

### 3.6 Mailspring Mailsync (GPL-3.0)

- [Mailspring-Sync](https://github.com/Foundry376/Mailspring-Sync): C++11
  per-account sync process, MailCore2 IMAP/SMTP, sqlite3 store, GPLv3;
  spawned/restarted by the Electron app; emits model changes as JSON lines
  on stdout; takes "tasks" on stdin that split into an immediate local op
  and a retryable remote op.
- **Sync design** (from its README — worth reading in full): background
  worker iterating folders using CONDSTORE/QRESYNC when available, else
  local/deep sync; foreground worker IDLEs on the primary folder; bodies
  fetched only for the last ~3 months (headers for older); stable IDs by
  header hashing; "fat table" = JSON blob column + queryable columns;
  everything inside sqlite transactions so a force-kill is safe; Gmail =
  All Mail/Spam/Trash + X-GM-LABELS instead of virtual mailboxes.
- **Reuse verdict**: GPL means reference-only, but its architecture doc is
  the best written description of a desktop mail sync engine anywhere —
  steal the *design* (process-per-account → connection-per-folder, task
  queue split local/remote, crash-safe sqlite).

### 3.7 Geary (LGPL-2.1-or-later, Vala)

- [GNOME/geary](https://gitlab.gnome.org/GNOME/geary): async `engine/`
  library — `imap/` (own IMAP stack in Vala), `imap-db/` (folder DB with
  GC), `imap-engine/` (replay operations, account synchronizer), `db/`
  (VersionedDatabase over SQLite, incl. in-memory test DBs), `rfc822/`;
  app layer adds `App.Conversation` threading. License: LGPL-2.1+
  (COPYING).
- **Reuse verdict**: LGPL links OK for commercial apps in principle, but
  it's Vala/GObject — value is the design: a *versioned* SQLite schema,
  replayable engine operations, GC of stale message parts.

### 3.8 Evolution Data Server / Camel (LGPL-2.0-only, C)

- [EDS](https://gitlab.gnome.org/GNOME/evolution-data-server): Camel
  provider framework; per-store `folders.db` SQLite — one summary table per
  folder of lightweight `CamelMessageInfo` rows + a `folders` table
  ([CamelDBSummary design](https://wiki.gnome.org/Apps(2f)Evolution(2f)CamelDBSummary.html),
  [Camel docs](https://gnome.pages.gitlab.gnome.org/evolution-data-server/camel/));
  license LGPL-2.0-only (docs header).
- **Reuse verdict**: the folder-summary-table design is the thing to copy —
  it is exactly the "envelope index" pattern Apple Mail also uses.

### 3.9 notmuch & mu (GPL-3.0+, C++ on Xapian GPL-2.0+)

- Tag/maildir indexers, not engines: immutable maildir + separate Xapian
  index; queries as tags. `notmuch` Rust bindings exist (vhdirk, GPL-3.0+).
- **Reuse verdict**: architecture lesson (separate the message corpus from
  the index; the index is disposable and rebuildable) and a license wall —
  replicate with tantivy/SQLite FTS5 instead.

### 3.10 aerc (MIT, Go)

- [aerc](https://git.sr.ht/~rjarry/aerc): MIT; worker-per-backend model —
  IMAP, Maildir, notmuch, mbox, and **JMAP** (added via
  ~rockorager/go-jmap; RFC 8620/8621, basic + OAUTHBEARER auth, send via
  JMAP submission) — confirmed in its changelog/docs (aerc-jmap(5)).
- **Reuse verdict**: not linkable (Go) but the backend-process isolation
  design is a proven answer to "one account's failure shouldn't stall the
  app".

### 3.11 K-9 Mail / Thunderbird for Android (Apache-2.0)

- [thunderbird-android](https://github.com/thunderbird/thunderbird-android):
  Apache-2.0; modular Gradle build with `backend:imap`, `backend:jmap`,
  `backend:pop3`, `feature:autodiscovery` (Thunderbird autoconfig),
  `feature:account:storage` (SQLite `MessageStore`), own MIME module
  (mime4j-descended), FSYNC push.
- **Reuse verdict**: not reusable (JVM), but it is the reference for
  mobile-grade incremental sync, account migrations, autodiscovery UX, and
  what a second JMAP client implementation looks like.

### 3.12 Apple Mail local store (proprietary)

- `~/Library/Mail/V{2..10}/MailData/Envelope Index` = SQLite with
  `envelopes`, `subjects`, `addresses`, `attachments`, `threads`,
  `mailboxes` tables; messages as numbered `.emlx` files (byte-count line +
  RFC 5322 + plist footer; `.partial.emlx` = headers-only) inside `.mbox`
  package dirs; per-message attachments in sibling `Attachments/` dirs.
- Sources: [macfor docs](https://docs.macfor.io/plugins/mail-apple/),
  [apple-mail-mcp architecture](https://imdinu.github.io/apple-mail-mcp/architecture-deep-dive/),
  [javarants Envelope Index walkthrough](https://javarants.com/build-your-own-mail-analyzer-for-mac-mail-app-747143e94ccc).
- **Reuse verdict**: an import/migration target and a proven local-store
  shape (envelope index DB + immutable per-message files); also the format
  to stay compatible with for "move from Apple Mail" onboarding.

### 3.13 Honorable mentions

- **mox** ([mjl-/mox](https://github.com/mjl-/mox), MIT, Go): modern
  all-in-one mail server whose `imapclient` package + extensive RFC notes
  make it a top-tier implementation reference.
- **Proton Bridge** ([proton-bridge](https://github.com/ProtonMail/proton-bridge),
  GPL-3.0): local IMAP/SMTP→Proton-API bridge; the model Sweetbriar wants
  for Proton accounts, but GPL — reference or protocol-level reimplementation
  only (see also `gopenpgp`).
- **trojitá** (C++/Qt, GPL): historically the most extension-complete IMAP
  client (early QRESYNC/COMPRESS/rev2 adopter); read for edge cases
  (unverified current maintenance status).
- **Nylas sync engine** (dead project): ancestor of Mailsync.

## 4. Options compared

### 4.1 IMAP client layer

| Option | Coverage | Async | License | Verdict |
|---|---|---|---|---|
| imap-codec + imap-next (+own client) | rev1+exts; rev2 in progress | sans-I/O | MIT/Apache | Best substrate; you write the client — acceptable, it's the differentiator |
| io-imap | widest ext list (CONDSTORE/QRESYNC/IDLE/MOVE/SORT/SASL) | sans-I/O + blocking + full | MIT/Apache | Adopt or vendor; <1.0 churn risk |
| async-imap | CONDSTORE/MOVE/UIDPLUS/IDLE/COMPRESS/METADATA | tokio/async-std | MIT/Apache | Proven bootstrap; you'll outgrow it |
| melib::imap | most complete incl. watchers | async | EUPL/GPL | Reference only |
| imap (jonhoo) | basics | sync | MIT/Apache | Skip — unmaintained |

### 4.2 Storage/search stack

| Layer | Options | Pick |
|---|---|---|
| Primary DB | rusqlite (bundled / bundled-sqlcipher) vs sqlx vs diesel | rusqlite — the store is schema-first, not ORM-shaped; sqlcipher gives at-rest encryption proven by Delta Chat |
| FTS | tantivy (MIT) vs SQLite FTS5 (in-tree) vs Xapian (GPL — out) | FTS5 to ship; tantivy when search must scale/impress |
| Message corpus | maildir-like files + envelope-index DB (Apple Mail/EDS/maildir crate patterns) vs blobs-in-DB | files + DB: crash-safe, exportable, Spotlight/backup friendly |

### 4.3 License ledger (every copyleft flag)

| Component | License | Commercial-friendly? | Action |
|---|---|---|---|
| imap-codec/types/next, io-imap, async-imap, imap-proto | MIT/Apache-2.0 | yes | use |
| lettre, mail-send, mail-parser, mailparse, mail-builder, mail-auth, jmap-client, calcard, encodify | MIT/Apache-2.0 | yes | use |
| **sieve-rs** | **AGPL-3.0-only** | **no** (unless buy SEL license) | avoid/reimplement/license |
| **melib** | EUPL-1.2 OR GPL-3.0+ | **no** | reference only |
| **sequoia-openpgp** | LGPL-2.0+ | linking OK w/ conditions; risky optics | prefer `pgp` |
| **notmuch crate/lib, Xapian** | GPL-3.0+/GPL-2.0+ | **no** | avoid |
| Mailsync, Proton Bridge | GPL-3.0 | **no** | reference only |
| ews, ms_graph_tb, Delta Chat core | MPL-2.0 | yes (file-level; publish modifications) | use/reference; plan upstreaming |
| rusqlite, sqlx, diesel, tantivy, maildir, maildirs, mbox | MIT/Apache/0BSD | yes | use |
| ammonia, lol_html, html2text, htmd, encoding_rs, chardetng | MIT/Apache/BSD | yes | use |
| oauth2, hickory-*, rustls, native-tls, security-framework | MIT/Apache/ISC | yes | use |
| pgp (rpgp), rsop | MIT/Apache | yes | use |
| icalendar, vparser, ics | MIT/Apache/ISC | yes | use |
| gpgme bindings | MIT/Apache | depends on libgpgme (LGPL) | avoid for shipped app |
| Stalwart server | AGPL-3.0/SEL | n/a (don't embed) | — |

## 5. Recommendations for Sweetbriar

| Layer | Primary pick | Fallback | Rationale |
|---|---|---|---|
| IMAP codec | `imap-types`+`imap-codec` 2.x | — | correctness, fuzzing, zero-copy |
| IMAP flow | `imap-next` | — | sans-I/O = fits any runtime/UI split |
| IMAP client | own thin client on imap-next; **vendor/track `io-imap`** | `async-imap` to bootstrap | io-imap's coverage is a roadmap of what to keep |
| SMTP | `mail-send` (+DKIM sign) | `lettre`, `io-smtp` | DKIM2-forward; tokio-only caveat |
| MIME parse | `mail-parser` (+`full_encoding`) | `mailparse` | SIMD, lazy, RFC8621 model |
| MIME build | `mail-builder` | lettre `Message` | pairs with parser |
| Auth results | `mail-auth` | — | DKIM/SPF/DMARC/ARC + DMARCbis |
| JMAP | `jmap-client` | — | only serious option |
| EWS | `ews` (thunderbird/ews-rs, MPL-2.0) | own SOAP client | only Rust EWS client; file-copyleft OK |
| MS Graph | study `ms_graph_tb` + neverest `msgraph`; build own thin client | — | both are young; Graph mail is huge |
| OAuth2 | `oauth2` + `keyring`/`security-framework` | — | standard |
| TLS | `rustls`+`rustls-platform-verifier`; `native-tls` feature for enterprise client certs | — | |
| DNS | `hickory-resolver` | — | SRV/MX autoconfig |
| HTML | `ammonia` sanitize + `lol_html` rewrite (cid/remote-block) | — | |
| HTML→text | `html2text` | mail-parser built-in | |
| Charset | `encoding_rs`+`chardetng` | — | |
| Sieve | avoid `sieve-rs` (AGPL) — own rules engine, or commercial license | — | AGPL conflict |
| iCal/vCard | `calcard` | `icalendar`+`vparser` | one dep incl. JS* conversion |
| OpenPGP | `pgp` (rpgp) | `sequoia-openpgp` (LGPL) | permissive + v6/PQC-ready |
| S/MIME | build on RustCrypto `cms`/`x509-cert` | `openssl` crate | gap — budget time |
| Store | `rusqlite` bundled (+sqlcipher option) | `sqlx` | envelope-index pattern: files + DB |
| Search | SQLite FTS5 → `tantivy` | — | MIT; upgrade path clear |
| Local fmt | `maildir` crate for export/interop | own | |
| Proton | own bridge/API backend | — | Bridge is GPL |
| Sync engine | **build** (neverest is the closest precedent — MIT) | contribute/collab | core differentiator |
| Threading | **build** (jwz + RFC 5256) | — | |
| Rules/filters | **build** Sieve-subset | license sieve-rs | AGPL |
| TNEF | **build/port** | — | |
| Autoconfig | build (Thunderbird autoconfig+SRV RFC 6186+autodiscover); neverest has reusable logic (MIT) | — | |

**Trade-offs and the strongest counter-argument.**

The recommendation that carries the most risk is betting Sweetbriar's IMAP
foundation on the duesee/pimalaya stack: `imap-codec` 2.x is still alpha
(years in alpha), `imap-next` is 0.3, `io-imap` is 0.7 and was renamed
mid-flight; all are effectively maintained by two individuals (NLnet-funded,
but still). The counter-position: **`async-imap` + `imap-proto` is what
actually ships to millions of Delta Chat users** — boring, proven, MIT —
and its missing pieces (QRESYNC, NOTIFY, OBJECTID) are exactly the pieces a
sync engine implements itself anyway. The mitigation resolves most of the
tension: keep a `Backend`/`MailTransport` trait boundary so the wire stack
is replaceable, use `imap-codec`/`imap-types` for the durable type layer
(melib proves this combination works), and treat the client wrapper —
io-imap or your own — as the swappable part.

A second real trade-off: `mail-parser` is fast and RFC-exact but enforces
limits (depth/parts/nesting) and an aggressive MSRV; `mailparse` is dumber
and proven in production by Delta Chat. Use mail-parser, keep mailparse in
mind for pathological corpora.

And the strategic one: **Sieve** — a Mailsmith-style rules engine is one of
Sweetbriar's stated goals, and the only Rust Sieve implementation is AGPL.
Either budget a small interpreter (a Sieve-subset compiler is a few weeks,
not months — most of sieve-rs's size is the 20+ RFC extension surface a
client doesn't need), negotiate the Stalwart commercial license, or accept
GPL-ing the app. Decide before designing the rules UX.

## 6. Risks and open questions

- **IMAP4rev2 timing**: if rev2 lands upstream (imap-codec) within the next
  year the bet pays off; if it stalls, Sweetbriar ships rev1+extensions —
  functionally equivalent on Gmail/Dovecot/Stalwart but check O365/Gmail
  rev2-only features (none currently required by servers — verify again at
  ship time).
- **Single-maintainer dependencies**: duesee, soywod (pimalaya), staktrace
  — plan a vendor/fork budget; NLnet funding partially de-risks it.
- **MS Graph surface**: `ms_graph_tb` is Thunderbird-shaped and young;
  Microsoft Graph mail API breadth (delta sync, MIME vs properties) needs a
  spike before committing the Exchange story.
- **Proton**: only GPL code exists; legal/terms check on API reuse needed —
  engineering estimate is meaningless until that's settled.
- **Gmail API vs IMAP**: label model mismatch is a design problem
  (Mailsync's approach: treat Gmail labels as metadata on All Mail) — decide
  per-backend early because it shapes the store schema.
- **sieve-rs/sequoia/Xapian licenses**: see ledger — any decision to
  open-source Sweetbriar later *expands* options but a commercial fork stays
  blocked on these.
- **Data safety**: Delta Chat's sqlcipher and Mailsync's
  everything-in-a-transaction are the two proven patterns; pick consciously
  (key management for sqlcipher is non-trivial on a client).
- **Open questions for follow-up research**: OAuth app registrations/
  verification costs for Gmail+MS365; Apple Mail schema drift across V10
  builds; whether io-imap's coroutine model composes with Swift's
  structured concurrency over FFI (unverified); real-world QRESYNC coverage
  (which servers actually support it — mostly Dovecot/Cyrus).

## 7. Sources consulted

Crate metadata: [crates.io API](https://crates.io) (per-crate version/license
endpoints, fetched 2026-10-11).

Repositories/docs fetched directly:

- https://github.com/duesee/imap-codec (+Cargo.toml, CHANGELOG, issues
  [#702](https://github.com/duesee/imap-codec/issues/702),
  [#704](https://github.com/duesee/imap-codec/issues/704))
- https://github.com/duesee/imap-next , https://github.com/duesee/imap-sec
- https://github.com/async-email/async-imap (README, CHANGELOG, Cargo.toml,
  docs.rs source of client.rs/extensions/)
- https://github.com/djc/imap-proto
- https://github.com/jonhoo/rust-imap
- https://github.com/pimalaya/io-imap , https://github.com/pimalaya/imap-client
  (move notice), https://github.com/pimalaya/neverest , io-smtp, himalaya
- https://github.com/stalwartlabs/mail-parser , mail-auth , mail-builder ,
  mail-send , jmap-client , sieve , calcard , https://github.com/stalwartlabs/stalwart (LICENSES/)
- https://github.com/lettre/lettre , https://github.com/async-email/async-smtp
- https://github.com/rpgp/rpgp , https://gitlab.com/sequoia-pgp/sequoia
- https://github.com/ramosbugs/oauth2-rs
- https://github.com/staktrace/mailparse , https://github.com/staktrace/maildir
- https://git.meli-email.org/meli/meli (melib README, imap.rs source,
  meli.conf(5) man page)
- https://github.com/chatmail/core (LICENSE, Cargo.toml, scheduler.rs)
- https://github.com/thunderbird/thunderbird-desktop (rust/ workspace
  Cargo.toml, ews_xpcom, ms_graph_tb),
  https://github.com/thunderbird/ews-rs ,
  https://source-docs.thunderbird.net/en/latest/rust/index.html ,
  https://bugzilla.mozilla.org/show_bug.cgi?id=1993555
- https://github.com/Foundry376/Mailspring , Mailspring-Sync (README)
- https://gitlab.gnome.org/GNOME/geary (COPYING, engine/),
  https://gitlab.gnome.org/GNOME/evolution-data-server ,
  https://wiki.gnome.org/Apps(2f)Evolution(2f)CamelDBSummary.html ,
  https://gnome.pages.gitlab.gnome.org/evolution-data-server/camel/
- https://git.sr.ht/~rjarry/aerc (LICENSE, CHANGELOG, aerc-jmap(5))
- https://github.com/thunderbird/thunderbird-android (LICENSE, settings.gradle.kts)
- https://raw.githubusercontent.com/notmuch/notmuch/master/COPYING ,
  https://raw.githubusercontent.com/djcb/mu/master/COPYING
- https://github.com/mjl-/mox (LICENSE.MIT)
- https://github.com/ProtonMail/proton-bridge (LICENSE)
- https://docs.macfor.io/plugins/mail-apple/ ,
  https://imdinu.github.io/apple-mail-mcp/architecture-deep-dive/ ,
  https://javarants.com/build-your-own-mail-analyzer-for-mac-mail-app-747143e94ccc ,
  https://bigbluemachine.wordpress.com/2012/11/10/osxmail/
- https://docs.rs for async-imap/melib/mail-auth source inspection
- https://www.rfc-editor.org/rfc/rfc9051 (IMAP4rev2 scope)

*Unverified items flagged inline: trojitá maintenance status, K-9's JMAP
backend maturity, Gmail/MS OAuth registration costs.*
