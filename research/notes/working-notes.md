# Working notes

Running takeaways while reading raw reports. Input to the curated docs.
`[verify]` marks claims to check before relying on them.

## Linux front end (raw/linux-native/glm.md)

- GTK 4 + libadwaita in Rust (gtk-rs, optionally Relm4) is the default
  for Ubuntu/GNOME: same language as the core, no FFI. Qt 6 via cxx-qt is
  the KDE-native fallback. iced/COSMIC still "experimental"; Slint is not
  desktop-native in look.
- GtkListView/GtkColumnView virtualize rows from a GListModel: the model
  can be a thin window over the core's store.
- WebKitGTK (webkit6 crate): `enable-javascript=false`,
  `enable-javascript-markup=false` (strips script at parse time),
  `auto-load-images=false`.
- Lessons: Thunderbird 102/128 data-loss regressions (per Betterbird);
  Geary died of under-resourcing; Akonadi shows the cost of a visible PIM
  daemon; Claws Mail's plain MH store is a longevity asset.
- Mailspring 1.26.0 (2026-10-02) rewrote mailsync so a message can live in
  many folders (fixes iCloud, Exchange duplicate Sent, Proton labels) and
  added CI tests against real Dovecot/Cyrus and "fake problematic IMAP
  servers" in Docker. [verify]
- Betterbird's feature table doubles as a demand survey: multi-line message
  list, regex search, >500 folders, tray icon.
- Share across platforms: view models + HTML sanitizing (ammonia) +
  strings (Fluent, `.ftl`) in Rust; nothing that draws pixels. Swift
  Fluent support unverified.
- Core API: async commands in, ordered events out, snapshot queries with
  stable opaque ids; never expose IMAP UIDs or rowids to the UI.
- Platform: libsecret, XDG portals via ashpd, GOA tokens as optional import
  (whether GOA hands out IMAP-capable OAuth tokens is unverified).
  Flatpak first, plus .deb.

## Platform facts checked by Claude (2026-10-11)

- macOS 27 "Golden Gate" released ~2026-09-14, Apple silicon only
  ([Eclectic Light](https://eclecticlight.co/2026/09/14/apple-has-released-macos-golden-gate-and-security-updates-to-tahoe-26-7-sequoia-15-8/)).
  No Intel build needed. Briefs said "macOS 26"; pending jobs were patched.
- EWS in Exchange Online: phased blocking from 2026-10-01, removed
  2027-04-01; on-prem unaffected
  ([MC1227454](https://mc.merill.net/message/MC1227454)).
- Apple Mail/Calendar on Mac use EWS for Microsoft 365 and are affected;
  Apple says it is "actively working with Microsoft" on Graph for a
  future macOS 27 update; iPhone/iPad use EAS and are unaffected
  ([MacRumors 2026-10-01](https://www.macrumors.com/2026/10/01/apple-mail-mac-stop-sync-microsoft-365/)).
- Proton relaunched mobile Mail apps 2025-09-25 on a Rust core, ~80% shared
  code ([BetaNews](https://betanews.com/2025/09/25/proton-mail-launches-new-mobile-apps-for-android-and-ios/)).
- Google: security assessment is required for restricted scopes when the
  app "has the ability to access data from or through a third-party
  server"; the page does not explicitly exempt client-only apps, but that
  is the reading. Reassess every 12 months
  ([Google](https://developers.google.com/identity/protocols/oauth2/production-readiness/restricted-scope-verification)).

## Account backends (raw/sources/devin.md)

- Graph is mandatory for Exchange Online. Delta queries per folder;
  `$value` MIME; upload sessions 3-150 MB; KQL `$search`. Webhooks need a
  public HTTPS endpoint, so a local-first desktop client polls delta.
  Throttle: 10k requests/10 min and 4 concurrent per app per mailbox.
  eM Client held Graph back ~2 years as "very buggy ... changing".
- Entra: multi-tenant app; tenants may block user consent; publisher
  verification (Partner Program account) is effectively required. Long
  lead time: start early.
- SMTP AUTH basic auth in Exchange Online: disabled by default for
  existing tenants after end of Dec 2026; final date announced 2H 2027.
- EAS: licensed protocol, legacy. Skip.
- Thunderbird shipped native EWS (Rust) in 145 (Nov 2025), Graph on roadmap;
  protocol crates are MPL (`ews_xpcom`, `graph_xpcom`, xml_struct). [verify]
- Gmail: IMAP + X-GM-EXT-1 (X-GM-MSGID, X-GM-THRID, X-GM-LABELS,
  X-GM-RAW) recovers labels/threads/search without per-project quotas.
  Gmail API quotas restructured 2026-05-01 with billing above 80M
  units/day/project "later in 2026". `https://mail.google.com/` (IMAP) is
  a restricted scope: OAuth verification needed either way; CASA only if
  data passes through vendor servers. Push via Pub/Sub needs a vendor relay
  (which would trigger CASA): poll or IDLE instead.
- Proton: Bridge (local IMAP 1143 / SMTP 1025, paid plans, GPL-3) is the
  only sanctioned path. Proton's Rust Mail SDK lives in the
  `ProtonMail/clients` monorepo but is not externally buildable. Direct
  API (e.g. unofficial `proton-core` crate) is ToS gray zone.
- Tuta, HEY: no third-party access. Import only.
- JMAP: Fastmail (OAuth for distributed apps, developer-friendly policy),
  Stalwart, Cyrus, James. JMAP Calendars draft near RFC (Oct 2026).
- iCloud: app-specific passwords; OAuth only for Apple-approved partners.
  Yahoo/AOL: OAuth needs manual approval; app passwords work.
- Internal model shaped like JMAP: `mailboxIds` set membership covers
  folders, Gmail labels, Graph categories (as keywords) and Proton labels;
  opaque per-backend sync cursors; `providerData` sidecar for extras.
- Backend order: IMAP+SMTP (with Gmail ext.) -> Graph -> EWS (on-prem) ->
  JMAP -> local mbox/Maildir/POP/NNTP (public-inbox).
- Mailspring pattern: one sync process per account, UI read-only on DB,
  writes as tasks (local op + remote op), deltas streamed to observers.
