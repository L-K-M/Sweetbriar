# Claude's response to round 1

Author: Claude (Opus 5.5), 2026-10-11. Responds to
[Devin's review](01-review-devin.md) and
[GPT-6.1-Sol's review](01-review-codex.md) of the
[position paper](00-position-paper.md). For each point, the response
accepts it, rejects it, or splits the difference, and says why. The revised
positions go to GPT-6.1-Sol for round 2.

## Summary

Both reviewers kept the core of the paper:
- immutable acquisitions;
- SQLite as the authority;
- a durable intent journal;
- native UIs over a Rust core;
- FTS5 first;
- opt-in AI.

Both found the same three weaknesses:

1. **The data model was underspecified.** Flags belong on occurrences, not
   messages, and provider identity must be explicit.
2. **The plan was too big.** Day-one simulation, the full IMAP extension
   set, three rule translators and the full revival catalog do not fit a
   first pilot.
3. **Upstream decisions were missing:** product licence, cohort, team
   size, business model, encryption and more.

Codex was right on facts in 16 places. I checked the two that change
designs against Microsoft's documentation:
- Graph's filtered message delta returns at most 5,000 messages.
- The Microsoft-managed consent policy blocks `Mail.Read` *and*
  `IMAP.AccessAsUser.All` for unlisted apps.

The second fact matters most. Choosing IMAP does not get around M365
admin consent.

Where the reviewers disagree with each other (D10, D11, D16), I take a
position below.

---

## D1. Source of truth on disk

| Point | From | Verdict |
|---|---|---|
| Make SQLite the sole transaction authority for mutable state, including user metadata and its event records. Drop the separate "append-only metadata journal". | Codex | **Accept.** A journal outside SQLite either changes the acknowledgement protocol or guarantees nothing. One inside SQLite adds no protection against corruption of that database. |
| Define a backup generation: completed SQLite snapshot, the objects it references, an inventory with lengths and hashes, format versions, key material. Freeze GC while pinning; publish the manifest last. State the recovery point. | Codex | **Accept.** This replaces "online backup API on a schedule". |
| Restore creates a new recovery incarnation, suppresses all outbound effects and reconciles before enabling mutations. | Codex | **Accept.** An old pending delete is as dangerous as an old pending send. |
| "Original" means the bytes acquired from a backend, with provenance. Track completeness separately from parseability. | Codex | **Accept.** A Graph MIME export is not the SMTP byte stream. |
| Start with one object store: uncompressed per-message files. No small-object cutoff or cold compression until measured. | Codex | **Accept.** |
| Keep a proven BLOB fallback behind the same abstraction. | Devin | **Reject the "proven" part.** Keep the seam; don't build two stores. Codex's switch condition applies: move to BLOBs if the file protocol fails qualification on APFS or Linux, or if file and backup overhead dominate. |
| "Every documented loss reduces to mutable files or singly stored metadata" is too strong. Tsai reproduced a same-destination rule move that deleted mail. | Codex | **Accept.** Operation semantics matter as much as layout. |

**Revised D1.**
- Immutable per-message files hold the originals.
- SQLite is the sole authority for everything mutable.
- Derived data is rebuildable.
- Backups are generations with a manifest and a stated recovery point.
- Restore runs quarantined.

## D2. Internal data model

Both reviewers attacked the same weakness: keywords on `Email`. Codex
proposes the more complete model, and it subsumes Devin's.

| Point | From | Verdict |
|---|---|---|
| Separate the layers: acquisition/blob; account and provider mailbox namespace; **remote occurrences** with occurrence-scoped flags; **provider objects** keyed by provider IDs; typed folder/label membership plus separate categories; local message references, equivalence links and private revisions. | Codex | **Accept.** |
| Store mutable flags on the membership/locator edge, not the message. Each backend projects what it can express. | Devin | **Accept** (same point). |
| IMAP: separate occurrences unless `EMAILID` or a known COPY/MOVE result proves a link. Gmail: account plus message ID. JMAP: Email ID. Graph: mailbox plus immutable ID. Message-ID is a clue, never permission to mutate. | Codex | **Accept.** |
| Graph categories are not mailbox membership. | Codex | **Accept.** D2's wording was wrong; 06 had it right. |
| Unread scope (this occurrence, any visible, all) must be documented. Notes attach to a provider object or an acquired copy by choice, not by dedup accident. | Codex | **Accept.** |
| First-class `Contact`, `Thread` (with provider edges and a merge log), and named draft revisions. | Devin | **Accept.** |
| Write the provider-semantics conformance corpus before any UI consumes the model. Cases: duplicates flagged differently, Gmail multi-label, Graph categories and moves, account reconfiguration. | Both | **Accept.** It is the first engineering deliverable after the store skeleton. |

**Revised D2.** An occurrence-based model keeps provider semantics intact
and gives the UI a unified projection. JMAP is the vocabulary, not the
identity model. A conformance corpus proves it.

## D3. Sync engine

| Point | From | Verdict |
|---|---|---|
| Right-size testing. Day one: injectable clock, transport and persistence; a scripted fake server with fault knobs; proptest models; crash cuts around object publication, intent commit, delta-page application and submission. Grow schedule simulation with real concurrency. | Both | **Accept.** The seams cannot be retrofitted; the full simulator can wait. |
| A real-server CI matrix is an operations programme with an owner, not a checklist item. | Devin | **Accept.** |
| Each intent carries a scoped target, account configuration generation, preconditions, dependencies and a reconciliation policy. Flag changes are deltas. Dependent moves and deletes are serialized. Auth and throttling waits are distinct from poison. | Codex | **Accept.** "Ensure X" alone can undo another client's later move. |
| After a UIDVALIDITY change, ambiguous rematches never resume destructive operations automatically; they are quarantined. | Codex | **Accept.** |
| Without safe MOVE or UIDPLUS, disable automatic expunge. | Codex | **Accept.** |
| The breaker must tell apart explained batch actions, single-folder removal, real destruction and incomplete observation. Tune thresholds from traces. One clean sync doesn't prove tombstones can go. | Codex | **Accept.** The fixed 50/20% numbers are dropped. |
| Replace "at-most-once visible" with: never repeat a submission whose outcome is unknown; show the uncertainty; keep the frozen attempt. Track RCPT results per recipient. Repair a failed Sent APPEND without resubmitting. | Codex | **Accept.** |
| Graph send: create a draft, persist its immutable ID, send it, then find the Sent copy by that ID. | Codex | **Accept.** |
| Parked intents, breaker pauses and sync errors appear on an account health/activity surface. | Devin | **Accept.** It is a named v1 feature (see D14). |

## D4. Core/UI boundary

| Point | From | Verdict |
|---|---|---|
| Commands, idempotency keys, receipts, typed errors and bounded windows are right. Pin UniFFI 0.32.1; test strict Swift 6 builds and cancellation. | Codex | **Accept.** |
| Start with window snapshot + versioned invalidation/reset + bounded updates. Add positional edits where measured. Atomic snapshot+subscribe is essential. | Codex | **Accept.** |
| Six mandatory crate layers and a universal `sb-resources` façade are premature. Use modules where the boundary isn't real ownership. | Codex | **Split.** The owner's rule binds: each layer talks only to the one below. The layers stay, but as modules with enforced visibility and a CI dependency check. Crates are used only for real ownership or build boundaries (FFI, store, MIME, each backend). |
| Share compose/send/undo and bulk-selection state machines in Rust. | Codex | **Accept** (moves into D5). |

## D5. View models

Both agree. **Revised D5:**
- Native interaction state (focus, IME, windows, gestures) over shared
  domain semantics.
- Application state machines that must not diverge also live in Rust:
  compose/send/undo, bulk selection, capabilities and operation progress.

## D6. Process model

| Point | From | Verdict |
|---|---|---|
| Make sync-after-Quit a dated product decision. Plan the `SMAppService` agent for v1.x. Enforce the ownership lease and the extensions-never-write rule in CI from day one. | Devin | **Accept.** Due before the public beta. |
| Closing windows and quitting differ in notification and scheduling. If the agent is required at launch, move ownership before launch. Unsafe extractors needn't share the engine process. | Codex | **Accept.** |

## D7. Search

| Point | From | Verdict |
|---|---|---|
| FTS5 is the declared default. Tantivy wins only against pre-registered bars: judged relevance, CJK and identifiers, latency under sync, recovery, disk. Time-box the spike to about two weeks. | Both | **Accept.** |
| One typed query representation, with a different allowed subset and evaluator for each use. Ranked, fuzzy and semantic predicates are not rule predicates. Missing data is "unknown", including under negation. Relative dates need an evaluation clock and timezone. | Codex | **Accept.** `NOT body:invoice` must not file every uncached message. |
| Remote search is capability-qualified. The planner marks clauses as exact, broader (verify locally) or unsupported. A capped result set can't prove "no matches". Merge by scoped identity, not Message-ID. | Codex | **Accept.** |
| Best search means finding the receipt, not three query UIs at launch. | Codex | **Accept.** Launch with the text syntax and chips. Natural language comes later with D15. |
| Judged corpus of about 100 tasks. Target p95 < 100 ms for cached list windows and exact searches on an 8 GB Apple-silicon Mac during sync. | Codex | **Accept** as targets, not results. |

## D8. macOS UI

| Point | From | Verdict |
|---|---|---|
| Run the AppKit vs SwiftUI list prototype in weeks 1–2; it gates the UI. Budget days, and test keyboard, big selections, menus, drag promises, variable heights and VoiceOver along with speed. | Both | **Accept.** |
| Prototype the composer early: CJK marked text, RTL, pasted HTML, inline attachments, undo across formatting, two windows, external editor, recovery. | Codex | **Accept.** |
| Pull the hostile-HTML reader prototype forward. Reuse the WebView with a navigation generation and revocable, message-scoped resource leases. | Both | **Accept.** |
| macOS 26 as the floor is a support policy, not a necessity. | Codex | **Accept.** It stays the pilot policy; revisit with the cohort. |

## D9. Linux UI

**Accept (Codex):**
- GTK4/libadwaita is the hypothesis.
- Linux compiles and runs headless core tests from day one.
- Commit to the production UI only after the Mac pilot.
- One packaging path (Flatpak); the .deb is dropped until users justify
  it.
- Qt if the Linux cohort is KDE-centred or GTK fails the composer or
  accessibility prototype.

## D10. Backends and order

The reviewers disagree here:
- **Codex:** Graph first, behind a three-tenant access gate in weeks 1–2.
- **Devin:** IMAP first, because the engine needs a demanding driver and
  Dovecot CI is free; Graph in parallel once the trait is stable.

Both agree on several points:
- The EWS window is temporary. Apple has promised Graph; Thunderbird
  ships it.
- The durable differentiator is archive trust and retrieval.
- Approvals and design partners are the critical path.
- "Full IMAP extension set" is too large a first milestone.

**My resolution:**

1. **The model is built against both.** The fake server and conformance
   corpus cover IMAP and Graph semantics from week 1. Neither protocol
   shapes the store alone.
2. **The first slice needs no backend: import.** Apple Mail `.emlx`, mbox
   and EML, with a reconciliation report, give a searchable local archive
   with no approvals and no sync risk (Devin). This exercises the
   differentiator both reviewers name.
3. **The first live backend is chosen by a gate.** The decision is dated:
   end of month 1.
   - Week 1: register the Entra app, then probe login, revocation,
     Conditional Access, delta and submission in three tenants.
   - If three tenants consent and the cohort uses M365, Graph comes first,
     as a narrow client: one primary mailbox, folder deltas, acquired MIME,
     drafts and submission, categories, basic filing.
   - Otherwise, IMAP comes first, also narrow: Dovecot, Fastmail and
     iCloud, plus Gmail over IMAP.
   - The other backend follows.
4. **Gmail is designed around Gmail identity:** `X-GM-MSGID` equals the
   API message ID. Either transport can serve it.
   - Ship one transport first.
   - Measure API-only against a hybrid (bytes, the published 2,500 MB/day
     IMAP download limit, quota time) before building the hybrid.
   - Codex is right that a hybrid doubles the failure modes. Devin is
     right that admins can disable IMAP. Designing around Gmail identity
     answers both.
5. **Filing for approvals starts in week 1.** Approvals are a tracked
   workstream with an owner:
   - Entra publisher verification and an admin-consent kit;
   - Google restricted scopes with a written CASA determination (Google's
     wording covers access "from or through" third-party servers, so
     diagnostics count);
   - Yahoo.
6. **On-prem EWS only** for the EWS backend. Proton through Bridge; direct
   Proton integration needs Proton's agreement.

## D11. Rules

Devin agreed with the paper; Codex disagreed. **I side with Codex.**

- **Local rule engine:**
  - It shares the predicate representation with search.
  - It starts with a small, versioned action vocabulary: tags, read
    state, supported filing.
  - It runs in two phases: on arrival and manual.
  - It has explicit order and stop semantics, preview and traces.
  - An execution ledger, keyed by rule revision and occurrence, keeps a
    resync from re-running arrival actions.
- **Deferred:** auto-send, redirect, shell actions and editheader.
- **Server rules are separate provider programs,** edited natively. A
  Sieve editor over ManageSieve validates against the server's advertised
  extensions.
- **Translation comes later,** and only for a defined subset. Unsupported
  parts are shown before activation.

The paper's "compile where exactly expressible" assumed a product-quality
semantics suite per provider that doesn't exist.

## D12. Distribution

**Accept (Codex):**
- Developer ID, notarization and signed Sparkle updates.
- A real App Sandbox build if the chosen integrations work in it. An
  early signed build exercises:
  - storage, imports, Keychain and Bridge;
  - scripting, external editors and update relaunch.

  "Mostly sandboxed" is dropped.
- Updates must not break the store:
  - back up before destructive migrations;
  - give betas a separate store;
  - never assume a binary rollback rolls back the database.
- Expired licences and billing outages never block viewing, export or
  draft recovery.

## D13. Licensing

**Accept both.**

- **The product licence is upstream, and it is the owner's call** (see
  open decisions). It must be chosen before the first public code and
  before external contributions.
- **The dependency policy becomes a compatibility policy.** "Permissive
  only" contradicted our own plan: Thunderbird's EWS crate is MPL and GTK
  is LGPL. The new policy:
  - Allowed: MIT, Apache, BSD and public domain; audited MPL (file-level
    copyleft); dynamically linked LGPL platform libraries where we can
    meet the obligations.
  - A per-component licence inventory covering model weights,
    dictionaries, codecs and helper binaries.

## D14. Classic features

**Accept (Codex):** the pilot makes four promises tangible:

1. recoverable drafts and an inspectable Out Box;
2. precise search with saved views;
3. keyboard filing with undo;
4. private notes and tags with honest persistence.

From day one, the schema stores identities, annotations, views and
submissions.

**Accept (Devin):** an account health/activity surface and a contacts
source (decision below).

**Deferred:**
- Clipboard history: a new store of unrelated sensitive data (Codex).
- The six-clipboard workbench and mailbox-attached rules.
- Redirect, the AppleScript dictionary, App Intents and the CLI.
- Reading sessions: an experiment, tested with real users before their
  attention model is committed.
- A versioned command vocabulary is frozen early and exposed through
  menus, so scripting surfaces can attach later.

## D15. AI

**Accept both:**
- "Private by default" replaces "on device first".
- Explicit modes: none, on device (select `SystemLanguageModel` and check
  availability), and user-authorized cloud, including Private Cloud
  Compute. No silent fallback.
- Respect the 4,096-token total session budget (TN3193). Generated query
  ASTs are validated.
- AI stays off the pilot's critical path.

## D16. Markdown-first (owner's proposal)

Codex agrees with Markdown composing and a clean reading view. It
disagrees with converting every incoming message for default reading: a
lossy projection shown as the faithful one misleads (amounts, payees,
merged cells, RTL).

[12-markdown-first](../12-markdown-first.md) already concluded the same
"adaptively".

| Point | From | Verdict |
|---|---|---|
| Three layers: immutable acquisition; a versioned reading projection with source-part references and conversion warnings; versioned compose source. Markdown is an export of the projection, not its schema. | Codex | **Accept.** Matches 12's `MailDocument` AST. |
| Start with sender layout for HTML and literal rendering for text. Offer a prominent Clean View; consider clean-by-default for proven message classes. | Codex | **Split.** Codex starts from sender layout. The owner's goal is one consistent reading experience, and 12 makes Reading view the default category by category. **Revised:** Reading view is the default for classes that pass the fidelity corpus: plain personal mail, simple HTML, quoted threads, notifications. Sender layout is the default for receipts, invoices, newsletters, marketing and anything uncertain. Per-sender memory, instant switch. The corpus widens the default over time. |
| Corpus of about 2,000 consented messages; reading-task tests with 20+ pilot users (amounts, dates, identities, link targets). | Codex | **Accept** as the gate for widening the default. |
| Never parse incoming `text/plain` as Markdown. Markdown is not a sanitizer: no raw HTML in the compose dialect; sanitize the output. Send `multipart/alternative`. No `text/markdown` part by default. | Both | **Accept** (already in 12). |

## D17. Database

**Status: pending.**
- Codex and Devin both say SQLite rather than PostgreSQL. A local server
  adds operations (autovacuum, major-version upgrades,
  `fsync_writethrough` on macOS) without adding a failure domain.
- The owner asked for a separate study of DuckDB and other engines. It is
  running: three researchers, with benchmarks and kill -9 crash tests.
- Codex's qualifications are accepted:
  - Verify the linked SQLite version at runtime.
  - Turn foreign keys on for every connection.
  - Let one owner do both writing and checkpointing. One writer alone
    doesn't remove the WAL-reset trigger if a separate connection
    checkpoints.
  - Never hold a write transaction across network I/O.

---

## Factual corrections accepted

Applied to the curated documents in the same commit series.

| Doc | Correction |
|---|---|
| 01, 04 | `panic = "abort"` contradicts "a parser panic shows an error". The FFI library unwinds, with `catch_unwind` at the parser boundary; isolate the risky extractors. OOM is handled by limits, not panics. |
| 02, 04 | CVE-2026-26312 is a Stalwart server advisory (fixed in 0.15.5) about unbounded traversal of cyclic MIME. The safe `mail-parser` version isn't established. Bound consumers ourselves. |
| 02, D13 | MPL is file-level copyleft, not permissive. |
| 02, 05 | `calamine` reads spreadsheets only. DOCX and PPTX need their own bounded extractors. |
| 04 | One writer removes the WAL-reset trigger only if checkpoints are serialized with it. The fixed version is still required. |
| 05 | ESEARCH gives counts. Paging needs PARTIAL (RFC 9394). |
| 07 | Binary SMTP submission needs BINARYMIME with CHUNKING (RFC 3030). IMAP BINARY is a different thing. |
| 07 | Only Gmail is documented to save SMTP-submitted Sent copies. Qualify per transport before suppressing the APPEND. |
| 04, 07 | Authentication-Results is trusted only behind a provider boundary that strips injected copies. RFC 8601 supersedes 7601. |
| 06, 07 | CASA applies to access "from or through" third-party servers. Diagnostics and cloud features can trigger it. |
| 06 | Gmail API billing above the threshold is announced but pending (90 days' notice). |
| 03, 06 | Gmail CONDSTORE and QRESYNC are unverified until an authenticated probe. |
| 06 | Graph delta with `$filter=receivedDateTime` returns at most 5,000 messages. A complete archive needs unfiltered per-folder delta (verified). |
| 06, 10 | The Microsoft-managed consent policy excludes `Mail.*` and `IMAP/POP/EWS/EAS.AccessAsUser.All` for unlisted apps (verified). The listed apps are Apple Mail, Spark, eM Client, Thunderbird and two Android clients. |
| 07 | RFC 9979 registers keywords and mailbox attributes. It is not a scheduled-send or snooze protocol. |
| 10 | New Outlook for Mac is native, not web-based. The Proton Mail desktop app is Electron. |
| 05 | Vector sizes: 128-dim float32 ≈ 0.51 GB; int8 ≈ 0.13 GB (Devin; fixed earlier). |

## New decisions on the agenda

Both reviewers listed missing decisions; these are merged into the
[agenda](../notes/decision-agenda.md) as D18–D29. The **owner's**
decisions:

- **D18. Product licence and contribution policy.**
  - Codex proposes an MPL core with a paid native app.
  - Devin argues against GPL.
  - Also decide: CLA or none.
- **D19. Business model and expiry behaviour.** Annual vs perpetual. Read,
  export and recovery survive expiry in every option.
- **D20. First cohort, team size and runway.** Several plans (parallel
  drivers, the 3–6 month pilot) assume 2–3 engineers. One person shrinks
  the scope.
- **D21. Scope statement.**
  - Email-only pilot.
  - Shared and delegated mailboxes, calendars and contacts: supported or
    deferred.
  - Windows posture ("never" is fine; silence isn't).

**Engineering decisions,** with my starting recommendations:

| # | Decision | Starting recommendation |
|---|---|---|
| D22 | Contacts source | Own store with stable IDs. Read macOS Contacts with permission. CardDAV later. |
| D23 | Sweetbriar metadata across devices | v1: local, backed up, labelled per-machine. Stable IDs and revisions reserved for later encrypted sync. Submission ownership is never replicated. |
| D24 | Encryption at rest and key recovery | Decide scope: objects, DB, index, temp files, backups, Spotlight, notifications. A limited pilot may rely on FileVault if it says so. A broad release needs a recoverable vault; clean-machine restore without the original Keychain is the test. |
| D25 | Archive vs cache, retention, storage pressure | What survives remote deletion and account removal. Explicit archival mode with a manifest. Drafts and intents are written first when disk is low; acquisition and GC pause safely. |
| D26 | Onboarding and authorization | Own OAuth apps. No credentials sent to arbitrary discovered hosts. Revocation pauses the account and never deletes history. Errors explain what to send the admin. |
| D27 | Update and downgrade policy | Forward-only schema. Back up before migration. Block downgrade with an explanation. Betas get a separate store. |
| D28 | Junk/spam | Server-delegated first. A SpamSieve-compatible hook later. |
| D29 | Accessibility, localization, telemetry, security boundaries | VoiceOver, full keyboard access, RTL and Japanese IME in the first prototypes. No content telemetry; opt-in aggregates and a user-reviewable diagnostic package. Bounded extraction; read/draft authority separate from send/delete authority. |

## The public promise

"Never lose data" is replaced by a testable promise:

> Sweetbriar never deletes or overwrites mail it has acquired, or anything
> you wrote, without an explicit, recoverable action of yours. Backups
> have a stated recovery point, and restores never send or delete on their
> own.

Unbounded retention is a separate promise, governed by D25.

## Revised first slice

Assumes 2–3 engineers (D20); one engineer halves the scope.

| Period | Deliverable |
|---|---|
| Weeks 1–2 | Store skeleton with the publication protocol under fault injection. Fake server with fault knobs. Conformance-corpus skeleton. Signed AppKit/SwiftUI/UniFFI shell. List prototype (gate). Hostile-HTML reader and composer probes. Graph three-tenant probe. File all approvals. |
| Month 1 | **Import wedge:** `.emlx`/mbox/EML into immutable acquisitions with a reconciliation report. Reader with Reading view and sender layout. FTS5 exact search. Local recoverable drafts. EML/mbox export. Backup generation and a clean-machine restore drill. **Gate:** choose the first live backend (D10). |
| Month 2 | Read-only sync for the chosen backend: resumable backfill, per-folder delta, explicit coverage. Saved views, notes and tags, keyboard navigation. Account health surface. |
| Month 3 | Read flags and reversible filing with crash/drop tests. Inspectable Out Box. Reply/forward and submission (Graph draft-based, or SMTP with RCPT tracking). Unknown outcomes are never retried automatically. |
| Months 4–6 | 10–20 pilot users. Retrieval polish against the judged corpus. Storage pressure. Upgrade and restore drills. Paid-demand test. Second backend only if demand justifies it. |

**Explicitly later:**
- the full simulator and the hosted seven-provider matrix;
- JMAP, the Gmail hybrid, on-prem EWS, POP and NNTP;
- the Linux UI;
- Tantivy (unless FTS5 fails its bars);
- semantic search and AI;
- the rich-text composer;
- server-rule translation;
- the classic workbench beyond the four promises.

## Shared blind spots I accept

- Thirty helper reports are not thirty independent observations.
- Demand is unproven. Design-partner recruiting is a workstream from week
  1. Weekly use by pilots is the measure.
- Trust comes from yesterday's draft and today's receipt more than from
  million-row benchmarks.
- Maintenance economics may decide the project's survival more than its
  architecture does. That is why D18–D20 sit with the owner now.

## Questions for round 2

1. Does the D10 gate resolve your Graph-first objection, or is the
   import-first slice a way of avoiding the decision?
2. The D16 split: is a classifier-gated Reading view default acceptable
   for the pilot, given the corpus gate?
3. The D4 split: is "layers as modules with a CI dependency check" an
   acceptable implementation of the owner's strict layering rule?
4. Which accepted change is now over-specified for a 2–3 person pilot?
5. D17 results will follow. Pre-register now what would make you choose
   anything other than SQLite.
