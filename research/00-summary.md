# Sweetbriar research: summary

Claude (Opus 5.5), 2026-10-11. This is the one-page answer to the owner's
questions. The reasoning, sources and disagreements are in the
[curated documents](README.md) and the [debate](debate/04-resolution.md).

**How it was produced:**
- Most topics were researched by two or more independent helper agents
  (Codex GPT-6.1-Sol, Devin SWE-2 Max, GLM-5.3, MiMo), working from
  primary sources: manuals, the Eudora source code, the original Musashi
  archives, vendor documentation.
- Claude merged the findings into curated documents and a position paper.
- GPT-6.1-Sol and Devin attacked that paper over two rounds. A fact check
  covered 18 recent claims, and Claude verified the high-impact facts by
  hand.

## The answers

**How should it be built?**
- **Core:** a Rust engine (store, sync, MIME, search, rules, drafts, send
  queue) under native shells: AppKit with SwiftUI islands on macOS, and
  later GTK4/libadwaita on Linux. They talk through a small command +
  windowed-query API over UniFFI, with a hand-written Swift adapter.
- **Process model:** the engine runs inside the app for the pilot. A
  login-item agent can take over later, behind an ownership lease.
- **Layering:** the owner's strict adjacent-layer rule is enforced by a
  module check in CI from the first commit. Crates exist only at real
  boundaries (FFI, store, MIME, each backend).
- Details: [01](01-architecture.md), [02](02-rust-ecosystem.md),
  [08](08-macos-ui.md), [09](09-linux.md).

**How do we make it fast?**
- **Never block the UI on the network or the disk.** Lists are windowed
  queries over covering indexes with keyset paging: at 1M rows, a page
  costs microseconds, versus 80 ms with `OFFSET`.
- **Proposed budgets** (p95 on a declared Apple-silicon reference Mac;
  not yet measured):
  - 100 ms to open a cached message, switch mailbox or show search
    results;
  - 50 ms visible for archive/delete;
  - durable local intent within 100 ms.
- The remaining time goes to sync protocols (deltas, batching) and an
  AppKit list, chosen against SwiftUI by a fair prototype.
- Details: [03](03-performance.md).

**How do we guarantee data safety?**
- **Never trust one copy or one write.** The store is split three ways:
  - original messages are immutable files, published with full fsync
    before anything references them;
  - SQLite is the sole authority for everything mutable (drafts,
    intents, notes, settings);
  - search and summaries are rebuildable.
- **Every remote change is a durable, preconditioned intent.**
  - There is no bare EXPUNGE.
  - Ambiguous matches are quarantined, never resumed automatically.
  - A breaker pauses unexplained mass disappearances.
  - A send whose outcome is unknown is never repeated automatically.
- **Backups and restores:** backups are coordinated generations with a
  stated recovery point. A restore suppresses all outbound effects until
  reconciled.
- **The public promise:** once Sweetbriar shows "Saved", what you wrote
  survives crashes and power loss. Archived mail is never deleted or
  overwritten except by an action you can undo or a policy you chose.
- Details: [04](04-data-safety.md).

**How do we get best-in-class search?**
- **Trust first.** That means:
  - exact results, never silently rewritten;
  - visible coverage;
  - recovery from index failure;
  - one typed query representation shared by search, saved views and
    rules, with "unknown" as a real answer for mail not yet downloaded.
- **Engine:** SQLite FTS5 is the starting engine. Tantivy replaces it
  only if it clears bars registered in advance.
- **Later:** server-side search fills the gaps for mail not on the Mac.
  Semantic search comes later, opt-in.
- Details: [05](05-search.md).

**How do we support Exchange, Proton and others?**
- **Exchange Online:** Microsoft Graph, since EWS is being shut off
  (blocking began 2026-10-01; removal 2027-04-01).
- **Gmail:** an account type built around Gmail identity, so either IMAP
  or the API can carry it.
- **Proton:** through Proton Bridge. Direct integration needs Proton's
  agreement, because its Rust client binding is AGPL.
- **Fastmail and Stalwart:** JMAP.
- **Generic providers:** IMAP.
- **Approvals are the real critical path:**
  - Microsoft's default consent policy blocks unlisted apps from reading
    mail, over Graph *and* over IMAP;
  - Google's restricted scopes need verification;
  - Yahoo needs manual approval.
- Details: [06](06-account-backends.md), [07](07-standards.md).

**What made the old clients beloved?**
- **Agency:**
  - find anything precisely;
  - act on what you see;
  - teach the app your workflow;
  - see what it's doing;
  - work fast from the keyboard.
- **The pilot makes four of these tangible:**
  1. recoverable drafts and an inspectable Out Box;
  2. precise search with saved views;
  3. keyboard filing with undo;
  4. private notes and tags.
- **The schema reserves room** for the rest of the
  [22-item revival catalog](11-classic-clients.md): identities, reading
  sessions, the text workbench, scriptability.
- Per-client studies: [Eudora](classics/eudora.md),
  [Mailsmith](classics/mailsmith.md),
  [Claris Emailer](classics/claris-emailer.md),
  [Musashi](classics/musashi.md).

**Markdown as the primary format?**
- **Compose:** yes. Plain text or Markdown, sent as
  `multipart/alternative` (plain text + sanitized HTML).
- **Reading:** yes, adaptively. A clean Reading view (from a semantic
  document model, not Markdown) becomes the default category by category
  as a fidelity corpus proves it safe. Until then, the sender's layout,
  sanitized, is the default.
- **Storage:** never as Markdown. Originals stay byte-exact, and incoming
  plain text is never parsed as Markdown.
- Details: [12](12-markdown-first.md).

**Postgres, DuckDB or another database?**
- **Bundled SQLite.** Four studies agree: two Postgres comparisons and
  two engine surveys with benchmarks and kill -9 crash tests. A third
  engine survey timed out.
- **Postgres** would add a server to run and upgrade on every Mac, with
  no new failure domain. Akonadi just moved from server databases to
  SQLite.
- **DuckDB** is an analytics engine: about 45× slower on point lookups,
  and its file can't be read by a second process while the app writes.
- **The rest:** key-value stores would mean writing our own query layer.
  Turso, a Rust rewrite of SQLite, is worth re-evaluating when it matures.
- **SQLite's odd defaults** are neutralized by configuration.
- **Measurable switch criteria** are registered before the Mac
  benchmarks.
- Details: [13](13-database-engine.md).

## Why a shared Rust core (the premise, argued)

**For:**
- The product's value is its logic: sync correctness, the durable store,
  search, rules, MIME and drafts. That logic must behave identically on
  every platform, and it is the hardest part to get right.
- Rust gives memory safety for parsing hostile mail.
- There is one store to qualify, and one place to fix a data-loss bug.
- Proton (mobile Mail on a Rust core, 2025), Element X, Firefox, Delta
  Chat and Ghostty show the pattern works.

**Against:**
- Dropbox abandoned its shared C++ core in 2019; sharing cost more than it
  saved.
- Element found the SDK harder than the apps.
- Thunderbird runs three separate stacks.
- A Swift-only Mac v1, with the core extracted when Linux becomes real,
  would ship sooner.

**Verdict: keep the Rust core.**
- The shared part here is the engine, not the UI, and the Linux client is
  a stated goal.
- The cost is contained by a narrow boundary (commands plus windowed
  queries), native view models, and starting with one platform.
- **Revisit if** the bridge becomes the bottleneck in measured
  interactions, or if Linux is dropped.

## What is settled

Agreed by Claude, GPT-6.1-Sol and Devin after two rounds. These are
subject to implementation evidence, not to further argument.

| # | Decision |
|---|---|
| D1 | Immutable per-message files; SQLite as the only mutable authority; rebuildable derived data; backup generations with manifests; quarantined restore. BLOBs remain a real switch behind the object-store contract. |
| D2 | Occurrence-based model with explicit provider identity. Flags live on occurrences, Graph categories are not folders, and Message-ID never authorizes merging. A conformance corpus comes before any UI. |
| D3 | Preconditioned intents; quarantined rematches; no unsafe expunge; frozen send attempts with per-recipient results; a breaker with conservative defaults; right-sized testing that grows with the engine; an account health surface. |
| D4–D5 | Commands, receipts and windowed snapshot/subscribe queries. Strict layering via a module check. Native interaction state; shared Rust semantics, including compose/send/undo. |
| D6 | In-process engine for the pilot. Sync-after-Quit is a product decision due before the public beta; the ownership lease is enforced from day one. |
| D7 | FTS5 first, Tantivy behind registered bars; per-use query subsets; capability-qualified server search. |
| D8–D9 | AppKit prior, tested against SwiftUI; reader, composer and accessibility probes early; macOS 26 floor as a policy. Linux core from day one; GTK UI and Flatpak after the Mac pilot. |
| D10 | Import first. The first live backend is picked by a dated gate. One provider, then others by demand. Approvals start in week 1. |
| D11 | A small, explainable local rule engine. Server rules are edited per provider; translation comes later. |
| D12–D13 | Developer ID + notarization + signed Sparkle; a real sandbox if the integrations allow; migration-safe updates; expiry never blocks reading or export. A licence compatibility policy (permissive + audited MPL + dynamic LGPL) with an inventory. |
| D14–D15 | Four pilot workflows; AI opt-in with explicit on-device/cloud modes, off the critical path. |
| D16 | Markdown compose; adaptive Reading view gated by the corpus. |
| D17 | Bundled SQLite (pinned ≥ 3.51.3, verified at runtime) for state; FTS5 for search. |
| D22–D29 | Starting recommendations in [debate/04-resolution](debate/04-resolution.md) and the round-2 reviews: contacts, per-machine metadata, encryption from a threat model, archive-by-default, onboarding, forward-only migrations, junk, accessibility/telemetry. |

## Decisions only the owner can make

These come before a credible schedule. The options and questions are in
both round-2 reviews ([Codex §4](debate/03-review-codex-r2.md),
[Devin §5](debate/03-review-devin-r2.md)).

1. **D18 Licence and contributions.**
   - Options: proprietary; source-available; MPL core + paid app;
     permissive core + paid app; GPL.
   - Will outside contributions be accepted, and under a CLA, a DCO or
     neither?
   - Decide before the first public code.
2. **D19 Business model.**
   - Options: annual, perpetual + upgrades, or support/sponsorship.
   - Reading, export and draft recovery survive expiry in every option.
3. **D20 First users, team and runway.** This decision sets the scope of
   everything else:
   - Who is reachable, can authorize the app, and needs these workflows?
     The candidates:
     - small Microsoft 365 organizations with archives;
     - Gmail or Workspace users;
     - mixed-provider power users;
     - classic-client refugees;
     - an archive companion first.
   - How many engineers? Plans assume 2–3; one engineer halves the scope.
   - What is your own primary mail provider? It becomes the default first
     backend if no cohort evidence exists by the gate.
4. **D21 Scope.**
   - Primary-mailbox email only, or also shared/delegated mailboxes,
     calendars and contacts?
   - Windows: never, later, or planned? Saying "never" is fine.

## Open, to be settled by evidence

- **D10 gate outcome:** Graph or IMAP first.
- **Gmail transport:** API-only vs a hybrid.
- **D16:** which message categories earn the Reading view default.
- **Search:** the Tantivy-vs-FTS5 bars, and where the index lives.
- **D17 Mac numbers:** SQLite commit latency with `F_FULLFSYNC` on APFS,
  and whether to use `cksumvfs`.
- **Unverified items:**
  - UniFFI's strict-Swift-6 compile of the real exports;
  - direct Apple Mail import under macOS 27 permissions;
  - Gmail CONDSTORE/QRESYNC on an authenticated account;
  - Google's written CASA determination.
- **Demand:** weekly use by pilots and willingness to pay. Forum
  complaints are not demand.

## First steps (evidence milestones)

1. **Foundations.**
   - Store skeleton under fault injection.
   - Module-layering check.
   - IMAP and Graph behavior specs.
   - Signed shell with list, reader and composer probes.
   - Own-tenant Graph probe.
   - Approvals started; design partners recruited.
2. **Local archive.**
   - `.mbox`/EML import with a reconciliation report.
   - Reader.
   - FTS5 search.
   - Drafts.
   - Export.
   - A clean-machine restore drill.
   - Gate: the first live backend.
3. **Live reading.** One provider: resumable backfill, explicit coverage,
   the health surface.
4. **Remote effects.** Flags, filing, reply/send and the Out Box, each
   behind crash/drop evidence.
5. **Pilot expansion.** 10–20 users, saved views, notes, Reading-view
   defaults where proven, and a paid-demand test.

## Biggest risks

1. **Lost mail or a duplicate send.** Remote effects under uncertainty
   (D3) can't be fixed by swapping a library. They are mitigated by
   intents, quarantine and operation-specific fault tests before each
   effect ships.
2. **A wrong identity model (D2)** would contaminate notes, rules, undo
   and deletes across the store. The conformance corpus comes first.
3. **Persistence, backup and keys (D1/D17/D24):** acknowledged state
   that can't be recovered. Qualify the flush path, ENOSPC, interrupted
   migrations and a clean-machine restore early.
4. **Access without adoption (D10/D20):** authorized users who never
   switch. Recruit design partners from week 1, and measure weekly use.
5. **Licence and funding (D18/D19):** obligations that can't be
   reversed once code and contributors exist.

## Map

- [README](README.md): document index and method.
- [debate/](debate/): the position paper, the reviews, the response and
  the resolution.
- [notes/decision-agenda.md](notes/decision-agenda.md): D1–D29.
- [notes/working-notes.md](notes/working-notes.md): every conflict and
  hand-verified fact.
- [raw/](raw/): helper reports verbatim, with benchmark scripts and
  results.
