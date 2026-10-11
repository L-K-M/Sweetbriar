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

## Standards (raw/standards/devin.md)

- Setup chain: known providers by domain and MX -> Autodiscover V1 (POX;
  V2 JSON works but undocumented) -> Thunderbird ISPDB (MPL-2.0 data, ~50%
  of accounts) -> autoconfig.<domain> / .well-known -> RFC 6186 SRV ->
  hostname probing -> manual. Autoconfig is becoming
  draft-ietf-mailmaint-autoconfig (-06, post-WGLC).
- MX -> provider mapping picks the *auth path* (OAuth endpoints, app
  password guidance), not just hosts.
- OAuth on macOS: ASWebAuthenticationSession + PKCE + reverse-domain custom
  scheme, loopback fallback (Google still allows loopback for desktop
  clients). Client IDs are public; refresh token is the secret.
  invalid_grant = re-authenticate, never delete the account.
- Yahoo OAuth needs manual approval; iCloud app-specific passwords only;
  Fastmail OAuth for all protocols; Gmail app passwords only with 2SV.
- MIME edge cases: broken RFC 2047, three filename encodings (2231, 2047,
  raw 8-bit), iso-8859-1 -> windows-1252, chardetng, malformed QP, EAI
  (parse UTF-8 headers; SMTPUTF8 only when advertised), format=flowed in
  and out, TNEF from [MS-OXTNEF] (~2k lines to write), message/rfc822,
  text/calendar, crypto parts are structure not attachments.
- Threading: JWZ locally + X-GM-THRID + Exchange Thread-Index /
  ConversationIndex; THREAD=REFS draft is dead; server THREAD optional.
  Localized reply prefixes (Aw:, Sv:, VS:, WG:, Odp:...).
- Sending: prefer 465 implicit TLS (RFC 8314), else 587; Gmail and M365
  save SMTP-submitted mail to Sent themselves: keep a per-provider
  "server saves sent" flag or you double-append (Thunderbird bug 1431562).
  DSN nice-to-have; BURL/FUTURERELEASE rare; undo/send-later client-side.
- Trust: show Authentication-Results only from the account's own authserv-id.
  BIMI: Apple Mail shows logos only for provider-asserted BIMI; client-side
  VMC/CMC validation would be a differentiator (later).
- RFC 9979 (May 2026, Jenkins/Fastmail + Eggert/Apple, Informational)
  registers $MailFlagBit0/1/2 (Apple's 7 flag colors over \Flagged; 111
  unassigned), $hasattachment, $muted, $notify, $canunsubscribe,
  $unsubscribed, $memo, $maskedemail, $followed, $new ... and mailbox
  attributes \Snoozed, \Scheduled, \Memos. Checked by Claude on
  rfc-editor.org. Round-trip Apple flag colors at launch.
- Sieve + ManageSieve (port 4190: Dovecot Pigeonhole, Cyrus, mailbox.org)
  = server-side rules for non-Gmail/Exchange accounts; Sieve editor is a
  differentiator. MDN: parse, never auto-send.
- macOS 27: no changes found that affect third-party mail clients.
- Tiered checklist (launch / soon / later) in the report, section 4.1.

## Search (raw/search/glm.md; codex report pending)

- macOS 27 Mail advertises a new relevance ranking with highlighted terms:
  relevance-first is now table stakes.
- GLM recommends Tantivy (MIT; BM25, phrase/slop, FuzzyTermQuery, regex,
  facets/aggregations, mmap, incremental segments; one writer process,
  readers in other processes) for text + SQLite as system of record;
  FTS5 viable for v1 if the query AST and extractor pipeline stay
  engine-agnostic. Tantivy has no vector support through 0.27; format
  churn across minors -> stamp index versions, auto-rebuild.
- FTS5: unicode61 (diacritics options), trigram (substring, 3-char floor),
  external-content/contentless-delete tables, bm25/snippet/highlight; no
  fuzzy; weak CJK.
- Xapian GPL (why notmuch/mu are GPL); Cyrus/Fastmail use a Xapian fork
  with CJK word boundaries.
- Tokenizer: charabia (MIT; jieba, lindera, Thai/Khmer, normalization);
  Japanese/Korean ~2-3 MiB/s, budget indexing time.
- Query language "SBQL": one AST, three front ends (token chips, Gmail-
  superset text syntax with notmuch-style boolean core and regex escape
  hatch, NL -> AST via rules then on-device LLM with guided generation,
  always shown as editable chips). Saved searches compose (`query:`).
- Ranking: BM25F field boosts (subject > participants > body) + recency
  decay + thread collapse + small boosts (direct-to-me, starred);
  relevance/date toggle; RRF fusion with optional vectors.
- Semantic (opt-in): EmbeddingGemma-300M (768 MRL->128, 100+ languages,
  Gemma license: check redistribution) or bge-small-en (MIT); ort or
  candle; sqlite-vec (pre-v1, brute force fine at <=1M) or USearch (HNSW,
  mmap views). 1M msgs: ~1-8 core-hours, 0.13-0.8 GB vectors (estimates).
- Server search: Gmail X-GM-RAW / API q= (full web grammar); Graph $search
  is date-sorted and capped at 1,000 results (client must rank, local
  cache depth = search radius); JMAP Email/query + SearchSnippet/get;
  IMAP ESEARCH + FUZZY (RFC 6203, RELEVANCY scores) where advertised.
  Merge by Message-ID / X-GM-MSGID / internetMessageId with "on server"
  badges; per-account fan-out with deadlines and cancellation.
- Attachments: filenames always; PDF via PDFKit (mac) / pdfium; OOXML via
  calamine + quick-xml; OCR opt-in (Vision / Tesseract). Pipeline stage
  with caps and per-account policy.
- Per-account indexes (reindex/delete atomically, fan-out maps 1:1).

## macOS UI (raw/macos-ui/codex.md; targets macOS 26 baseline; devin report pending)

- Recommend AppKit shell + interaction-heavy controls (windows, menus,
  toolbars, mailbox outline, message list, composer, responder chain);
  SwiftUI for settings, inspectors, bounded panels (NSHostingSceneRepresentation
  on 26). Strongest counter: WWDC25 claims 6x faster loading and up to 16x
  faster updates for Mac SwiftUI lists >100k items; so build a fair
  AppKit-vs-SwiftUI list prototype on the same Rust query API and reverse
  if SwiftUI matches interaction quality.
- Message list: view-based NSOutlineView (flat and threaded share one
  model), reused cells, heights cached by (revision, width, font, preview
  mode), no reloadData for flag changes; ID-based selection survives
  sorts; drag-out .eml via NSFilePromiseProvider. Targets: p95 keyboard
  selection feedback <50 ms; no frame overruns warm (16.7/8.3 ms).
- Reader: WKWebView only for HTML; allowsContentJavaScript=false for all
  navigations; nonPersistent data store; WKContentRuleList deny-all
  compiled before display, fail closed; sanitize in Rust (ammonia); cid:
  via app-specific WKURLSchemeHandler scheme per message (never global
  Content-ID lookup); explicit "load remote content" via app-controlled
  fetch/rewrite, never enabling JS; reconstitute on content-process
  termination. WKProcessPool is deprecated and no-op: drop pool tuning;
  at most one spare viewer if measured. Dark mode: honor author
  color-scheme, offer original/light toggle, no blanket inversion.
  Plain text: noneditable NSTextView on TextKit 2. Reply-quoting-selection
  with JS off is an open prototype question.
- Composer: NSTextView/TextKit 2; modes plain (format=flowed in the shared
  serializer), restricted rich text, Markdown (pulldown-cmark, sanitized).
  Durable draft document model lives in Rust with revisions; attributed
  strings are presentation only. Attachments copied into the core's
  immutable store before "saved". States Editing / Saved locally /
  Queued / Sending / Sent|failed|uncertain. Writing Tools: protect quotes
  and signatures via ignored ranges; keep pre-operation revision.
- One command layer for menus, shortcuts, AppleScript (.sdef; async via
  suspendExecution), Services, App Intents, notification actions, Quick
  Look, Share extension (import into durable draft first), Focus filters,
  mailto (RFC 6068, header-injection safe), MenuBarExtra.
- Foundation Models on-device model: 4,096-token context; locale and
  hardware availability vary; model changed in 26.4 (retest prompts).
  Optional, user-invoked, no mail-mutating tools.
- Distribution: Developer ID + notarization + Sparkle (EdDSA) first,
  mostly sandboxed design to keep MAS possible; MAS forbids own updater and
  downloaded code, needs consent for post-quit helpers. Mimestream: direct,
  sandboxed with small unsandboxed helper; MailMate left MAS (2012) over
  sandbox; Airmail MAS-only; Spark both.
- No second authoritative DB in SwiftData/Core Data.
- Required prototypes before product work: list comparison; hostile-HTML
  reader with network tracing; crash-tested drafts/attachments; signed
  distribution build (default mail app, Share ext, scripting, helper).
- Study NetNewsWire (open source: NSTableView timeline, NSOutlineView
  sidebar, WKWebView detail), Things, Mimestream. NetNewsWire's author on
  Liquid Glass adoption cost (2026-09-22).

## Claris Emailer, codex (raw/emailer/codex.md) — additions to the devin report

- More careful chronology; the 2007 Entourage retrospective misdates 1.0
  (says 1996): use only for personnel testimony. Source code apparently
  lost (2005 Copyright Office submission by a user; not proof). AOL access
  code and StuffIt engine licensing complicated any rescue.
- 2.0: 19 user-defined named/colored priorities + reserved Alert; Easy
  Setup; unlimited accounts; Actions gained multiple tests/effects incl.
  run AppleScript, add sender to contacts, print, add/remove group
  members; "Go to New Mail"; Do Not Schedule hold in Out Box; individual
  recipients markable for resending; Save As Text + XTND translator;
  Option-Stop cancels pending connections. Macworld 7/1997: "the best
  e-mail client currently available for the Macintosh"; MacUser 8/1997
  found 2.0 more complex.
- Much of the celebrated power came from Fog City's script catalog: BBEdit
  editing, quote reduction/unwrap, digest extraction, mail merge,
  templates/glossaries, FileMaker archiving. -> a "text workbench" and
  external-editor round trip are revivable.
- Today: Outlook for Mac documents server rules only ("client rules are no
  longer supported"); Apple Mail rules sync via iCloud but their scripts
  and sounds do not; Spark can't assign Gmail labels itself; Mimestream
  edits Gmail server filters.
- Out Box should distinguish Draft / Held / Ready / Submitting / Accepted
  / Needs attention; Graph sendMail 202 Accepted != delivered; JMAP
  maxDelayedSend = 0 means no server delayed send.
- Actions: preview on selected mail, "why this matched", per-message
  execution records so resyncs don't repeat side effects, clear "runs on
  this Mac" vs "runs on server".
- Durability first; first slice = receive, search, read, reply, draft,
  queue send, keyboard filing, restore from backup — all responsive
  during sync/indexing. Test: triage 20 messages, file several, draft a
  reply without losing place or waiting on network.

## macOS UI, devin (raw/macos-ui/devin.md) — agrees with codex; additions

- Same verdict: AppKit-owned structure, SwiftUI islands; NSOutlineView /
  NSTableView list; NSTextView/TextKit 2 composer; WKWebView for HTML.
  Target macOS 26.0 minimum, arm64-first.
- macOS 26: AppKit automatically tracks @Observable reads in draw/layout/
  updateConstraints (back-deploys to 15 with NSObservationTrackingEnabled):
  AppKit and SwiftUI views can share the same @Observable view models.
  [verify]
- macOS 27: refined Liquid Glass (readability), concentric corners, menu
  item symbols hidden by default again; SwiftUI Document API; NSRefresh
  Controller; Icon Composer `.icon` icons. Sparkle 2.10 needs macOS 12+.
- App Intents has a dedicated `.mail` schema domain: 12 intents
  (create/update/save/open/delete/send draft incl. scheduling,
  openMessage, reply, forward, update, archive, delete) and 5 entities
  (account, draft, mailbox, message, thread) wired into Siri, Shortcuts,
  Spotlight. Confirmed by Claude: Apple documents an App Intents mail
  schema domain (archiveMail, sendDraft, createDraft, replyMail, ...;
  [Apple](https://developer.apple.com/documentation/appintents/app-schema-domain-mail.md)).
  Apple Intelligence routes to the user's default app for the domain.
  High-value adoption target.
- SwiftUI Table pain points at scale: identity-diffing cost, sort hangs,
  context menu doesn't give "clicked but unselected" Mac semantics,
  NSHostingView-in-cell sizing bugs. NetNewsWire forbids Auto Layout and
  stack views inside table cells for performance.
- WKWebView: devin favors prewarming one hidden view at launch and reusing
  one per pane (first WebContent process costly); codex says only if
  measured. Load message HTML itself from the custom scheme so cid:
  resolution is uniform. MailMate (r6272 notes) documents the same
  content-rule + scheme-handler recipe; Mimestream blocks ~78 tracker
  patterns. Own evaluateJavaScript still works with page JS off.
- format=flowed: always honor on display; MailMate disabled sending it by
  default in 2021 (Outlook/Gmail ignore it). Emit for plain-only messages,
  behind a preference.
- Composer: Plain + Markdown (generate multipart/alternative) first, WYSIWYG
  later; STTextView as fallback TK2 component; SwiftUI TextEditor not
  composer-grade yet.
- AppleScript: use scripting access groups (sdef) so sandboxed callers
  can target Sweetbriar; Mailsmith was scriptable *and recordable*.
- Billing direct via Paddle/FastSpring; MailMate $10/quarter; Mimestream
  $49.99/yr; Nova $99 + $49/yr updates.

## Mailsmith (raw/mailsmith/devin.md, manual-based)

- Bare Bones, code name "Bluto", 1.0 1998-05-04 ($79), POP3/SMTP only;
  BBEdit text engine; NeoAccess object DB; 1.5 (2002) Carbon + PCRE; 2.0
  (2003) OS X only, SpamSieve/PGP/Address Book integration, Macworld
  Eddy; 2009 moved to Stickshift Software (Rich Siegel), freeware, closed
  source; 2.2 storage became "outboard data files, one per message" +
  rebuildable DB (for Spotlight, performance, disaster recovery); 2.3
  Rebuild/Reconstruct Mailboxes; last 2.4.3 2018; 32-bit, dead since
  Catalina. Died of "competing with free" (Apple Mail) and never shipping
  the promised IMAP (Gruber left in 2007 for IMAP/iPhone).
- Neuburg's "Iron Triangle": BBEdit-class text handling, scriptable +
  recordable + attachable AppleScript, distributed filtering.
- Distributed filtering: filters are named objects attached to mailboxes;
  arriving mail is offered to top-level mailboxes' filters (alphabetical);
  a Deposit drops it in and offers it to that mailbox's children: the
  mailbox tree is the flowchart. One filter reusable across mailboxes.
  Outgoing mail runs through the same system. Make a Filter from a model
  message; auto filters on mailbox creation; Re-Apply Filters for
  "cradle to grave" flows; filter logging; XML import/export.
- Criteria include Sender is Known, Sender's Group Membership, Server
  Account, List-ID, Notes, Spam Score, Partial, On Server; match methods
  incl. PCRE grep and Approximately Matches (fuzzy 20-100%); no nested
  grouping. ~21 actions incl. Deposit/Transfer/Copy, labels, priority,
  status, Set Subject, sound, Notify, Print, autoreply from file, Report
  to SpamCop, Run Script (on FilterMessage), Extract Enclosures, Stop.
- Search: Advanced Query uses the same criteria engine as filters;
  "Find Messages About" relevance search (1.5.3); PCRE everywhere (manual
  has a ~30-page regex tutorial); Spotlight importer `kind:mailsmith`.
- Editor: 6-slot clipboard history, Paste Quoted, unlimited undo, split
  panes, rectangular selection, Emacs bindings, Rewrap Quoted Text, quote
  level +/-, Strip Quotes, Zap Gremlins, Change Case, Balance.
- Composition: per-message private Notes (never sent, searchable);
  configurable attribution (#date#, ICU formats); netiquette warnings
  (excessive quoting, sigdash, "me too", unbracketed URLs); random
  signatures; Send Now vs Queue; Redirect and Re-Send; Reply to List
  (List-Post); format=flowed (2.2).
- Clippings with placeholders: #CLIPBOARD#, #DATETIME fmt#, #INSERTION#
  and jump markers, #SCRIPT name#, #SYSTEM shell# (insert command output),
  #UUID#; sets auto-activate per account.
- Scripting: Menu Scripts `Menu•Item` intercept menu commands (Gruber's
  Send Now guard); Startup/Shutdown items; filter Run Script; BBEdit
  plug-in API; "Run Unix command on each received message" (pipe filter).
- Misc: POP Monitor (triage the server mailbox before download); 32
  labels; per-mailbox notification mute; Quiet Time; Copy Item Link
  (`mailsmith:` URL); digest bursting; any menu key reassignable and any
  menu item hideable; synthesized plain-text view of HTML-only mail.
- Reviews: Tsai (ATPM 1999) "distributed filtering is revolutionary";
  Porter (TidBITS 2002) "no program gives you more or better ways to
  create and use filters"; Neuburg 2003; Ihnatko (Macworld 2003) cons: no
  IMAP, LDAP, non-Roman text, as-you-type spelling. Watts Martin (Tsai,
  Aug 2026): "The last email program I truly loved was Bare Bones'
  Mailsmith"; MailMate is its closest descendant.
- Lessons: Reconstruct lost labels/notes/status because they lived only
  in the DB -> journal metadata or mirror into keywords/headers.
- Devin's picks: 2.2 storage model; one criteria engine shared by search,
  filters and scripting; mailbox-attached filters as flagship (plus a
  flat-rules view over the same engine and a separate server-rule editor);
  editor commands; Notes; automation API + AppleScript façade + menu
  intercept scripts; skip recording, POP Monitor, menu hiding.

## Performance (raw/performance/codex.md; glm report pending)

- Budgets (proposed, p95 on a reference Apple-silicon Mac): cold launch to
  useful mailbox 1.0 s (p99 2.0), warm 400 ms; switch mailbox first rows
  100 ms; open cached message readable 100 ms (selection feedback 50 ms);
  cached 50-message thread 100 ms selected / 250 ms expanded;
  archive/delete visible 50 ms, durable intent 100 ms; search top results
  100 ms after last keystroke incl. 30-50 ms coalescing; compose editable
  100 ms; scrolling <5 ms main-thread work per update (16.7/8.3 ms frames).
- Day-one commitments ranked: (1) local snapshots + durable local
  mutations; (2) bounded work, cancellation, reserved interactive
  capacity; (3) stable identity + memberships + paged summaries;
  (4) virtualized rows + progressive rendering; (5) maintained local search
  with explicit coverage; (6) delta sync + connection reuse; (7) measured
  prefetch; (8) deferred launch work + instrumentation. Bulk actions
  operate on a query/selection representation, not serialized IDs.
- Hosted IMAP CAPABILITY probes (pre-auth, 2026-10-11): Fastmail
  advertises IMAP4rev2, CONDSTORE, QRESYNC; Yahoo advertises OBJECTID and
  PARTIAL; M365 MOVE/UIDPLUS/IDLE; Gmail X-GM-EXT. Server docs: Dovecot
  rev2 experimental (default off), Cyrus broadest, Stalwart rev2 yes but
  COMPRESS/NOTIFY planned. Negotiate after auth; keep capability record.
- Gmail API new-project quota: 6,000 units/user/min; messages.get = 20 ->
  1M messages via messages.get >= 55.6 hours at one user's max. Prefer
  bulk IMAP for Gmail import. history.list: expired ID -> 404 -> full
  reconcile; watch needs renewal and drops notifications.
- Graph: per-folder delta; use `Prefer: IdType="ImmutableId"` (ordinary
  IDs change on move); notifications avg <1 min, max 3 min; delta token
  validity not fixed; commit cursor atomically with applied changes.
- Connections: start at 2 per account (reserve one interactive), max ~4;
  batch UIDs; at 100 ms RTT five serial round trips = 500 ms.
- Prefetch hypothesis: selected + next 5 + previous 2; visible thread
  bodies first; adapt to direction, hit rate, battery, network cost.
- Scheduler: priority draft/outbox writes > selected fetch + current search
  > visible metadata > nearby prefetch > historical sync > indexing. Batches
  ~250 metadata changes. Map to macOS QoS explicitly (Rust priorities
  don't become thread QoS automatically). spawn_blocking can't be aborted:
  cooperative cancellation + SQLite interrupt.
- Schema sketch: message (local id), remote_locator (account, mailbox,
  UIDVALIDITY, UID; many per message), membership (mailbox, message)
  WITHOUT ROWID, thread_edge/thread_summary, mailbox_message_summary
  (materialized rows), blob (hash, codec, length, location, integrity),
  draft_revision, settings, mutation_outbox, sync_cursor, index_journal.
  Covering index on (mailbox_id, received_at DESC, message_id DESC, ...).
  Cursor pagination with row values; ordered-ID vector for scrollbar
  jumps (500k IDs = 4 MB).
- Codex's own benchmark (1M rows, Linux, SQLite 3.40): first page 0.07 ms;
  OFFSET 900000 80 ms; cursor to same page 0.08 ms.
- WAL: one writer worker, 2-4 readers, short snapshots, scheduled
  checkpoints, never hold a transaction across network/UI. synchronous=
  FULL; evaluate fullfsync latency. Page size 4K first (can't change in
  WAL), mmap off first.
- Raw bytes: immutable hash-addressed files first behind an abstraction
  (SQLite's 2017 test: ~10 KB BLOBs 35% faster than files); commit order:
  temp file -> flush -> rename + dir sync -> DB reference; orphans GC'd.
  zstd 1-3 for cold content. Dedupe bytes separately from logical
  identity; Message-ID not unique; Mailspring's header-hash IDs can
  collide (documented in its README).
- Threading: persist header graph; incremental component updates;
  provider keys (X-GM-THRID, JMAP threadId, Graph conversationId) stored
  as account-scoped hints; cross-account threading a separate view policy.
- FTS5 prototype first, benchmark Tantivy on 1M/5M fixtures; engine-neutral
  AST; index journal for replay/rebuild.
- WebView: one active + maybe one spare after measurement; launch shows
  stored snapshot before discovery/reconcile/index verify/prewarm. Memory
  target ~250 MiB app/core + 250 MiB WebKit helpers.
- Measurement: OSSignposter spans correlated with Rust tracing IDs
  (input, queue wait, SQL, blob, decode, FFI, diff, readable); XCTest
  metrics on physical Macs nightly; Enron (~0.5M msgs) + deterministic
  1M/5M synthetic fixtures (rewrite Message-ID/References together);
  RTT 20/100/300 ms, throttling, disk-full, crash at each boundary.
- Mimestream regressions worth testing: CPU during large-cache refresh and
  Select All; typing lag in attachment-heavy threads.

## Musashi (raw/musashi/codex.md; codex extracted the original archives and read EN+JP manuals)

- Author Mamoru Misono (弥園 護), SONO SOFTWARE / Sonosoft, sole
  proprietorship from 1997-07-01 (incorporated 2008; company still exists,
  no evidence of Musashi maintenance). Earliest versions Japanese-only;
  2.0.5 circulating by 1997-10 (68K/PPC, EN/JP); 2.2.2 in EN/FR/IT/JP;
  3.0 1998-12 (hierarchical folders, multilingual fonts, Remote Mail);
  3.3 2001 SMTP AUTH; 3.4 Mac OS X + Classic 2002; 3.4.2 2006; 3.5 2008
  freeware (not open source). Shareware ¥3,000 / US$25-33 via Kagi/Vector.
- Design intent (Misono, Vector 1998, translated): "a responsive, compact
  style" while other apps grew heavy; usable on older Macs.
- Virtual Message Viewer: new arrivals appear together in one viewer with
  a column showing the folder each was filed into (by filters); search
  results use the same viewer; normal message operations work there.
  A 2005 user (212 folders in Apple Mail) missed exactly this: unread
  mail gathered in one place, filed after reading. -> Sweetbriar "reading
  session": stable ordered list across folders/accounts; reading doesn't
  steal position; explicit refresh; separate Read / Done / File actions.
- Multi-user within the app: manager password, per-user config, switch
  user without quitting, shared "common mail" folder, Local Forward
  between users without a server. (Engst 2003: OS accounts make this less
  needed.)
- Remote Mail: fetch headers/size from POP, mark retrieve/delete, then
  execute (like Mailsmith's POP Monitor / Eudora server status).
- Japanese text correctness as a user decision: JIS output, inline input
  conversion, per-language fonts (avoid half-width-kana mojibake),
  Cyrillic/Turkish/Greek converters. -> decoding override with preview
  that never rewrites originals; CJK test corpus; lindera-tantivy.
- Attachments inline in prose: icons at the caret, movable, deletable
  like a character; placement preserved for Musashi readers.
- Templates as plain-text files with optional To/Cc/Bcc/Subject/Account/
  Signature headers + body (documented format).
- Filters: max 2 conditions (AND/OR), incoming/sending/manual triggers,
  first match wins, highlight colors.
- Spatial folder UI: large/small icons or list, free icon placement,
  custom folder icons, empty/non-empty folder pictures; customization
  "Modules" (published resource format) with third-party module ecosystem
  on Vector.
- Reading: Space pages and advances; Control-1..9 scroll speeds; three
  quote-depth colors; configurable reply intro with date/time-zone format.
- Warning: release notes document fixed mail-loss bugs (mail vanished
  after forced quit; on cancelled user switch; deletes bypassing Trash;
  multiline Bcc exposure). "Automatically answer Yes" dialog timer = a
  data-safety anti-pattern.
- Reviews: Macworld 2002 (Engst) "fast and easy to use" but weak search/
  filtering, no inline HTML, no IMAP, two mice; reader letter defends it
  as "the easiest multiple-user environment to set up". Brightblack
  reviews 88% (2.2), 92% (3.x).
- Codex: Thunderbird filters run after sending too, so outgoing filters
  are not extinct; claims of "lost features" must be checked carefully.
- Extracted manuals are copyrighted (freeware licence keeps ownership):
  cite, don't commit.

## Eudora, codex (raw/eudora/codex.md) — additions to manual/devin

- Qualcomm said Eudora was profitable but didn't fit the wireless business
  (2006). Penelope/OSE (Thunderbird-based) is unsupported; panned by the
  faithful for different feel and mailbox format. HERMES revival: Windows
  Eudoramail 8.0; Mac "Eos" 6.3 (2026-04-02) can't run on Arm/Catalina+.
- Interaction gems: Option-click "select similar" (same correspondent,
  subject...); search results behave like a mailbox and can be acted on;
  Engst 2004: one-second search over 33 mailboxes for unread/labeled mail
  as a personal "attention queue" spanning his filing system (same idea as
  Musashi's viewer); file replies beside originals (Fcc preference);
  editable received subject/body ("Pencil") -> local overlays with
  provenance, never rewriting originals; glossary expansion in body;
  x-eudora-setting: links for hidden prefs; BossWatch (Windows 7) warns on
  configured recipients.
- Windows 7: X1 indexed search (Paid), saved searches, S/MIME.
- Engst 2011 migration of ~1M messages: tools disagreed (970,283 vs
  849,206 converted) -> imports need reconciliation reports.
- Filters as an inspectable program: phases after receipt / before
  submission / after confirmed submission / manual; simulation view ("42
  match, 11 would move, none would send"); rule trace per message; rule
  revisions; nested conditions but simple default editor.
- Redirect honesty: RFC 5322 Resent-*; DMARC is now RFC 9989 (May 2026,
  obsoletes 7489/9091) [verify]; fall back to forward-as-attachment where
  faithful redirect isn't possible; Turbo-style action keeps source until
  send outcome known.
- Thunderbird is the strongest counter-proposal (saved searches, outgoing
  filters, redirect extension) if the architecture constraints changed.

## Architecture, codex (raw/arch-core/codex.md) — agrees with devin on shape; additions

- Element's 2025-11 retrospective: parity took longer than expected; SDK
  work more complex than app work -> don't build several front ends while
  inventing the engine. 1Password abandoned its SwiftUI Mac app: Rust
  doesn't make a native front end cheap. Proton Mail facade: UniFFI,
  app-lifetime MailSession, live queries returning initial results +
  WatchHandle.disconnect(); reports shared regressions and platform silos.
- Delta Chat incident: ambiguous `dc_is_configured()` after an error made
  an encrypted account look unconfigured; UI cleanup could delete it ->
  error taxonomy (NotFound, Locked, Unavailable, Corrupt, NeedsMigration);
  a failed open never authorizes account creation or deletion.
- UniFFI 0.32.1 (2026-09-08); Swift async template lacks automatic
  Swift->Rust cancellation: add a withTaskCancellationHandler adapter +
  explicit Rust request/token (Signal's race-aware pattern). Swift 6 async
  foreign-trait fix merged 2026-07-15 (verify in the pinned version).
  UniFFI is binary lifting/lowering, not JSON.
- Cancellation is operation-specific: queries/previews disposable;
  save_draft and enqueue_send take caller-generated idempotency keys; a
  durable receipt means accepted; dropping the Swift waiter never cancels
  an accepted operation; cancelling a queued send is its own command;
  send states Queued / Submitting / Accepted / Failed / OutcomeUnknown.
- Subscriptions: pull-based `next_batch(limit)` with one outstanding pull,
  optional coalesced "data available" wake-up; bounded buffers ->
  ResetRequired on overflow (never drop a positional edit); snapshot +
  subscription established atomically; generation-tagged edits; a query
  generation is not a long-lived SQLite read transaction. Start 200
  rows/page, 600-row cache. BoltFFI streams drop events when full:
  unsuitable for positional diffs without gap/reset handling.
- Bodies via MessageHandle/BodyLease (chunks or permission-controlled
  file/descriptor); chunk/file first.
- One EngineHost owns the Tokio runtime per engine process; store worker
  for blocking SQLite; never block_on on main thread; shutdown records
  pending work.
- Process: one process first; if sync-after-quit is a release requirement,
  move the sole owner into an SMAppService per-user agent; exclusive
  engine-ownership lease so agent and in-process fallback never both
  write. IPC DTOs separately versioned (UniFFI handles can't cross IPC).
  Share extension hands off via the agent or an atomic versioned file in
  the App Group; never a second writer.
- Intel: macOS 26 supports four 2019/2020 Intel models; macOS 27 doesn't.
  Apple silicon first; universal only by explicit policy.
- Strict layering (matches the user's rule): L0 shells; L1 SweetbriarBridge
  + sb-ffi / sb-native-client; L2 sb-service (commands, receipts, query
  windows, subscriptions, DTOs/errors); L3 sb-engine (EngineHost, sync,
  drafts, send, rules, undo, reconciliation, index scheduling); L4
  sb-resources (single facade over leaves; maps types/capabilities/
  errors); L5 leaves sb-store (SQLite, FTS5, blobs, journal, backups),
  sb-mime, sb-search (optional Tantivy), sb-platform (secrets/OS),
  sb-backend-{imap,graph,gmail,jmap,proton}. No sb-common; leaves never
  call each other; FTS5 lives inside sb-store.
- Index watermark: record indexing work + revision in the store
  transaction; advance searchable watermark only after index commit.
- First milestone: one account path, real mailbox + composer + search
  window, 100k and 1M fixtures; cached first page <100 ms p95; <4 ms
  main-thread work per UI batch; cancellation/lag/restart/crash tests;
  strict Swift 6 compile of the actual exports.

## Performance, GLM (raw/performance/glm.md) — agreements and conflicts

- Agrees: 100 ms ceiling / 50 ms design point; everything from local
  store; SQLite WAL single writer + readers, covering indexes, summary
  tables, zstd content-addressed blobs; JWZ incremental + provider IDs;
  NSTableView for 500k rows (SwiftUI List ~2 s for 10k rows in community
  reports; Table lazy but unbenchmarked at 500k); plain-text fast path;
  signposts + XCTest perf gates on a 1M corpus; "% of interactions under
  budget" metric (opt-in, content-free telemetry).
- CONFLICT 1 (Gmail transport): GLM says Gmail IMAP and Exchange Online
  lack QRESYNC/CONDSTORE, so use Gmail API (history.list) and Graph delta.
  devin/sources prefers IMAP + X-GM-EXT for Gmail (quotas, billing);
  codex/performance computes >=55.6 h to import 1M messages through
  messages.get under new-project quotas and suggests bulk IMAP import with
  API deltas as a possible hybrid. -> Debate item. Claude checked: Gmail
  did roll out IMAP CONDSTORE (Thunderbird
  [bug 912216](https://bugzilla.mozilla.org/show_bug.cgi?id=912216)
  discusses problems after the rollout), so GLM's "no CONDSTORE" is
  doubtful; QRESYNC unconfirmed. Settle with an authenticated CAPABILITY
  probe in the first prototype.
- CONFLICT 2: GLM recommends a "pooled, prewarmed WKWebView (shared
  process pool)". WKProcessPool is deprecated and a no-op (codex, devin);
  pooling advice is stale. Prewarm only if measured.
- GLM's counter-argument worth keeping: headers-only cache with bodies on
  demand is cheaper; rebuttal: search/power features/never-lose-mail need
  the full local store; prefetch policy is a dial, the schema isn't.

## Mailsmith, codex (raw/mailsmith/codex.md) — additions

- Corrects the brief: successor was "Stickshift Software" (not "Stick
  Software"). 2.2 manual documents as-you-type spelling, so old reviews'
  complaint is version-bound.
- Ihnatko worried send failures could be missed in the (errors) mailbox;
  queued messages were uneditable; regexes could be dangerous (freeze);
  alphabetical mailbox order silently changed filter results -> explicit
  order, stable IDs, preview, incremental results, protect responsiveness.
- Porter (MacTech 2004): barely used the mouse thanks to keyboard
  assignment. Gruber's Send Now guard (menu script vetoes Cmd-E when text
  is selected) -> named pre-send checks + after-action hooks.
- Revival bundles: six clipboards + append-copy + Paste Quoted;
  quote-depth rewrap with hard/soft line control; split view +
  rectangular edits + invisibles; editor regex replace + mailbox regex
  query + shared rule criteria; script-aware clippings + menu pre/post
  hooks; private notes + message links (sweetbriar: URLs).
- Graph only allows subject changes on drafts -> "Set Subject" must be a
  local display overlay for received mail.
- SpamSieve lists Mailsmith as legacy; Spark lacks SpamSieve integration
  (insufficient extensibility) -> extension points matter.
- MailMate is the strongest living counterexample (plain text/Markdown,
  external editor, rules on any mailbox): study it.
- Remote inspection should start read-only (POP Monitor made permanent
  deletion too easy).

## Musashi, primary artifacts (raw/musashi-artifacts/codex.md) and devin (raw/musashi/devin.md)

- Codex wrote a verified pure-Python StuffIt extractor (classic + SIT5;
  methods 13 and 15 "Arsenic", LGPL attribution to XADMaster) and
  extracted all four archives: 128 files, 3,747 resources, all checksums
  pass. Tool lives in /home/paseo/sb-research/jobs/musashi-artifacts-codex/
  tools (not committed: LGPL-derived; extracted manuals are copyrighted).
- Complete menu inventory recovered (30 MENU resources in 1.2.4, 43 in
  3.4.1) plus context menus (AMIT): e.g. 3.4.1 Do menu has New Message
  -> {accounts}, Check Mail -> {accounts}, Remote Mail -> {accounts};
  message viewer Do: Move, Copy, Trash, Reply (Reply / Reply to All /
  Reply to Sender), Forward (Forward / Direct Forward / Local Forward),
  Edit, Send, Highlight (7 colors), Options (Get Info, Add to address
  book, Apply filters, Remove attachments, Mark read/unread); Send Now /
  Send Later; long-press (~0.5 s) on compose/check/send buttons shows
  account or timing choices.
- 1.2.4 had permission-controlled shared mailboxes between local users
  (recipients may add into and/or move out of the owner's mailbox; cannot
  rename/delete); 3.4.1 replaced it with Local Forward.
- Editable message metadata: subject and status (opened/replied/
  forwarded/sent) via Get Info.
- Search: status shortcuts (Unread/Read/Unsent/Sent), two condition
  regions, date comparisons, highlight filter, scope (all / current
  viewer / folder); "Call filter" loads a rule's conditions into search;
  searching while a results viewer is front refines the results.
- Templates also accept Reply-To (3.2.1); reply intro uses %N %S %D.
- Stale-doc lesson: manual says CRAM-MD5 only, binaries also have
  LOGIN/PLAIN (release notes 2001-12-22).
- Devin: from Osaka; 2.x stored mbox-like per-mailbox files; 3.x moved to
  per-user single-file Incoming/Outgoing Archives + Message Index, which a
  2002 migrant cited as a fragility/backup worry. "Musashi-V" rewrite
  beta (2000-10 to 2001-01) promised labels, named saved searches
  decoupled from filters, thread sorting, mail-merge "repeated send",
  unlimited folder depth; never shipped. ATPM review: tiny footprint
  ("runs on 2MB RAM"), stable, clean; dislikes: thin English manual,
  sequential polling, 68k attachment decode bugs.

## More facts checked by Claude (2026-10-11)

- Thunderbird shipped native Microsoft Graph support (email only, no
  calendar yet) in release 154, 2026-08-18; announced on the Thunderbird
  blog in September 2026
  ([blog](https://blog.thunderbird.net/2026/09/thunderbird-desktop-new-protocol-support-microsoft-graph-api/)).
- Gmail ends "Send mail as" for third-party addresses on web and mobile,
  fully removed January 2027, and ends Gmailify too; Workspace send-as
  and Gmail aliases unaffected
  ([Android Authority](https://androidauthority.com/gmail-killing-third-party-send-as-feature-3694659)).
  Relevant to identities: users who relied on Gmail to send as their
  other address need a client that does it.
- AppKit automatic Observation tracking: opt-in on macOS 15 via
  `NSObservationTrackingEnabled`, on by default in the 2026 releases
  ([Apple](https://developer.apple.com/documentation/appkit/updating-views-automatically-with-observation-tracking-in-appkit.md)).

## D16 Markdown-first, GLM (raw/markdown/glm.md) + Devin's debate view

- Both say: **split the proposal.** Writing in Markdown is low risk and
  differentiating (MailMate precedent; Mimestream shortcuts; Markdown
  Here). Reading: **adaptive normalization, not normalize-everything.**
- Render model is a richer document AST (Pandoc-like), not Markdown:
  CommonMark can't carry dir/lang, data-table semantics, cid image maps.
  Markdown is an export of the AST (GLM: store CommonMark+GFM + sidecar
  metadata as a versioned cache).
- Classify per message: text-like mail (plain, quoted threads, patches,
  notifications, simple HTML passing a fidelity gate) renders normalized
  natively (TextKit 2, no WebContent process: privacy + dark mode + speed);
  layout mail (newsletters, receipts, marketing, calendar, AMP) renders the
  sanitized original by default. Instant "Show original", per-sender
  memory. Normalized form is a derived, versioned, discardable cache.
- Wire format: multipart/alternative = rendered text/plain (format=flowed)
  + email-safe generated HTML with inlined CSS. `text/markdown` (RFC 7763)
  is unused by mainstream clients; MailMate uses a private
  `markup=markdown` parameter on text/plain; optional per identity.
- Tooling: comrak 0.56 (652/652 CommonMark, 670/670 GFM), pulldown-cmark,
  htmd 0.5.5 (~16 ms for a 1.37 MB page on M4), html2text, dom_smoothie
  (readability), ammonia, syntect.
- Replies: quote from AST as Markdown blockquote, "keep original
  formatting" fallback; forward as untouched message/rfc822.
- Effort: composer + wire format ~4-6 weeks; reading pipeline ~1
  engineer-year for "good", then an ongoing fidelity treadmill. Fallback:
  a reader-mode toggle with the same internals (80% of benefit).
- Risks: fidelity and trust (receipts, legal notices, phishing cues);
  needs a differential corpus harness (pandoc as oracle, golden
  snapshots) before shipping; encrypted mail: derive on open, don't cache.

## D17 database, Devin (raw/database/devin.md + devin-artifacts/bench.py)

- Benchmark (shared Linux box; SQLite 3.40.1 WAL+FULL vs PostgreSQL 16.2
  via pip `pgserver`, Unix socket; 500k mail-shaped rows): point lookup
  7 us vs 116 us (16x); keyset page 197 us vs 508 us; FTS5 query ~4x
  faster than tsvector+GIN, and its index builds 7x faster; single-row commits fsync-bound for
  both (~250 vs 193/s); readers never blocked in WAL. Postgres wins only
  with many concurrent writers, which the single-writer design doesn't have.
- **Akonadi switched its default to SQLite** (upstream MR !311; KDE PIM
  26.04.1 per Gentoo news 2026-05-23, checked by Claude:
  [Gentoo news](https://gentoo.nic.cz/rsync/metadata/news/2026-05-23-kdepim-sql-backend-change/2026-05-23-kdepim-sql-backend-change.en.txt))
  after ~17 years of per-user MySQL/MariaDB/Postgres daemons ("running a
  daemon brings its own set of problems"). The closest precedent ran the
  experiment and reversed it.
- Postgres-specific hazards on desktops: yearly major-version datadir
  upgrades (pg_upgrade/dump-restore), libc/ICU collation drift corrupting
  indexes, a postmaster to babysit, sandbox/MAS friction (unix socket +
  shared memory).
- Every surveyed desktop mail client is SQLite (+ files): Apple Mail,
  Outlook for Mac (Outlook.sqlite), Mailspring, Delta Chat, Evolution,
  eM Client; Signal Desktop and Apple Photos too.
- Tighten SQLite plan: latest SQLite is 3.54.0 (2026-10-09) [verify];
  `reserve_bytes=8` + cksumvfs for page checksums; VACUUM INTO snapshots
  or Litestream-style WAL streaming for backups.
- Postgres becomes right for a future server-side sync service or shared
  team mailboxes, as a service; the client store then becomes a replica.
