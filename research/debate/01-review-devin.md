<!-- Round 1 independent review by Devin (SWE-2 Max), 2026-10-11, of debate/00-position-paper.md. Verbatim. -->

# Adversarial review: Sweetbriar foundational recommendations (round 1)

Reviewer: Devin (SWE-2 Max), 2026-10-11. Object of review:
`research/debate/00-position-paper.md` plus the curated documents,
working notes and raw reports it cites. I verified a sample of
load-bearing external facts against the live web (EWS timeline, SQLite
WAL-reset fix version, Gmail API quotas, Graph subscription lifetimes,
Airmail CVEs) and note in-world claims that can't be externally checked.

Overall: the paper is unusually well-grounded — most recommendations are
the right shape, most cited facts check out, and the counter-arguments
are honest. My disagreements concentrate on D2, D3, D7 and D10, plus
what the agenda *omits*. The paper's biggest structural weakness is that
it sequences work as if the constraint were engineering order, when the
real critical path is provider approvals and design-partner recruiting.

---

## 1. Verdict table

| # | Verdict | One line why |
|---|---------|--------------|
| D1 | Agree | Immutable files + SQLite authority + rebuildable derived data is the right split for "never lose data"; the counter (BLOBs) loses on blast radius, export and third-party access |
| D2 | Agree with changes | JMAP-shaped core is right, but flags must live on the membership/locator edge (IMAP stores them per (mailbox, UID)), and contacts/threads need to be first-class |
| D3 | Agree with changes | Intent journal + convergence replay + no bare EXPUNGE are right; "deterministic simulation from day one" conflates three differently-priced things — descope the expensive part |
| D4 | Agree | Commands + windowed pull subscriptions + typed errors + UniFFI behind an adapter is correct; pull-not-push is the right call |
| D5 | Agree | Native view models; the interaction layer is thin and platform-idiomatic, the shared layer already carries the semantics |
| D6 | Agree with changes | In-process v1 is fine, but sync-after-quit is a launch-line feature for the Outlook-switcher audience — force the product decision early, enforce the lease in CI |
| D7 | Agree with changes | The gate is right, the bias is wrong: default to FTS5 and make Tantivy survive named failure bars, or the project builds two engines |
| D8 | Agree | AppKit shell + SwiftUI islands + `NSOutlineView` list is right; the fair-prototype escape hatch is the correct hedge |
| D9 | Agree | GTK4 + libadwaita in Rust, WebKitGTK locked down, Flatpak first — right stack, documented Qt fallback is enough |
| D10 | Agree with changes | Ordering is right but framed as serial when drivers parallelize once the trait stabilizes; Gmail-API should be a designed-for hybrid, not "optional"; the EWS window is shorter than implied |
| D11 | Agree | Own rule engine + compile-out-where-exact + ManageSieve editor is the right answer; the counter (server rules only) fails on offline/mixed-provider reality |
| D12 | Agree | Developer ID + notarization + Sparkle, mostly-sandboxed, MAS kept open — standard indie-correct |
| D13 | Agree with changes | Keeping deps permissive is right, but the paper defers the decision that is actually upstream (product licence); if GPL were chosen, D7/D11/D10 answers change |
| D14 | Agree with changes | Right list, right "data model from day one" framing; two dependencies are undecided elsewhere (contacts source, activity/status surface) |
| D15 | Agree with changes | Right posture, stale framing: "on device first" must become "private by default" because macOS 27's own stack now routes through Private Cloud Compute |

Net: no outright disagreement. The paper errs toward *under*-committing
in three places (Gmail hybrid, FTS5 default, sync-after-quit decision)
and *over*-committing in one (simulation harness breadth).

---

## 2. Where Claude is wrong or under-commits

### D2 — the data model underspecifies the part that bites

The paper's stated counter — per-folder flags — is real, and the
mitigation offered ("`providerData` sidecar", "per-locator flags where
the backend scopes them") is too weak for what is actually the common
case. In IMAP, the same logical message copied to two mailboxes is two
UID instances with *independent* `\Seen`/`\Flagged` state. In JMAP,
`keywords` are per-`Email` — one object, one flag set. If Sweetbriar's
canonical model stores keywords on the message, then IMAP accounts get a
flag semantics that is *wrong by construction* and users see states
merge across folders that shouldn't.

**My alternative:** store mutable flags on the membership edge
(`membership(mailbox, message, seen, flagged, keywords…)`) or on the
remote locator, not on `Email`. Then each backend *projects* what it can
express: JMAP/Gmail collapse per-membership flags to per-message
keywords (with a documented merge rule — e.g. `$seen` = seen in the
Inbox membership or majority), IMAP gets exact per-folder flags, Graph
gets per-folder state plus categories. The rule for the model is: hold
the *most expressive* semantics any real backend has, and make the
poorer projections explicit and labelled. This is strictly more work
than `mailboxIds`+keywords, but it's the difference between "preserves
each provider's semantics" (the landscape doc's own pillar #1) and
flattens-them-quietly.

Three more gaps in D2 as stated:

- **Contacts/people are not modeled at all.** D14 wants nicknames,
  Quick Recipients, "sender intersects address book/group" rule
  criteria, and identity suggestions — all presuppose a first-class
  `Contact`/address-book entity. Where do addresses live (own store,
  macOS Contacts.framework, CardDAV)? Undecided anywhere (see §4).
- **Thread identity deserves to be an entity**, not just "provider
  threadId hints plus a local References graph": thread merge/split is a
  user-visible action (Eudora/MailMate territory), provider hints
  conflict, and the paper's own D14 "reversible manual grouping" needs a
  stable thread id with provenance. Name it `Thread` with
  `provider_thread_id` edges and a merge log.
- **Drafts vs messages.** `EmailSubmission`-like outbox records are
  listed; the draft *revision* model (immutable revisions + conflict
  branches, already in 04-data-safety) is Sweetbriar's own and should be
  named in the model, because "a draft is not an Email in a Drafts
  folder" is a real divergence from JMAP.

**What would change my mind:** a demonstration that treating IMAP
per-folder flags as per-message keywords doesn't produce user-visible
flag corruption in the common flows (copy to folder A, read in A, still
unread in B). I don't believe that demonstration exists — Mailspring's
2026 rewrite moved to a many-folders-per-message model for adjacent
reasons (iCloud folders, Exchange duplicate Sent, Proton labels), i.e.
even a shipping client had to pay this retrofit.

### D3 — "deterministic simulation from day one" prices three things as one

The paper bundles: (a) seeded fake IMAP/JMAP/Graph servers with injected
faults, (b) proptest state-machine tests vs an in-memory IMAP model,
(c) deterministic *schedule* simulation à la FoundationDB/TigerBeetle,
(d) a real-server CI matrix. These differ in cost by an order of
magnitude:

- (a) is not optional and is nearly free — you need a scriptable fake
  IMAP server for ordinary development anyway; adding fault knobs
  (drop-after-COPY, UIDVALIDITY reset, partial pages) is incremental.
- (b) is cheap and catches the convergence bugs that matter.
- (c) — controlled task interleavings and a virtual clock/disk over the
  whole engine — is the months-long item. Its yield scales with engine
  concurrency bugs, which scale with code that doesn't exist yet.
- (d) is not a "test", it's an *operations program*: hosted Gmail/M365/
  iCloud/Fastmail test accounts get locked, OAuth test apps get
  suspended, credentials rotate. Somebody owns that forever. The paper
  should budget it as an ongoing cost with an owner, not a checklist
  item.

**My alternative:** day one = injected clock/network/disk *seams* (the
part that genuinely can't be retrofitted) + the seeded fake server +
proptest model. Deterministic schedule-simulation gets introduced
incrementally as the engine gains concurrency — and its acceptance
evidence is measured in found bugs. The paper's own strongest counter is
half right: the expensive part can wait; the *seams* cannot. The
counter's claim that "Mailspring shipped for years with drop-on-crash"
actually proves *my* point, not Claude's — Mailspring's failure mode
(dropped in-flight tasks) is fixed by the intent journal (cheap), not by
the simulator (expensive).

Also: the "poisoned op parks after N attempts" needs a visible
per-account health surface (a stuck queue is how "rules stopped working"
bugs look to users — see Apple Mail's silent-rules complaints in the
landscape doc). Parked intents are UI data, not just engine state.

**What would change my mind:** evidence that Sweetbriar's engine ends
up more like FoundationDB (arbitrary interleavings across many
concurrent actors) than like a sync client (a handful of per-account
actors with a serialized writer). The layering doc's one-writer,
per-account-drivers design actually *limits* interleaving state
explosion, which lowers (c)'s yield further.

### D7 — the gate is right; the default should be flipped

Claude recommends "a measured decision... choose Tantivy only if it is
materially better." In practice, an open-ended bake-off between two
engines with one shared pipeline drifts into building both (the corpus
already designed Tantivy's outbox/watermark machinery in detail —
sunk-cost gravity). And "materially better" has no threshold, so the
decision can be re-litigated forever.

**My alternative:** FTS5 is the declared default for v1. Pre-register
the failure bars: FTS5 loses *only* if (i) judged-relevance quality on a
fixed query set is materially worse (defined number, e.g. nDCG gap),
(ii) CJK/fuzzy requirements can't be met with a custom tokenizer +
trigram + a spellfix-style suggestion corpus, or (iii) indexing
throughput/latency misses stated numbers at 1M. Everything else — query
AST, extraction pipeline, search_outbox publication, coverage UI — is
engine-neutral work that ships either way. Rationale: "one durability
story" is not a nicety for this product, it's the brand; Tantivy's real
edges (fuzzy, facets, ranking control, CJK tokenizers) are exactly the
features the benchmark must judge with *users' queries*, not latency.
And note the cost asymmetry the paper under-weights: a second store
means a second crash-consistency protocol to prove under the §"prove
the invariants" regime — Tantivy isn't just a dependency, it's a second
durability surface.

**What would change my mind:** the benchmark itself. If FTS5+
custom-tokenizer misses materially on judged relevance or CJK, Tantivy
wins on evidence and the outbox design already exists. I'm not arguing
FTS5 forever — I'm arguing the default and the exit criteria.

### D10 — the sequencing debate is misframed, and Gmail-API is wrongly "optional"

Two corrections and one reframing:

**(a) Drivers aren't serial once the `AccountBackend` trait exists.**
The paper's numbered list (IMAP → Graph → JMAP → Gmail API → EWS →
local) reads as a single queue. The real dependency graph is: store +
sync engine + trait must come first, and *IMAP is the right driver to
build them against* — Dovecot in CI for free, IMAP's pathological
semantics (UIDVALIDITY, expunges, per-folder flags) stress-test the
model, and Gmail-via-IMAP covers the biggest first-user pool. But Graph
should start as soon as the *trait* stabilizes, not after IMAP ships —
it's REST/delta, shares almost no machinery with the IMAP driver, and
can be built by a different workstream. I'd restate the order as: "IMAP
first because the engine needs a driver; Graph in parallel from trait
stability; everything else strictly by demand."

**(b) Gmail-API should be a designed-for hybrid, not "optional for
label-heavy users."** The corpus's own arithmetic makes the case better
than the paper does: `messages.get` at 20 units under a 6,000
units/user/min cap makes API *bulk* import take ≥55.6h — but that argues
for IMAP-for-bulk, not for IMAP-for-everything. `history.list` calls
are infrequent per-account polling, so they stay cheap under the same
per-user cap, and they give true label/thread deltas with no
`autoExpunge`/`expungeBehavior` footguns. Meanwhile the
paper's own capability notes record that Workspace admins can disable
IMAP entirely — at which point an IMAP-only Gmail driver is a dead
account type for exactly the enterprise-adjacent users in the target
segment. Design the Gmail driver now as two coordinated transports
(IMAP: bodies, bulk, fallback; API: deltas, labels, settings) joined on
`X-GM-MSGID` = API id. Shipping IMAP-only first is fine; treating the
API side as optional invites the retrofit the rest of the paper warns
about.

**(c) The EWS window is shorter than "urgent" implies.** Verified this
week: phased blocking started 2026-10-01, `EWSAllowedAppIDs` becomes
mandatory 2026-10-10 (unlisted clients lose EWS even where tenants
re-enable it), permanent removal 2027-04-01 — and Apple is already
publicly committed to shipping Graph in a macOS 27 update. By the time
Sweetbriar has users (≥12 months out), Apple will very likely have
shipped, and Thunderbird/eM Client already serve the displaced today.
So: don't distort the build order for the window. *Do* recruit design
partners from the displaced population now — they're precisely the
"consequential archive + M365 at work" segment — and have the
admin-consent kit ready before they ask.

**What would change my mind:** a signed-up cohort of M365-only design
partners who can't be served any other way, or a date slip from Apple
making the window real.

### D13 — the deferred decision is upstream of this one

The paper says "permissive dependencies only; product licence is a
separate decision for the user." Fine — but then the agenda should note
that the product-licence decision must be taken *before* first public
code and that it's upstream of D13: if the owner chose GPL, the
sieve-rs/melib/Xapian doors open and D7/D11 answers shift. (I think GPL
is still wrong for this product — the things GPL unlocks are
"read-don't-link" cases anyway for architecture reasons, and it poisons
the commercial option the owner clearly wants — but the coupling should
be made explicit rather than assumed away.)

### D15 — right posture, stale framing

"On device first" is a commitment that Apple's own stack no longer
makes: the landscape doc records that macOS 27 adds a generalized
`LanguageModel` protocol and `PrivateCloudComputeLanguageModel`, so
"Foundation Models" ≠ on-device. Reframe the promise as **private by
default** (on-device → user-authorized PCC → user-authorized external
cloud, as the three-mode recommendation in 10-landscape already says)
and make the paper agree with its own source doc. Also note Proton
Scribe already ships local+cloud drafting — "no AI required" is a
selling point, "on-device only" is a handicap users will route around
anyway.

### D6 — the real decision is being deferred implicitly

In-process v1 is right, and the paper says the right mechanism
(transport seam in L1, exclusive lease). What's missing is a forcing
function: "if background sync becomes a release requirement" will be
decided by default at the worst possible time (post-launch, when the
retrofit is hardest). For the Outlook-switcher segment, sync-after-quit
+ notifications is close to table stakes. Decide it as a *dated product
decision* — my recommendation: plan the `SMAppService` agent for v1.x,
enforce the ownership lease and extensions-never-write rules in CI from
day one. The cost of pre-committing is near zero; the cost of drifting
is the rewrite the counter-argument describes.

### D14 — two hidden dependencies

The list is well-chosen, but two items have undecided foundations:
identities/quick-recipients/nickname-criteria all need a contacts source
(nothing anywhere decides where addresses live), and "an inspectable Out
Box" generalizes into an *account health/activity surface* — parked
poison ops, circuit-breaker pauses, sync errors, per-account "last
clean sync" — which deserves to be a named v1 feature, not implied.
Eudora's task/status window and Thunderbird's Activity Manager are the
precedents; the landscape doc's "see what the app is doing" is the user
demand.

---

## 3. Factual errors and stale claims

Verified-by-me claims that are **correct** (so the reviewer-after-me can
skip them): EWS timeline (Oct 1 2026 blocking start, `EWSAllowedAppIDs`
mandatory Oct 10, April 1 2027 removal — MC1227454/MC1485116);
SQLite WAL-reset fix = 3.51.3 (Antithesis reproduced in ~15 min,
Tailscale postmortem — and note Tailscale's post says the fix shipped
*as* 3.52.0 while the libsql issue says 3.51.3; both can be true via a
backported patch release — the paper's "≥3.51.3 or backports" phrasing
is the right way to say it); Gmail API new-project quotas
(6,000 units/user/min, `messages.get` = 20, 80M/day billing threshold —
the 55.6 h arithmetic checks out); Graph subscription lifetime for mail
(10,080 min = "under seven days", 1,440 min with resource data — the
doc's "under 7 days / under 1 day" is right); Airmail CVEs
(CVE-2018-15667 URL-scheme send, -15668 attachment exfiltration,
-15669 WebView deny-list bypass — corpus cites 15667/15669 correctly,
and could add 15668 for completeness); the ~35% small-BLOB claim matches
sqlite.org's fasterthanfs test; "Engst's two converters disagreed by
121,077" — arithmetic checks (970,283 − 849,206).

**Errors / stale items:**

1. **05-search, vector storage math.** "1M × Matryoshka-128 ≈ 0.13 GB"
  only holds with int8 quantization (1 byte/dim). At float32 it's
  ~0.51 GB — 4× understated. The same paragraph lists "768-dim float32 =
  3.07 GB" and "int8 = 0.77 GB", so the 0.13 GB figure is silently
  mixing precisions. Fix: state precision per number
  (128-dim f32 ≈ 0.51 GB; 128-dim int8 ≈ 0.13 GB).
2. **Position paper D15 vs its own source.** "~3B, 4K context; no
   cloud" is stale against 10-landscape's own notes (macOS 27 PCC
   routing, generalized `LanguageModel`, TN3193 revised for 26.4). The
   position paper should have said "private by default" — see §2.
3. **Position paper D10 understates the EWS door closing.** "Phased out
   since 2026-10-01" omits the Oct-10 allow-list requirement: from
   2026-10-10, apps not in a tenant's `EWSAllowedAppIDs` lose EWS *even
   where the tenant re-enables it* — i.e. there is no remaining "EWS
   bridge" fallback for Exchange Online worth mentioning anywhere, and
   on-prem-only EWS (the paper's #5) is the correct and only scope.
4. **Working notes — Gmail "Send mail as" scope.** The verified claim
   says third-party send-as ends on "web and mobile" January 2027 and
   Workspace send-as is unaffected — which is right but should be
   stated more carefully in 06-account-backends: the gap users hit is
   *consumer* Gmail send-as; Sweetbriar's identities must then send
   through the third-party address's own SMTP, which needs per-identity
   outgoing-server config (already implied by "identities with context
   rules"; make it explicit).
5. **In-world claims I cannot externally verify** (consistent
   internally, flagged so nobody treats my silence as verification):
   UniFFI 0.32.1's exact date and the July-2026 Swift-6 async-trait fix;
   BoltFFI's existence/benchmarks and Crux 0.19's move; Mailspring
   1.26.0's many-folders rewrite; eM Client 11 details; Notion Mail's
   2026-09-22 shutdown; Thunderbird 154's Graph; RFC 9979's contents;
   macOS 27 API details (`PrivateCloudComputeLanguageModel`, Document
   API); Proton's 2026-10-09 terms change; DavMail 7.0.0; Mailspring's
   header-hash collision documentation. All are plausible; all should
   get a re-check tag at build time.
6. **Open items left open in the corpus that the paper quietly relies
   on:** Gmail IMAP QRESYNC (unconfirmed — settle with an authenticated
   probe, already noted); macOS 27 app-data access permissions for
   import (marked verify — affects the import wedge in §6); Swift
   Fluent support (unverified — affects the shared-strings plan);
   Google CASA applicability to a local-only client (needs the written
   determination, already on the critical path).

No correction overturns a decision; #1 and #2 are the only ones I'd call
real errors, and they're small.

---

## 4. Missing foundational decisions

In rough order of how much they hurt if never decided:

1. **Product licence** (D13's deferral). Before first public code.
   It is upstream of D7/D11/D10 dependency choices and of the
   neverest-collaboration question. Decide: OSI vs source-available vs
   proprietary; who can build/distribute; CLA or none.
2. **Business model and licence-expiry behavior.** The landscape doc
   floats $50–80/yr *and* notes subscription fatigue — unresolved.
   Whatever is chosen, "view, export and draft recovery keep working
   after expiry" must be designed into the licensing path (it's a
   data-safety promise, not a pricing detail). Also decide: trial shape,
   perpetual-vs-subscription, upgrade pricing — each has different
   build consequences (license server, grace states, offline
   activation).
3. **Where addresses live (contacts source).** Nicknames, Quick
   Recipients, identity suggestions, "sender ∈ group" rule criteria,
   avatars — D14 assumes them. Options: own store synced via CardDAV,
   macOS Contacts.framework bridge, both. This is a schema decision,
   not a feature decision.
4. **Sync of Sweetbriar-owned metadata across a user's machines.** The
   target user has a Mac and later a Linux box. Rules, identities,
   signatures, settings, notes, saved views, snooze/follow-up state are
   all currently "authoritative, local SQLite" — i.e. single-machine.
   Options: project to provider primitives where possible (keywords,
   RFC 5464 METADATA, Graph categories — partial coverage only), a
   CloudKit/iCloud path on Mac (doesn't reach Linux), a small own
   sync service (privacy/cost/ToS implications), or an explicit,
   productized non-goal ("per-machine rules, exportable"). A declared
   non-goal is acceptable for v1 *if the schema keeps these objects
   portable* — but the decision belongs on the agenda because it shapes
   D1's user-data tables.
5. **Encryption at rest.** Currently "decide per product" buried in
   04-data-safety. The store holds Proton plaintext, OAuth-adjacent
   metadata and the index; "FileVault handles it" is a decision too —
   make it explicitly, because the alternative (SQLCipher + encrypted
   blob codec + encrypted index) must be designed into sb-store's VFS
   and the blob abstraction now to avoid a migration later.
6. **Telemetry, crash reporting, diagnostics.** A mixed Rust/Swift
   crash pipeline, symbol handling (dSYMs + Rust symbols), opt-in
   protocol-trace capture with redaction — mail logs carry secrets;
   the data-safety doc scans for token patterns in CI but no product
   decision exists for what leaves the machine. For a privacy-
   positioned product this needs an answer before the first beta, and
   it interacts with Google's CASA determination.
7. **Onboarding and account setup as a designed flow.** 07-standards
   has the detection chain, but the *decision* — email-first autodetect
   vs provider buttons, where import sits in first-run, how errors
   teach ("IMAP disabled by your admin — here's what to send them") —
   is the first-run experience and the difference between a client and
   a sysadmin tool. Includes: import-first as a wedge (see §6).
8. **Update and migration rollback policy.** Sparkle is decided; the
   data-safety doc has verified migrations; what's missing is the
   *downgrade* answer — forward-only schema with blocked-downgrade UX
   vs downgrade-safe windows. An auto-update that migrates the store
   and then gets reverted is a data-loss vector squarely in this
   product's threat model.
9. **Junk/spam strategy.** Server-delegated only vs a
   SpamSieve-compatible interface (Mac power users will demand it —
   Mailsmith shipped it) vs on-device classification. Currently absent
   everywhere including D14.
10. **Team size and runway assumptions.** The single missing input
    under half the cost arguments in the corpus ("2–3× a naive engine",
    "months of simulation work", "four drivers = four obligations").
    At 1–2 people several recommendations need re-scoping; at 4–6 the
    parallel-driver plan in §2-D10 is free. State it.

Also worth one line each: accessibility gates for the custom list (in
the prototype, already), localization tech (Fluent vs generated
xcstrings — unverified assumption), and a Windows posture statement
("never" is fine; silence isn't).

---

## 5. Risk ranking — what hurts most and is hardest to reverse

1. **D1 (store shape).** Migrating a million-message store in place,
   under a "never lose data" brand, is the worst retrofit on the list.
   *De-risk:* land the blob abstraction with the publication protocol
   (tmp→fsync→rename→dirfsync→DB-ref) under fault injection *before any
   user data exists*; keep the BLOB-fallback proven behind the same
   abstraction; ship export before sync mutations.
2. **D3 (sync correctness).** One famous data-loss incident ends the
   product; correctness can't be retrofitted. *De-risk:* the 04 build
   order (no writes → reversible mutations → destructive ops) is itself
   the mitigation — hold the line; poison-op isolation + circuit
   breaker + protocol-trace audits are cheap and non-negotiable; the
   expensive sim machinery is *not* on this critical path (contra D3's
   framing).
3. **D10's approvals, not D10's code.** Entra publisher verification,
   Google restricted-scope verification + written CASA determination,
   Yahoo/AOL approval, and *tenant-level admin consent* are the real
   critical path — months of wall-clock that no engineering order
   changes. *De-risk:* file everything in week 1 (the paper says this —
   make it a tracked workstream with an owner); get 3–5 real M365
   tenants through the consent kit via design partners before
   committing to "Exchange works" as a launch claim.
4. **D2 (data model).** Every backend and screen depends on it; the
   per-membership-flags mistake would surface as exactly the
   label/folder corruption the product promises to prevent. *De-risk:*
   write the provider-semantics conformance suite *first* — one logical
   operation applied across Dovecot/Gmail/Bridge/fake-Graph asserting
   end-state invariants — before any UI consumes the model.
5. **D8 (list stack).** Wrong call = rewriting the main window; caught
   late it's a year. *De-risk:* the two-week fair prototype in weeks
   1–2 (not "before product work" vaguely — it gates all UI), behind a
   cell-renderer seam so a later switch is bounded.

Runner-ups: D6 (lease discipline must be enforced, not designed), D16
(normalization fidelity is a trust surface — a mangled receipt
undermines "trustworthy"), and D13-licence (a wrong public licence
announced early can't be unpublished).

---

## 6. First vertical slice (3–6 months)

**My ordering:**

- **Weeks 0–4 — foundations and the two gates.** Repo + layering CI +
  the store skeleton behind the blob abstraction; the **seeded fake
  IMAP server with fault knobs** (a dev tool, not extra harness); the
  **AppKit-vs-SwiftUI list prototype** on the real query API (D8 gate —
  everything downstream waits on it); **file all provider approvals**.
- **Months 1–3 — the slice that demos.** One IMAP account (Dovecot in
  CI; Gmail as the real-world stretch) → immutable ingest → mailbox
  list → locked-down reader (sanitized WKWebView + TextKit plain path —
  the hostile-HTML prototype is a named gate, do it here) → FTS5 search
  → local drafts → SMTP send through the real outbox state machine.
  **Plus import** (Apple Mail `.emlx`/mbox with the reconciliation
  report): a design partner gets their archive searchable in week one
  without trusting sync at all. Import is an acquisition feature, not
  just a safety feature — this is my biggest ordering change vs the
  corpus's implied plan. Keyboard filing joins the slice if cheap —
  it's product identity, not garnish.
- **Months 3–6 — reversible world.** Flags/moves with verification +
  tombstones + circuit breaker; rules MVP (arrival phase only); second
  source type begins — **Graph driver in parallel once the trait is
  stable** (per §2-D10), and iCloud/Yahoo/Fastmail presets to harden
  the IMAP path. Real-server CI starts with Dovecot/Cyrus/Stalwart
  containers; hosted accounts (Gmail, M365) added once the matrix has
  an owner.

**Explicitly waits:** deterministic *schedule* simulation (seams and
seeded-faults don't wait); JMAP; the Gmail-API transport (the
*interface* doesn't wait — see D10); Tantivy unless FTS5 fails its
gate; the Linux UI; the agent/daemon (the lease doesn't wait); semantic
search; AI surfaces; rich-text composer; ManageSieve editor; Proton
beyond Bridge detection.

**Where I challenge Claude's implied ordering:** (i) import/export
moves from "safety feature" to "wedge"; (ii) Graph is parallelized, not
queued behind a finished IMAP; (iii) the sim harness is right-sized —
the corpus's own precedent (Nucleus, TigerBeetle) is a *destination*,
not a day-one deliverable; (iv) the reader prototype is pulled forward
because hostile-HTML correctness is harder to retrofit than a composer.

---

## 7. Where Claude and I agree — and could both be wrong

1. **Classic revival as identity.** We both buy the D14 thesis; its
   counter ("nostalgia isn't demand") may be more right than either of
   us admits. The paying wedge may simply be "fast, trustworthy, M365
   works," with Mailsmith features as retention rather than
   acquisition. The data-model commitments (notes, overlays, session
   state) are being baked in before a single design partner has
   answered which revivals matter.
2. **"Never lose data" as the brand.** We both endorse it; it's also an
   asymmetric-liability promise — competitors' bugs are forgiven,
   Sweetbriar's first lost message is a brand-killer precisely because
   of the promise. Worth considering whether the public claim is
   "provably safer" rather than "never".
3. **The shared-Rust-core premise itself.** The corpus's own precedents
   (Dropbox abandoning shared C++, Element's parity overruns,
   1Password's SwiftUI retreat, Thunderbird's three stacks) are the
   strongest evidence *against* the premise, and neither of us priced
   the alternative: Swift-only for the Mac v1, extract the shared core
   when Linux is real. The premise is probably right for this product's
   never-lose/search/rules ambitions — the logic core is the product —
   but it deserves to be *argued*, not inherited.
4. **Pull-based windowed projections + UniFFI at real chattiness.** We
   both accept ~0.6–2 µs per call and pull-not-push on paper; neither
   has measured a 120 Hz diff stream through the real DTOs. The
   architecture doc lists it as an open question; it should be a
   prototype.
5. **Proton via Bridge is "support".** Bridge is paid-tier-only,
   another daemon, and lands plaintext in our cache — for real users
   "works with Proton" may be honorary. Both papers treat it as checked
   coverage; the honest capability matrix should say "via Bridge,
   paid plans only".
6. **Mobile-shaped decisions.** Everything assumes desktop; mail
   triage for the target segment happens on phones. Neither of us
   asked whether D1/D2/D6 choices preclude a UniFFI-driven mobile shell
   later (they mostly don't — but the licensed/notification/relay
   decisions could).
7. **Team size.** Neither of us states it. See §4-10 — the whole cost
   structure floats on it.

---

## 8. The two new proposals

### D16 — Markdown as the primary mail format

**My independent view: split it.** The proposal fuses two features with
different risk profiles.

**Writing in Markdown: yes, first-class, low risk.** MailMate has a
decade of proof; the corpus already plans it. The details that matter:
send `multipart/alternative` with `text/plain` (rendered, format=flowed
behind the preference already decided) + `text/html` generated through
an email-safe pipeline (comrak/pulldown-cmark → inlined CSS — `juice`
or equivalent — with a fixed, tested template). Do **not** send a
`text/markdown` (RFC 7763/7764) third part by default: alternative-part
selection differs across clients, and leaking source to a
least-common-denominator reader is worse than shipping clean HTML;
offer it as an opt-in for Markdown-aware correspondents. Keep the
source in the draft record. Quoting HTML replies: blockquote-ify when
the source is clean, else fall back to quoting generated HTML — never
mangle the original to fit Markdown's limits.

**Reading: adaptive normalization — not "normalize everything."**
Option (b) from the brief, sharpened:

- The pipeline is HTML/plain → sanitized DOM → **canonical document
  AST** → renderer. The intermediate is *not* Markdown — Markdown can't
  carry data tables, footnotes, image placement or quote attribution
  cleanly. Think Pandoc's AST: a document model with Markdown as one
  *export* of it, not its schema.
- A classifier picks the default render per message: texty mail
  (plain, simple multipart, quoted threads, lists, personal HTML) →
  normalized native render through TextKit 2 — this is where the
  privacy/speed/memory win actually lives (no WebContent process for
  most mail, consistent typography/dark mode/a11y, better quoting and
  search text). Layout mail (newsletters, receipts, marketing,
  invites, anything scoring low confidence) → the locked-down original
  HTML path that already exists.
- "Show original" is one keystroke, always; per-sender memory
  ("always original for receipts from X"); the normalized form is
  *derived data* — stored as a rebuildable projection versioned with
  the extractor, never written back into authority. Search/snippets
  index the normalized text.

**Why not option (a) (normalize everything):** the mail that matters
most — order receipts, itineraries, invoices, calendar invites — is
exactly where normalization loses information, and "which version is
true?" becomes a permanent UX tax. MailMate renders everything through
its *own* HTML — that's the safer prior art: consistent rendering
without semantic conversion. Net-net normalization should win on
fidelity only where fidelity is cheap.

**The honest risks:** phishing cuts both ways (normalization strips
deceptive styling — likely net-positive for safety — but also strips
legitimate brand cues users rely on); legal/compliance users need "what
was actually sent" to be trivially reachable (it is — immutable
originals — but the toggle must be sticky per sender); and the
classifier needs a real corpus and a fidelity metric, or the feature
ships as the thing users turn off once and never revisit.

**Recommendation:** Markdown compose + canonical-AST rendering
pipeline + adaptive default with per-sender override. This fits both
audiences — MailMate-grade text handling for the classics diaspora,
original HTML for mainstream mail — and it's a genuine differentiator
rather than a gimmick, *if* the classifier and the original-toggle
are first-class.

### D17 — "A real database (Postgres) for max safety and speed?"

**No. Bundled SQLite is the right answer, and the reasoning matters
more than the answer.**

- **Safety isn't in the engine.** Sweetbriar's guarantees come from
  immutable originals, the intent journal, ordered fsyncs, verified
  backups and the invariant test harness — none of which change under
  Postgres. The two engines' recent histories are symmetric anyway:
  Postgres had fsyncgate (2018, fsync errors not crashing the server,
  fixed pg11+); SQLite had the WAL-reset bug (16 years latent, found by
  Tailscale, fixed 3.51.3). The lesson isn't "pick the serious engine",
  it's "prove the invariants under faults" — which the plan already
  does.
- **Operability is where Postgres loses — decisively.** A consumer
  desktop app would ship a server process: startup ordering before
  "useful mailbox" (the 1.0 s budget dies at postmaster start), a
  socket/port lifecycle, crash/recovery ordering vs the app process,
  and — the killer — **major-version upgrades on end-user machines**:
  pg_upgrade/dump-reload on every yearly major, plus the ICU/glibc
  collation problem that silently corrupted indexes for real users in
  2023–2025 and still generates warnings. App Sandbox/Flatpak friction,
  backup requiring pg_basebackup-style machinery vs SQLite's online
  backup API and `VACUUM INTO`. **Akonadi is the fifteen-year
  cautionary tale**: a visible PIM daemon on a server database became
  KDE's most-mocked component precisely through upgrade/reliability
  pain — and the corpus's own linux doc cites it.
- **Speed: the DB isn't the bottleneck.** This workload is one writer,
  short snapshots, keyset-paged covering-index reads — SQLite's home
  turf, embedded with zero IPC. Postgres buys concurrent-writer MVCC
  for a design that has one writer by invariant. The real hot paths are
  MIME, indexing and the network.
- **The alternatives don't change it.** libSQL/Turso's Rust rewrite
  (concurrent writers) is the only interesting one — young durability
  record; LMDB/redb/fjall are KV, not SQL; DuckDB is OLAP; embedded-
  Postgres crates (PGlite is WASM, pg-embed downloads binaries) inherit
  every operability problem above.
- **When Postgres becomes right:** a hosted multi-device sync/index
  *service*, shared/team mailboxes, or analytics — all server-side
  futures where it would run as a service, not embedded. If one of
  those lands, the decision is about the service's DB, not the client's
  store.

Keep: bundled SQLite ≥3.51.3, the documented pragma set, the
`sqlite_version()` check in every host/extension, and a written
re-visit trigger (measured writer contention; a hosted service; a
SQLite-specific defect) so the question doesn't get relitigated by
vibes.

---

*Reviewer note on process: position paper read in full; all curated
documents, the decision agenda and working notes read; raw reports
consulted where the synthesis cited them; external verification limited
to load-bearing facts (EWS, SQLite, Gmail quotas, Graph limits, CVEs).
In-world claims beyond real-world reach are flagged, not trusted.*
