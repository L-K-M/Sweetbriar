<!-- provenance: helper=glm model=zai-coding-plan/glm-5.3 started=2026-10-11T18:49:21+00:00 exit=0 brief=research/briefs/ -->

# Making Sweetbriar fast and responsive — research report

Compiled 2026-10-11 for the Sweetbriar email client project (Rust core + native macOS-first UI).
All time-sensitive facts were checked against live sources on 2026-10-11; the check date is given
for each. Facts that could not be verified are marked **(unverified)**.

> Note on macOS: per the correction to the brief, the current macOS is **27 "Golden Gate"**
> (GA 2026-09-14; 27.0.1 on 2026-09-28; Apple silicon only — it is the first release that drops
> Intel) ([Wikipedia: macOS](https://en.wikipedia.org/wiki/MacOS),
> [Wikipedia: macOS Golden Gate](https://en.wikipedia.org/wiki/MacOS_Golden_Gate), checked 2026-10-11).
> macOS 26 Tahoe (2025) is the previous release and still runs on Intel. Everything in this report
> targets macOS 27 while remaining compatible with 26.

## 1. TL;DR

- **Budget "instant" at 100 ms and design for 50 ms.** The 0.1 s / 1 s / 10 s limits have been
  stable UX guidance since 1993 ([NN/g](https://www.nngroup.com/articles/response-times-3-important-limits/));
  RAIL refines this to "process input in 50 ms, respond in 100 ms, produce a frame in ≤10 ms"
  ([web.dev](https://web.dev/articles/rail)); Superhuman treats 100 ms as a *maximum* and aims for
  <50 ms, with a newer renderer showing mail in 1–2 frames (<32 ms)
  ([Superhuman blog, 2022](https://blog.superhuman.com/superhuman-is-built-for-speed/)).
- **Every user-visible action must be served from the local store.** Sync never blocks the UI.
  Superhuman does exactly this: local database, prerendering of likely-next threads, minimal
  animation ([Superhuman](https://blog.superhuman.com/superhuman-is-built-for-speed/)).
- **Build the local store as the core of the product**: SQLite in WAL mode (readers never block
  the writer), one writer + reader pool, covering indexes for the mailbox list query, zstd for
  body blobs, content-hash dedup ([SQLite WAL](https://www.sqlite.org/wal.html),
  [zstd](https://facebook.github.io/zstd/)). A March 2026 SQLite WAL bug (fixed in 3.51.3,
  2026-03-13) means: pin a current SQLite.
- **Extension-aware sync is the difference between fast and slow IMAP.** QRESYNC/CONDSTORE
  (RFC 7162), ESEARCH, UIDPLUS, MOVE, LIST-STATUS, COMPRESS, OBJECTID (RFC 8474) — supported in
  full by Cyrus and Stalwart, partially by Dovecot, and *not* by Gmail IMAP or Exchange Online,
  which pushes those accounts to their APIs (Gmail API `history.list`, Graph delta queries).
- **Gmail, Microsoft 365, Fastmail, Stalwart all have delta-sync APIs**; design the sync engine
  around a generic "change log" abstraction with per-account adapters from day one.
- **Thread with JWZ (RFC 5256 REFERENCES) incrementally on the client**, but adopt server thread
  IDs where they exist (Gmail `X-GM-THRID`, JMAP `threadId`, Graph `conversationId`).
- **On macOS, a 500k-row mailbox means NSTableView (or SwiftUI `Table`) with fixed-height rows,
  not `List`/`LazyVStack`.** Measured: SwiftUI `List` on macOS takes ~2 s to render 10k rows
  (fixed row heights cut it to 0.3 s); `Table` is lazy; AppKit sorts instantly where SwiftUI
  beach-balls ([SO 72070486](https://stackoverflow.com/questions/72070486/bad-performance-of-large-swiftui-lists-on-macos),
  [r/SwiftUI](https://www.reddit.com/r/SwiftUI/comments/1dvva7k/swiftui_mac_performance/)).
- **HTML mail via a prewarmed, pooled WKWebView; a native plain-text fast path for everything
  else.** Each WKWebView drags its own WebContent process; "three webviews means seven processes"
  ([Embrace](https://embrace.io/blog/wkwebview-memory-leaks/), 2023).
- **Measure or it didn't happen**: os_signpost intervals in the app, XCTest performance tests in
  CI on a 1M+ message corpus (Enron ~0.5M messages/1.7 GB, replicated), and a Superhuman-style
  "% of interactions under 100 ms" dashboard.

## 2. Findings

### 2.1 Latency budgets (Q1)

**Established guidance.**

- NN/g (Nielsen, 1993, updated 2014): **0.1 s** feels instantaneous / direct manipulation;
  **1 s** keeps the flow of thought; **10 s** keeps attention; longer operations need
  percent-done indicators
  ([source](https://www.nngroup.com/articles/response-times-3-important-limits/)).
- Google's RAIL model: **respond to input within 100 ms** (process events within **50 ms** to
  leave headroom), **produce animation frames in ≤10 ms** (16 ms budget at 60 fps), keep idle
  work chunks ≤50 ms, load interactive content in <5 s
  ([source, last updated 2020-06-10](https://web.dev/articles/rail)). Note RAIL itself is now
  described by Google as superseded for web by Core Web Vitals, but its psychophysics (traceable
  to NN/g) remains the reference for app latency.
- Superhuman attributes the 100 ms rule to Gmail creator Paul Buchheit, treats 100 ms as the
  *maximum*, targets **<50 ms**, and reported a renderer fast enough to show emails "in 1–2
  Chrome frames (<32 ms)". They also cite the classic business numbers: Amazon +100 ms ≈ −1 %
  sales; Google +500 ms ≈ −20 % traffic
  ([source, 2022](https://blog.superhuman.com/superhuman-is-built-for-speed/)).
  Their monitoring metric is "**the % of events under 100 ms**"
  ([source, 2026-07-17](https://blog.superhuman.com/performance-metrics-for-blazingly-fast-web-apps/)).
- Apple publishes **no numeric latency budget** in the HIG that I could verify; Apple's
  performance guidance for Mac apps is instead about energy, QoS classes and idleness
  (Energy Efficiency Guide archive, below). Practical frame budgets on Apple hardware: 60 Hz =
  16.7 ms/frame; ProMotion 120 Hz displays (MacBook Pro) halve that to 8.3 ms — treat *scrolling
  and animation* as an 8 ms budget to be safe (derived from display refresh rates, not an Apple
  claim).

**Proposed budgets for Sweetbriar** (synthesis of the above; the p50/p95 split follows
Superhuman's "percent of events under target" philosophy):

| Interaction | Budget p50 | Budget p95 | Notes |
|---|---|---|---|
| Cold launch → first window + mailbox list | 800 ms | 1.5 s | restore last state from disk; no network on critical path |
| Warm launch | 250 ms | 500 ms | |
| Switch mailbox (cached) | 16 ms | 50 ms | first frame of new list; query from local store only |
| Switch mailbox (uncached folder, local data) | 100 ms | 250 ms | paged query, render first page |
| Open message (plain text, cached) | 16 ms | 50 ms | native TextKit path, no web view |
| Open message (HTML, cached) | 50 ms | 100 ms | prewarmed WKWebView, prerendered if predicted |
| Open message (body not local) | skeleton <100 ms | full <1.5×RTT | never a blank window; fetch on demand |
| Open 50-message thread (collapsed) | 50 ms | 150 ms | from denormalized thread summary table |
| Archive / delete / flag | next frame (≤16 ms) | 50 ms | optimistic local apply; server sync async |
| Search-as-you-type (local index, 1M msgs) | first results 50 ms | 100 ms/keystroke | keystroke echo itself ≤16 ms |
| Compose window opens (caret blinking) | 50 ms | 100 ms | preloaded template window |
| Scroll 500k-message list | 0 dropped frames | hitches <1 % of frames | cell reuse, fixed row height, prefetch |

Rules of thumb that fall out: any interaction ≤100 ms needs no spinner; 100 ms–1 s needs an
immediate (≤50 ms) state change even if fake (optimistic UI); >1 s needs a progress indication;
>10 s needs percent-done and cancellability (NN/g, RAIL, loc. cit.).

### 2.2 Why email clients feel slow (Q2)

A catalog of real causes, each with either a cited example or a clearly-marked general claim:

1. **Sync work on, or blocking, the UI thread.** The classic pattern: opening a folder triggers a
   server round trip before anything renders. The fix (serve from local store, sync in
   background) is exactly what Superhuman describes — a local database that "can be displayed
   extremely quickly — even when you're offline" ([Superhuman](https://blog.superhuman.com/superhuman-is-built-for-speed/)).
2. **Fetching bodies at open time.** Header-only caches mean every message open is a network
   round trip plus MIME parse plus render. Google's Gmail API sync guide explicitly recommends
   caching `format=full` results the first time a message is retrieved so later opens are local
   ([source](https://developers.google.com/gmail/api/guides/sync)).
3. **IMAP round-trip amplification.** Without CONDSTORE/QRESYNC, reconnecting clients re-fetch
   all flags; Fastmail has written about exactly this cost and the role of CONDSTORE/MODSEQ and
   COMPRESS=DEFLATE in shrinking it
   ([Fastmail blog, 2009](https://www.fastmail.com/blog/help-test-proxy-to-improve-imap-performance/)).
   Gmail IMAP models labels as folders ([Gmail IMAP extensions](https://developers.google.com/gmail/imap/imap-extensions)),
   so a naive folder-walking client multiplies every operation by the number of labels — the
   "Gmail label explosion."
4. **HTML rendering cost.** Web-grade layout (email HTML/CSS is adversarial) inside a
   general-purpose engine. Each `WKWebView` runs web content in separate processes, and
   instantiation is expensive — first-navigation hangs are common enough that warming up a
   throwaway view at launch is a standard workaround
   ([SO 74301868](https://stackoverflow.com/questions/74301868/wkwebview-ios-slow-on-first-launch),
   [Embrace, 2023](https://embrace.io/blog/wkwebview-memory-leaks/)).
   MailMate renders messages in a standard WebView ([manual](https://manual.mailmate-app.com/)),
   which is one reason HTML mail there is heavier than its plain-text-first editing model
   suggests.
5. **Huge mailboxes vs. list virtualization.** SwiftUI `List` on macOS: "takes several seconds"
   to render 1k–5k items; ~2 s for 10k fixed-height rows, ~0.3 s after forcing fixed row heights;
   `List` is "not lazy on macOS" while `Table` is
   ([SO 72070486](https://stackoverflow.com/questions/72070486/bad-performance-of-large-swiftui-lists-on-macos)).
   Filtering a 16k-item `List` took ">20 seconds"
   ([SO 63075386](https://stackoverflow.com/questions/63075386/swiftui-performance-issues-with-list-with-large-data-set)).
   `List` builds per-item metadata for change tracking even for off-screen rows
   ([SO 68315292](https://stackoverflow.com/questions/68315292/swiftui-list-is-extremely-slow-at-showing-actions-leading-trailing-contextmenu)).
   Real-world report: 10k-row table view in SwiftUI beach-balls for 10+ s on sort; the same app
   in AppKit is "buttery smooth" ([r/SwiftUI, 2024](https://www.reddit.com/r/SwiftUI/comments/1dvva7k/swiftui_mac_performance/)).
6. **Threading recomputation.** Full REFERENCES threading is a sort plus graph rebuild over the
   whole mailbox (RFC 5256 §3 defines the six-step algorithm, including the subject-merge table
   and dummy-node pruning — [RFC 5256](https://datatracker.ietf.org/doc/rfc5256/)). Done
   eagerly for every flag change it stalls UIs; it must be incremental (see 2.6).
7. **Search over unindexed data.** Client-side substring scan of 1M messages is seconds-to-
   minutes; indexed engines (Xapian in notmuch) are the counter-example — notmuch's premise is
   that even "millions of messages… it's just plain not much mail"
   ([notmuch](https://notmuchmail.org/)). IMAP `SEARCH` pushes the cost to servers that often
   lack indexes; Gmail's answer is `X-GM-RAW` to reuse the web/mail engine
   ([Gmail IMAP extensions](https://developers.google.com/gmail/imap/imap-extensions)).
8. **Attachment previews and image decode on the main thread.** Decoding a 12 MP camera JPEG is
   tens of MB of pixel data; doing it on the main thread during scroll produces hitches.
   (Standard Apple guidance: downsample and decode off the main thread — WWDC session "Images
   and Graphics Best Practices", WWDC18 **(unverified this session)**.)
9. **Main-thread database access / checkpoint starvation.** In SQLite WAL mode a long-running
   read transaction can prevent checkpoints from completing, growing the WAL and slowing *all*
   reads ([SQLite WAL guide, §6](https://www.sqlite.org/wal.html)) — an interactive app that
   holds read transactions open on the main thread degrades itself.
10. **Cache bloat without admission control.** Mimestream's 1.9 release notes call out "adaptive
    cache management" as a performance fix
    ([Mimestream blog, 2026-01-15](https://mimestream.com/blog/1.9-released)) — evidence that
    unbounded local caches eventually hurt (disk pressure, launch-time index checks).
11. **Cold-start costs stacked on first open**: process launch + DB open + index verify +
    WKWebView spawn + OAuth refresh all landing on the first click. Each is individually fine;
    together they blow the 100 ms budget (see WKWebView prewarming refs in 2.7).

### 2.3 Sync protocols and extensions that matter (Q3)

**The extension list and why each matters for speed.**

| Extension | RFC | Why it matters |
|---|---|---|
| IMAP4rev2 | [RFC 9051](https://datatracker.ietf.org/doc/rfc9051/) | Cleans up rev1 (no more STATUS-in-SELECT surprises, better LIST); requires CONDSTORE, ESEARCH, UIDPLUS, etc. as baseline |
| CONDSTORE / QRESYNC | [RFC 7162](https://datatracker.ietf.org/doc/rfc7162/) | The big one: reconnect and get *only* changed flags / new messages since a MODSEQ+UID pair, plus vanished-message handling — no full flag re-fetch |
| IDLE | [RFC 2177](https://datatracker.ietf.org/doc/rfc2177/) | Server-push "something changed" without polling |
| NOTIFY | [RFC 5465](https://datatracker.ietf.org/doc/rfc5465/) | IDLE for the whole account (all folders) — one connection hears about everything |
| COMPRESS=DEFLATE | [RFC 4978](https://datatracker.ietf.org/doc/rfc4978/) | Big bandwidth savings on flag/header sync; Fastmail shipped it explicitly to cut sync traffic ([Fastmail, 2009](https://www.fastmail.com/blog/help-test-proxy-to-improve-imap-performance/)) |
| MOVE | [RFC 6851](https://datatracker.ietf.org/doc/rfc6851/) | Archive/delete without COPY+STORE\Deleted+EXPUNGE round trips |
| UIDPLUS | [RFC 4315](https://datatracker.ietf.org/doc/rfc4315/) | APPEND returns the UID (no search-after-append); EXPUNGE reports what vanished |
| ESEARCH | [RFC 4731](https://datatracker.ietf.org/doc/rfc4731/) | Range/UID search results instead of huge sequence lists |
| LIST-STATUS | [RFC 5819](https://datatracker.ietf.org/doc/rfc5819/) | Folder list + UNSEEN/COUNTS in one command — one round trip for the sidebar |
| SPECIAL-USE | [RFC 6154](https://datatracker.ietf.org/doc/rfc6154/) | Find Drafts/Sent/Archive without heuristics (Gmail supports it) |
| OBJECTID | [RFC 8474](https://datatracker.ietf.org/doc/rfc8474/) | Stable EMAILID/MAILBOXID — cheap "did this message change anywhere" checks; helps dedup across folders |
| BINARY | [RFC 3516](https://datatracker.ietf.org/doc/rfc3516/) | Fetch decoded base64 bodies server-side when useful |
| PREVIEW | [RFC 8970](https://datatracker.ietf.org/doc/rfc8970/) | Server-generated text previews — free snippet column |
| PARTIAL | [RFC 9394](https://datatracker.ietf.org/doc/rfc9394/) | Paged FETCH/SEARCH — bounded response sizes, great for huge mailboxes |
| LITERAL+ / LITERAL- | [RFC 7888](https://datatracker.ietf.org/doc/rfc7888/) | Non-synchronizing literals remove a round trip per large command |
| SORT/THREAD | [RFC 5256](https://datatracker.ietf.org/doc/rfc5256/) | Server-side sort/thread for online clients |
| Pipelining / multiple connections | base IMAP | IMAP is fully pipelineable; a few parallel connections (one IDLE + N workers) hides RTT. Server connection limits vary **(unverified per server)** |

**Server support matrix** (verified sources; per-server notes below):

| Server | IMAP4rev2 | QRESYNC | OBJECTID | PREVIEW/PARTIAL | JMAP | Delta-API alternative |
|---|---|---|---|---|---|---|
| Gmail | ✗ | ✗ (not advertised) | ✗ | ✗ | ✗ | **Gmail API `history.list`** + Pub/Sub push |
| iCloud | ✗ **(unverified)** | ✗ **(unverified)** | ✗ | ✗ | ✗ | none public (XAPPLEPUSHSERVICE legacy, Apple Mail only **(unverified)**) |
| Fastmail | (runs Cyrus; deployment version unverified) | ✓ (Cyrus) | ✓ (Cyrus) | ✓ (Cyrus) | ✓ | JMAP itself |
| Microsoft 365 | ✗ | ✗ | ✗ | ✗ | ✗ | **Graph delta queries** + change notifications |
| Yahoo | ✗ **(unverified)** | ✗ **(unverified)** | ✗ | ✗ | ✗ | none |
| Dovecot | (2.3.19+ claims rev2 **(unverified)**) | ✓ | (2.3.19+ **(unverified)**) | ✗ (NOTIFY needs `mailbox_list_index`) | ✗ | — |
| Cyrus | ✓ | ✓ | ✓ | ✓ | ✓ (provisional; PushSubscription not yet supported) | — |
| Stalwart | ✓ | ✓ | ✓ (advertises "dozens of extensions") | ✓ **(verify against CAPABILITY)** | ✓ (all extensions + WebSocket) | — |

Per-server details and sources:

- **Gmail.** Google documents its IMAP extensions: `X-GM-EXT-1` gate; SPECIAL-USE with `\All`
  etc.; `X-GM-RAW` (server search); `X-GM-MSGID` (unique message identity — dedup!);
  `X-GM-THRID` (server threading); `X-GM-LABELS` (labels, including via `STORE`). Its example
  CAPABILITY shows `IMAP4rev1 UNSELECT LITERAL+ IDLE NAMESPACE QUOTA ID XLIST CHILDREN
  X-GM-EXT-1` ([Gmail IMAP extensions, page updated 2026-09-15](https://developers.google.com/gmail/imap/imap-extensions)).
  Conspicuously absent from Google's own documentation: CONDSTORE/QRESYNC, IMAP4rev2, OBJECTID —
  Gmail IMAP is deliberately frozen at rev1-plus-extensions **(absence verified against Google's
  doc example; live CAPABILITY capture not performed)**. The fast path for Gmail is the **Gmail
  API**: full sync via `messages.list` + batched `messages.get`, then partial sync via
  `history.list` with `startHistoryId`; history records are "typically available for at least one
  week", and an HTTP 404 means "fall back to full sync"; push notifications (Cloud Pub/Sub)
  trigger partial sync only when needed
  ([sync guide](https://developers.google.com/gmail/api/guides/sync)).
- **iCloud.** Apple publishes server settings but no extension inventory I could verify; treat
  iCloud as plain IMAP4rev1 + IDLE and verify at runtime via CAPABILITY **(unverified)**.
- **Fastmail.** Runs **Cyrus** as its mail store
  ([Fastmail open-source page](https://www.fastmail.com/company/open-source/)), exposes
  JMAP + IMAP + POP ([Fastmail dev](https://www.fastmail.com/dev/)), and has supported
  CONDSTORE/QRESYNC since at least 2010
  ([blog](https://www.fastmail.com/blog/new-popimap-server-version/)). Cyrus's current (3.12.x)
  documentation lists support for RFC 9051, 7162, 8474, 8970, 9394, 4978, 6851, 4315, 4731,
  5819, 6154, 5465, 3516, 7888, 5256, and JMAP 8620/8621 (provisional; PushSubscription
  unsupported) ([Cyrus RFC list, checked 2026-10-11](https://www.cyrusimap.org/imap/rfc-support.html)).
  Fastmail's production Cyrus version is not published **(unverified)** — probe CAPABILITY.
- **Microsoft 365 / Exchange Online.** Basic auth is *fully disabled* (since Oct 2022, no
  re-enable; OAuth2/XOAUTH2 required for IMAP/POP/SMTP; EWS frozen feature-wise since 2018 and
  pointed at Graph) ([Deprecation of Basic authentication, page updated 2026-07](https://learn.microsoft.com/en-us/exchange/clients-and-mobile-in-exchange-online/deprecation-of-basic-authentication-exchange-online)).
  Observed pre-login CAPABILITY: `IMAP4 IMAP4rev1 AUTH=PLAIN AUTH=XOAUTH2 SASL-IR UIDPLUS MOVE ID
  UNSELECT CHILDREN IDLE NAMESPACE LITERAL+` — i.e. **no CONDSTORE/QRESYNC/ESEARCH**
  ([SO 65690142](https://stackoverflow.com/questions/65690142/is-there-a-detailed-specification-for-outlook-365-imap-which-rfcs-does-it-comp);
  post-login list may add more **(unverified)**). The fast path is **Microsoft Graph delta
  queries**: per-folder `messages/delta` with opaque `skipToken`/`deltaLink` state, `$select`
  for lean rows, `Prefer: odata.maxpagesize` for paging, `receivedDateTime` filters (capped at
  5,000 items with `$filter`), no `$search` in delta
  ([delta-query-messages, updated 2025-08](https://learn.microsoft.com/en-us/graph/delta-query-messages)).
  Delta is *per folder* — track each folder separately.
- **Yahoo.** No authoritative extension list consulted **(unverified)**; treat as plain
  IMAP4rev1 + IDLE until probed.
- **Dovecot.** "Dovecot supports a lot of IMAP extensions"; METADATA, COMPRESS, SEARCH=FUZZY,
  SPECIAL-USE need explicit enabling; NOTIFY requires `mailbox_list_index=yes`
  ([Dovecot docs, 2.3 branch — now marked unmaintained in favor of 2.4](https://doc.dovecot.org/2.3/configuration_manual/protocols/imap_server/)).
  Dovecot 2.3.19+ advertising IMAP4rev2 **(unverified this session — check the 2.4 docs/changelog)**.
- **Cyrus.** See the RFC list above; the current docs (version 3.12.4) enumerate everything on
  this report's list ([source](https://www.cyrusimap.org/imap/rfc-support.html)).
- **Stalwart.** "IMAP4rev2 and IMAP4rev1 with dozens of supported extensions," full JMAP for
  Mail + Sieve + WebSocket + Blob/Quota extensions ([Stalwart mail-server page, checked
  2026-10-11](https://stalw.art/mail-server)).

**JMAP** ([RFC 8620](https://datatracker.ietf.org/doc/rfc8620/) core,
[RFC 8621](https://datatracker.ietf.org/doc/rfc8621/) mail) is architecturally what Sweetbriar
wants from every server: batched method calls over HTTP, explicit per-object state strings for
delta sync (`Email/changes`), server-side `threadId`, and push (event source stream or Web Push
per RFC 8620 §7 — note Cyrus does not yet implement PushSubscription, loc. cit.). Fastmail,
Cyrus, and Stalwart speak it; Gmail/M365 do not.

### 2.4 Sync strategy (Q4)

**Local-first, delta-driven, priority-scheduled.** The consensus architecture across the fast
clients:

1. **Everything interactive reads the local store; the network never gates the UI.** (Superhuman:
   local DB + prerendering, loc. cit.; Google's own Gmail API guidance: cache `format=full` on
   first fetch so re-opens are local, loc. cit.)
2. **Headers first, bodies on demand — plus prefetch.** Initial sync: folder list (LIST-STATUS /
   Graph folders / JMAP Mailbox/get) → recent-window headers (e.g. last 30–90 days) → bodies for
   the inbox tail. Bodies fetch on first open, then cache. Prefetch heuristics, in priority
   order: (a) unread messages in the visible folder; (b) neighbors in the open thread;
   (c) next likely messages (Superhuman prerenders "the email threads you're most likely to
   view soon" — [source](https://blog.superhuman.com/superhuman-is-built-for-speed/));
   (d) remaining bodies only at `utility`/`background` priority.
3. **Delta sync when the server can do it; QRESYNC/CONDSTORE when it can't.**
   - JMAP: `Email/changes` with `sinceState`.
   - Gmail API: `history.list` from `startHistoryId`; 404 ⇒ full sync
     ([guide](https://developers.google.com/gmail/api/guides/sync)).
   - Graph: per-folder `messages/delta` with `deltaLink` persistence
     ([guide](https://learn.microsoft.com/en-us/graph/delta-query-messages)).
   - IMAP with QRESYNC: SELECT … QRESYNC (uidvalidity, modseq, known-UIDs) → server returns
     flag deltas + VANISHED; LIST-STATUS refreshes counts in one round trip.
   - IMAP without QRESYNC (Gmail, Exchange): fallback pipeline — STATUS per folder (cheapest
     change detector), then UID SEARCH UID ranges / `UID FETCH 1:* FLAGS` deltas by UID window;
     rely on IDLE to trigger. For Gmail prefer labels (`X-GM-LABELS`) over folder-walking to
     avoid label explosion; dedup with `X-GM-MSGID` (all from
     [Gmail IMAP extensions](https://developers.google.com/gmail/imap/imap-extensions)).
4. **One push channel, few workers.** One IDLE connection (or NOTIFY where supported; Pub/Sub or
   Graph webhooks for API accounts) receives "something changed"; a small worker pool (2–4
   connections) does the actual fetching; pipelining hides RTT. Mail submission on its own
   connection so a big APPEND never blocks sync.
5. **Priority queue so user-visible work runs first.** Tag every job: `user-interactive`
   (open this body now), `user-initiated` (open this folder), `utility` (prefetch neighbors),
   `background` (bulk history import, reindex). On macOS this maps directly onto
   `NSQualityOfService`/GCD QoS classes; Apple's Energy Efficiency Guide's whole "Prioritize
   Work" chapter is this idea
   ([archive](https://developer.apple.com/library/archive/documentation/Performance/Conceptual/power_efficiency_guidelines_osx/PrioritizeWorkAtTheAppLevel.html)).
6. **Energy: App Nap is not a scheduling system.** App Nap throttles priority/timers/I/O of
   non-foreground apps *after the fact*; Apple explicitly says don't rely on it — suspend work
   yourself when the app is inactive, take a power assertion (`NSProcessInfo`) only for
   user-initiated long work (e.g. initial import), and reschedule discretionary work
   ([App Nap guide](https://developer.apple.com/library/archive/documentation/Performance/Conceptual/power_efficiency_guidelines_osx/AppNap.html)).
   Use `NSBackgroundActivityScheduler`-style deferral for bulk indexing (guide's "Schedule
   Background Activity" chapter, loc. cit.). In practice: hold an IDLE socket (near-zero CPU),
   and batch sync wakeups.
7. **Crash- and quota-safe writes**: journal every mutation (see store design); apply server
   deltas idempotently keyed on UIDs/EMAILIDs; never trust sequence numbers.

### 2.5 Local store design for millions of messages (Q5)

**Engine: SQLite.** Every serious local-first client ends up here or reinvents it (Superhuman's
local database; notmuch/Xapian for search; Mailspring's separate C++ engine with its own local
store). Chosen facts that shape the design:

- **WAL mode**: "readers do not block writers and a writer does not block readers"; writes are
  sequential appends; only one writer at a time; auto-checkpoint at 1000 pages (~4 MB at the
  4096-byte default page size). Long-running readers can starve checkpoints and grow the WAL —
  keep read transactions short and move checkpointing off the interactive thread. Page size
  cannot be changed after entering WAL mode. Transactions ≳100 MB behave worse than rollback
  journal (mail bulk-import should be chunked, e.g. 10–50 MB per transaction)
  ([SQLite WAL guide, updated 2026-08-25](https://www.sqlite.org/wal.html)).
- **Current-event note**: a rare WAL-reset corruption bug (multi-connection, tight-timing race)
  was found 2026-03-03 and fixed in SQLite 3.51.3 (2026-03-13), with backports to 3.44.6 and
  3.50.7. A multi-threaded WAL user like Sweetbriar must ship ≥3.51.3 (or a patched vendored
  build) ([same page, §11](https://www.sqlite.org/wal.html)).
- **Concurrency model**: one writer connection (or a serialized writer thread owning the
  connection), plus a reader pool. WAL gives snapshot isolation per read transaction; open a
  fresh short transaction per query batch rather than one giant one (checkpoint starvation, loc.
  cit.).
- **mmap**: `PRAGMA mmap_size` (typically ≥256 MB) removes a kernel→user copy on reads and can
  substantially speed blob reads; it mostly benefits queries, can *reduce* performance in some
  cases, and turns I/O errors into potential crashes — opt-in, measure
  ([SQLite mmap guide](https://www.sqlite.org/mmap.html)). In SQLite's blob benchmark, mmap +
  incremental blob I/O beat raw file reads by ~2× on macOS/Android and ~10× on Windows
  ([35% faster than the filesystem, 2025-11-13 update](https://www.sqlite.org/fasterthanfs.html)).
- **WITHOUT ROWID**: clustered on the PRIMARY KEY — one B-tree instead of two, ~half the space
  and up to ~2× faster for non-integer/composite-PK lookups; keep rows small (rule of thumb:
  <1/20 of a page, ~200 bytes at 4 KiB) ([guide, updated 2026-10-09](https://www.sqlite.org/withoutrowid.html)).
  Great for junction tables (folder membership), wrong for anything blobby.
- **Raw RFC 5322 blobs: disk files or DB?** SQLite's measurements: reading ~10 KB blobs from
  SQLite is ~35 % faster than individual files, and 10 K blobs use ~20 % less space (open/close
  amortization; page-packing). Jim Gray's classic SQL-Server study put the crossover around
  250 KB–1 MB. Larger blobs and cold caches increasingly favor files
  ([fasterthanfs](https://www.sqlite.org/fasterthanfs.html)).
  **Recommendation**: message bodies as *content-addressed files on disk* (zstd-compressed),
  metadata + search index in SQLite — bodies average tens-to-hundreds of KB, we want cheap
  partial reads (PARTIAL-friendly), cheap crash-safe appends, and simple eviction; attachments
  always on disk. Revisit with measurements once a corpus is loaded.
- **Compression**: zstd 1.5.7 on Silesia: level 1 = 510 MB/s compress, **1550 MB/s decompress**,
  ratio 2.90; `--fast=3` = 635/1980 MB/s at 2.24; lz4 decompresses at 3850 MB/s but ratio 2.10;
  zlib -1 = 105/390 MB/s (obsolete choice here). Decompression speed is ~flat across levels, so
  even level 6–9 costs little at read time. zstd's dictionary mode dramatically improves small
  inputs (<few KB) — relevant for individual MIME parts and headers
  ([official benchmarks](https://facebook.github.io/zstd/)). Email text typically compresses
  far better than Silesia's 2.9 (HTML mail is highly repetitive **(expectation, not measured)**).
  Store the *original* size + 8-byte magic in the blob header; decompress on read at
  GB/s — effectively free.
- **Dedup across folders/labels**: identity by content hash (blake3 of the normalized RFC 5322)
  as the blob key; per-account server identity columns (`X-GM-MSGID`, RFC 8474 `EMAILID`,
  Graph `id`, JMAP `id`). A message in N labels = N rows in a `folder_messages` junction table +
  1 blob. Gmail makes this essential (labels-as-folders, loc. cit.). Reference-count blobs for
  eviction.
- **Full-text search**: SQLite **FTS5** (standard SQLite feature, see sqlite.org docs) is the
  pragmatic default; notmuch's Xapian proves the scale target — "on the order of millions of
  messages… quickly search all of it" ([notmuch](https://notmuchmail.org/)). FTS5 supports
  external-content tables so the index can reference message bodies without duplicating them.
- **Schema sketch** (denormalize for the hot path):
  - `messages(msg_id PK, account_id, server_id, thread_id, date_rcvd, date_sent, from_, to_,
    snippet, flags, modseq, size, blob_hash)` — one row per unique message;
  - `folder_messages(folder_id, msg_id, uid, flags_local, PRIMARY KEY(folder_id, uid)) WITHOUT
    ROWID` + `(msg_id)` index — the IMAP-shaped junction;
  - **`mailbox_summary`** — the denormalized list the UI actually queries: per folder, a
    precomputed covering row set (or per-folder partial index) so "first 100 messages of Inbox,
    newest first, with sender/snippet/flags" is a single covering-index scan with zero joins;
  - `threads(thread_id PK, root_msg, last_msg_date, count, unread, snippet)` — precomputed
    thread rollups so a 50-message thread view is one row + one paged child query;
  - `blobs(hash PK, refcount, compressed_size, original_size, state) WITHOUT ROWID`;
  - `fts` (FTS5, external content) + `sync_state(folder_id PK, uidvalidity, uidnext, modseq,
    cursor)` — one row per folder, also storing Gmail `historyId` / Graph `deltaLink` /
    JMAP `sinceState` per account.
- **Numbers to plan around**: 1M messages ≈ metadata ~1–2 GB SQLite + bodies ~5–20 GB
  compressed on disk (rough, corpus-dependent **(estimate)**); Enron (0.5M messages, no
  attachments) is 1.7 GB *uncompressed* ([CMU](https://cs.cmu.edu/~enron/)) — i.e. ~3.4 KB per
  message before compression, which zstd should halve or better.

### 2.6 Threading at scale (Q6)

- **The algorithm**: JWZ's REFERENCES threading, standardized verbatim as IMAP `THREAD
  REFERENCES` in [RFC 5256](https://datatracker.ietf.org/doc/rfc5256/) (the RFC credits
  Zawinski's algorithm, "used in Netscape Mail and News 2.0–3.0"; jwz's original page at
  jwz.org/doc/threading.html was unreachable this session). Its six steps: link messages by
  Message-ID/References (with loop checks and truncation tolerance), root the parentless
  messages, prune dummy containers, sort sibling groups by sent date, merge same-base-subject
  threads via a subject table, re-sort. RFC 5256 also fixes the pitfalls: normalize msg-ids
  (quoting variants must compare equal), dedupe duplicate Message-IDs, never re-parent an
  existing link, and never create cycles.
- **Incremental threading.** The algorithm is *local*: a newly arrived message only (a) creates
  or joins the container for its Message-ID, (b) links its parent chain (References), and
  (c) possibly merges two roots by subject. Store a persistent `message_id → container` map and
  per-thread rollups (`last_msg_date`, `unread`, `count`) in the `threads` table; recompute
  only the affected thread's summary on insert — O(new mail), not O(mailbox). This is the same
  shape notmuch uses to keep tagging/threading cheap across millions of messages
  ([notmuch design notes / front page](https://notmuchmail.org/)).
- **Server thread IDs where they exist** (use them as *hints*, keep local threading as ground
  truth since coverage varies):
  - Gmail `X-GM-THRID` on FETCH/SEARCH — the thread ID "in the same manner as in the Gmail web
    interface" ([Gmail IMAP extensions](https://developers.google.com/gmail/imap/imap-extensions));
    also exposed as `threadId` in the Gmail API (same doc family).
  - JMAP `threadId` on every Email ([RFC 8621](https://datatracker.ietf.org/doc/rfc8621/)).
  - Graph `conversationId` on message resources **(standard Graph mail resource; not re-verified
    this session)**.
  - IMAP `THREAD` server-side (RFC 5256) — useful for online spot-checks, not for an
    offline-first client.

### 2.7 UI performance on macOS 27/26 (Q7)

**What changed in macOS 27 for this topic.** Golden Gate (GA 2026-09-14, 27.0.1 on 2026-09-28)
is a refinement release of Tahoe's design system: window-border and traffic-light (Liquid
Glass) restorations, changed animations, pull-to-refresh arriving in **Mail** and Safari, and
the platform is now **Apple-silicon-only** ([Wikipedia: macOS Golden
Gate](https://en.wikipedia.org/wiki/MacOS_Golden_Gate), MacRumors citations therein, checked
2026-10-11). No new UI-performance framework was announced that affects this report's
conclusions; the practical consequences are: (1) test on both 26 and 27 — animation-timing
changes can surface main-thread jank that was previously masked; (2) Intel is gone from the
support floor, which *raises* the performance baseline you may assume.

**The mailbox list at 500k rows.**

- **NSTableView (view-based) is the proven tool**: constant memory via cell reuse
  (`makeView(withIdentifier:)`), lazy row loading, and it's what heavy Mac apps actually use.
  SwiftUI's macOS `Table` (macOS 12+) is documented-lazy and wraps AppKit machinery; a Stack
  Overflow answer on exactly this problem recommends `Table` for large lists because "`List`
  seems to be not lazy on macOS… `Table`… is lazy"
  ([SO 72070486](https://stackoverflow.com/questions/72070486/bad-performance-of-large-swiftui-lists-on-macos)).
- **Measurements of the alternatives failing**: rendering 10k fixed-height rows in `List` ~2 s
  on an M1 Max (0.3 s after pinning row heights); 1k–5k rows "several seconds" unpinned (same
  source); `List` re-render after filter change on 16k items >20 s
  ([SO 63075386](https://stackoverflow.com/questions/63075386/swiftui-performance-issues-with-list-with-large-data-set));
  10k-row SwiftUI table beach-balls 10+ s on column sort vs. instant in AppKit
  ([r/SwiftUI](https://www.reddit.com/r/SwiftUI/comments/1dvva7k/swiftui_mac_performance/));
  `List` even tracks metadata for off-screen rows for its change animation
  ([SO 68315292](https://stackoverflow.com/questions/68315292/swiftui-list-is-extremely-slow-at-showing-actions-leading-trailing-contextmenu)).
- **Practical rules**: fixed row height (or accurate estimates); never hand `List` a 500k-array
  `ForEach`; drive the table from an id-ordered paged query (fetch pages of ~500 around the
  scroll position, `rowForView` triggers lookups); coalesce and apply diffs *off* the main
  thread, then deliver one batched change per frame; stable IDs = msg_id; avoid the
  `.id(UUID())` nuke (it works by disabling diffing/animation —
  [SO 59604764](https://stackoverflow.com/questions/59604764/performance-issue-with-swiftui-list),
  [Hacking with Swift](https://www.hackingwithswift.com/articles/210/how-to-fix-slow-list-updates-in-swiftui)).
- **Keeping the main thread free**: all parsing, crypto, imaging, DB access on QoS-appropriate
  queues; the main thread only lays out. Apple's energy guide doubles as the responsiveness
  guide here — the same prioritization avoids both battery drain and jank
  ([Energy guide archive](https://developer.apple.com/library/archive/documentation/Performance/Conceptual/power_efficiency_guidelines_osx/PrioritizeWorkAtTheTaskLevel.html)).
- **Preloading the next messages**: prefetch the next page of summaries as the user scrolls
  (query ahead ~1 screen), and prefetch likely-opened bodies (Superhuman prerenders likely
  threads, loc. cit.).

**WKWebView for HTML mail.**

- Cost structure: each WKWebView's web content runs in a separate WebContent process (visible in
  Activity Monitor); "three webviews means you have seven processes," and allocating new ones
  instead of reusing old ones is the top leak/perf mistake
  ([Embrace, 2023](https://embrace.io/blog/wkwebview-memory-leaks/)). Some pages reach
  "200+ MB per" webview ([Capacitor issue #6887](https://github.com/ionic-team/capacitor/issues/6887)).
  `WKProcessPool` is the sanctioned way to run multiple views in one process
  ([Apple docs](https://developer.apple.com/documentation/webkit/wkprocesspool)).
- First navigation pays process spawn; warm it up at launch with a throwaway
  `WKWebView` + `loadHTMLString("", baseURL: nil)`
  ([SO 74301868](https://stackoverflow.com/questions/74301868/wkwebview-ios-slow-on-first-launch));
  prewarming libraries exist ([WebViewWarmUper](https://github.com/bernikovich/WebViewWarmUper)),
  and other mail-client projects pre-warm their renderer pool exactly to skip "cold WebKit
  cost" ([brev PR #98](https://github.com/henrikogaard/brev/pull/98) — the pool "starts empty").
  Reusing one view is measurably faster than fresh views per navigation
  ([SO 36667317](https://stackoverflow.com/questions/36667317/can-using-multiple-wkwebviews-be-as-fast-as-reusing-the-same-webview)).
- **Design**: a small pool (1 visible + 1 prewarmed, LRU to ~3) of WKWebViews over a shared
  process pool and a `nonPersistent` data store; load sanitized HTML via `loadHTMLString` with
  `baseURL: nil` (no remote subresources by default — block via `WKContentRuleList` as the brev
  project does, loc. cit.).
- **Plain-text fast path**: the majority of list-visible rendering and all plaintext mail should
  use TextKit 2/Core Text — no web process at all. (MailMate generates HTML even for plain text
  and displays in a WebView — [manual](https://manual.mailmate-app.com/) — a deliberate choice
  with a rendering cost we need not copy.)
- **Image decoding off the main thread**, downsampling to display size before handing to the
  view (WWDC18 "Images and Graphics Best Practices" guidance **(unverified this session)**).

**Launch time & memory.** Treat launch as a measured budget (2.1). Mailspring markets "<1 s
launch" and "~50 % less RAM" as features — evidence these are user-visible differentiators
([getmailspring.com](https://getmailspring.com/)). Tactics: no network at launch; restore last
mailbox from the snapshot; defer non-critical services; keep the FTS index warm with a small
page cache (`PRAGMA cache_size`) sized to the working set.

### 2.8 How the fast clients do it (Q8)

| Client | Approach | Evidence |
|---|---|---|
| **Superhuman** | 100 ms rule as product principle; local database of mail; prerender/preload likely threads; minimal animations; keyboard-first; measures "% of events under 100 ms"; new renderer renders mail in 1–2 frames (<32 ms) | [Built for speed (2022)](https://blog.superhuman.com/superhuman-is-built-for-speed/); [Performance metrics (2026-07-17)](https://blog.superhuman.com/performance-metrics-for-blazingly-fast-web-apps/) |
| **Mimestream** | Native Mac app over the **Gmail API** (not IMAP); releases advertise performance work: 1.9 "improved performance, adaptive cache management"; 1.10 "lightning-fast Private Push notifications" (push without polling) | [1.9 (2026-01-15)](https://mimestream.com/blog/1.9-released); [1.10 (2026-07-14)](https://mimestream.com/blog/1.10-released) |
| **Apple Mail** | Unpublished internals; macOS 27 added pull-to-refresh to Mail; widely-reported slowness with very large mailboxes and rebuild-lost-search anecdotes are community lore **(unverified)** | [macOS 27 features](https://en.wikipedia.org/wiki/MacOS_Golden_Gate) |
| **MailMate** | AppKit IMAP client, keyboard-centric; universal mailboxes + powerful smart mailboxes backed by a local database (manual includes "Rebuild Database"); WebView-based message display; plain-text composition; Thread Arcs view | [freron.com](https://freron.com/); [manual](https://manual.mailmate-app.com/) |
| **Spark** | Cloud-assisted engine; no engineering sources consulted this session | **(not verified)** |
| **Mailspring** | Fork line of Nylas; sync in a **native C++ engine** ("not a web app running in a browser tab"), ~50 % less RAM, <1 s launch; MIT open source (engine inspectable) | [getmailspring.com](https://getmailspring.com/) |
| **mutt / notmuch** | mutt: terminal client, renders only what's visible; notmuch: Xapian full-text indexer over maildir, premise = "millions of messages" searched quickly; began as a C rewrite of Sup's performance-critical parts; ships a perf test suite + downloadable corpus | [notmuchmail.org](https://notmuchmail.org/); [performance page](https://notmuchmail.org/performance/) |

The convergent pattern across Superhuman, Mimestream, MailMate, Mailspring, notmuch:
**local store + local index + prefetch, with the network reduced to change events.**

### 2.9 Measurement (Q9)

- **In-app: os_signpost.** Wrap every budgeted interval (launch, mailbox open, message open,
  first-search-result, sync-cycle) in `os_signpost` intervals via `OSSignposter`; they render in
  Instruments (Points of Interest, os_signpost instruments) and can be exported from CI runs.
  (Standard Apple API: [OSSignposter docs](https://developer.apple.com/documentation/os/ossignposter)
  — **page not fetched this session**.) Add XCTest `measure {}` performance tests with baselines
  so regressions fail CI; run them on a fixed corpus and a fixed machine class.
- **Product metric: percent-under-100 ms.** Adopt Superhuman's framing — the health metric is
  the *share of interactions completing under the budget*, per interaction type, not the average
  ([Superhuman, 2026-07-17](https://blog.superhuman.com/performance-metrics-for-blazingly-fast-web-apps/)).
- **Instruments on macOS 27**: Time Profiler for main-thread work; Animation Hitches and the
  SwiftUI instrument for frame drops; Allocations/Leaks for the WKWebView pool; os_signpost
  track for your intervals; Energy Log for background-sync cost (App Nap visibility via Activity
  Monitor's Energy pane — [Apple guide](https://developer.apple.com/library/archive/documentation/Performance/Conceptual/power_efficiency_guidelines_osx/AppNap.html)).
- **Corpora**: **Enron** — ~150 users, ~0.5M messages, May 2015 edition is a 1.7 GB tar.gz (no
  attachments) ([CMU](https://cs.cmu.edu/~enron/)). To test 1M+ messages: replicate/duplicate
  Enron 2–4× with rewritten Message-IDs (keeps real-world shape), or synthesize with
  realistic MIME mixes. notmuch ships its own performance suite and a downloadable corpus as a
  model for harness design ([notmuch performance](https://notmuchmail.org/performance/)).
  CI gate examples: import 1M messages < N minutes; mailbox switch p95 < 50 ms with 1M rows;
  keystroke-to-result < 100 ms on a 1M-message FTS query; launch < 800 ms cold.
- **SQLite-specific**: use the kvtest methodology (fixed blob corpus, repeated runs, cache
  warmed) when tuning blob storage ([kvtest write-up](https://www.sqlite.org/fasterthanfs.html));
  EXPLAIN QUERY PLAN in tests to pin covering-index usage for the hot queries.

## 3. Options compared

### 3.1 Protocol strategy per account type

| Option | Gmail | M365/Exchange | Generic IMAP (Fastmail/Cyrus/Stalwart/Dovecot) | Verdict |
|---|---|---|---|---|
| IMAP-only client | Possible but no QRESYNC ⇒ periodic flag re-scans; labels-as-folders cost | Possible (OAuth2) but extension-poor (observed: UIDPLUS, MOVE, IDLE, LITERAL+ only) ⇒ slow reconnects | Excellent where server is Cyrus/Stalwart | Rejected as the only path |
| API-only for big providers + IMAP elsewhere | Gmail API w/ history.list + Pub/Sub | Graph delta + notifications | n/a (JMAP where offered) | **Recommended** |
| JMAP-first, IMAP fallback | ✗ | ✗ | Best where available (Fastmail, Stalwart, Cyrus) | **Recommended where present** |

### 3.2 Local store

| Option | Pros | Cons |
|---|---|---|
| SQLite (metadata + FTS5) + zstd blob files | Proven scale (WAL, mmap, WITHOUT ROWID); crash-safe; single-writer simplicity; FTS built in | Two stores to keep consistent; blob GC by refcount |
| Everything in SQLite (blobs included) | One transaction across metadata+blob; ~35 % faster small-blob reads than files; ~20 % smaller | Big blobs bloat DB/WAL; worse partial reads; vacuum pain at 20 GB+ |
| Custom store (e.g. LMDB/redb) + tantivy | Purpose-built | Reinventing SQLite's battle-tested crash safety; ecosystem cost |

Decision inputs from 2.5: SQLite's own blob-vs-files data favors the DB up to ~10–100 KB blobs
and files above ([fasterthanfs](https://www.sqlite.org/fasterthanfs.html)); mail bodies sit at
the boundary, and dedup/eviction/partial-fetch push toward files.

### 3.3 macOS message list

| Option | Scales to 500k? | Evidence |
|---|---|---|
| SwiftUI `List` | No | 2 s for 10k rows; 20 s filter of 16k; per-item metadata overhead (2.7) |
| SwiftUI `Table` | Probably (lazy) | Recommended for large lists on macOS ([SO 72070486](https://stackoverflow.com/questions/72070486/bad-performance-of-large-swiftui-lists-on-macos)) |
| NSTableView + NSViewCells | Yes (proven) | The AppKit answer the Reddit reporter fell back to (2.7) |
| LazyVStack in ScrollView | No | Non-reusing; blows up on long scrolls ([SO 58256373](https://stackoverflow.com/questions/58256373/performance-issue-when-creating-a-list-of-1000-elements-in-swiftui)) |

## 4. Recommendations for Sweetbriar

1. **Adopt the latency budget table in 2.1 as a product spec**, with the "% of interactions
   under budget" metric reported in CI and (opt-in) telemetry. Design point: 50 ms, ceiling
   100 ms.
2. **Day one: the store is the product.** SQLite (WAL, ≥3.51.3 given the 2026-03 WAL bug),
   single writer + reader pool, denormalized `mailbox_summary` + `threads` tables, FTS5 index,
   zstd content-addressed body blobs on disk with refcounted dedup. This is the decision that
   is hardest to change later — every later feature (search, threading, offline, multi-account)
   sits on it.
3. **Day one: account adapter trait with a delta-sync contract.**
   `trait AccountBackend { fn changes(since: Cursor) -> Vec<Change>; fn fetch(ids) -> …; }`
   with adapters: Gmail API (historyId), Graph (deltaLink), JMAP (sinceState), IMAP+QRESYNC
   (modseq/uidvalidity), IMAP-fallback (STATUS + UID windows). Include push plumbing (IDLE /
   NOTIFY / Pub/Sub / Graph webhooks) in the same trait.
4. **Local-first UI contract**: the UI layer is *only ever* allowed to read the local store;
   all network is behind the sync engine's priority queue (QoS-mapped). Optimistic mutations
   journaled and reconciled.
5. **macOS UI**: NSTableView-backed list (SwiftUI `Table` acceptable) with fixed row heights,
   paged queries, batched diffs off-main; plain-text fast path via TextKit 2; pooled, prewarmed
   WKWebView (shared process pool, non-persistent store, content-rule ad blocker) for HTML;
   image decode downscaled off-main. Target 26 and 27; verify animations on both since 27
   changed system animation timing (2.7).
6. **Threading**: incremental JWZ/RFC 5256 REFERENCES in the core with the subject-merge table;
   server thread IDs (X-GM-THRID / JMAP threadId / conversationId) stored as hints and used for
   UI grouping when present.
7. **Energy**: background sync at utility QoS, IDLE-only waiting, explicit assertions for
   user-initiated imports, respect App Nap by going idle on our own (2.4).
8. **Measure from the first build**: os_signpost intervals for every budget row; XCTest perf
   tests on the Enron-derived 1M-message corpus as a required CI gate.

**Trade-offs I accept**: disk usage for the full local copy (tens of GB for power users);
complexity of five sync adapters; weaker offline search for accounts whose bodies we haven't
prefetched yet (mitigated by prioritized prefetch).

**Strongest counter-argument**: *local-first with full copies is over-engineering for v1 — a
headers-only cache with bodies-on-demand is cheaper to build, uses far less disk, and most
users never scroll past the first screen.* It's a fair attack: Superhuman-level speed can be
approximated for the "top of inbox" use case with a much smaller cache. The rebuttal — and why
I'd still build the full store — is that Sweetbriar's stated goals (best-in-class search,
classic-client power features, huge mailboxes, "never lose mail") all *require* the local index
and body store; bolting it on later means a painful migration of the exact schema decisions
(2.5) that are hardest to change, and the incremental cost at v1 is mostly the prefetch policy,
which is a dial, not an architecture.

## 5. Risks and open questions

- **Unverified server facts** (probe CAPABILITY at runtime; re-verify before committing):
  Gmail's live post-login CAPABILITY (is QRESYNC still absent?); iCloud's and Yahoo's extension
  sets; Dovecot 2.4's IMAP4rev2/OBJECTID status; Exchange Online's post-login capabilities;
  Fastmail's deployed Cyrus version.
- **Graph SMTP AUTH / basic-auth endgame**: Microsoft has an updated deprecation timeline for
  SMTP AUTH basic (techcommunity link in the Exchange doc); watch it — sending must move first
  to Graph `sendMail` if SMTP AUTH OAuth flows tighten further.
- **Gmail API quotas** (per-user rate limits) shape batch sizes for initial sync — need
  explicit quota testing **(not measured this session)**.
- **SQLite WAL bug class**: the 2026-03 WAL-reset episode (fixed 3.51.3) is a reminder to pin
  and track SQLite releases; also note the Aug 2026 public reproducer
  ([theconsensus.dev post cited by sqlite.org](https://www.sqlite.org/wal.html)).
- **SwiftUI `Table` at 500k rows**: "lazy" per community evidence, but no 500k-row public
  benchmark was found; prototype NSTableView-vs-Table at 1M early.
- **macOS 27 regressions**: animation-timing changes may require re-tuning custom animations;
  Apple's 27 release notes should be monitored
  ([link](https://developer.apple.com/documentation/macos-release-notes/macos-27-release-notes))
  **(not fetched this session)**.
- **Search index rebuild cost**: FTS5 indexing 1M messages will take minutes on first import —
   needs progress UI, background QoS, and resumability (notmuch's compact/write-lock behavior
   is a cautionary tale, [performance page](https://notmuchmail.org/performance/)).
- **Privacy/telemetry tension**: performance telemetry must be opt-in and content-free given
  Sweetbriar's audience.

## 6. Sources consulted (2026-10-11)

Latency & UX: [NN/g response-time limits](https://www.nngroup.com/articles/response-times-3-important-limits/) ·
[web.dev RAIL](https://web.dev/articles/rail) ·
[Superhuman — built for speed](https://blog.superhuman.com/superhuman-is-built-for-speed/) ·
[Superhuman — performance metrics](https://blog.superhuman.com/performance-metrics-for-blazingly-fast-web-apps/)

Protocols & servers: [RFC 5256 (SORT/THREAD, full text incl. REFERENCES algorithm)](https://datatracker.ietf.org/doc/rfc5256/) ·
RFC index pages: [9051](https://datatracker.ietf.org/doc/rfc9051/), [7162](https://datatracker.ietf.org/doc/rfc7162/), [8474](https://datatracker.ietf.org/doc/rfc8474/), [8970](https://datatracker.ietf.org/doc/rfc8970/), [9394](https://datatracker.ietf.org/doc/rfc9394/), [8620](https://datatracker.ietf.org/doc/rfc8620/), [8621](https://datatracker.ietf.org/doc/rfc8621/) ·
[Gmail IMAP extensions (Google, updated 2026-09-15)](https://developers.google.com/gmail/imap/imap-extensions) ·
[Gmail API sync guide](https://developers.google.com/gmail/api/guides/sync) ·
[Microsoft Graph delta-query messages](https://learn.microsoft.com/en-us/graph/delta-query-messages) ·
[Exchange Online basic-auth deprecation (updated 2026-07)](https://learn.microsoft.com/en-us/exchange/clients-and-mobile-in-exchange-online/deprecation-of-basic-authentication-exchange-online) ·
[SO: Outlook 365 IMAP capability capture](https://stackoverflow.com/questions/65690142/is-there-a-detailed-specification-for-outlook-365-imap-which-rfcs-does-it-comp) ·
[Cyrus IMAP RFC support list (3.12.4 docs)](https://www.cyrusimap.org/imap/rfc-support.html) ·
[Cyrus features](https://www.cyrusimap.org/imap/concepts/features.html) ·
[Stalwart mail server](https://stalw.art/mail-server) ·
[Dovecot IMAP server docs (2.3)](https://doc.dovecot.org/2.3/configuration_manual/protocols/imap_server/) ·
[Fastmail dev/API](https://www.fastmail.com/dev/) ·
[Fastmail open source (Cyrus)](https://www.fastmail.com/company/open-source/) ·
[Fastmail CONDSTORE/QRESYNC post (2010)](https://www.fastmail.com/blog/new-popimap-server-version/) ·
[Fastmail IMAP perf post (2009)](https://www.fastmail.com/blog/help-test-proxy-to-improve-imap-performance/)

Storage: [SQLite WAL (updated 2026-08-25)](https://www.sqlite.org/wal.html) ·
[SQLite mmap](https://www.sqlite.org/mmap.html) ·
[SQLite WITHOUT ROWID (updated 2026-10-09)](https://www.sqlite.org/withoutrowid.html) ·
[SQLite fasterthanfs (2025-11-13)](https://www.sqlite.org/fasterthanfs.html) ·
[zstd benchmarks](https://facebook.github.io/zstd/)

macOS & UI: [Wikipedia: macOS](https://en.wikipedia.org/wiki/MacOS) ·
[Wikipedia: macOS Golden Gate](https://en.wikipedia.org/wiki/MacOS_Golden_Gate) ·
[Apple: App Nap (Energy guide archive)](https://developer.apple.com/library/archive/documentation/Performance/Conceptual/power_efficiency_guidelines_osx/AppNap.html) ·
[WKProcessPool](https://developer.apple.com/documentation/webkit/wkprocesspool) ·
[Embrace: WKWebView memory/process cost](https://embrace.io/blog/wkwebview-memory-leaks/) ·
[SO 74301868 WKWebView warm-up](https://stackoverflow.com/questions/74301868/wkwebview-ios-slow-on-first-launch) ·
[SO 36667317 webview reuse](https://stackoverflow.com/questions/36667317/can-using-multiple-wkwebviews-be-as-fast-as-reusing-the-same-webview) ·
[WebViewWarmUper](https://github.com/bernikovich/WebViewWarmUper) ·
[brev renderer prewarm PR](https://github.com/henrikogaard/brev/pull/98) ·
[Capacitor #6887 (webview memory)](https://github.com/ionic-team/capacitor/issues/6887) ·
[SO 72070486 large SwiftUI lists on macOS](https://stackoverflow.com/questions/72070486/bad-performance-of-large-swiftui-lists-on-macos) ·
[SO 63075386 List 16k items](https://stackoverflow.com/questions/63075386/swiftui-performance-issues-with-list-with-large-data-set) ·
[SO 68315292 List metadata overhead](https://stackoverflow.com/questions/68315292/swiftui-list-is-extremely-slow-at-showing-actions-leading-trailing-contextmenu) ·
[SO 59604764 .id(UUID())](https://stackoverflow.com/questions/59604764/performance-issue-with-swiftui-list) ·
[Hacking with Swift: slow List updates](https://www.hackingwithswift.com/articles/210/how-to-fix-slow-list-updates-in-swiftui) ·
[r/SwiftUI mac performance](https://www.reddit.com/r/SwiftUI/comments/1dvva7k/swiftui_mac_performance/) ·
[SO 58256373 LazyVStack](https://stackoverflow.com/questions/58256373/performance-issue-when-creating-a-list-of-1000-elements-in-swiftui) ·
[OSSignposter docs](https://developer.apple.com/documentation/os/ossignposter) (not fetched)

Clients & corpora: [Mimestream blog](https://mimestream.com/blog) ([1.9](https://mimestream.com/blog/1.9-released), [1.10](https://mimestream.com/blog/1.10-released)) ·
[freron.com (MailMate)](https://freron.com/) · [MailMate manual](https://manual.mailmate-app.com/) ·
[getmailspring.com](https://getmailspring.com/) ·
[notmuchmail.org](https://notmuchmail.org/) · [notmuch performance](https://notmuchmail.org/performance/) ·
[Enron corpus (CMU)](https://cs.cmu.edu/~enron/)

Search engines used to locate sources (results pages, not content):
[Brave Search](https://search.brave.com) (Fastmail/Cyrus, Superhuman, SwiftUI, WKWebView,
Exchange capabilities). DuckDuckGo and Mojeek were rate-limited/captcha-blocked on 2026-10-11.
jwz.org/doc/threading.html timed out (HTTP and via Wayback 403); RFC 5256 was used as the
authoritative replacement.
