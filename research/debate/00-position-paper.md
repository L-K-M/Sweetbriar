# Position paper: foundational recommendations (draft for adversarial review)

Author: Claude (Opus 5.5), 2026-10-11. Basis: the curated documents
`research/0*.md`, the working notes and the raw reports in `research/raw/`.
This paper states one recommendation per decision on the
[decision agenda](../notes/decision-agenda.md), the reasoning, and the
strongest argument against it. GPT-6.1-Sol is asked to attack it.

Product premise (from the user): native and fast on macOS first, later
native on Linux; a shared Rust core; never lose data; best-in-class search;
non-standard sources (Exchange/M365, Proton, Gmail API, JMAP); revive what
made classic clients (Eudora, Mailsmith, Claris Emailer, Musashi) beloved.
A long-term project.

---

## D1. Source of truth on disk

**Recommendation.** Three kinds of data, with explicit authority:

1. **Original message bytes:** immutable, content-addressed blobs (files
   behind a blob abstraction; small-blob cutoff open). Written temp →
   fsync → rename → directory fsync before any DB reference. These are the
   rebuild source for everything derived from mail.
2. **User intent and user data:** SQLite (bundled ≥ 3.51.3, WAL,
   `synchronous=FULL`, `fullfsync` on macOS, one writer). This holds the
   operation journal/outbox, drafts with revisions, settings, identities,
   rules, local labels, statuses, notes, display overlays and sync
   cursors. Authoritative, so not rebuildable from blobs. Protect it with
   the online backup API on a schedule plus an append-only metadata
   journal, so a reconstruct-from-blobs keeps labels and notes. (Mailsmith
   2.3's Reconstruct lost them.)
3. **Derived data:** summaries, thread graph, FTS index, embeddings. Fully
   rebuildable, versioned, excluded from Time Machine.

Plus mbox/EML/Maildir export from day one, an import reconciliation report,
and a refusal to keep the store in iCloud, Dropbox or network mounts.

*Why:* every documented data-loss story (Catalina Mail, Thunderbird
compaction, PST ceilings, Emailer 2.0, Eudora .toc, Mailsmith Reconstruct)
is either a single mutable file rewritten in place or metadata that existed
in only one place.

*Strongest counter:* a single SQLite database with BLOBs gives one
transaction and one backup unit. Files add a commit protocol and GC, and
SQLite measured small BLOBs 35% faster than files. Message bytes are also
replicated on the server for most accounts.

## D2. Internal data model

**Recommendation.** JMAP-shaped semantics without speaking JMAP internally:

- `Email` with a stable local id and a `mailboxIds` set (folders = one,
  Gmail labels = many, plus Graph categories and Proton labels).
- `keywords`; provider `threadId` hints plus a local References graph.
- `Identity`, `EmailSubmission`-like outbox records.
- Per-mailbox `remote_locator` rows (IMAP mailbox + UIDVALIDITY + UID), so
  IMAP's per-folder semantics are never flattened.
- Opaque per-backend sync cursors: MODSEQ, Gmail historyId, Graph
  deltaLink, JMAP state.
- A `providerData` sidecar for escape hatches.

Local-only metadata (Eudora-style statuses, colours, notes, display
subjects) is explicitly marked as synced where a capability exists
(keywords, RFC 9979 flags, categories) and local otherwise.

*Strongest counter:* a JMAP shape fits IMAP-folder users poorly
(per-folder flags, same message in two folders with different flags) and
fits Exchange quirks poorly; the abstraction leaks either way.

## D3. Sync engine

**Recommendation.** A deterministic state machine per account:

- **Intent journal:** every mutation is a row written in the same
  transaction as its local effect. Replay is a convergence check ("ensure
  X"), not a blind re-issue. A poisoned op parks after N attempts without
  blocking independent ops.
- **Deletes:** never a bare EXPUNGE (the IMAP layer lacks the verb). Use
  UID MOVE or verified COPY → UID STORE → UID EXPUNGE. Default delete is
  move to Trash. Tombstones survive until the server confirms and one
  clean sync has passed.
- **Circuit breaker** on mass disappearance (e.g. > 50 messages or > 20%
  of a folder, or a UIDVALIDITY change that would delete in bulk): pause
  and ask.
- **UIDVALIDITY change:** freeze the folder's queue, resync, rematch by
  hash and Message-ID.
- **Sending:** at-most-once *visible*. States Held (undo) → Queued →
  Submitting → Accepted | Failed | OutcomeUnknown. The same Message-ID on
  retry; check Sent before resending after an ambiguous drop.
- **Testing from day one:** deterministic simulation (seeded fake IMAP,
  JMAP and Graph servers, injected faults), proptest state-machine tests
  against an in-memory IMAP model, and a real-server CI matrix (Dovecot,
  Cyrus, Stalwart, Gmail, M365, iCloud, Fastmail).

*Strongest counter:* this costs perhaps 2–3× a naive engine, and the
simulation harness is months of work a small project may never recoup.
Mailspring shipped for years with a simpler drop-on-crash queue.

## D4. Core/UI boundary

**Recommendation.** Commands (with idempotency keys and durable receipts)
plus windowed observable query projections. Pull-based `next_batch`
subscriptions with a coalesced wake-up and `ResetRequired` on overflow.
Bodies through leases (chunks or files). A typed error taxonomy. UniFFI
0.32 behind a hand-written Swift adapter for cancellation and main-actor
batching; `sb-native-client` for GTK. Strict adjacent layering L0–L5 (see
[01-architecture](../01-architecture.md)).

*Strongest counter:* a Crux/1Password-style shared view-model core would
share more UI behavior with Linux and make the core testable as "the app".
UniFFI's per-call costs and the hand adapter add work.

## D5. Where view models live

**Recommendation.** Native view models (Swift `@Observable`, GTK models)
over shared Rust query projections. Shared in Rust: sorting, threading,
search grammar, rules, drafts, undo, list windows. Native: focus,
selection gestures, window layout, editor and IME state, menus.

*Strongest counter:* duplicating view logic twice is how front ends drift.
1Password's view-models-in-core shipped a sidebar change with zero
front-end edits.

## D6. Process model

**Recommendation.** In-process engine in v1. The app stays running with no
windows, like Apple Mail, so notifications keep working. The transport seam
in L1 allows moving the sole writer into an `SMAppService` per-user agent
later, guarded by an exclusive ownership lease. Extensions never write.

*Strongest counter:* background sync after quit is what desktop mail users
expect from Outlook and Proton Bridge. Retrofitting a daemon onto an
in-process design reopens every ownership assumption. Build it day one.

## D7. Search

**Recommendation.** One query AST shared by search, saved views (smart
mailboxes) and rules, with three front ends: token chips, a Gmail-superset
text syntax with a notmuch-style boolean core and regex as an expert mode,
and natural language turned into the AST on device, always shown as
editable chips.

- **Engine:** a measured decision. Prototype Tantivy (fuzzy matching,
  CJK tokenizers via lindera/charabia, field boosts, facets) and SQLite
  FTS5 on the same extraction pipeline and relevance set, at 100k and 1M
  messages. Choose Tantivy only if it is materially better. Either way,
  publish through a replayable `search_outbox` with a commit-payload
  watermark, so the index stays derived and crash-safe.
- **Server search** fans out per account (X-GM-RAW, Graph `$search`, JMAP
  `Email/query`, IMAP ESEARCH/FUZZY) and merges by message identity with
  "on server" badges and visible local coverage.
- **Semantic search** is opt-in later: EmbeddingGemma or bge-small,
  sqlite-vec, fused by RRF.

*Strongest counter:* deferring the choice risks building twice. And if
FTS5 is "good enough" on the benchmark, a separate Tantivy store adds a
recovery/overlay subsystem for marginal gains.

## D8. macOS UI

**Recommendation.** AppKit shell; SwiftUI islands (settings, inspectors,
onboarding). `NSOutlineView` list. One reused, locked-down `WKWebView`
reader plus a TextKit 2 plain-text reader. `NSTextView`/TextKit 2 composer
with plain text and Markdown first. One command layer for menus,
AppleScript, App Intents (mail domain) and Services. Floor macOS 26,
arm64 first. A fair SwiftUI list prototype can overturn the list choice.

*Strongest counter:* SwiftUI on macOS 26/27 is Apple's direction and
claims 6–16× list gains. Two UI toolkits cost more than SwiftUI with a few
AppKit wrappers.

## D9. Linux UI

**Recommendation.** GTK4 + libadwaita in Rust (gtk-rs, optionally Relm4),
calling the core directly. WebKitGTK with JavaScript and JS markup
disabled. Flatpak first, plus a .deb. Qt/cxx-qt is the documented fallback
if KDE matters.

*Strongest counter:* libadwaita looks foreign on KDE, where many power
users live. Qt is native there and its web engine is Chromium.

## D10. Backends and order

**Recommendation.**

1. Generic IMAP4rev1/rev2 + SMTP with the full extension set, including
   Gmail via IMAP + X-GM-EXT with OAuth (avoids Gmail API per-project
   quotas and billing), iCloud, Yahoo, Fastmail, and Proton via Bridge.
2. Microsoft Graph for Exchange Online and Outlook.com (immutable ids,
   per-folder delta, polling). Urgent: Apple Mail on Mac relies on EWS,
   which Exchange Online has been phasing out since 2026-10-01.
3. JMAP (Fastmail, Stalwart, Cyrus).
4. Optional Gmail API mode for label-heavy users.
5. EWS for on-prem Exchange via Thunderbird's MPL crate.
6. Local mbox/Maildir, POP3, read-only NNTP/public-inbox.

Start Google restricted-scope verification (with a written CASA
determination), Entra publisher verification plus an admin-consent kit
(Microsoft's managed consent policies let named clients such as Apple Mail,
Spark, eM Client and Thunderbird through; Sweetbriar won't be on that
list), and Yahoo OAuth applications early. Proton direct integration
needs Proton's written agreement: its public Rust `mail-uniffi` binding is
AGPL-3.0-only. Tuta and HEY: import only.

*Strongest counter:* the EWS shutdown is a once-in-a-decade opening; Graph
should be first, because generic IMAP is a crowded, commoditized market.
Also, Gmail over IMAP lacks QRESYNC and loses label fidelity next to
`history.list`.

## D11. Rules

**Recommendation.** Sweetbriar's own rule engine (no AGPL Sieve). One
predicate language with search. Explicit phases: after receipt, before
submission, after confirmed submission, manual. Explicit ordering and
stop semantics. Preview on existing mail ("42 match, 11 would move, none
would send"), per-message execution records and rule traces, revisions.
Optional Mailsmith-style mailbox-attached rules as an advanced mode.
Compile to Sieve, Gmail filters or Graph rules only where exactly
expressible, labelled "runs on server" vs "runs on this Mac". A Sieve
editor via ManageSieve.

*Strongest counter:* client rules only run while the Mac is on. Most
users are better served by a great editor for server rules. A second rule
engine duplicates and conflicts.

## D12. Distribution

**Recommendation.** Developer ID + notarization + Sparkle, a mostly
sandboxed design, own billing. The Mac App Store stays possible later.

*Strongest counter:* the Store gives discovery and trust to a new,
unknown mail client. Airmail lives there.

## D13. Licensing posture

**Recommendation.** Permissive dependencies only, to keep both
open-source and commercial options open. The product licence is a
separate decision for the user.

*Strongest counter:* going GPL would unlock melib, notmuch/Xapian,
sieve-rs and Proton Bridge code, and community goodwill.

## D14. Classic features that define the product

**Recommendation.** Core identity, needed in the data model from day one:

- **Identities/personalities:** first-class, with reply routing.
- **An inspectable Out Box** with per-message and per-recipient states,
  send now vs queue, timed queue (Eudora, Emailer).
- **One query language with saved views and "reading sessions":** a
  stable attention queue across folders and accounts (Musashi's Virtual
  Message Viewer, Engst's Eudora workflow).
- **Explainable rules** (D11).
- **Keyboard-first filing:** type-to-file with learned destinations
  (Emailer Cmd-Opt-F, Mailsmith 2.1).
- **A text workbench:** quote-depth rewrap, Paste Quoted, clipboard
  history, clippings and templates with placeholders, an external-editor
  round trip (Mailsmith).
- **Private notes and local display overlays** for messages (Mailsmith,
  Eudora).
- **Redirect, Send Again, Make Filter, select similar** (Eudora).
- **Scriptability:** AppleScript, App Intents and a CLI over one command
  layer (all four clients).

Later: statistics window, tone and recipient pre-send checks
(MoodWatch/BossWatch), digest view (Content Concentrator), POP-style
remote inspection. Not revived: ESP, Eudora's ad-supported mode, POP-only
assumptions, monolithic stores.

*Strongest counter:* nostalgia isn't demand. Every competitor already has
some of these. The product should win on everyday excellence (speed,
search, Exchange) and add power features gradually.

## D15. AI

**Recommendation.** Opt-in, on device first (Apple Foundation Models:
~3B, 4K context; local models on Linux). No mail-mutating tools.
Deterministic features never depend on it. Use it for natural-language
query → AST, thread summaries, triage suggestions. No cloud by default.
"No AI required" is itself a selling point.

*Strongest counter:* users increasingly expect strong summarization and
drafting. On-device models are weak, and refusing cloud models cedes
quality to competitors.

---

## Questions for the reviewer

1. Which recommendations are wrong, and what would you choose instead?
2. Which facts here are wrong or outdated as of 2026-10-11?
3. What foundational decision is missing from D1–D15?
4. Rank the risks: which choice, if wrong, hurts most and is hardest to
   reverse?
5. Where should the first vertical slice cut through these decisions?
