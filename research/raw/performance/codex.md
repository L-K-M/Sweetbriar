<!-- provenance: helper=codex model=gpt-6.1-sol started=2026-10-11T18:14:20+00:00 exit=0 brief=research/briefs/ -->

# Making Sweetbriar fast and responsive

**Research date: 2026-10-11.** This is an architecture research report, not a benchmark of an existing Sweetbriar implementation. Published facts, historical measurements, and proposed engineering targets are identified separately. Linked sources were consulted on 2026-10-11. A provider’s documentation or server implementation does not prove what every deployed endpoint advertises.

## 1. TL;DR

- **Make ordinary interactions local.** Propose a 100 ms p95 budget for mailbox switching, cached message opening, archive/delete feedback, and compose opening. Network completion needs its own measurement. [Apple responsiveness guidance](https://developer.apple.com/documentation/xcode/improving-app-responsiveness)
- **Treat frame time as a separate requirement.** Target less than 5 ms of main-thread work per update, with display deadlines of 16.7 ms at 60 Hz and 8.3 ms at 120 Hz. [Apple responsiveness guidance](https://developer.apple.com/documentation/xcode/improving-app-responsiveness)
- **Choose identity, durability, cancellation, and bounded data transfer on day one.** These are architectural recommendations: stable messages with separate mailbox memberships, a durable mutation outbox, recoverable drafts, and paged summaries.
- **Use provider-specific delta sync behind a shared core.** Gmail history, Graph per-folder delta, JMAP state changes, and IMAP QRESYNC have different semantics. [Gmail sync](https://developers.google.com/workspace/gmail/api/guides/sync), [Graph delta](https://learn.microsoft.com/en-us/graph/delta-query-messages), [IMAP QRESYNC](https://www.rfc-editor.org/rfc/rfc7162.html)
- **SQLite is a strong default, subject to workload design.** WAL permits readers and one writer to coexist, but checkpoints and long-lived readers affect latency. [SQLite WAL](https://sqlite.org/wal.html)
- **Evaluate SwiftUI on the target OS.** Apple reported 6× faster loading and up to 16× faster updates for large macOS lists in WWDC25; those are relative vendor measurements, not proof of email workload latency. [What’s new in SwiftUI](https://developer.apple.com/videos/play/wwdc2025/256/)
- **Start useful, then become complete.** Proposed policy: recent metadata first, visible bodies next, incremental offline coverage in the background, with explicit search and offline completeness.
- **Measure while syncing and under failure.** A fast idle demo does not establish responsiveness during imports, indexing, large threads, or crash recovery. This is a proposed acceptance requirement.

## 2. Findings

### 2.1. Latency budgets: what should feel instant?

Superhuman’s 2022 explanation sets a 100 ms interaction rule, says it aims below 50 ms where possible, and describes local caching plus preloading/prerendering likely threads. These are firsthand design claims and marketing, not independent p95 measurements. Google’s RAIL guidance budgets 100 ms for visible response and 50 ms for input handling. Its current page recommends Core Web Vitals over RAIL for web performance goals; RAIL remains a useful conceptual reference for this native client. [Superhuman, 2022-06-28](https://blog.superhuman.com/superhuman-is-built-for-speed/), [RAIL, updated 2020-06-10](https://web.dev/articles/rail)

Apple’s developer performance guidance is more directly applicable: discrete delays become noticeable around 50–100 ms; continuous interaction must fit the display interval. Apple suggests less than 5 ms of main-thread update work. The HIG asks apps to show content promptly, keep other actions available while loading, and communicate progress. It does not establish a universal numerical cold-launch requirement. [Understanding UI responsiveness](https://developer.apple.com/documentation/xcode/understanding-user-interface-responsiveness/), [HIG loading, checked 2026-10-11](https://developer.apple.com/design/human-interface-guidelines/loading)

**Proposed Sweetbriar targets, not published measurements:** measure event-to-useful-visible-result across Rust, Swift, and rendering. Establish a reference machine of an Apple-silicon Mac with 16 GiB RAM, SSD, macOS 26, a populated account, and a Release build; record exact model and OS build in every result. Use p95 as the primary gate and p99 to reveal stalls.

| Operation | Proposed p95 | Proposed p99 / additional requirement |
|---|---:|---|
| Cold process launch with existing local store | Useful mailbox within 1.0 s | Within 2.0 s; independent of network; separately measure genuinely cold filesystem cache |
| Warm launch | 400 ms | 800 ms |
| Switch mailbox | First useful rows within 100 ms | 200 ms; preserve selection and scroll state |
| Open cached ordinary message | Readable content within 100 ms | 200 ms; header/selection feedback within 50 ms |
| Open uncached message | Header and fetch state within 100 ms | Track network-to-readable separately; no universal bound over arbitrary networks |
| Open a cached 50-message thread | Summaries and selected readable message within 100 ms | Visible expanded content within 250 ms; full traversal measured separately |
| Archive/delete | Visible state within 50 ms; durable local intent within 100 ms | Durable intent within 200 ms; remote acknowledgement separate |
| Search as you type | Useful top results within 100 ms of final keystroke | 200 ms; proposed 30–50 ms coalescing included in the budget |
| Open compose window | Editable fields within 100 ms | 200 ms; attachment/signature work cannot block typing |
| Scroll a 500k-message mailbox | Main-thread update under 5 ms | Sustained display cadence; no whole-mailbox work per frame |

A collapsed 50-message thread is not equivalent to rendering 50 HTML documents. Test both selected-message reading and “expand all,” but permit progressive offscreen rendering for the latter. Likewise, launching a newly added million-message account is an import workflow, not ordinary cold launch. These are proposed measurement definitions.

### 2.2. Why email clients feel slow

The mechanisms below are documented; their application to Sweetbriar is an engineering inference, not an allegation that every named client implements them badly.

| Cause | Mechanism and example | Design response |
|---|---|---|
| Sync blocks UI | Main thread is busy or waits for I/O, locks, or a worker; Apple distinguishes busy and blocked hangs | Asynchronous snapshots; no sync barrier before showing cached mail |
| Bodies fetched only after opening | Reading adds network requests; Gmail explicitly recommends caching full/raw content | Prefetch likely next bodies; measure cache-hit rate |
| Serial IMAP work | Dependent SELECT/SEARCH/FETCH steps add round trips; pipelining cannot remove true dependencies | Connection reuse, UID batching, delta sync |
| Huge mailbox eagerly materialized | Every row is transferred, formatted, sorted, and retained | Bounded summaries, no MIME in row models |
| Threading globally recomputed | Rebuilding all conversations makes arrival cost grow with archive size | Incremental graph updates and cached summaries |
| Search over uncached bodies | Local search lacks content; scans or remote search interrupt typing | Maintained text index and clear coverage |
| Main-thread database work | SQL can block on storage, checkpoints, or locks | Reader workers and short transactions |
| HTML and large conversations | Style/layout and many expanded documents amplify renderer work | Render visible members progressively; plain-text fast path |
| Preview and image work | Decode and preview generation compete with frames | Bounded workers; previews on demand |
| Label multiplication | Gmail exposes labels as folders; one message appears in several views | Stable identity plus memberships |

Sources: [Apple responsiveness](https://developer.apple.com/documentation/xcode/improving-app-responsiveness), [Gmail caching](https://developers.google.com/workspace/gmail/api/guides/sync), [IMAP pipelining](https://www.rfc-editor.org/rfc/rfc9051.html#section-5.5), [SQLite WAL](https://sqlite.org/wal.html), [Gmail IMAP extensions](https://developers.google.com/workspace/gmail/imap/imap-extensions). Detailed image, threading, HTML, and large-list evidence follows.

### 2.3. Protocols: eliminate work and round trips

The useful distinction is between **a protocol feature, a server implementation, and an enabled hosted endpoint**. Sweetbriar should negotiate capabilities after TLS/authentication, keep a capability record for diagnostics, and retain a correct fallback. An extension can reduce transfer without making server computation cheap.

| Feature | Concrete benefit and limitation | Specification |
|---|---|---|
| IMAP4rev2 | Incorporates several formerly separate extensions, including MOVE, UIDPLUS, ESEARCH, LIST-STATUS, and BINARY; it does not imply QRESYNC | [RFC 9051, 2021-08](https://www.rfc-editor.org/rfc/rfc9051.html) |
| CONDSTORE / QRESYNC | MODSEQ identifies changed flags; QRESYNC adds efficient reconnect reconciliation and VANISHED deletions. It does not fetch every new body | [RFC 7162, 2014-05](https://www.rfc-editor.org/rfc/rfc7162.html) |
| IDLE | Selected-mailbox notifications without repeated polling; restart IDLE at least every 29 minutes and reconcile after reconnect | [RFC 2177, 1997-06](https://www.rfc-editor.org/rfc/rfc2177.html) |
| NOTIFY | Subscribe to specified events across mailbox sets; reduces one-IDLE-connection-per-folder pressure | [RFC 5465, 2009-02](https://www.rfc-editor.org/rfc/rfc5465.html) |
| COMPRESS=DEFLATE | Compresses the IMAP stream, particularly useful for repetitive metadata/text; consumes CPU | [RFC 4978, 2007-07](https://www.rfc-editor.org/rfc/rfc4978.html) |
| MOVE | Server-side move avoids the client COPY/flag/expunge sequence; atomicity applies to individual messages, not necessarily the entire set | [RFC 6851, 2013-01](https://www.rfc-editor.org/rfc/rfc6851.html) |
| UIDPLUS | APPENDUID/COPYUID map new identities; UID EXPUNGE targets specific deleted messages | [RFC 4315, 2005-12](https://www.rfc-editor.org/rfc/rfc4315.html) |
| ESEARCH | Request COUNT/MIN/MAX instead of an enormous UID list when those are sufficient | [RFC 4731, 2006-11](https://www.rfc-editor.org/rfc/rfc4731.html) |
| LIST-STATUS | Retrieve folder information and requested status together | [RFC 5819, 2010-03](https://www.rfc-editor.org/rfc/rfc5819.html) |
| SPECIAL-USE | Identify Sent, Drafts, Junk, Trash, Archive, etc. without localized-name guesses; not itself a throughput feature | [RFC 6154, 2011-03](https://www.rfc-editor.org/rfc/rfc6154.html) |
| OBJECTID | Stable MAILBOXID/EMAILID/THREADID can simplify identity, moves, and deduplication | [RFC 8474, 2018-09](https://www.rfc-editor.org/rfc/rfc8474.html) |
| BINARY | Retrieve transfer-decoded MIME parts, avoiding base64 expansion in the response | [RFC 3516, 2003-04](https://www.rfc-editor.org/rfc/rfc3516.html) |
| PREVIEW | Server-generated snippets avoid downloading complete bodies just to populate rows; LAZY permits cheap-only generation | [RFC 8970, 2020-12](https://www.rfc-editor.org/rfc/rfc8970.html) |
| PARTIAL | Page SEARCH results and UID FETCH selection; this is not a body byte-range extension or arbitrary relevance sorting | [RFC 9394, 2023-06](https://www.rfc-editor.org/rfc/rfc9394.html) |
| LITERAL+ | Send client literals without waiting for a continuation. LITERAL- limits this to 4,096 octets; rev2 incorporates LITERAL-, not unlimited LITERAL+ | [RFC 7888, 2016-05](https://www.rfc-editor.org/rfc/rfc7888.html) |

IMAP allows pipelining, but dependencies and ambiguous untagged responses restrict which commands can overlap. Prefer UID batches and avoid doing one SEARCH/FETCH per message. Proposed connection policy: start with two connections per account, reserve interactive capacity, and allow a measured ceiling of four where the provider permits it. More connections are not a substitute for batching. At an assumed 100 ms RTT, five serial round trips cost at least 500 ms before server work or payload transfer—arithmetic, not a provider benchmark. [IMAP command concurrency](https://www.rfc-editor.org/rfc/rfc9051.html#section-5.5)

**Server implementation evidence, checked 2026-10-11:** `D` means documented implementation; `E` experimental/configuration-dependent; `P` explicitly planned, not implemented; `?` means **(unverified)**. Dovecot’s RFC list and configuration document support most established extensions; its current source exposes rev2 behind an experimental setting with a false default. Cyrus documents the broadest verified set here. Stalwart’s inventory, updated 2026-10-03, explicitly lists COMPRESS and NOTIFY as planned and OBJECTID as a newer bis draft. These statements do not establish any particular deployment’s configuration. [Dovecot RFC inventory](https://doc.dovecot.org/latest/core/summaries/rfc.html), [Dovecot configuration](https://doc.dovecot.org/latest/core/config/imap.html), [Dovecot rev2 setting](https://raw.githubusercontent.com/dovecot/core/main/src/imap-login/imap-login-settings.c), [Cyrus RFC support](https://www.cyrusimap.org/stable/imap/rfc-support.html), [Stalwart RFC inventory](https://stalw.art/docs/development/rfcs/)

| Feature | Dovecot | Cyrus | Stalwart |
|---|---|---|---|
| IMAP4rev2 | E | D | D |
| CONDSTORE / QRESYNC | D | D | D |
| IDLE | D | D | D |
| NOTIFY | D, requires mailbox-list index | D | P |
| COMPRESS=DEFLATE | D | D | P |
| MOVE | D | D | D |
| UIDPLUS | D | D | D |
| ESEARCH | D | D | D |
| LIST-STATUS | D | D | D |
| SPECIAL-USE | D, configure mailbox roles | D | D |
| OBJECTID | ? | D | Bis draft; RFC 8474 compatibility unverified |
| BINARY | D | D | D |
| PREVIEW | D | D | D |
| PARTIAL | ? | D | ? |
| LITERAL+ | D | D | D |

**Hosted endpoint evidence:** on 2026-10-11 at 18:32 UTC, this research connected over certificate-verified TLS on port 993, issued CAPABILITY, and logged out, without authenticating. `A` below means explicitly advertised in that response; `D` means official documentation additionally confirms the feature; `?` means **(unverified)**, not unsupported. Pre-authentication lists may omit extensions available after login. Probe transcripts are saved in [protocol-probes.json](codex-artifacts/protocol-probes.json). Gmail’s documentation additionally confirms SPECIAL-USE and its X-GM extensions. [Gmail extension documentation](https://developers.google.com/workspace/gmail/imap/imap-extensions)

| Feature | Gmail | iCloud | Fastmail | Microsoft 365 | Yahoo |
|---|---|---|---|---|---|
| IMAP4rev2 | ? | ? | A | ? | ? |
| CONDSTORE / QRESYNC | ? | ? | A / A | ? | ? |
| IDLE | A | ? | A | A | A |
| NOTIFY | ? | ? | ? | ? | ? |
| COMPRESS=DEFLATE | ? | ? | ? | ? | ? |
| MOVE | ? | ? | A | A | A |
| UIDPLUS | ? | ? | A | A | A |
| ESEARCH | ? | ? | Rev2 includes it | ? | ? |
| LIST-STATUS | ? | ? | Rev2 includes it | ? | A |
| SPECIAL-USE | D | ? | A | ? | A |
| OBJECTID | ? | ? | ? | ? | A |
| BINARY | ? | ? | Rev2 includes it | ? | ? |
| PREVIEW | ? | ? | ? | ? | ? |
| PARTIAL | ? | ? | ? | ? | A |
| LITERAL+ | ? | ? | A | A | A |

The probed hosts were `imap.gmail.com`, `imap.mail.me.com`, `imap.fastmail.com`, `outlook.office365.com`, and `imap.mail.yahoo.com`. Apple documents iCloud IMAP access in its 2026-02-06 support page; Microsoft documents tenant settings that can disable IMAP, including security defaults. Therefore an unavailable IMAP connection is not proof that the account has no mail API. Fastmail publishes both IMAP and JMAP access. [iCloud settings](https://support.apple.com/en-ie/102525), [Exchange Online IMAP settings, updated 2023-11-02](https://learn.microsoft.com/en-us/exchange/clients-and-mobile-in-exchange-online/pop3-and-imap4/pop3-and-imap4), [Fastmail developer access](https://www.fastmail.com/dev/)

**JMAP:** RFC 8620 batches method calls with result references, advertises server limits, and provides state-based changes plus EventSource/push mechanisms. RFC 8621 adds Email/queryChanges, mailbox memberships, previews, and thread IDs. A changes request can fail with `cannotCalculateChanges`; resynchronization remains necessary. Fastmail, Cyrus, and Stalwart document JMAP support. Cyrus specifically says PushSubscription methods are not yet supported; do not equate “JMAP implemented” with every push transport. Recommend JMAP as the preferred path where available, with capability-driven properties and bounded batches. [JMAP core, 2019-07](https://www.rfc-editor.org/rfc/rfc8620.html), [JMAP Mail, 2019-08](https://www.rfc-editor.org/rfc/rfc8621.html), [Fastmail](https://www.fastmail.com/dev/), [Cyrus limitations](https://www.cyrusimap.org/stable/imap/rfc-support.html), [Stalwart](https://stalw.art/docs/development/rfcs/)

Stalwart’s current configuration documents EventSource, push-subscription, and WebSocket notification controls. For a running desktop client, prefer an advertised direct event stream when available; durable state reconciliation still supplies correctness after missing events. Verify each account’s Session/event endpoint rather than assuming a hosted push service from the protocol name. [Stalwart JMAP push settings, checked 2026-10-11](https://stalw.art/docs/ref/object/jmap/)

**Gmail API:** use `history.list` for changes since the saved history ID. An expired ID yields HTTP 404 and requires full reconciliation. Gmail says history is typically available for at least a week, but sometimes less. `watch` notifications contain a history ID, not complete changes; renew at least every seven days, preferably daily. Delivery may be delayed/dropped, and Gmail limits notifications to one per second per user. Retain periodic reconciliation. Cloud Pub/Sub is a transport/infrastructure choice, not automatic native macOS delivery. [Sync guide, updated 2026-09-15](https://developers.google.com/workspace/gmail/api/guides/sync), [Push guide, updated 2026-09-15](https://developers.google.com/workspace/gmail/api/guides/push)

**Important current quota change:** Google’s quota page, updated 2026-09-10, gives new projects created on/after 2026-05-01 a 6,000-unit/user/minute and 1,200,000-unit/project/minute limit; `messages.get` costs 20 units and `history.list` two. Existing qualifying projects have different legacy limits. At those new-project values, one million separate message-get requests alone take **at least 55.6 hours** at one user’s theoretical maximum: 1,000,000 × 20 ÷ 6,000 ÷ 60. This excludes all other requests, contention, retries, and foreground capacity. Thread-oriented retrieval may change the calculation; do not promise million-message onboarding in minutes. [Gmail quota reference](https://developers.google.com/workspace/gmail/api/reference/quota)

For large Gmail imports, benchmark thread retrieval and bulk IMAP fetching where IMAP is permitted. A hybrid backend needs tested cross-protocol identity mapping and reconciliation; the 55.6-hour calculation is specific to separate message-get requests, not a lower bound for every ingestion strategy.

**Microsoft Graph:** message delta is **per folder**, with opaque `nextLink` pages and a final `deltaLink`; select only necessary fields. Use `Prefer: IdType="ImmutableId"` consistently because ordinary Outlook IDs change on moves; immutable IDs remain stable within a mailbox, with documented archive/export exceptions. Outlook limits are 10,000 requests per ten minutes and four concurrent requests per app/mailbox, checked 2026-10-11. Batch requests do not remove those limits. Follow `Retry-After`, reserve an interactive slot, and store folder cursors with committed changes. [Message delta](https://learn.microsoft.com/en-us/graph/delta-query-messages), [Immutable IDs](https://learn.microsoft.com/en-us/graph/outlook-immutable-id), [Outlook throttling](https://learn.microsoft.com/en-us/graph/throttling-limits)

Graph change notifications can trigger delta retrieval, but their published Outlook delivery estimate is under one minute on average and up to three minutes maximum; this is not a 100 ms arrival guarantee. Subscription renewal and reachable delivery infrastructure need design. Outlook delta-token validity is not a fixed interval, so reconciliation must survive token invalidation. Recommendation across every backend: commit cursor advancement atomically with applied changes; retain downloaded mail and pending user intent during resync. [Notification latency and lifetime, checked 2026-10-11](https://learn.microsoft.com/en-us/graph/change-notifications-overview), [Delta-token validity](https://learn.microsoft.com/en-us/graph/delta-query-overview)

### 2.4. Sync strategy and scheduling

**Recommendation:** make all metadata local eventually, download recent readable bodies early, and grow an explicitly managed offline archive in the background. A headers-only account starts cheaply but cannot promise complete offline body search. Full raw download provides stronger offline coverage but consumes time, bandwidth, and disk. Google’s own guidance prioritizes enough recent messages for a responsive interface, then caches full/raw content when retrieved. [Gmail sync, updated 2026-09-15](https://developers.google.com/workspace/gmail/api/guides/sync)

Proposed initial prefetch policy: fetch the selected message, then the next five and previous two in the current navigation order; fetch visible thread bodies before collapsed older members. Adapt to navigation direction, hit rate, network cost, battery state, and cache pressure. Cancel obsolete speculative work; never cancel committed user intent. Large attachments and remote images need separate policies. These counts are starting hypotheses to measure, not evidence-backed universal optima.

Use a shared Rust scheduler with separate resource limits for network requests, database writes, MIME parsing, search, and preview generation. Reserve capacity for interactive work. Proposed order: durable draft/outbox writes; selected-message fetch and current search; visible metadata refresh; nearby prefetch; historical sync; indexing and maintenance. Promote dependencies and age queued work so a busy user cannot starve background completion indefinitely.

Do not run an entire import as one uninterruptible transaction. Start with bounded batches of about 250 metadata changes and tune against measured writer occupancy. An already running batch still delays a higher-priority draft save. Similarly, cancelling a search must interrupt computation or abandon it promptly, not merely hide its result. Tokio documents that started `spawn_blocking` tasks cannot be aborted and recommends limiting CPU concurrency; use cooperative cancellation, bounded executors, and SQLite interrupt handles where applicable. [Tokio spawn_blocking, checked 2026-10-11](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html), [rusqlite interrupt support](https://docs.rs/rusqlite/latest/rusqlite/)

On macOS, map short UI work to `userInteractive`, selected-message/search work to `userInitiated`, visible imports to `utility`, and archive indexing/maintenance to `background`. Apple’s archived energy guide says idle apps should spend at least 90% of their time at utility QoS or lower. This is OS guidance, not a promise that a Rust future’s logical priority automatically becomes a macOS thread QoS. Implement and profile that platform adapter explicitly. [Apple QoS guide](https://developer.apple.com/library/archive/documentation/Performance/Conceptual/power_efficiency_guidelines_osx/PrioritizeWorkAtTheTaskLevel.html)

App Nap can throttle CPU, I/O, and timers when the user is not interacting. Apple recommends proactively suspending discretionary work instead of relying on App Nap to make the app idle. For Sweetbriar, prefer push/IDLE plus reconnect reconciliation over short polling timers; use narrowly scoped activities for required user work and end them promptly. The notification-while-quit requirement remains a separate product decision. [Apple App Nap guide, archived 2016-09-13](https://developer.apple.com/library/archive/documentation/Performance/Conceptual/power_efficiency_guidelines_osx/AppNap.html)

### 2.5. Local storage at million-message scale

**Recommendation:** SQLite for authoritative metadata, drafts, settings, mutations, and synchronization state; a versioned blob layer for immutable original message bytes; a rebuildable search index. Keep the storage API independent of Swift and Linux UI toolkits.

Proposed logical schema:

| Table | Purpose and important keys |
|---|---|
| `message` | Compact integer local ID; account-scoped provider ID; dates, parsed headers, body availability, blob reference |
| `remote_locator` | IMAP account/mailbox/UIDVALIDITY/UID tuple mapped to local message; multiple locators allowed |
| `membership` | `(mailbox_id, message_id)` primary key; occurrence-specific flags/MODSEQ where backend semantics require them |
| `thread_edge`, `thread_summary` | References graph and incrementally maintained last date, count, participants, unread summary |
| `mailbox_message_summary`, `mailbox_thread_summary` | Materialized row fields and sort keys for each presentation mode |
| `blob` | Content hash, format version, codec, original length, location, integrity state |
| `draft_revision`, `settings`, `mutation_outbox` | Authoritative local user data and retryable remote intent |
| `sync_cursor`, `index_journal` | Backend state tokens and durable work needed to update a separate index |

Do not force generic IMAP per-folder flags into Gmail/JMAP global keyword semantics. Preserve backend distinctions beneath a coherent UI. IMAP UIDs are scoped by mailbox and UIDVALIDITY; a bare UID is not a global message identity. Gmail’s X-GM-MSGID identifies the same message across labels, whereas its X-GM-THRID identifies a conversation. [IMAP identity rules](https://www.rfc-editor.org/rfc/rfc9051.html), [Gmail identifiers](https://developers.google.com/workspace/gmail/imap/imap-extensions)

Use `WITHOUT ROWID` selectively for compact compound-key membership/summary tables. It can avoid a second B-tree for non-integer primary keys; ordinary `INTEGER PRIMARY KEY` tables already use the rowid efficiently. It is not universally faster, and incremental BLOB I/O is unavailable on WITHOUT ROWID tables. [SQLite WITHOUT ROWID](https://sqlite.org/withoutrowid.html)

Index the actual list access pattern, for example `(mailbox_id, received_at DESC, message_id DESC, sender, subject, flags)`. This is a covering index when the query needs only those columns, avoiding table lookups. Cap stored preview/subject data and measure write amplification rather than putting every display field in every index. Maintain summaries in the same metadata transaction; index maintenance is a cost paid on writes to make reads predictable. [SQLite query planning](https://sqlite.org/queryplanner.html)

For sequential navigation, use a cursor:

```sql
SELECT message_id, received_at, sender, subject, flags
FROM mailbox_message_summary
WHERE mailbox_id = ?
  AND (received_at, message_id) < (?, ?)
ORDER BY received_at DESC, message_id DESC
LIMIT 100;
```

SQLite’s documentation explains why OFFSET grows with skipped rows and shows row-value cursor pagination. A cursor alone does not solve arbitrary scrollbar jumps: propose a compact ordered-ID vector or generation-checked sparse anchors. For scale intuition, 500,000 eight-byte IDs occupy 4,000,000 bytes before container overhead; 500,000 rich Swift message objects are a different allocation problem. [SQLite scrolling windows](https://sqlite.org/rowvalue.html)

**Original experiment, 2026-10-11:** this research created 1,000,000 synthetic metadata rows and a covering date index. On a shared Linux x86-64 Xeon W-2145 host, Python 3.11.2/SQLite 3.40.1, 4 KiB pages, 32 MiB SQLite cache, mmap disabled, WAL/FULL, five warmups and 50 measured repetitions produced:

| Fetch 100 rows | Median | p95 |
|---|---:|---:|
| First page | 0.073 ms | 0.270 ms |
| Page after `OFFSET 900000` | 79.764 ms | 105.906 ms |
| Same distant page using date/ID cursor | 0.080 ms | 0.312 ms |

The database occupied 142,614,528 bytes, about 136 MiB. All three queries used the covering index. These are warm metadata-query results; OS caches were not flushed, helpers were running concurrently, and no macOS UI, full-text search, cold I/O, or write durability was measured. SQLite 3.40.1 was the available benchmark library, **not** the proposed shipping version. [Reproducible script](codex-artifacts/bench-sqlite.py), [Recorded results](codex-artifacts/bench-results.json)

**Concurrency and durability:** WAL allows concurrent readers and a writer, but only one writer at a time. SQLite normally auto-checkpoints at 1,000 pages; long reader transactions can prevent progress and grow the WAL. Propose one owned writer worker, two to four reader connections, short read snapshots, bounded writes, and separately scheduled checkpoints. Never hold a transaction while awaiting network or UI work. [SQLite WAL](https://sqlite.org/wal.html)

Keep `synchronous=FULL` for authoritative data. In WAL mode, NORMAL preserves consistency but can lose recently committed transactions after power loss; FULL adds a WAL sync per commit. On macOS, `fullfsync` controls use of `F_FULLFSYNC` and defaults off: explicitly evaluate enabling it for the authoritative store, including latency on target disks. An optimistic visual update is reversible; “saved” should mean durable commit under the declared storage guarantees. [SQLite synchronous and fullfsync](https://sqlite.org/pragma.html)

**2026 dependency requirement:** SQLite 3.51.3, released 2026-03-13, fixes the WAL-reset corruption bug. Pin a vetted release containing that fix, or an explicitly verified backport; inspect the actual linked SQLite version, not just the Rust wrapper version. This is a minimum fix requirement, not a claim that 3.51.3 is the latest release. [SQLite 3.51.3 release notes](https://sqlite.org/releaselog/3_51_3.html)

Start with 4 KiB pages; compare 8/16 KiB before settling on the file format. Page size cannot be changed while in WAL mode. Start with mmap disabled, then benchmark a bounded mapping such as 256 MiB. Memory mapping can save copies but introduces mapped-I/O error handling concerns; mapped address space is not identical to resident memory. Bound each connection’s cache and include all caches in the memory budget. [WAL page-size restriction](https://sqlite.org/wal.html), [SQLite mmap](https://sqlite.org/mmap.html), [SQLite page/cache settings](https://sqlite.org/pragma.html)

**Raw messages: files or SQLite BLOBs?** Preserve original RFC 5322/MIME bytes independently of parsed display text. Prefer immutable, hash-addressed files initially for large raw messages and attachments, behind an abstraction that permits small BLOBs later. The strongest evidence against “files are always faster” is SQLite’s published experiment: 100,000 approximately 10 KiB BLOBs were about 35% faster to read/write and used about 20% less disk than separate files. Those are historical 2017 workload results, not a modern APFS email benchmark; write settings and checkpoint accounting also limit their durability comparison. Measure Sweetbriar’s size distribution and export/backup workflows before choosing a cutoff. [SQLite small-BLOB experiment](https://sqlite.org/fasterthanfs.html), [Fastmail’s message-format references](https://www.fastmail.help/hc/en-us/articles/1500000278382-Email-standards)

Proposed file commit protocol: write a temporary immutable blob, flush it, publish it with the platform’s required rename/directory-persistence steps, then commit its database reference. Crashes before the database commit leave collectible orphan files rather than references to missing content. Garbage collection must respect live references, draft revisions, and backups. This is a design proposal requiring fault injection and macOS/Linux filesystem review; SQLite cannot make arbitrary external files part of its transaction. Use SQLite’s backup facilities plus a consistent blob manifest, rather than copying only a live database file. [SQLite atomic-commit constraints](https://sqlite.org/atomiccommit.html), [SQLite corruption hazards](https://www.sqlite.org/howtocorrupt.html), [rusqlite backup API](https://docs.rs/rusqlite/latest/rusqlite/)

**Compression:** benchmark zstd levels 1–3 for cold raw content, without changing the original bytes after decompression. The reference project’s current published Silesia benchmark reports zstd 1.5.7 level 1 at ratio 2.896, compression 510 MB/s, decompression 1,550 MB/s on an i7-9700K/Ubuntu 24.04. These are in-memory corpus results, not email or Apple-silicon predictions. JPEG/ZIP/encrypted content needs its own measurements; keep lengths, checksums, codec versions, and decompression bounds. Avoid a startup dependency on decompressing the entire archive. [Zstandard benchmark, checked 2026-10-11](https://github.com/facebook/zstd)

**Deduplication:** share immutable bytes separately from logical message identity. Multiple Gmail labels should add memberships, not copies of bodies. Identical raw bytes may share a blob while remaining separate messages; Message-ID alone is not a safe uniqueness constraint. Mailspring’s engineering README describes a header-hash identity scheme that can hide one of two identical-header messages—an explicit warning against that shortcut. As arithmetic, one million messages averaging 100 KiB require about 95.4 GiB raw; four redundant full copies require about 381.5 GiB before indexes. [Gmail message identity](https://developers.google.com/workspace/gmail/imap/imap-extensions), [Mailspring’s documented collision](https://github.com/Foundry376/Mailspring-Sync)

**Search:** SQLite FTS5 supplies BM25, phrase/prefix queries, snippets, and configurable tokenizers; external-content indexes require maintained synchronization. Tantivy supplies a Rust search engine with segmented indexes and explicit commits; indexing/merging creates another resource workload. Recommend an engine-neutral query AST and an FTS5 prototype first, compared with Tantivy on 1M/5M fixtures before deciding. A separate index needs a durable SQLite index journal, generation tracking, replay, and rebuild. Neither cached headers nor an index of only downloaded bodies constitutes complete body search. [FTS5](https://sqlite.org/fts5.html), [Tantivy architecture](https://docs.rs/tantivy/latest/tantivy/)

Concrete maintained candidates, as rendered by their documentation on **2026-10-11**, are `rusqlite` 0.40.2, `tantivy` 0.26.2, and `zstd` 0.14.2, with Tokio for scheduling. These identify research candidates, not a tested dependency combination. [rusqlite](https://docs.rs/rusqlite/latest/rusqlite/), [Tantivy](https://docs.rs/tantivy/latest/tantivy/), [zstd](https://docs.rs/zstd/latest/zstd/), [Tokio](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html)

### 2.6. Threading without rebuilding the archive

JWZ-style REFERENCES threading builds containers from Message-ID, References, and In-Reply-To, allowing placeholders for missing ancestors and guarding against cycles. RFC 5256 standardizes a REFERENCES algorithm based on this work, including duplicate/missing-ID handling and subject-related rules. JWZ’s original page could not be fetched through HTTPS, HTTP, or the attempted archive fallback; the accessible RFC is the verified basis here. [IMAP threading, RFC 5256, 2008-06](https://www.rfc-editor.org/rfc/rfc5256.html)

Proposed implementation: persist header relationships and an index from normalized Message-ID to candidate containers. On arrival, update the affected component, fill placeholders, and refresh affected mailbox/thread summaries. Treat subject-only grouping as a weaker heuristic and retain separate records for duplicate IDs. Removing a visible message need not erase graph structure needed by its descendants. Reconcile late ancestors and imports in background batches.

“Incremental” does not mean every change is constant-time: one old message can join large components, and changed grouping can affect many summaries. Bound foreground work, make rebuilds cancellable, and show a stable snapshot while reconciliation runs. Keep threading derivable from preserved headers so algorithm improvements do not threaten original mail.

| Backend | Provider conversation key | Sweetbriar implication |
|---|---|---|
| Gmail IMAP / API | X-GM-THRID / provider thread ID | Preserve Gmail grouping within that account; do not treat it as message identity |
| JMAP | Email `threadId`, Thread objects | Use provider-defined threads and mailbox memberships |
| Graph | Message `conversationId` | Preserve it as an account-scoped provider key |
| Generic IMAP / imported mail | Local REFERENCES graph; server THREAD if offered | Local algorithm provides consistent offline behavior |

Sources: [Gmail thread IDs](https://developers.google.com/workspace/gmail/imap/imap-extensions), [JMAP Mail](https://www.rfc-editor.org/rfc/rfc8621.html), [Graph message resource](https://learn.microsoft.com/en-us/graph/api/resources/message?view=graph-rest-1.0), [RFC 5256](https://www.rfc-editor.org/rfc/rfc5256.html).

The Gmail API exposes `threadId` and `labelIds` directly on Message objects; its immutable message `id` remains a separate field. [Gmail Message resource, updated 2026-05-06](https://developers.google.com/workspace/gmail/api/reference/rest/v1/users.messages)

Store both provider grouping and local reference relationships. Unified or cross-account threading should be a separate, reversible view policy. A provider conversation ID does not eliminate the need for local ordered members, unread counts, previews, and partial-coverage handling; this is an architectural inference.

### 2.7. Native macOS 26 UI and message rendering

**AppKit versus SwiftUI:** NSTableView explicitly reuses views through `makeView(withIdentifier:owner:)`. SwiftUI is also capable of large lists: at WWDC25 Apple reported 6× faster loading and up to 16× faster updates for a 100,000-item macOS list, plus idle-time row prefetching. The talk supplies relative measurements, not absolute latency, complete hardware details, or a direct NSTableView comparison. **A controlled macOS 26 email benchmark comparing the toolkits is unverified.** [NSTableView reuse API](https://developer.apple.com/documentation/appkit/nstableview/makeview%28withidentifier%3Aowner%3A%29), [SwiftUI improvements, WWDC25](https://developer.apple.com/videos/play/wwdc2025/256/)

Apple’s 2023 SwiftUI performance talk explains eager gathering of List/Table identities, even when rows are created lazily. Conditional row counts, type erasure, filtering in view builders, and expensive body evaluation can undermine performance. These are documented historical mechanisms to test on macOS 26, not proof that every earlier problem persists unchanged. LazyVStack’s lazy view creation alone does not promise cheap whole-data sorting, identity generation, or updates. [Demystify SwiftUI performance, WWDC23](https://developer.apple.com/videos/play/wwdc2023/10160/)

Recommendation: start the performance-critical mailbox pane with NSTableView and stable IDs; use SwiftUI where it improves development of surrounding native UI. Build a small SwiftUI List/Table comparison using the same paged Rust summaries before making this permanent. Strongest counterargument: macOS 26’s SwiftUI improvements may meet every target with less integration code. Choose from measured navigation, selection, accessibility, update storms, and 500k-row scrolling—not framework reputation.

Across either toolkit, pass compact immutable row DTOs, not MIME trees or all messages, over the Rust/Swift boundary. Subscribe to bounded changes and apply insert/remove/update diffs on the main thread; coalesce background updates without postponing user input. Preserve selection and scrolling by stable identity. Cache formatters and measured row presentation; prefetch the next summaries and bodies off the UI thread. UniFFI is a concrete Rust-to-Swift binding candidate, but cancellation, ownership, callback execution, and transfer-size limits need an explicit contract. [UniFFI documentation, checked 2026-10-11](https://mozilla.github.io/uniffi-rs/latest/)

An `async` function is not proof that CPU work leaves MainActor. Apple’s WWDC25 concurrency talk illustrates image work accidentally staying on the UI actor. Put SQL, MIME, indexing, sanitization, and decode on appropriate bounded workers; deliver only ready presentation data to UI code. [Embracing Swift concurrency, WWDC25](https://developer.apple.com/videos/play/wwdc2025/268/)

**WKWebView:** WebKit separates UI, web-content, and networking responsibilities into processes, but HTML parsing, style/layout, image decoding, IPC, and process startup still consume time and memory. No reliable universal “WKWebView startup costs X ms” figure was verified. Measure first-use versus reused views, simple versus pathological HTML, and memory including child processes. Current Apple documentation marks `WKWebViewConfiguration.processPool` deprecated and says using multiple WKProcessPool instances no longer has an effect; old pool-control recipes should not drive a new architecture. [WebKit architecture](https://docs.webkit.org/Deep%20Dive/Architecture/WebKit2.html), [WebKit rendering concepts](https://docs.webkit.org/Getting%20Started/Introduction.html), [Process-pool deprecation, checked 2026-10-11](https://developer.apple.com/documentation/webkit/wkwebviewconfiguration/processpool)

Proposed policy: one active reader WebView and, only if measurements justify it, one idle prewarmed spare. Reuse actual views, reset per-message state, and dispose of excess views under memory pressure. Do not allocate 50 WebViews for a 50-message thread. Prewarming after useful first paint trades later latency for memory/energy; WebKit’s own bug discussions show that prewarm timing is workload-sensitive. Private prewarm SPI is not a supported application API. [WebKit prewarming discussion, 2018-09-06](https://bugs.webkit.org/show_bug.cgi?id=189364), [Prewarming trade-off, 2023-08-08](https://bugs.webkit.org/show_bug.cgi?id=259927)

Provide a native plain-text reader fast path using NSTextView, preserving original content and quote navigation. For HTML, prepare sanitized content off the main thread and progressively render visible thread members. Keep remote-image fetching and attachment Quick Look separate from initial readability. Generate appropriately sized thumbnails with ImageIO on bounded workers, and keep oversized/full-resolution decode out of scrolling. These are proposals; exact memory savings require representative MIME and image fixtures. [ImageIO thumbnail API](https://developer.apple.com/documentation/imageio/cgimagesourcecreatethumbnailatindex%28_%3A_%3A_%3A%29)

Launch should open the existing store and produce the saved mailbox snapshot before starting account discovery, full reconciliation, search-index verification, or WebView prewarming. Recover required journal state without scanning every blob. Proposed ordinary-use memory target: approximately 250 MiB for the application/core plus 250 MiB for WebKit helpers, with large-import and multi-window budgets reported separately. These are initial engineering limits, not measured product facts.

### 2.8. What fast clients actually document

Evidence quality matters: source code and maintainer explanations establish mechanisms; product descriptions establish intended behavior; neither provides a controlled current cross-client ranking.

| Client | Verified evidence | Lesson and limit |
|---|---|---|
| Superhuman | Its 2022 speed article describes local storage/offline operation and preloading/prerendering likely next threads | Anticipation and local interaction are deliberate architecture; published speed claims are not a native macOS benchmark. [Engineering explanation](https://blog.superhuman.com/superhuman-is-built-for-speed/) |
| Mimestream | January 2026 release blog describes adaptive caching; its technical push document describes Pub/Sub routing while Gmail access tokens remain on-device, with a direct encrypted connection on macOS | Provider-specific sync and privacy-aware push can coexist. No absolute search/open latency is published in these sources. [1.9 engineering/product update](https://mimestream.com/blog/1.9-released), [Private Push architecture](https://mimestream.com/trust/private-push) |
| Apple Mail | macOS 26 guide offers All/Recent/None for certain attachment types; “Recent” means 15 months, while media attachments are always downloaded | Separate attachment policy from basic readability. Proprietary database, renderer-pooling, and sync scheduling internals are **(unverified)** here. [macOS 26 attachment policy](https://support.apple.com/guide/mail/cpmlprefacctinfo/16.0/mac/26) |
| MailMate | Its manual documents `.eml` messages and Spotlight indexing, plus default blocking of external HTML references | Durable originals and controlled external loading are useful patterns; no current controlled million-message latency measurement was verified. [MailMate manual](https://manual.mailmate-app.com/preferences) |
| Spark | Official support says its offline cache covers the last month, including attachments when caching over Wi-Fi, and distinguishes server-held mail from local cache | A responsiveness cache is not a complete offline archive. Detailed scheduler/database performance claims are **(unverified)**. [Spark cache policy, checked 2026-10-11](https://sparkmailapp.com/help/troubleshooting/how-to-clear-cache-in-spark) |
| Mailspring | Open sync engine: native C++/Mailcore2, SQLite, foreground/background workers, durable tasks, JSON change feed. Gmail sync focuses on All Mail/Spam/Trash and labels, avoiding redundant virtual-folder work | Reuse its separation of UI and sync, retryable tasks, and provider-aware deduplication lessons. Its README also questions large JSON rows for scans. These are implementation descriptions, not Sweetbriar performance estimates. [Sync-engine design](https://github.com/Foundry376/Mailspring-Sync) |
| mutt / notmuch | mutt documents header/body caches and IMAP extension settings; notmuch documents Xapian indexing. A maintainer’s 2010 thread-query optimization reduced one test from 4.523 s to 1.779 s by reducing queries/object work | Caches, indexed search, and avoiding N+1 work matter even without graphical rendering. That historical result is not a modern sub-100 ms guarantee. [mutt manual, HTTP fallback](http://www.mutt.org/doc/manual/), [notmuch](https://notmuchmail.org/), [Maintainer measurement, 2010-11-16](https://nmbug.notmuchmail.org/nmweb/show/20101116161842.GA2439%40mit.edu) |

Concrete regressions are more instructive than reputation. Mimestream’s 2026-05-27 release notes fix high CPU during large-cache refresh and Select All; its 2026-03-20 notes fix typing lag in attachment-heavy long threads and erroneous eviction of old threads with new messages. Its 2021-04-15 notes explicitly added table-row reuse to avoid memory bloat. These are dated, fixed issues—not allegations about current releases. They support testing bulk selection, background cache churn, and composing inside large conversations. [Mimestream release history](https://mimestream.com/releases)

### 2.9. Measurement and representative corpora

Instrument user-visible spans with `OSSignposter`/`os_signpost`, correlated with Rust `tracing` IDs: input received, queue wait, SQL, blob read, decode, FFI transfer, UI diff, and readable content. Apple’s current guidance uses `OSSignposter` to display concurrent tasks in Instruments. Do not equate an SQL completion or `WKNavigationDelegate.didFinish` callback with pixels being visible. [Apple recording performance data](https://developer.apple.com/documentation/os/recording-performance-data)

Use Instruments Time Profiler, Hangs, Allocations, storage/network profiling, and the SwiftUI instrument where appropriate. WWDC25 introduced an instrument showing SwiftUI update causes and effects. Run UI performance tests on physical Macs using XCTest metrics, including `XCTApplicationLaunchMetric` and custom `XCTOSSignpostMetric` intervals. The older `XCTOSSignpostMetric.applicationLaunch` is deprecated as checked 2026-10-11. [SwiftUI instrument, WWDC25](https://developer.apple.com/videos/play/wwdc2025/306/), [Apple launch-metric deprecation](https://developer.apple.com/documentation/xctest/xctossignpostmetric/applicationlaunch)

**Proposed CI plan:** portable Rust benchmarks on every change; fixed-machine macOS interaction runs nightly and before release. Keep exact hardware, OS, compiler, SQLite build, corpus seed, power state, and cache state with results. Gate absolute p95 targets and statistically credible regressions; do not estimate a meaningful p99 from ten samples. Exercise launch, mailbox navigation, search, compose typing, scrolling, and thread expansion during initial sync and indexing.

The CMU Enron distribution contains about 0.5 million messages from about 150 users. Its 2015-05-07 archive is approximately 1.7 GB compressed; attachments are omitted, addresses were repaired, and some messages were redacted. It therefore provides realistic historical text and folders but weak coverage of modern HTML and attachment costs. [CMU Enron dataset](https://www.cs.cmu.edu/~enron/)

Generate deterministic 1M- and 5M-message fixtures in addition to Enron. Include one 500k-message mailbox, many small folders, 1/5/20 memberships per message, 50/1,000-message threads, out-of-order replies, malformed MIME, Unicode, calendar invites, HTML newsletters, inline images, large attachments, and encrypted messages. When scaling Enron, deliberately rewrite Message-ID and References together for independent conversations; preserve duplicate bytes in separate deduplication tests. Blind duplication would make identity and search tests misleading.

Add protocol replay and failure scenarios: 20/100/300 ms simulated RTT, limited bandwidth, disconnects, expired delta cursors, provider throttling, disk-full, crashes at each durability boundary, and restore from backup. Record bytes transferred, requests, cache hits, CPU time, energy, writer queue depth, WAL size, index lag, and total memory including WebKit helper processes. These are proposed Sweetbriar tests.

### 2.10. Highest-impact decisions and day-one commitments

This ranking is an engineering recommendation, ordered by expected effect on perceived speed rather than implementation effort.

| Priority | Decision | Why it affects speed | Commit on day one? |
|---|---|---|---|
| 1 | Local snapshots and durable local mutations | Removes network waits from ordinary navigation and editing | Yes: ownership and outbox semantics permeate the app |
| 2 | Bounded work, cancellation, and interactive resource reservation | Keeps imports/indexing from stealing responsiveness | Yes: scheduler and FFI contracts; tune numeric limits later |
| 3 | Stable identity, memberships, and paged summaries | Avoids label duplication, whole-mailbox transfers, and repeated aggregate work | Yes: data model is expensive to retrofit |
| 4 | Virtualized native rows and progressive message rendering | Keeps list and thread cost proportional to visible content | Yes: presentation contract; toolkit can remain replaceable |
| 5 | Maintained local search with explicit coverage | Prevents scans and misleading incomplete results | Yes: query semantics and index journal; engine can be benchmarked later |
| 6 | Delta sync and connection/request reuse | Reduces catch-up, bandwidth, and background interference | Yes: backend state machines; extension coverage can grow |
| 7 | Measured prefetch and cache policy | Converts likely future reads into cache hits | Contract now; heuristic counts and sizes later |
| 8 | Deferred launch work and continuous instrumentation | Prevents startup regressions and makes tail latency visible | Yes: useful-first-launch path and trace IDs |

Other hard-to-change commitments are original-byte preservation, versioned migrations, backup/restore, crash-safe draft/attachment ownership, and whether downloaded mail is an archive or an evictable cache. Cache size, zstd level, connection ceilings, mmap size, and prewarm count are intentionally tunable. Select All and bulk actions should operate on a query/selection representation rather than serializing every message into the UI.

## 3. Options compared

These are engineering assessments based on the evidence above; they are not measured rankings of Sweetbriar implementations.

| Choice | Advantage | Main cost / strongest alternative argument | Proposed choice |
|---|---|---|---|
| Headers/on-demand vs full offline vs progressive offline | On-demand starts cheaply; full offline supports complete body search; progressive coverage combines quick start with eventual completeness | Progressive coverage complicates search completeness and eviction; full offline creates large imports | Progressive download, user-visible coverage, pinned full-offline option |
| Generic IMAP everywhere vs provider APIs | One protocol reduces backend implementation work; native APIs expose stable identities and deltas | Several APIs increase testing and infrastructure; quotas can dominate import speed | IMAP fallback; Gmail API, Graph, and JMAP as first-class backends |
| SQLite raw BLOBs vs external immutable blobs | Database BLOBs simplify atomic ownership; files support independent large-object streaming/export | Files require their own commit/GC/backup protocol; small BLOBs can outperform files | SQLite metadata/user state; blob abstraction with initially external large originals |
| FTS5 vs Tantivy | FTS5 reduces cross-store coordination; Tantivy gives a Rust search subsystem with configurable indexing | Separate index introduces lag/replay/merge work; FTS5 may require more custom ranking/tokenization | Prototype FTS5; benchmark both before committing the engine |
| NSTableView vs SwiftUI List/Table | AppKit offers direct row reuse/control; SwiftUI offers declarative native composition | AppKit bridges add code; modern SwiftUI may meet the budgets without them | AppKit baseline plus a measured macOS 26 comparison |
| Eager HTML/prewarm vs lazy reader | Warm renderers can improve subsequent reading; lazy rendering reduces idle footprint | Prewarming spends memory/energy before benefit is certain | Plain-text fast path; bounded WebView reuse; prewarm only after measurement |

## 4. Recommendations for Sweetbriar, trade-offs, and counter-argument

Build a **local, durable Rust core with native presentation adapters**. The core should expose paged mailbox/thread summaries, cancellable searches, body availability, and durable command results. Swift owns macOS presentation; it should not need to understand IMAP sequence numbers, SQL connections, or index commits. This boundary also makes a later native Linux UI practical without transferring macOS scheduling policy into the shared core.

Make a durable command path the foundation: update local state and record intent transactionally, then send the remote operation and reconcile acknowledgement. Keep revisioned draft content and locally owned attachment bytes. Preserve pending intent through crashes and cursor invalidation. Undo should be a modeled operation, not merely an animation. The trade-off is more state-machine work than a UI that waits for the server.

Make completeness explicit: distinguish metadata synchronized, body downloaded, and text indexed. Let users pin accounts/mailboxes for offline retention, and prevent eviction of unique local data, drafts, queued outgoing content, and pinned archives. A “cache” that silently evicts the only copy would violate the project goal. Attachment extraction and semantic/AI features should not sit on the initial read/search path.

Implement provider-aware backends without forcing identical semantics: Gmail labels and history, Graph folder deltas and immutable IDs, JMAP state/threads, IMAP UIDVALIDITY/MODSEQ. The trade-off is a larger compatibility matrix. The alternative—pretending every source is an IMAP folder tree—pushes complexity into identity, duplicates, and user-visible behavior later.

Approve storage and toolkit choices using a concrete foundation prototype: one million local messages, one 500k mailbox, one real API backend plus one IMAP backend, interactive use during sync/indexing, and crash recovery around draft/outbox/blob commits. Run the latency table on a physical macOS 26 reference machine. Keep search and blob implementations behind replaceable interfaces until these results exist.

**Strongest counter-argument:** this architecture purchases responsiveness with substantial engineering complexity, disk usage, background I/O, and recovery obligations. A smaller recent-mail cache with server search and mostly remote mutations could ship sooner and consume fewer resources for small accounts. SQLite-only raw storage and an all-SwiftUI UI would further simplify the first release. Sweetbriar’s long-term goals—large archives, cross-provider support, reliable offline work, and no lost drafts—justify the added core design, but do not justify implementing every extension or tuning every knob before a measured end-to-end prototype.

## 5. Risks and open questions

- **Capability uncertainty:** the hosted matrix contains deliberately unverified cells. Repeat authenticated capability/behavior tests with representative accounts, regions, and tenant policies before relying on an extension. Validate UIDVALIDITY resets, expunges, move mappings, and malformed server responses.
- **API economics and imports:** the current Gmail quota page specifies an 80-million-unit daily project threshold and planned future billing changes; exact future charges are **(unverified)**. Recheck commercial terms before launch, and model multiple simultaneous imports plus reserved foreground quota. [Gmail quotas, checked 2026-10-11](https://developers.google.com/workspace/gmail/api/reference/quota), [Google’s tiering plan, updated 2026-09-03](https://developers.google.com/workspace/tools-safety)
- **No absolute data-loss guarantee from performance settings:** define the recoverable failure model. FULL synchronization still depends on filesystem/device behavior; corruption, disk failure, and user deletion require backups and restore testing. Preservation of downloaded mail cannot recover content never downloaded before server loss. [SQLite atomic-commit assumptions](https://sqlite.org/atomiccommit.html)
- **Send ambiguity:** a disconnect after submission can leave acceptance uncertain. Design provider-specific reconciliation and visible pending/uncertain state; do not assume blind retries provide exactly-once sending. This is an open correctness requirement, separate from archive/delete optimism.
- **Notifications and sleep:** decide whether mail must arrive while the app is quit, asleep, or offline. Pub/Sub/Graph delivery and subscription renewal may require an optional relay. Reconnection storms and token expiry need tests. Mimestream demonstrates a relay that forwards change signals while Gmail tokens stay on-device; that is a design reference, not proof Sweetbriar’s relay would have identical properties. [Mimestream architecture](https://mimestream.com/trust/private-push)
- **Unverified macOS numbers:** no Sweetbriar UI, APFS store, cold WebView, or current cross-client benchmark was run. The original Linux experiment proves a specific pagination effect only. Validate all proposed budgets, memory limits, and toolkit choices on physical Macs.
- **Search completeness and privacy:** decide attachment text, languages/tokenization, quoted-text weighting, encrypted-mail indexing, and whether remote search supplements local search. Decrypted indexes and cached previews need explicit retention/access policies; they are separate from raw encrypted messages.
- **Migration and backups:** version schema, blobs, and index generations independently; test older-store upgrades with disk-full and interrupted migration. A separate blob store makes consistent backup/restore a first-class subsystem.
- **Adversarial content and representativeness:** bound MIME nesting, HTML complexity, image dimensions, decompressed sizes, and pathological threads. Enron cannot substitute for contemporary attachment/HTML fixtures. [Enron corpus limitations](https://www.cs.cmu.edu/~enron/)

## 6. Sources actually consulted

All links below were consulted on **2026-10-11**; historical publication/update dates are given in the findings where relevant. The inventory lists sources used by the report. Dovecot/Cyrus pages were also fetched directly when the web reader failed; mutt was retrieved over HTTP after HTTPS certificate failure. Failed JWZ/archive fetches are disclosed in §2.6 and were not used as verified evidence.

- **Latency and energy:** [Apple responsiveness guidance](https://developer.apple.com/documentation/xcode/improving-app-responsiveness), [RAIL](https://web.dev/articles/rail), [Understanding UI responsiveness](https://developer.apple.com/documentation/xcode/understanding-user-interface-responsiveness/), [HIG loading](https://developer.apple.com/design/human-interface-guidelines/loading), [Apple QoS guide](https://developer.apple.com/library/archive/documentation/Performance/Conceptual/power_efficiency_guidelines_osx/PrioritizeWorkAtTheTaskLevel.html), [Apple App Nap guide](https://developer.apple.com/library/archive/documentation/Performance/Conceptual/power_efficiency_guidelines_osx/AppNap.html).

- **Protocol specifications:** [RFC 7162](https://www.rfc-editor.org/rfc/rfc7162.html), [RFC 9051](https://www.rfc-editor.org/rfc/rfc9051.html), [RFC 2177](https://www.rfc-editor.org/rfc/rfc2177.html), [RFC 5465](https://www.rfc-editor.org/rfc/rfc5465.html), [RFC 4978](https://www.rfc-editor.org/rfc/rfc4978.html), [RFC 6851](https://www.rfc-editor.org/rfc/rfc6851.html), [RFC 4315](https://www.rfc-editor.org/rfc/rfc4315.html), [RFC 4731](https://www.rfc-editor.org/rfc/rfc4731.html), [RFC 5819](https://www.rfc-editor.org/rfc/rfc5819.html), [RFC 6154](https://www.rfc-editor.org/rfc/rfc6154.html), [RFC 8474](https://www.rfc-editor.org/rfc/rfc8474.html), [RFC 3516](https://www.rfc-editor.org/rfc/rfc3516.html), [RFC 8970](https://www.rfc-editor.org/rfc/rfc8970.html), [RFC 9394](https://www.rfc-editor.org/rfc/rfc9394.html), [RFC 7888](https://www.rfc-editor.org/rfc/rfc7888.html), [RFC 8620](https://www.rfc-editor.org/rfc/rfc8620.html), [RFC 8621](https://www.rfc-editor.org/rfc/rfc8621.html), [RFC 5256](https://www.rfc-editor.org/rfc/rfc5256.html).

- **Servers and provider APIs:** [Gmail sync](https://developers.google.com/workspace/gmail/api/guides/sync), [Graph delta](https://learn.microsoft.com/en-us/graph/delta-query-messages), [Gmail IMAP extensions](https://developers.google.com/workspace/gmail/imap/imap-extensions), [Dovecot RFC inventory](https://doc.dovecot.org/latest/core/summaries/rfc.html), [Dovecot configuration](https://doc.dovecot.org/latest/core/config/imap.html), [Dovecot rev2 setting](https://raw.githubusercontent.com/dovecot/core/main/src/imap-login/imap-login-settings.c), [Cyrus RFC support](https://www.cyrusimap.org/stable/imap/rfc-support.html), [Stalwart RFC inventory](https://stalw.art/docs/development/rfcs/), [iCloud settings](https://support.apple.com/en-ie/102525), [Exchange Online IMAP settings](https://learn.microsoft.com/en-us/exchange/clients-and-mobile-in-exchange-online/pop3-and-imap4/pop3-and-imap4), [Fastmail developer access](https://www.fastmail.com/dev/), [Stalwart JMAP push settings](https://stalw.art/docs/ref/object/jmap/), [Push guide](https://developers.google.com/workspace/gmail/api/guides/push), [Gmail quota reference](https://developers.google.com/workspace/gmail/api/reference/quota), [Immutable IDs](https://learn.microsoft.com/en-us/graph/outlook-immutable-id), [Outlook throttling](https://learn.microsoft.com/en-us/graph/throttling-limits), [Notification latency and lifetime](https://learn.microsoft.com/en-us/graph/change-notifications-overview), [Delta-token validity](https://learn.microsoft.com/en-us/graph/delta-query-overview), [Fastmail’s message-format references](https://www.fastmail.help/hc/en-us/articles/1500000278382-Email-standards), [Graph message resource](https://learn.microsoft.com/en-us/graph/api/resources/message?view=graph-rest-1.0), [Gmail Message resource](https://developers.google.com/workspace/gmail/api/reference/rest/v1/users.messages), [Google’s tiering plan](https://developers.google.com/workspace/tools-safety).

- **Storage, search, and Rust libraries:** [SQLite WAL](https://sqlite.org/wal.html), [Tokio spawn_blocking](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html), [rusqlite interrupt support](https://docs.rs/rusqlite/latest/rusqlite/), [SQLite WITHOUT ROWID](https://sqlite.org/withoutrowid.html), [SQLite query planning](https://sqlite.org/queryplanner.html), [SQLite scrolling windows](https://sqlite.org/rowvalue.html), [SQLite synchronous and fullfsync](https://sqlite.org/pragma.html), [SQLite 3.51.3 release notes](https://sqlite.org/releaselog/3_51_3.html), [SQLite mmap](https://sqlite.org/mmap.html), [SQLite small-BLOB experiment](https://sqlite.org/fasterthanfs.html), [SQLite atomic-commit constraints](https://sqlite.org/atomiccommit.html), [SQLite corruption hazards](https://www.sqlite.org/howtocorrupt.html), [Zstandard benchmark](https://github.com/facebook/zstd), [FTS5](https://sqlite.org/fts5.html), [Tantivy architecture](https://docs.rs/tantivy/latest/tantivy/), [zstd](https://docs.rs/zstd/latest/zstd/), [UniFFI documentation](https://mozilla.github.io/uniffi-rs/latest/).

- **Native UI, rendering, and measurement:** [What’s new in SwiftUI](https://developer.apple.com/videos/play/wwdc2025/256/), [NSTableView reuse API](https://developer.apple.com/documentation/appkit/nstableview/makeview%28withidentifier%3Aowner%3A%29), [Demystify SwiftUI performance, WWDC23](https://developer.apple.com/videos/play/wwdc2023/10160/), [Embracing Swift concurrency, WWDC25](https://developer.apple.com/videos/play/wwdc2025/268/), [WebKit architecture](https://docs.webkit.org/Deep%20Dive/Architecture/WebKit2.html), [WebKit rendering concepts](https://docs.webkit.org/Getting%20Started/Introduction.html), [Process-pool deprecation](https://developer.apple.com/documentation/webkit/wkwebviewconfiguration/processpool), [WebKit prewarming discussion](https://bugs.webkit.org/show_bug.cgi?id=189364), [Prewarming trade-off](https://bugs.webkit.org/show_bug.cgi?id=259927), [ImageIO thumbnail API](https://developer.apple.com/documentation/imageio/cgimagesourcecreatethumbnailatindex%28_%3A_%3A_%3A%29), [macOS 26 attachment policy](https://support.apple.com/guide/mail/cpmlprefacctinfo/16.0/mac/26), [Apple recording performance data](https://developer.apple.com/documentation/os/recording-performance-data), [SwiftUI instrument, WWDC25](https://developer.apple.com/videos/play/wwdc2025/306/), [Apple launch-metric deprecation](https://developer.apple.com/documentation/xctest/xctossignpostmetric/applicationlaunch).

- **Client engineering and documentation:** [Superhuman](https://blog.superhuman.com/superhuman-is-built-for-speed/), [Mailspring’s documented collision](https://github.com/Foundry376/Mailspring-Sync), [1.9 engineering/product update](https://mimestream.com/blog/1.9-released), [Private Push architecture](https://mimestream.com/trust/private-push), [MailMate manual](https://manual.mailmate-app.com/preferences), [Spark cache policy](https://sparkmailapp.com/help/troubleshooting/how-to-clear-cache-in-spark), [mutt manual, HTTP fallback](http://www.mutt.org/doc/manual/), [notmuch](https://notmuchmail.org/), [Maintainer measurement](https://nmbug.notmuchmail.org/nmweb/show/20101116161842.GA2439%40mit.edu), [Mimestream release history](https://mimestream.com/releases).

- **Test data:** [CMU Enron dataset](https://www.cs.cmu.edu/~enron/).

Original research artifacts: [IMAP probe script](codex-artifacts/probe-imap.py), [IMAP capability transcripts](codex-artifacts/protocol-probes.json), [SQLite experiment script](codex-artifacts/bench-sqlite.py), [SQLite results](codex-artifacts/bench-results.json).