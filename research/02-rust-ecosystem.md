# 02 — The Rust email ecosystem: reuse vs build

Based on [raw/rust-ecosystem/devin.md](raw/rust-ecosystem/devin.md) (with
its notes in [raw/rust-ecosystem/devin-notes/](raw/rust-ecosystem/devin-notes/)),
cross-checked against the architecture, standards and data-safety reports.
Versions as of 2026-10-11.

## Summary

- **Codecs and MIME are solved.** The sync engine, local store,
  threading, unified search, rules and the Microsoft/Proton backends are
  not. That is where Sweetbriar's value lives.
- Keep dependencies **compatible with a commercial option**: MIT/Apache/BSD,
  audited MPL-2.0 (file-level copyleft, not permissive) and dynamically
  linked LGPL platform libraries, with a per-component licence inventory
  (revised after round 1). The traps: `sieve-rs` (AGPL), `melib`
  (EUPL/GPL), notmuch/Xapian/mu (GPL), Mailsync and Proton Bridge (GPL),
  `sequoia-openpgp` (LGPL).
- Wrap every wire stack behind Sweetbriar's own `AccountBackend` boundary,
  so single-maintainer crates stay swappable.

## Picks per layer

| Layer | Pick | Fallback | Notes |
|---|---|---|---|
| IMAP types/codec | `imap-types` + `imap-codec` (2.x alpha, duesee, NLnet) | — | Fuzzed, zero-copy, misuse-resistant; `quirk_*` flags for broken servers; rev2 being assembled |
| IMAP flow | `imap-next` (sans-I/O) | — | Runtime-agnostic, fits the engine/FFI split |
| IMAP client | Own thin client on `imap-next`; track/vendor `io-imap` 0.7 (pimalaya) | `async-imap` 0.12 (Delta Chat) to bootstrap | io-imap: CONDSTORE+QRESYNC, IDLE, MOVE, UIDPLUS, SORT/THREAD, XOAUTH2/OAUTHBEARER, SCRAM. async-imap: no QRESYNC |
| SMTP | `lettre` 0.11 or `mail-send` 0.6 (DKIM signing, XOAUTH2) | `io-smtp` | mail-send is Tokio-only and lacks OAUTHBEARER |
| MIME parse | `mail-parser` 0.11 (Stalwart) with `full_encoding` | `mailparse` 0.18 | SIMD, lazy, 41 charsets incl. CJK, RFC 8621 body model, fuzzed + Miri; aggressive MSRV; Stalwart CVE-2026-26312 (cyclic MIME traversal) was fixed in the server; the safe `mail-parser` version is not established, so bound consumers |
| MIME build | `mail-builder` 1.0 | lettre `Message` | |
| Auth results | `mail-auth` 0.13 | — | DKIM, DKIM2 draft, ARC, SPF, DMARCbis (RFC 9989–9991); no BIMI |
| JMAP | `jmap-client` 0.4 | — | Core, mail, WebSocket, EventSource; the only maintained Rust client |
| EWS (on-prem) | `ews` (Thunderbird, MPL-2.0) | own SOAP | The only Rust EWS client, shipping in Thunderbird |
| Graph | Own thin client; study Thunderbird's in-tree `ms_graph_tb` and pimalaya `neverest` | — | Both are young |
| OAuth | `oauth2` 5.0 | — | Plus platform credential storage |
| TLS | `rustls` + `rustls-platform-verifier`; `native-tls` feature for enterprise client certificates | — | 1Password uses rustls + platform verifier (2026) |
| DNS | `hickory-resolver` | — | SRV/MX for autoconfig |
| HTML | `ammonia` (sanitize) + `lol_html` (rewrite cid/remote) | — | `html2text` / mail-parser for text alternatives |
| Charset | `encoding_rs` + `chardetng` | — | WHATWG set |
| Calendar/contacts | `calcard` 0.3 | `icalendar` + `vparser` | iCal, vCard, JSCalendar, JSContact |
| OpenPGP | `pgp` (rpgp) 0.21 | `sequoia-openpgp` (LGPL) | RFC 9580, v6 keys, PQC feature |
| S/MIME | **gap**: RustCrypto `cms`/`x509-cert` | `openssl` crate | Or Security.framework on macOS |
| Store | `rusqlite` 0.40, bundled SQLite ≥ 3.51.3 | `sqlx` | Optional `bundled-sqlcipher` |
| Search | SQLite FTS5 → `tantivy` 0.26/0.27 | — | See [05-search](05-search.md) |
| Local formats | `maildir` (0BSD), `maildirs`, `mbox` | own | Export and import |

## What Sweetbriar must build

1. The **sync engine**: incremental UID/MODSEQ and provider deltas,
   UIDVALIDITY, reconciliation, the intent queue, partial-body policy.
   pimalaya's `neverest` (MIT; IMAP + Graph + DAV + a local store,
   NLnet-funded to 2027) is the closest precedent and a possible partner.
2. The **local store and threading** (JWZ / RFC 5256, provider hints).
3. **IMAP NOTIFY and OBJECTID**, and full IMAP4rev2: upstream contributions.
4. A **TNEF decoder** from [MS-OXTNEF] (about 2k lines; the only crate died
   in 2019).
5. **S/MIME.**
6. **Graph and Gmail API backends.** Label↔mailbox mapping is design work,
   not a crate.
7. **Proton:** Bridge-backed IMAP first; there is no permissive direct
   option ([06-account-backends](06-account-backends.md)).
8. **Autoconfig:** Thunderbird ISPDB data (MPL), SRV, Autodiscover;
   neverest's logic is MIT.
9. **CardDAV/CalDAV clients:** no mature crate.
10. A **rules engine**: the only Rust Sieve is AGPL. A Sieve-subset
    interpreter is weeks, not months. Decide before designing the rules UX
    ([11-classic-clients](11-classic-clients.md)).

## Engines worth studying (design, not code)

| Engine | Licence | Lesson |
|---|---|---|
| Delta Chat core (`chatmail/core`) | MPL-2.0 | Rust email engine under native UIs; per-folder IMAP loops; sqlcipher; JSON-RPC over FFI or stdio. Chat-shaped model, so not liftable |
| meli / melib | EUPL/GPL | Most complete client-side Rust IMAP (uid_store, watch, cache). Read, don't link |
| pimalaya (io-imap, neverest, himalaya) | MIT/Apache | Sans-I/O layering; sync engine in progress; collaboration candidate |
| Mailspring Mailsync | GPL-3 | Best written description of a desktop sync engine: CONDSTORE/QRESYNC background worker + IDLE foreground worker, bodies for recent ~3 months, all in transactions, Gmail via All Mail + X-GM-LABELS. Its header-hash ids can collide |
| Thunderbird Rust (ews, graph) | MPL-2.0 | New protocol work goes in Rust; its mbox/.msf storage is what not to copy |
| Geary | LGPL | Versioned schema, replayable engine operations, GC of stale parts |
| Evolution / Camel | LGPL | Per-folder summary tables (the "envelope index" pattern) |
| Apple Mail | proprietary | Envelope Index SQLite + immutable `.emlx`; an import target |
| notmuch / mu | GPL | The index is disposable; the corpus is truth |
| aerc | MIT (Go) | Worker per backend; JMAP client reference |
| K-9 / Thunderbird Android | Apache-2.0 | Mobile-grade incremental sync, autodiscovery UX |
| mox | MIT (Go) | Excellent RFC-annotated IMAP client reference |

## Risks

- `imap-codec` 2.x has been alpha since 2024. `io-imap` was renamed
  mid-2026. Two individuals maintain the IMAP stack (NLnet-funded). The
  mitigation is the trait boundary plus vendoring.
- `mail-parser` MSRV jumps (1.98 in 0.11.9) can force toolchain upgrades.
- A Sieve decision deferred too long shapes the rules UX by accident.

## Key sources

- [imap-codec](https://github.com/duesee/imap-codec), [io-imap](https://github.com/pimalaya/io-imap), [neverest](https://github.com/pimalaya/neverest), [async-imap](https://github.com/async-email/async-imap)
- [mail-parser](https://github.com/stalwartlabs/mail-parser), [mail-auth](https://github.com/stalwartlabs/mail-auth), [jmap-client](https://github.com/stalwartlabs/jmap-client), [sieve-rs licence](https://github.com/stalwartlabs/sieve)
- [thunderbird/ews-rs](https://github.com/thunderbird/ews-rs), [Mailspring-Sync README](https://github.com/Foundry376/Mailspring-Sync)
- [chatmail/core](https://github.com/chatmail/core), [meli](https://git.meli-email.org/meli/meli)
