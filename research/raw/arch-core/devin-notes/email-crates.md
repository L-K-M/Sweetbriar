# Rust Crates for an Email Client Core (Oct 2026)

## 1. IMAP Clients

| Crate | Version | Last Release | Async | Status / Fitness |
|---|---|---|---|---|
| `imap` (jonhoo) | 2.4.1 stable / 3.0.0-alpha.15 | Jan 2021 / Feb 2025 | No (sync) | Stable branch unmaintained since 2021; 3.0 alpha stalled. Avoid for new work. |
| `async-imap` (async-email) | 0.12.0 | 2026-09-30 | tokio + async-std | Active, 55 dependents. Built on `imap-proto` 0.17, MSRV 1.88 (Rust 2024). Best supported async client. |
| `imap-proto` (djc) | 0.17.0 | 2026-09-28 | nayer (codec layer) | Low-level parser/builder; the substrate async-imap uses. Actively maintained. |
| `imap-codec` (duesee) | 1.0.0 stable; 2.0.0-alpha.9 | 1.0: 2023-08; 2.0-a.9: 2026-07-19 | transport-agnostic (sync/async via features) | **1.0 shipped.** IMAP4rev1 + extensions, server+client, 91% test coverage, misuse-resistant types. 2.0 alpha extends further. Risk: single maintainer (bus factor 1), 58 open issues, 53 stars. |

**Verdict:** `async-imap` for a client today; `imap-codec` (+ `imap-types`) is the best foundation if you want to own the protocol layer (used by Stalwart's server). Both `imap` and the abandoned `tokio-imap` are dead ends.

## 2. SMTP / ESMTP

| Crate | Version | Last Release | Async | Notes |
|---|---|---|---|---|
| `lettre` | 0.11.23 | 2026-08-03 | tokio/async-std | De facto standard; 19.9M downloads, 460 dependents. Builder API, connection pooling, TLS via rustls/native-tls/boring, SMTP AUTH. Does **not** parse mail. Very active (monthly releases). |
| `mail-send` (stalwart) | 0.6.2 | 2026-08-18 | tokio only | DKIMv1 + DKIMv2 signing, XOAUTH2, mechanism negotiation (CRAM-MD5→DIGEST-MD5→XOAUTH2→LOGIN→PLAIN). Pairs with `mail-parser`/`mail-builder`. |

**Verdict:** `lettre` for sending unless you need DKIM signing, in which case `mail-send`.

## 3. MIME Parsing / Building

| Crate | Version | Last Release | Sync/Async | Notes |
|---|---|---|---|---|
| `mail-parser` (stalwart) | 0.11.9 | 2026-09-09 | sync parse | 4.2M downloads, 147 dependents. Zero-copy, 41 charsets, fuzzed + MIRI-tested, conforms to RFC 5322/2045-2049/2231/2557/6532 + JMAP RFC 8621 §4.1.4 body shape. Production-grade. |
| `mail-builder` (stalwart) | **1.0.0** | 2026-09-12 | sync build | **1.0 shipped Sep 2026.** 1.46M downloads. Automatic optimal encoding, fast base64, no deps. |
| `mailparse` (staktrace) | 0.18.0 | 2026-10-05 | sync | 15M downloads, 110 dependents. Simple MIME tree parser; good enough for clients, less RFC-complete than mail-parser. |
| `imap-proto` | 0.17.0 | 2026-09-28 | — | IMAP parser only, not general MIME. |

**Verdict:** `mail-parser` + `mail-builder` is the strongest combo (JMAP-aware, proven in Stalwart Mail Server at scale).

## 4. JMAP

| Crate | Version | Last Release | Async | Notes |
|---|---|---|---|---|
| `jmap-client` (stalwart) | 0.4.3 | 2026-09-28 | async + blocking feature | 85K downloads, 112 stars. Full RFC 8620 (Core), 8621 (Mail), 8887 (WebSocket); Sieve draft. EventSource + WebSocket streams. Only mature Rust JMAP client. |

**Adoption:** Fastmail production since 2019. Cyrus IMAP 3.12.4 implements Core+Mail+Blob+Quotas+Sieve+WebSocket; Calendars/Contacts in development. Stalwart became the first full JMAP server (incl. Calendars/Contacts) Oct 2025; Nextcloud Mail JMAP is in progress (issue #2931). RFC pipeline: 8620/8621 (2019), 8887 WebSocket, 9404 Blob, 9425 Quotas, 9661 Sieve, 9007 MDN, 9219 S/MIME. JMAP Contacts published ~Dec 2024; Calendars imminent.

## 5. Local Mail Search

| Option | Version | Notes |
|---|---|---|
| `tantivy` | 0.26.2 (2026-09-08) | Rust-native Lucene: BM25, mmap, incremental indexing, <10ms startup. 19.8M downloads, 326 dependents. Index ~0.3-0.5 GB per 1,000 emails with bodies. Used by Bichon (Rust email archiver), rusmes-search, ess. Best Rust choice. |
| SQLite FTS5 | (rusqlite) | ess uses SQLite as canonical store + Tantivy for search; FTS5 alone is decent for smaller corpora. |
| Blazegraph | — | JVM-based, not Rust-native; not worth it for a local client. |

**Verdict:** Tantivy (with SQLite for canonical state) is the standard pattern as of 2026.

## 6. CalDAV / CardDAV / EWS

| Crate | Version | Last Release | Notes |
|---|---|---|---|
| `libdav` | 0.11.0 | 2026-09-05 | CalDAV + CardDAV async client, service discovery, typed requests (pimsync). Only usable option. |
| `minicaldav` | — | 2025 | Tiny sync CalDAV client; self-described "neither correct nor sufficiently tested." Used by karlender. |
| `calcard` (stalwart) | 0.3.14 | 2026-09-15 | iCalendar/vCard **and** JSCalendar/JSContact parse/generate + cross-conversion, RRULE expansion. New (May 2025) but very active. |
| `ews` (thunderbird) | 0.1.2 | — | EWS XML types for Thunderbird's Exchange support. **Avoid:** Microsoft begins disabling EWS in Exchange Online Oct 2026, full disable April 2027. |

## 7. PGP / S-MIME

| Crate | Backend | Notes |
|---|---|---|
| `rpgp` | pure Rust | Permissive license (MIT/Apache). Used by Delta Chat (audited), GpgFrontend, himalaya. IMPL_STATUS updated Sep 2025; higher-level API evolving in rpgpie. |
| `proton-rpgp` | pure Rust | Proton's fork (0.3.1, Apr 2026), used in proton-crypto; some RUSTSEC advisories in deps. |
| `sequoia-pgp` | Nettle (default), OpenSSL, Botan, RustCrypto, CNG | Most complete: RFC 9580/9980 + RFC 4880, `sq` CLI, WoT, Autocrypt. Stricter licensing; **not security-audited** (funding). |

**Verdict:** rpgp for permissive licensing + Delta Chat compatibility; Sequoia for completeness (GPL-2.0+ w/ linking exception).

## 8. Delta Chat Core as a Reusable Engine

`chatmail/core` (formerly deltachat-core-rust) — 911 stars, v2.58.0 (Aug 2026), monthly releases. Bundles async IMAP (`async-imap`), async SMTP (`async-smtp`), MIME build/parse, rPGP + Autocrypt + SecureJoin, and Iroh P2P. It is **not an embeddable library**: an application core exposed via `deltachat-rpc-server` (stdio JSON-RPC) or legacy C bindings (`libdeltachat`, deprecated). Use as reference design or embed the binary, not as a cargo dependency. Its stack choices validate the recommendations above.

## 9. Maildir / mbox / .eml

| Crate | Version | Notes |
|---|---|---|
| `mailbox-formats` (planetaryescape) | — | All four mbox variants + basic Maildir, streaming reader, locking strategies. No MIME decoding — pair with `mail-parser`. |
| `mailparse` | 0.18.0 | .eml parsing (MIME tree), not a folder format. |

## 10. OAuth2 for Mail

| Crate | Version | Last Release | Notes |
|---|---|---|---|
| `oauth2` (ramosbugs) | 5.0.0 | 2025-01-21 | 56.6M downloads, 379 dependents. MSRV 1.65. Generic RFC 6749 + introspection/revocation; Google + Microsoft Graph examples. Stable but quiet since Jan 2025. |
| `mailledger-oauth` | 0.0.2 | 2026-01-17 | Mail-specific: XOAUTH2 + OAUTHBEARER SASL, preconfigured Gmail/Outlook/Yahoo, PKCE + device flow. Very new, low adoption. |

**Driver:** Microsoft permanently disables Basic Auth for IMAP/SMTP April 2026 — OAuth2 is **mandatory** for Outlook/Microsoft 365. Google requires OAuth for third-party apps. Use `oauth2` 5.0 as the base plus a SASL XOAUTH2/OAUTHBEARER wrapper.

## Summary Stack for a Production Client Core

- **IMAP:** `imap-codec`/`imap-types` (own layer) or `async-imap` (client)
- **SMTP:** `lettre` (or `mail-send` for DKIM)
- **MIME:** `mail-parser` + `mail-builder`
- **JMAP (optional):** `jmap-client`
- **Search:** Tantivy + SQLite
- **Cal/Card:** `libdav` + `calcard`
- **PGP:** `rpgp` (permissive) or `sequoia-pgp` (complete)
- **Local formats:** `mailbox-formats`
- **OAuth:** `oauth2` 5.0

## Sources

- crates.io + docs.rs: all crates listed above (versions and release dates pulled Oct 11, 2026)
- GitHub: duesee/imap-codec, lettre/lettre, stalwartlabs/{mail-parser,mail-builder,jmap-client,calcard,mail-send}, quickwit-oss/tantivy, planetaryescape/mailbox-formats, thunderbird/ews-rs, rpgp/rpgp, chatmail/core, ramosbugs/oauth2-rs, jooooscha/minicaldav, inspect.software
- sequoia-pgp.org/status; IETF RFCs 8620, 8621, 8887, 9404, 9425, 9661, 9007, 9219
- Cyrus IMAP 3.12.4 JMAP docs; Fastmail "Ten years of JMAP" (Dec 2024); Stalwart JMAP Calendars announcement (Oct 2025); Microsoft EWS deprecation timeline (Oct 2026/Apr 2027 disablement)
