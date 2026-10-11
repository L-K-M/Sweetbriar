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

## Claris Emailer (raw/emailer/devin.md, manual-based)

- Fog City Software (Guy Kawasaki's company; lead dev Jud Spencer),
  licensed to Claris Apr 1995, 1.0 Jul 1995, 2.0 Apr 1997, final 2.0v3
  ~Jan 1998; killed in the Claris -> FileMaker restructuring. Core team
  (Spencer, Crevier, Cortright, Marandino) went to Microsoft: Outlook
  Express Mac -> Entourage -> Outlook for Mac (community recollection).
- Soul: one In Box, Out Box, Address Book and rule engine across all
  services (POP/SMTP, AOL, CompuServe, eWorld, RadioMail). Offline-first:
  compose locally, connect in batches manually or on named Schedules;
  Locations = per-network connection profiles.
- Loved: Mail Actions (ordered, stop-able, criteria on address book /
  group membership, actions set priority / file / auto-reply with edited
  text / auto-forward; "don't auto-reply to mailing lists"); Cmd-Opt-F
  type-the-folder-name filing; auto-file after read/send/reply;
  auto-delete read/sent/deleted after N days; Auto File Log (durable
  audit of what rules filed where); named colored priorities; address
  book type-to-filter; per-recipient send status with resend to one
  recipient; "Send Now" for one message vs flush queue; random
  signatures; configurable attribution line; holding Cmd reveals button
  shortcuts; status window shows the subject being downloaded; deep
  AppleScript + Fog City community script library.
- Disliked: 1.x file-per-message wasted HFS blocks; 2.0 monolithic DB
  corrupted, needed rebuild rituals, all-or-nothing backup, hard to
  leave; slow all-folder search; no HTML.
- Lesson: never ship opaque storage without built-in export (mbox/EML)
  and integrity tooling; avoid modal connection UI.
- [verify] Thunderbird Graph near-parity mid-2026; Mimestream notes Gmail
  API billing later in 2026 (consistent with sources report).

## Data safety (raw/data-safety/devin.md)

- Incidents and root causes: Apple Mail Catalina 2019 (migration reported
  success unverified; moves persisted header-only copies to server, which
  synced deletions back); Thunderbird mbox compaction corruption (fixed by
  rewriting the compactor, 128 ESR; auto-repair of local folders limited
  because repair itself loses data); PST size ceilings; Proton Bridge UIDs
  changing without UIDVALIDITY change, "Recovered Messages" deleted on
  reset, leaked gluon DBs; K-9 9.0 settings migration silently made deletes
  permanent (#9034); one failing APPEND blocked K-9's whole op queue
  (#1155); Eudora .toc corruption; Airmail 3 URL-scheme exfiltration CVEs;
  Mailspring drops in-flight tasks on launch instead of replaying.
- Design: raw messages as immutable files (Maildir-style tmp -> fsync ->
  rename -> dir fsync), SHA-256 per message; SQLite as rebuildable index;
  oplog/intent queue authoritative, written in the same transaction as
  the local effect; replay = convergence check; poisoned op parks after N
  attempts; per-account ordering only where needed.
- Never bare EXPUNGE (make the IMAP layer lack the verb): UID MOVE, else
  COPY -> verify COPYUID -> UID STORE \Deleted -> UID EXPUNGE. Default
  delete = move to Trash. Tombstones until server confirms + one clean
  sync. Circuit breaker (e.g. >50 msgs or >20% of folder, or UIDVALIDITY
  change producing mass deletes): pause and ask.
- UIDVALIDITY change: freeze queue, resync, rematch by Message-ID/hash.
  Gmail: read ImapSettings autoExpunge/expungeBehavior; X-GM-MSGID as key.
- SQLite: bundle >= 3.51.3 (WAL-reset bug 3.7.0-3.51.2 loses commits when
  connections write+checkpoint concurrently; found by Tailscale, reproduced
  by Antithesis in ~15 min; checked on [sqlite.org](https://sqlite.org/wal.html):
  fixed 2026-03-13, backports 3.44.6 and 3.50.7, "tight timing" race); Apple's system SQLite maps fullfsync
  to F_BARRIERFSYNC, not a drive flush [verify]. WAL + synchronous=FULL +
  fullfsync + checkpoint_fullfsync; single writer; VACUUM INTO before
  migrations; integrity_check; Time Machine: back up messages, exclude
  index/caches; warn if store is in iCloud/Dropbox/network FS.
- Sending: at-most-once *visible*. States held(undo) -> queued ->
  smtp-in-flight -> sent-unconfirmed -> confirmed|failed. Same Message-ID on
  retry; after a drop post-DATA, look up Sent by Message-ID before resend;
  show "possibly sent". 4xx backoff with jitter, 5xx fail. SIZE first;
  large files via link upload.
- Drafts: local append-only snapshots (debounced, fsync before "saved"),
  one Message-ID per draft, APPEND+UIDPLUS, REPLACE (RFC 8508) if offered.
- Security: tokens in Keychain ThisDeviceOnly, non-synchronizable; JS off
  via WKWebpagePreferences.allowsContentJavaScript; block remote incl. CSS
  (EFAIL: decrypted content and remote loads never share a context);
  quarantine xattr on saved attachments; fuzz MIME; panics -> "render
  failed"; mail-parser had CVE-2026-26312 (OOM on cyclic MIME).
- BIMI still an Internet-Draft: branding, not trust.
- Testing: deterministic simulation (FoundationDB, TigerBeetle VOPR,
  Dropbox Nucleus in Rust: 3 trees remote/local/synced must converge),
  proptest state machine vs in-memory IMAP model, imaptest, Cassandane,
  real-server CI matrix, toxiproxy faults. Invariant table I1-I10.
- Open: FTS5 vs Tantivy (one vs two storage engines with crash semantics).
- RESOLVED CONFLICT: this report says Exchange Online SMTP AUTH basic is
  rejected Mar-Apr 2026; that plan was revised on 2026-01-27: unchanged
  until end of Dec 2026, then off by default for existing tenants (admins
  can re-enable), unavailable for new tenants, final removal date to be
  announced 2H 2027 ([o365reports](https://o365reports.com/deprecation-of-smtp-authentication-client-submission-in-exchange-online/),
  checked by Claude). sources/devin is correct.

## Rust ecosystem (raw/rust-ecosystem/devin.md)

- Codecs and MIME are solved; sync engine, store, threading, search,
  rules are not (and are where Sweetbriar's value lives).
- IMAP: imap-types/imap-codec (2.x alpha since 2024; fuzzed, zero-copy,
  quirk_* flags for broken servers) + imap-next (sans-I/O). Clients:
  io-imap 0.7 (pimalaya; CONDSTORE+QRESYNC, IDLE, MOVE, UIDPLUS, SORT/
  THREAD, OAUTHBEARER/XOAUTH2, SCRAM) or async-imap 0.12 (Delta Chat;
  CONDSTORE, MOVE, UIDPLUS, IDLE, COMPRESS, METADATA, no QRESYNC). jonhoo
  `imap` unmaintained. melib most complete but EUPL/GPL. No Rust NOTIFY,
  OBJECTID; IMAP4rev2 being assembled in imap-codec 2.x.
- SMTP: lettre 0.11 (MIT, most used), mail-send (DKIM sign, XOAUTH2, no
  OAUTHBEARER), io-smtp new.
- MIME: mail-parser 0.11.9 (SIMD, lazy, zero-copy, 41 charsets, fuzzed,
  Miri; emits RFC 8621 text/html/attachment lists; limits; MSRV churn) +
  mail-builder 1.0. mailparse as robust fallback. TNEF and S/MIME are gaps.
- mail-auth (DKIM/DKIM2 draft/ARC/SPF/DMARC incl. DMARCbis RFC 9989-9991;
  no BIMI). jmap-client 0.4 (core, mail, WebSocket, EventSource).
- calcard (iCal/vCard/JSCalendar/JSContact), rpgp `pgp` 0.21 (RFC 9580,
  PQC), oauth2 5.0, rustls + platform verifier vs native-tls (client certs),
  hickory DNS, ammonia + lol_html, encoding_rs + chardetng.
- License ledger: AVOID sieve-rs (AGPL, only Rust Sieve), melib (EUPL/GPL),
  notmuch/Xapian/mu (GPL), Mailsync and Proton Bridge (GPL); sequoia is
  LGPL (prefer rpgp). MPL-2.0 OK: Thunderbird `ews` crate, `ms_graph_tb`
  (in-tree), Delta Chat core.
- Engines to study: Delta Chat core (MPL; per-folder IMAP loops, sqlcipher),
  meli (uid_store/watch/cache), pimalaya neverest (MIT sync engine in
  progress: IMAP + Graph + DAV + local pimdir store; NLnet-funded to 2027;
  collaboration candidate), Mailsync README (best written description of a
  desktop sync engine: CONDSTORE/QRESYNC background worker + IDLE
  foreground worker; bodies only last ~3 months; stable IDs by header hash;
  everything in transactions; Gmail = All Mail + X-GM-LABELS), Geary
  (versioned schema, replay ops), EDS Camel folder summary tables, Apple
  Mail Envelope Index + .emlx, K-9/Thunderbird Android, mox imapclient.
- Rules: Sieve-subset interpreter is "a few weeks"; decide before rules UX.
