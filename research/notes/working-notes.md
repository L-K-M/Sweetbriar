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

## Eudora 6.2 manual (raw/eudora-manual/devin.md)

- Local truth: mbox mailboxes on disk; Out is a real, inspectable,
  editable queue with send-pipeline statuses (Unsent, Sendable, Queued,
  Timed Queue, Sent, Replied, Forwarded, Redirected).
- Personalities: full identities (server, SMTP, address, signature,
  stationery, check options); replies come from the receiving personality;
  personality is a filter criterion and action.
- Filters: Incoming / Outgoing / Manual triggers; 2 terms with
  and/or/unless; arbitrary headers, body, Junk Score, Personality;
  "intersects nickname/address book"; ~19 actions (Make Status/Priority/
  Label/Personality/Subject, Play Sound, Speak, Open, Print, Notify,
  Forward, Redirect, Reply With stationery, Server Options, Copy,
  Transfer, Move Attachments, Skip Rest); first match wins; Make Filter;
  "dinosaur" icon for filters unused 30 days; filter report.
- List: toggleable columns incl. Junk Score, Mood, Server Status;
  Shift-click multi-key sort, sticky per mailbox; 5 priorities; 15 colored
  labels; Group Subjects.
- Content Concentrator: read a whole thread/sender as one concatenated
  document with quotes and redundant headers trimmed.
- Composing: per-message icon bar overrides (signature, encoding, wrap,
  keep copy, receipt); Stationery (captures icon-bar state; Reply With
  stationery powers auto-replies); MoodWatch chili peppers.
- Send Again; Redirect (Resent-*) and Turbo Redirect (redirect + queue +
  delete in one gesture); refuses auto-reply/redirect on Precedence
  bulk/list.
- ScamWatch: hover shows true URL; deceptive links warn + confirm.
- Addressing: nicknames (incl. "me"), Quick Recipients (MRU), contact-photo
  toolbar buttons; drag message onto a photo = forward (Option = redirect).
- Search: criteria-row Find with regex, results act like a mailbox and can
  be saved; toolbar field searches mail or web; Web Words; Link History.
- Contextual Filing: select a word, context menu offers mailboxes whose
  name matches.
- Usage Statistics window; Extra Warnings settings; Scripts menu folder;
  EMSAPI plug-ins; tabbed tool windows; customizable toolbar.
- POP "skip messages over N KB" + per-message Fetch/Delete server actions
  (modern analog: envelope+PREVIEW-only sync with explicit fetch).
- Devin's top revival picks: Identities, Redirect, Send Again + Stationery,
  outgoing/manual filters, first-class statuses, colored labels, multi-key
  sort, Concentrator digest view, link inspector, tone advisory, Timed
  Queue, Quick Recipients, nicknames, statistics, Scripts menu, regex
  search. Not ESP (file sync over mail).
- Caveats: Redirect vs DMARC alignment; client-side scheduled send needs
  the app (or a helper) running; labels/colors have no cross-backend
  standard (local canonical, project to keywords/categories/labels).
- [verify] Gmail ending Send-as for third-party addresses Jan 2027.

## Landscape, partial (raw/modern-landscape/glm.md, run killed at 13 min)

- Apple Mail: recurring search failures (macOS 26.2 tokenization bug on
  short alphanumeric codes), rules silently stop after upgrades, sync
  stalls. macOS 27 adds a new Mail search ranking and "Suggestions".
- New Outlook for Mac: slow (5-10 s actions per users/HN), cloud-routed,
  no full offline, can't add local IMAP, so no Proton Bridge accounts.
- Spark 3: webby, routes credentials through Readdle servers, upsells.
  Superhuman $30/mo. HEY: closed silo. Notion Mail: Gmail-only, cloud.
- Mimestream: native, Gmail API only, $49.99/yr, direct sale, local only.
- MailMate: loved search + Markdown, IMAP only, single developer,
  subscription switch (2025-07) controversial; 2.0 still beta.
- Postbox: dead (eM Client acquired it in 2024).
- Thunderbird: Gloda search unreliable; Supernova UI slowness; announced
  native Microsoft Graph support Sept 2026; Thundermail hosting on
  Stalwart (JMAP); iOS app still in development (Oct 2026).
- Unmet power-user demand: fast + native + server-agnostic (IMAP +
  Exchange/Graph + Gmail + Proton); great search; server rules / Sieve
  editor ("literally no apps to manage sieve scripts"); Markdown AND
  WYSIWYG; scripting/keyboard; local-first privacy. New indie entrants
  pitch exactly this (Penguin Mail, Talanoa, Marco) [verify].
- AI: liked when opt-in for triage, summaries, NL search; rejected when
  forced, undisableable or cloud-scanning. "No AI" is a selling point.
  Apple Foundation Models: ~3B on-device model, free, offline, guided
  generation and tool calling; macOS 27 adds Private Cloud Compute
  routing, custom models ("Core AI"), an `fm` CLI [verify details].

## Architecture (raw/arch-core/devin.md)

- Precedents for "Rust core + native UI": Proton Mail mobile (Rust core,
  ~80% shared, crash rate 0.12% -> 0.05% on iOS), Element X
  (matrix-sdk-ffi over UniFFI), Firefox (application-services "megazord"
  xcframework), Bitwarden, Wire core-crypto, 1Password (hand FFI, view
  models in core, Typeshare), libsignal (own bridge macro), Delta Chat
  (Rust email engine; JSON-RPC API over CFFI in-process or stdio
  out-of-process), Mullvad (daemon + gRPC), Ghostty (Zig core, SwiftUI +
  GTK4 shells), Mailspring (per-account C++ mailsync process, UI
  read-only on DB, tasks in, JSON deltas out).
- Counter-precedents: Thunderbird runs three separate native stacks (iOS
  is pure Swift on SwiftNIO); Dropbox abandoned its shared C++ mobile core
  in 2019 (tooling, debugging, hiring).
- UniFFI 0.32.x is the default: async both ways, callback "sink" traits
  for push (no first-class streams), partial Swift 6 strict concurrency
  (async foreign traits fail in v6 mode, issue #2929). Per-call
  serialization ~0.6-2 us; never ship lists or bodies through it.
- BoltFFI (Dec 2025): zero-copy, async streams, claims far faster; Crux
  moved to it (crux 0.19, June 2026). Too young to bet on; revisit 2027.
- Pattern: coarse façade objects + windowed list handle + VectorDiff-style
  diff stream (Reset/Insert/Set/Remove/...) + controller (set_filter,
  add_one_page). Bodies/attachments by path or blob reference.
- One tokio runtime in core; long ops return cancellable handles; data
  safety must not depend on cancellation semantics.
- macOS process model: phase 1 in-process; phase 2 `sweetbriard`
  LaunchAgent via SMAppService owning sync and all writes (background
  sync, notifications, crash isolation); UI and extensions over XPC or a
  Unix socket. XPC services die with the app, so they can't do background
  sync. SQLite multi-process works (WAL, persist-WAL) but has no cross-
  process change notification: one writer, others go through it.
- Build: per-arch cargo -> xcframework -> local SPM binary target in the
  app repo (post-deprecation Mozilla pattern); arm64 only is fine.
  Release: panic=abort, LTO, strip with dSYMs. Core size ~8-25 MB.
- Linux consumes the façade crate directly (no FFI).
- Proposed crates: ui-api (façade), ffi (UniFFI only), ipc, sweetbriard,
  core, sync, store (SQLite + blobs + tantivy), backend-{imap,jmap,smtp,
  graph,gmail,carddav,ews}, mime, crypto, proto.
- Open: view models in Rust (Crux/1Password style) vs Swift @Observable
  over diff streams. Devin leans Swift view models for v1.
