# Topic: Crate selection with current versions, and the public-domain licence

An implementation plan for Sweetbriar is about to be written. The project
snapshot is in `./research/` (crate survey in
`research/02-rust-ecosystem.md`, database in `research/13-database-engine.md`,
search in `research/05-search.md`, architecture in
`research/01-architecture.md`, licence policy D13). Verify everything
against crates.io, docs.rs and upstream repositories as of 2026-10-12.

## Part A: crates for the first milestones

For each, give the current version and release date, licence, MSRV,
maintenance status, and a recommendation with the strongest alternative:

1. SQLite: `rusqlite` with `bundled` (which SQLite version does the
   current `libsqlite3-sys` bundle? is it ≥ 3.51.3 and not the withdrawn
   3.52.0?), FTS5 enablement, `LIBSQLITE3_FLAGS` usage; migration tooling
   (`rusqlite_migration` or own).
2. MIME: `mail-parser` (current version, MSRV), `mail-builder` for
   composing; mbox and EML reading (`mbox` crates or own); Apple Mail
   `.mbox` export format specifics.
3. IMAP client for the first backend (Proton Bridge, generic IMAP later):
   `async-imap`, `imap-next`/`imap-codec`, `imap` (sync), others;
   support for UIDPLUS, MOVE, CONDSTORE/QRESYNC, IDLE, LITERAL+, ID;
   Tokio compatibility; licence (note any GPL/AGPL).
4. SMTP: `lettre` vs `mail-send`.
5. TLS: `rustls` + `tokio-rustls`, custom certificate trust for Proton
   Bridge's self-signed cert; platform verifier crate.
6. Async runtime and FFI: Tokio, UniFFI 0.32.2 (and the unreleased Swift 6
   async fix noted in `research/01-architecture.md`), `uniffi` build and
   bindgen workflow.
7. Secrets: `keyring` crate vs platform-specific code (macOS Keychain
   data-protection keychain, Linux Secret Service), `secrecy`.
8. Testing: `proptest`, `insta`, a test IMAP server (Dovecot in Docker in
   CI, `greenmail`, or an in-process fake), fault injection helpers.
9. **Module-boundary enforcement**: tools that can enforce the owner's
   strict adjacent-layer rule inside a crate (imports, paths, re-exports):
   `cargo-modules`, `cargo-deny` (crate graph only), custom `syn`-based
   check, `tach`-like tools for Rust; recommend one.
10. `cargo-deny` configuration for advisories and licences.

## Part B: licence

The owner wants **Sweetbriar's own code in the public domain** ("if
possible depending on licenses we use"), with **no contributions
accepted** (anyone may fork). Research:

1. Which instrument fits best for software, with sources: CC0-1.0, the
   Unlicense, 0BSD, or a dual offering (e.g. "Unlicense OR MIT").
   Consider: jurisdictions where authors can't fully waive copyright (the
   owner appears to be in Switzerland), fallback licences, patent
   clauses, OSI/FSF status, SPDX identifiers, how well `cargo-deny`,
   GitHub and package managers recognise each.
2. Compatibility: can a public-domain project depend on MIT, Apache-2.0,
   BSD, MPL-2.0, LGPL (GTK on Linux), OpenSSL-style licences? What
   obligations attach to **distributed binaries** (attribution notices,
   NOTICE files, MPL source availability for modified files, LGPL
   relinking), and which dependency licences must be excluded (GPL, AGPL)
   because they would force the combined work's licence?
3. Practicalities: the LICENSE file, a "no contributions" CONTRIBUTING
   statement and GitHub settings, a third-party notices file generated
   in CI (`cargo-about` or similar), and how Apple's and Linux
   distribution channels (Flathub) handle a public-domain app.
4. Recommendation with the strongest counter-argument.
