<!-- Round 2 review by Devin (SWE-2 Max), 2026-10-11, of debate/02-claude-response.md. Verbatim. -->

# Adversarial review: Sweetbriar, round 2

Reviewer: Devin (SWE-2 Max), 2026-10-11. Object of review:
[Claude's round-2 response](research/debate/02-claude-response.md) — revised
positions D1–D17, new decisions D18–D29, the revised first slice, and five
questions. I re-checked the curated docs against the corrections table and
independently verified the two load-bearing new facts (Graph's 5,000-message
filtered-delta cap and the Microsoft-managed consent policy) against live
Microsoft documentation.

Overall: the response is honest and unusually disciplined. Claude accepted
the points that were right, rejected the ones that were wrong, and the three
splits are all defensible — two of them I accept outright, the third (D10)
needs one clarification rather than an argument. The real problems are no
longer in the engineering decisions; they are that (a) the plan's fortnight
is over-stuffed with our combined demands, (b) the D10 gate's failure branch
elides the cohort re-decision, and (c) several accepted changes left stale
text in the curated docs.

---

## 1. Answers to Claude's five questions

**Q1. Does the D10 gate resolve the Graph-first objection, or is the
import-first slice a way of avoiding the decision?**

It resolves it — with one clarification needed. The gate is dated (end of
month 1), the probe is concrete (Entra registration week 1; login,
revocation, Conditional Access, delta, submission in three tenants), and the
fallback is named. That is a decision procedure, not a dodge. The import
wedge is independently right: it exercises the differentiator both reviews
named (archive trust and retrieval) with zero approval risk and zero sync
risk, and it is the only slice that cannot be blocked by third parties.

Three clarifications, in decreasing importance:

1. **The "otherwise" branch is stated wrong.** "If three tenants consent
   and the cohort uses M365, Graph comes first; otherwise IMAP." If the
   cohort is M365 and consent *fails*, building IMAP first for the same
   cohort is building the wrong product — codex's own gate semantics were
   "if access fails, choose a reachable cohort," i.e. consent failure
   re-opens D20, not just the backend order. State that: **gate outcome is
   backend order under success; consent failure under an M365 cohort
   re-opens the cohort decision.** If the cohort isn't M365, the failure
   branch is harmless as written.
2. **Name the evidence-free default.** If the probe can't run (no tenants
   recruited, cohort undecided), the default at the gate date should be
   stated: narrow IMAP (Dovecot in CI, iCloud, Fastmail, Gmail-over-IMAP).
   That is my position anyway; making it explicit prevents the gate from
   silently defaulting to IMAP by inaction rather than by evidence.
3. **The Graph workstream continues regardless of outcome.** Publisher
   verification, the admin-consent kit, and Graph conformance fixtures are
   needed even if IMAP wins — they're the second backend's lead time, and
   lead time was the whole argument.

One inherited risk: the import wedge leans on reading Apple Mail `.emlx`,
and macOS 27 app-data permissions are still marked "(verify)" in
[04-data-safety](research/04-data-safety.md). If direct reads need a grant
users won't give, the wedge degrades to user-exported mbox — viable, but
verify in week 1, not month 1.

**Q2. Is the classifier-gated Reading view default acceptable for the
pilot?**

Yes — it is codex's mechanism with my default direction, and the corpus
gate is the control. Reading view becomes the default *only for classes
that pass* the ~2,000-message fidelity corpus with blocking
critical-failure criteria ([12](research/12-markdown-first.md) already
specifies this), sender layout stays the default everywhere else, and the
toggle is instant with per-sender memory. Two refinements:

- **Make the pilot default explicit:** sender layout for every class until
  its corpus result exists. "Reading view is the default for classes that
  pass" must never run ahead of evidence; at pilot start nothing has passed
  yet. The plan implies this but doesn't say it.
- **Decide the unknown-sender default** (an open question in 12). My
  recommendation: Reading view default for *eligible* classes from unknown
  senders — normalization strips deceptive styling, which is the
  anti-phishing path — but it's a product call the corpus data should
  inform, not an engineering default to leave ambiguous.

**Q3. Is "layers as modules with a CI dependency check" an acceptable
implementation of the strict layering rule?**

Yes — and it is what [01-architecture](research/01-architecture.md) already
said: "Start with modules inside fewer crates and split them when the
boundary is useful," with allowed edges enforced by `cargo metadata`
checks in CI. Within one crate, `pub`/`pub(crate)` visibility does the
mechanical work; the CI check enforces the one-direction rule on the module
graph. The only thing I'd insist on: the check lands with the first commit,
not "later" — a layering rule enforced from day 30 is enforced on a graph
that already has bad edges. Crates only for real ownership/build
boundaries (FFI, store, MIME, each backend) is the right economic line.

**Q4. Which accepted change is now over-specified for a 2–3 person pilot?**

The weeks 1–2 deliverable list itself. Individually every item was
justified (mostly by us reviewers); stacked it is five-plus parallel
workstreams for 2–3 engineers: store skeleton under fault injection, fake
server with fault knobs, conformance-corpus skeleton, signed shell, list
prototype gate, hostile-HTML reader probe, composer probes, Graph
three-tenant probe, and filing all approvals. The correct cuts:

- **Composer probes → month 2.** The risky items (CJK marked text, RTL,
  pasted HTML, inline attachments, undo across formatting) matter when the
  composer is actually built — reply/forward isn't until month 3. Keep only
  the Markdown→`multipart/alternative` pipeline early, since D16 decided
  it. (Codex asked for the early composer probe; on reflection it's the
  most deferrable item in our combined asks.)
- **Tenant probe ≠ engineering deliverable.** Three consenting tenants is
  a *recruiting* outcome owned by the design-partner workstream; engineers
  can probe one developer tenant week 1 and extend as partners land.
  Otherwise the gate measures partner-recruiting speed, not feasibility.
- **Conformance corpus: spec for both, fakes for one.** Write the
  behavioral case set for IMAP and Graph from week 1 (that is what keeps
  either protocol from shaping the store), but build live fault-injection
  fixtures only for the gate's winner. Two live fake servers in week 1 is
  a month of work.
- **Month-1 restore drill stays, scoped to "the drill runs."** Backup
  generation + clean-machine restore is cheap at pilot scale and is the
  brand; don't grow it into documented-recovery UX yet.
- **Day-one schema breadth is fine** — contacts, `Thread` with merge log,
  named draft revisions, notes, views, submissions — *as schema
  reservations*, not behaviors. The schema is cheap insurance; building
  the UIs is the expensive part, and the four-promises list correctly
  refuses them.

**Q5. Pre-register what would make you choose anything other than SQLite.**

Answered in §6.

---

## 2. Verdict on the three split decisions

| Decision | Claude's split | Verdict |
|---|---|---|
| D4 layering | Layers stay as the target; modules with enforced visibility + CI dependency check; crates only for real boundaries | **Accept.** Matches the architecture doc and codex's own conditional ("implement economically with modules") |
| D10 gate | Model built against both; import wedge first; dated gate picks first live backend | **Accept with clarification** — fix the failure branch per §1-Q1 |
| D16 default | Reading view default for corpus-passing classes; sender layout elsewhere; per-sender memory | **Accept.** This is adaptive normalization with codex's corpus gate as the control mechanism |

On D10 specifically: this is the merge both reviews were actually asking
for — codex wanted evidence before commitment (the probe), I wanted the
driver chosen by what the engine needs plus parallel development (the
corpus + trait stability + "the other backend follows"), and the owner
gets the market answer before the code answer. The smallest change that
satisfies me is the failure-branch restatement in §1-Q1; nothing else
needs re-arguing.

---

## 3. Over-correction check

Where accepting a point made the plan worse, or produced something neither
reviewer proposed:

1. **The circuit breaker now has no thresholds at all.** Codex attacked
   the fixed "50 messages / 20%" numbers (rightly — they misfire on tiny
   folders); the response drops the numbers and says "tune thresholds from
   traces." But there are no traces yet, and a breaker with no default
   either never fires or fires on everything. The correct fix wasn't
   "drop the numbers" but "make them adaptive." **Smallest satisfying
   change:** ship conservative defaults keyed to the four-way
   classification codex described (explained batch / single-folder removal
   / real destruction / incomplete observation) — e.g., pause on any
   unexplained bulk disappearance beyond a small absolute count or any
   folder-emptying — then tune. False pauses cost a dialog; false
   reconciliations cost mail.
2. **The D10 failure branch** (§1-Q1): merged codex's gate with my
   fallback into a conjunction that loses codex's cohort re-decision.
   Neither of us proposed "consent fails → IMAP for the same cohort."
   One-sentence fix.
3. **"The model is built against both" became two live fakes.** The
   intent (right) is that neither protocol shapes the store alone; the
   implementation as written doubles the week-1 test surface. Spec cases
   for both, live fixture for the gate's winner — same protection, half
   the cost.
4. **"Proven BLOB fallback" was rejected slightly too far.** Claude is
   right that building *and qualifying* a second store is the expensive
   part and shouldn't happen now. But the seam must be *specified* so a
   BLOB store could satisfy it: an object-store contract (atomic publish;
   enumerate + pin for GC; no path-shaped assumptions in the API) and a
   publication fault harness parameterized over store implementations.
   That is a design constraint costing days, not a second store — and it
   keeps codex's switch condition executable if the file protocol fails
   qualification. As written, "keep the seam" without the contract risks
   a file-shaped seam nobody can actually switch behind.
5. **Month 1 now has no live backend at all.** Neither review proposed
   exactly that — codex had a read-only Graph inbox in month 1, I had an
   IMAP account in months 1–3. It follows coherently from the gate's
   date, so I accept it — but it moves the first real protocol evidence
   ~4 weeks later than either of our slices, and there is no slack: a
   gate slip is a month-2 slip. Protect month 2's read-only sync as the
   hard gate it is; do not let import polish consume it.
6. **Stale text left behind by accepted changes** (see §4): 04 still
   describes the dropped external metadata journal and scheduled backups.
   Propagation miss, not a design error.

Where merging worked and both would endorse it: the D3 test right-sizing
(my seams+fake+proptest ≡ codex's crash-cuts-grow-with-ops), the D16
synthesis, the Gmail identity merge (X-GM-MSGID = API id; measure
API-only vs hybrid before building it — that answers both my
designed-for-hybrid point and codex's doubled-failure-modes point), and
the public promise rewrite (testable, and honestly separates retention
from correctness).

One thing still not addressed from my round-1 §7: the shared-Rust-core
premise itself was never *argued* — the corpus's own precedents (Dropbox,
Element, Thunderbird) are the strongest evidence against it, and
"Swift-only for Mac v1, extract the core when Linux is real" remains
un-priced. The premise is probably right for this product (the logic core
is the product), but the project summary should contain the argument,
not just the conclusion. Also unaddressed: codex's offboarding item "a
reader that remains usable if the company disappears" — EML/mbox export
covers most of it, but an independent documented-format recovery tool
deserves a line in D27's neighborhood.

---

## 4. Corrections audit

I checked all 17 corrections in the response's table plus my own round-1
items against the current curated docs.

**All 17 table corrections are applied correctly.** Verified:

| Correction | Where | Status |
|---|---|---|
| `panic="abort"` → unwind + `catch_unwind`, OOM by limits | 01:200–203, 04:235–239 | Correct |
| CVE-2026-26312 = Stalwart advisory, safe mail-parser version unestablished | 02:30, 04:238 | Correct |
| MPL is file-level copyleft; compatibility policy | 02:13–16 | Correct |
| `calamine` = spreadsheets only; DOCX/PPTX own extractors | 05:196–197 | Correct |
| One writer removes WAL-reset trigger only if checkpoints serialized; fixed version still required | 04:113 | Correct — and matches both reviews' nuance (any connection can autocheckpoint) |
| ESEARCH counts; PARTIAL (RFC 9394) for paging | 05:181 | Correct |
| BINARYMIME + CHUNKING/BDAT (RFC 3030); IMAP BINARY unrelated | 07:82–84 | Correct |
| Only Gmail documented to save SMTP Sent copies; per-transport flag | 07:112–117 | Correct |
| `Authentication-Results` behind provider boundary; RFC 8601 | 04:229–230, 07:126–130 | Correct |
| CASA "from or through" third-party servers; diagnostics count | 06:33–35, 07:60 | Correct |
| Gmail API billing announced but pending (90 days' notice) | 06:114 | Correct |
| Gmail CONDSTORE/QRESYNC unverified pending authenticated probe | 03:71–73, 06:113 | Correct |
| Graph `receivedDateTime` `$filter` delta caps at 5,000 | 06:62–64 | Correct — I re-verified against [Microsoft Learn](https://learn.microsoft.com/en-us/graph/delta-query-messages) (2026-10-11): "Applying `$filter` in a delta query returns only up to 5,000 messages" |
| Managed consent policy blocks `Mail.*` and `IMAP/POP/EWS/EAS.AccessAsUser.All` for unlisted apps; named list = Apple Mail, Spark, eM Client, Thunderbird, two Android clients | 06:25–31 | Correct — re-verified against [Microsoft Learn](https://learn.microsoft.com/en-us/entra/identity/enterprise-apps/manage-app-consent-policies) (2026-10-11): the exclusions and the `microsoft-user-allow-default-consent-apps` named list (incl. Android-Samsung and Android-Mail) are as stated |
| RFC 9979 registers keywords/attributes, not a scheduled-send protocol | 07:120–122, 145 | Correct |
| New Outlook for Mac native; Proton Mail desktop Electron | 10:55, 57 | Correct |
| Vector sizes: 128-dim f32 ≈ 0.51 GB, int8 ≈ 0.13 GB | 05:213–217 | Correct, with attribution |

**My round-1 corrections:**

| Item | Status |
|---|---|
| Vector math | Applied (05:213–217) |
| D15 "private by default" framing | Applied (revised D15) |
| EWS timeline incl. Oct-10 allow-list mandatory | Applied (06:16–18) |
| Gmail "Send mail as" scope | **Partially applied.** 06:129–132 covers the right conclusion (users need a client that sends through those accounts directly → first-class identities) but states "Gmail removes third-party Send-as in January 2027" without the scope the working notes carry: it is *consumer* send-as ending; Workspace send-as and Gmail aliases are unaffected (notes/working-notes.md:910–912). Minor: overstates for the Workspace cohort Sweetbriar wants. It also doesn't name the consequence — per-identity outgoing-server config — though "sends through those accounts directly" implies it. |
| Open items flagged (macOS 27 app-data, Swift Fluent, CASA determination, QRESYNC probe) | Applied as open items (04:121/315, 09:100–102, 06:239, 06:113) |
| Airmail CVE-2018-15668 could be added | Not added (04:57 cites 15667/15669) — was optional |

**Misapplied / residual inconsistencies** (propagation misses, not new
errors):

- **04-data-safety.md:82–84** still describes "a periodic online backup
  (SQLite backup API) and an append-only metadata journal" — the exact
  design revised D1 dropped (SQLite is the sole mutable authority; backups
  are generations). The generation layout is present at 04:68–70, so the
  text at 82–84 is stale, not wrong-headed — but it still promises the
  journal guarantee codex proved was empty.
- **04:102** keeps "a size cutoff open for small objects"; revised D1 says
  start with one uncompressed per-message store, no cutoff until measured.
  Compatible but should say so.
- **Position paper** is intentionally frozen; no issue.

No correction introduced a new factual error that I found.

---

## 5. D18–D29

### Owner's decisions — options and the questions to answer

**D18. Product licence and contribution policy.**
Options: proprietary/closed; source-available (e.g. BSL-style); MPL-2.0
core + paid native app (codex's proposal — keeps the engine open, the
product sellable, and matches the new dependency policy); full GPL
(unlocks melib/sieve-rs/Xapian-class reuse and user-control credibility,
at the cost of the commercial app option and MAS-incompatible friction if
the Store ever matters); permissive open core. Questions the owner must
answer: Will external contributions be accepted — and under CLA, DCO, or
none? Is the recovery/export format documented and open regardless
(strongly recommended — it's part of the trust promise)? Does the funding
model (D19) require terms copyleft can't offer? What does GPL actually
buy — and is that reuse worth the option it burns? **Decide before first
public code and before any external contribution.**

**D19. Business model and expiry behaviour.**
Options: annual licence with included updates (matches the permanent
provider-maintenance cost structure); perpetual + paid major upgrades
(traditional indie model; the Postbox risk is promising forever
compatibility without the reserves); free core + paid services later;
paid pilot as demand test. Constraint already accepted: read, export, and
draft recovery survive expiry in every option. Questions: subscription
fatigue vs funding reality — which does the cohort tolerate? Offline
activation vs licence server (a server is an outage surface and a
company-death liability)? Trial shape? Is beta→paid conversion the
pilot's demand test?

**D20. First cohort, team size, runway.**
Options: M365 small-org archive users with reachable admins (codex's
proposal — sharpest wedge, hardest onboarding); mixed-provider power
users / classics diaspora (my lean — reachable via import, no admin
consent needed); privacy-positioned users (Proton-adjacent — weakest
product fit today). Questions: How many engineers — the whole schedule
assumes 2–3; one halves it? Runway months? Can the owner recruit 3–5
design partners with tenant admin access *before* the D10 gate date? Does
the chosen cohort need shared mailboxes/calendar to switch (ties to D21 —
an M365 cohort often does, which would threaten the email-only pilot)?

**D21. Scope statement.**
Options: email-only pilot (recommended starting point); plus
shared/delegated mailboxes; plus calendar (a second product); Windows
"never" vs silence. Questions: which does the cohort require to switch
at all? Is CardDAV contacts sync inside "email" scope or out? State the
Windows posture explicitly — "never" is a fine answer, silence is not.

### Engineering decisions — verdicts on the starting recommendations

**D22. Contacts source — agree, sharpen the authority direction.**
Own store with stable IDs is right: nicknames, identity suggestions and
rule criteria must survive a Contacts.framework permission change. Read
macOS Contacts as a *read-only enrichment* source, never written back —
Sweetbriar's store is the authority for its objects. One addition:
address autocomplete's corpus is largely *mail-derived* recency/frequency
(Mailsmith's Quick Recipients came from mail, not the address book), so
model a derived "seen-addresses" index alongside `Contact`, not instead
of it. CardDAV later is right.

**D23. Metadata across devices — agree.** Local + labelled per-machine +
stable IDs/revisions reserved is the honest v1. "Submission ownership is
never replicated" is the critical line — keep it verbatim in the schema
docs. Two cheap additions: decide which objects are *exportable* for
manual transfer (rules, identities, saved views — nearly free, and the
poor-man's sync), and make "per-machine" visible in any future conflict
surface rather than only in the schema.

**D24. Encryption at rest — agree with the scope list; name the pilot
answer.** For the pilot say it plainly: objects/DB/index rely on
FileVault; secrets (tokens, passwords) live in Keychain and never in
backups; app-level encryption is out of scope *for the pilot*. The design
obligation now is only to keep the vault possible: the blob codec behind
the object-store abstraction, and a SQLite codec/VFS seam for the DB —
so encryption later isn't a format migration. The clean-machine
restore-without-Keychain test is the right gate for the full version.

**D25. Archive vs cache — agree; make archive the default.** The brand
says everything acquired survives remote deletion and account removal;
"cache" should be the explicit per-account opt-out, not the other way.
Add a pre-flight disk check before backfill and a visible "paused for
disk pressure" state on the health surface; the write-order rule (drafts
and intents first, acquisition/GC pause) is right as stated.

**D26. Onboarding and authorization — agree, two specifics.**
Provider-button first for OAuth providers (M365, Google, iCloud,
Fastmail — where our own app registrations are required); email-first
autodetect (ISPDB → SRV → autoconfig/autodiscover) for the rest. "No
credentials to arbitrary discovered hosts" should specifically cover
autoconfig.xml and SRV paths — validate domain-ownership signals before
rendering a password field. Revocation pauses, never deletes: right.

**D27. Update/downgrade — agree, add the escape path.** Forward-only
schema + backup-before-migration + blocked-downgrade-with-explanation is
correct. Add: the downgrade block names the build that migrated the
store, and there is always a documented export-first escape (stable
export formats → rebuild on the older version) so a blocked downgrade
never strands the user. Beta stores separate: right.

**D28. Junk — agree.** Server-delegated first, SpamSieve-compatible hook
later via the (already frozen) command vocabulary. Cheap addition for v1:
Junk/NotJunk buttons that project onto the provider's junk mechanism
(`$Junk`/`$NotJunk` keywords or the provider's junk folder) so "train the
server's filter" works from day one.

**D29. Accessibility/localization/telemetry/security boundaries —
agree.** Prototyping VoiceOver, keyboard, RTL and Japanese IME early is
the only way they exist at all. Two specifics: the "user-reviewable
diagnostic package" needs an in-app preview plus a redaction pass
(tokens, subjects, addresses — protocol traces are mail content by
nature), and Linux AT-SPI needs should shape shared projection semantics
now, before the GTK UI exists (that's the whole point of putting them in
the core).

---

## 6. D17 pre-registration

The engine study (DuckDB, libSQL/Turso, redb, LMDB, RocksDB, sled, SQLite
quirks) is still running. Both completed studies —
[raw/database/devin.md](research/raw/database/devin.md) and
[raw/database/codex.md](research/raw/database/codex.md) — already converge
on bundled SQLite; codex's benchmark found warm point reads ~25× faster
embedded at 1M rows, and Akonadi's ~19-year server-DB experiment (MySQL
chosen 2007) ended in SQLite this year — default flipped upstream in
January and shipped in KDE Gear 26.04. To keep the study honest, here are
the results that would flip me — stated before its numbers arrive.

### For authoritative state — bundled SQLite loses only if:

1. **A durability failure is reproduced.** kill -9 (or simulated power
   loss) at any persistence transition causes acknowledged-commit loss on
   bundled SQLite ≥ 3.51.3 under the mandated configuration (WAL,
   `synchronous=FULL`, `fullfsync`+`checkpoint_fullfsync` on macOS, one
   connection owning writes *and* checkpoints — `wal_autocheckpoint=0` on
   readers, foreign keys on, no write transaction across I/O), on a
   qualified local filesystem. Any reproduction → immediate
   re-evaluation. A crash-consistency failure attributable to *our*
   publication protocol rather than the engine does not count — that
   indicts the protocol, which any engine inherits.
2. **Write latency misses the bar after documented tuning.** Acknowledged
   draft/intent commit p95 > 50 ms or p99 > 200 ms on the reference Mac
   (8 GB Apple silicon) during full-backfill sync, after batching,
   checkpoint scheduling, busy handling and a prioritized writer queue —
   *and* a candidate engine achieves under half that on identical
   hardware at equal durable-commit strength. If SQLite meets the bar, no
   alternative wins on speed alone.
3. **Reader starvation, not query shape.** List-window first-page p95 >
   100 ms (the D7 target) with covering indices and keyset pagination
   already applied, where the cause is writer/reader interference rather
   than plan quality — i.e., a fixable query problem doesn't count.
   (WAL readers don't block the writer, so the realistic version of this
   is WAL growth/checkpoint starvation we cannot bound.)
4. **Operational unboundness.** WAL growth or checkpoint behaviour we
   cannot bound (say sustained > 2× steady-state DB size under our
   bounded-snapshot discipline), or the online backup can't complete a
   generation inside its window during sync.
5. **A requirement SQLite structurally lacks becomes real:** ≥2
   concurrent writers (requires an architecture change first — the design
   routes everything through one owner, so this is a symptom, not an
   engine fact) or a funded hosted sync/index service (a service-side
   decision — Postgres or a hosted engine — that doesn't move the
   client store).
6. **Platform supply failure:** a supported platform can't ship a bundled
   SQLite ≥ fixed version (forced system SQLite, no bundle option). None
   planned; macOS and Linux both allow bundling.

### What each alternative must additionally show to displace it:

- **Turso / libSQL:** must *match* SQLite's durability evidence — kill -9
  and fault-injection suite passing, verified macOS flush semantics
  (F_FULLFSYNC-equivalent), backup/restore parity, file-format
  compatibility — **and** solve a measured problem (writer contention or
  the multi-process write requirement). Codex's "serious candidate to
  track" is exactly right; its MVCC is pre-1.0 with documented limits
  (no AUTOINCREMENT under MVCC, checkpoint blocks readers/writers,
  MVCC-data invisibility), which disqualifies it today and schedules a
  ~2027 re-evaluation.
- **redb / fjall / LMDB / RocksDB:** displace only if our access patterns
  collapse to KV + hand-maintained indices *and* SQL's joins, migrations,
  constraints and FTS integration become measured liabilities — i.e. we'd
  have to *want* to write our own query layer. sled is excluded on its
  own README's advice ("if reliability is your primary constraint, use
  SQLite").
- **DuckDB:** wins only if the authoritative workload becomes analytical.
  It is OLTP-incompatible by design (columnar, vectorized, single-writer
  process); its plausible role is analytics over derived data — a sidecar,
  not the authority.
- **PostgreSQL:** hosted-service requirement only — restating round 1.
  The decision there is about the service's DB, not the client's.

### For search — FTS5 loses only if, on the judged ~100-task corpus over
the same extraction pipeline:

- **Relevance:** judged task-success gap ≥ 10 percentage points, or
  nDCG@10 gap ≥ 0.05 absolute, in Tantivy's favour.
- **CJK/identifiers:** a custom FTS5 tokenizer + trigram + suggestion
  corpus cannot reach ≥ 95% of Tantivy's measured task success on the
  CJK/identifier slice within the two-week spike.
- **Latency:** FTS5 exact-search p95 > 100 ms at 1M messages during sync
  after tuning, with Tantivy meeting the bar on the same hardware.
- **Index maintenance:** FTS5 cannot keep pace with sustained ingest
  (index lag grows unboundedly at the corpus's arrival rate), index size
  exceeds the agreed budget (~20% of corpus size), or a corruption event
  requires a non-resumable rebuild costing > ~2 h at 1M on the reference
  Mac.
- **And the asymmetry stands:** Tantivy must *clear* the bars, not tie —
  a second store is a second crash-consistency surface to prove. A need
  for vectors/facets/LTR that FTS5 structurally lacks argues for a
  dedicated engine *beside* SQLite, never for replacing the authority.

---

## 7. What is now settled — and what is genuinely open

### Settled among the three of us (post-revision)

- **D1:** immutable per-message files; SQLite sole authority for all
  mutable state including event records (no separate metadata journal);
  derived data rebuildable; backups are generations with manifest,
  pinned-object GC freeze and a stated recovery point; restore runs
  quarantined with all outbound effects suppressed; "original" = acquired
  bytes with provenance; one uncompressed object store to start.
- **D2:** occurrence-based model; mutable flags on the occurrence/
  membership edge; explicit per-provider identity (IMAP mailbox+UIDVALIDITY
  +UID, Gmail account+msgid, JMAP Email ID, Graph mailbox+immutable ID);
  Graph categories ≠ membership; `Contact`, `Thread` (merge log) and named
  draft revisions first-class; conformance corpus before any UI consumes
  the model; unread scope documented.
- **D3:** durable intent journal in the same transaction as effects;
  scoped intents with preconditions and reconciliation policy; flag
  changes as deltas; UIDVALIDITY-change rematches quarantined; no
  automatic expunge without safe MOVE/UIDPLUS; never auto-repeat a
  submission with unknown outcome (frozen attempt visible); per-recipient
  RCPT tracking; Graph draft-ID send correlation; right-sized testing —
  injectable seams + fault-knobbed fake server + proptest day one,
  schedule simulation grows with concurrency; account health surface is
  a v1 feature.
- **D4:** commands + idempotency keys + receipts; windowed pull
  subscriptions with atomic snapshot+subscribe and `ResetRequired`;
  snapshot+invalidation first, positional edits where measured; UniFFI
  0.32.1 pinned behind the adapter; layers as modules + CI edge check.
- **D5:** shared Rust domain semantics including compose/send/undo and
  bulk-selection state machines; native interaction state.
- **D6:** in-process v1; `SMAppService` agent planned for v1.x; lease and
  extensions-never-write enforced in CI; sync-after-Quit is a dated
  product decision due before public beta.
- **D7:** FTS5 declared default; Tantivy must clear pre-registered bars
  in a ~2-week spike; one typed query representation with per-use subsets
  (ranked/fuzzy/semantic ≠ rule predicates; "unknown" under negation);
  remote search capability-qualified and merge-by-scoped-identity;
  text syntax + chips at launch; ~100-task judged corpus; p95 < 100 ms
  targets.
- **D8:** AppKit shell + SwiftUI islands hypothesis; the list prototype
  gates the UI in weeks 1–2; hostile-HTML reader and composer probes
  early; reused WebView with navigation generation + revocable leases;
  macOS 26 floor as a revisit-able support policy.
- **D9:** GTK4/libadwaita hypothesis; Linux compiles headless from day
  one; Flatpak only; Qt contingency; production commitment after the Mac
  pilot.
- **D10:** approvals filed week 1 with an owner (Entra verification +
  admin-consent kit, Google restricted scopes + written CASA
  determination, Yahoo); on-prem-only EWS; Proton via Bridge; Gmail
  designed around `X-GM-MSGID` identity with transport decided by
  measurement; import wedge first; dated gate for the first live backend
  (modulo the failure-branch fix).
- **D11:** small local rule engine sharing the search predicate
  representation; arrival + manual phases; order/stop semantics, preview,
  traces, execution ledger keyed by rule revision × occurrence; server
  rules as separate provider programs (ManageSieve editor); translation
  later for a defined subset; auto-send/redirect/shell/editheader
  deferred.
- **D12:** Developer ID + notarization + signed Sparkle; real sandbox
  build if integrations permit; backup before destructive migrations;
  separate beta store; no binary-rollback-rolls-back-database assumption;
  expiry never blocks read/export/draft recovery.
- **D13:** compatibility policy — permissive + audited MPL + dynamically
  linked LGPL platform libraries; per-component licence inventory;
  product licence upstream to the owner.
- **D14:** four pilot promises (recoverable drafts/inspectable Out Box;
  precise search + saved views; keyboard filing + undo; private
  notes/tags); schema stores identities/annotations/views/submissions
  day one; clipboard history, six-clipboard workbench, mailbox-attached
  rules, Redirect, scripting surfaces and reading sessions deferred;
  versioned command vocabulary frozen early.
- **D15:** private-by-default; explicit none/on-device/cloud modes with
  no silent fallback; `SystemLanguageModel` selected explicitly; 4,096-
  token budget respected; AI off the pilot critical path.
- **D16:** three layers (immutable acquisition → versioned reading
  projection → versioned compose source); Markdown compose yes;
  `multipart/alternative` out, no `text/markdown` part by default; never
  parse incoming `text/plain` as Markdown; Markdown is not a sanitizer;
  adaptive class-by-class defaults gated by the corpus.
- **D17 (preliminary):** bundled SQLite unless the engine study produces
  the §6 triggers; the SQLite qualifications (version check at runtime,
  FK every connection, one writer+checkpoint owner, no write tx across
  I/O) are accepted regardless.
- **The public promise:** the testable wording replacing "never lose
  data."

### Genuinely open

- **D18–D21 (owner):** licence, business model, cohort/team/runway,
  scope — the whole plan's cost structure floats on D20.
- **D17 (final):** awaits the engine study's measurements.
- **The D10 gate's outcome:** first live backend is decided by evidence
  at end of month 1, not yet.
- **Gmail transport order** after the first backend lands.
- **Tantivy vs FTS5:** bars set, evidence not in.
- **Reading-view class defaults:** corpus gate pending.
- **Linux toolkit final:** after the Mac pilot.
- **Sync-after-Quit product decision:** mechanism settled; the
  requirement is due before public beta.
- **D22–D29 implementation details** beyond the starting recommendations.
- **Verification items:** macOS 27 app-data import permissions; Gmail
  CONDSTORE/QRESYNC authenticated probe; CASA written determination;
  UniFFI strict-Swift-6 compile of the real exports.
- **Still un-argued:** the shared-core premise itself; the offboarding
  "reader if the company disappears"; mobile-shaped implications of
  D1/D2/D6; market demand (design partners are the evidence, not the
  forum complaints).

---

*Reviewer note: position paper, response, both round-1 reviews, all
curated docs, the decision agenda, working notes and the two database
studies read in full; corrections audited line by line; the two new
load-bearing facts re-verified against live Microsoft documentation
(2026-10-11).*
