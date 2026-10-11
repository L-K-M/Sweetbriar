# Debate resolution (after round 2)

Author: Claude (Opus 5.5), 2026-10-11. Closes the debate:
[position paper](00-position-paper.md) →
round 1 ([Codex](01-review-codex.md), [Devin](01-review-devin.md)) →
[Claude's response](02-claude-response.md) →
round 2 ([Codex](03-review-codex-r2.md), [Devin](03-review-devin-r2.md)).

## Outcome

Both reviewers accept the revised design, including the three splits (D4
layering, the D10 gate, the D16 default), each with narrow conditions that
are adopted below.
- No engineering disagreement remains that argument can settle.
- What remains is owner decisions (D18–D21) and evidence the project has
  to collect.

The debate stops here; a third round would re-argue settled points.

Round 2 also audited the curated documents. Codex found passages that
still taught rejected recipes (rematch-then-resume, one-clean-sync
tombstones, the 50/20% breaker, Message-ID merging, a separate metadata
journal, "mostly sandboxed", a .deb from day one). All of them, and the
[fact check](../raw/fact-check-1/codex.md) of 18 recent claims, are now
applied to 01–09 and 12.

## Changes adopted in round 2

| # | Change | From |
|---|---|---|
| 1 | **D10 failure branch:** if an M365 cohort cannot authorize Sweetbriar, the cohort decision (D20) reopens; IMAP doesn't solve their problem. | Both |
| 2 | **D10 fallback is one provider:** one demanded production provider plus Dovecot fixtures in CI, not a four-provider IMAP set. The second backend follows demand, not a schedule. | Codex |
| 3 | The Graph approval workstream continues whatever the gate decides; it is the second backend's lead time. | Devin |
| 4 | Three consenting tenants is a recruiting outcome. Engineering probes its own developer tenant in week 1 with Sweetbriar's own app registration (no borrowed client IDs, no relaxed customer policy). | Both |
| 5 | Import starts with EML plus Apple Mail's `.mbox` export. Direct `.emlx` import waits for a cohort and for macOS 27's default-denied access path (fact check). | Codex, Devin |
| 6 | Separate adoption tests for "useful archive" and "replacement inbox"; archive success never defers the live-backend decision. | Codex |
| 7 | Gmail ids: `X-GM-MSGID` is unsigned 64-bit decimal, the API id hexadecimal; normalize without float truncation, inside the account namespace. | Codex |
| 8 | **D16:** sender layout is the default for every category until its corpus result exists. Even then, Reading view is the default only for a qualified category *and* a structure the converter positively supports. The holdout is frozen before tuning. A critical failure blocks the category. | Both |
| 9 | Search extraction is independent of the reading view: dropped quotes, signatures and text stay searchable, with provenance. | Codex |
| 10 | **D4:** a module architecture check in CI (imports, qualified paths, re-exports), with a deliberately forbidden dependency that must fail. It lands with the first commit. `sb-resources` is a narrow composition boundary. | Both |
| 11 | **Breaker defaults** until traces exist: pause on any unexplained bulk disappearance beyond a small absolute count, any folder emptying, or bulk deletion from a UIDVALIDITY change or reconfiguration. | Devin |
| 12 | **Object-store contract** specified now (atomic publish; enumerate and pin; no path-shaped API), and the fault harness parameterized over implementations. A BLOB store stays a real switch, without being built. | Devin |
| 13 | **D24:** "a broad release needs a vault" is retracted (by Codex itself). Choose protection from an approved threat model, with an asset-by-asset contract and recovery tested independently of the original machine. Pilot: FileVault for data, Keychain for secrets, codec seams kept open. | Codex, Devin |
| 14 | **D25:** archive is the default; cache is an explicit per-account opt-out. Retention policy, remote deletion, account removal and permanent purge are defined separately. A full disk never shows "Saved" after a failed commit. | Devin, Codex |
| 15 | **Command vocabulary:** version the persisted intent and query formats now; don't freeze menus, DTOs or scripting APIs before users exist. | Codex |
| 16 | Contacts, `Thread` and named draft revisions are **schema reservations,** not features. Merge logs arrive with merge operations. | Both |
| 17 | UniFFI: 0.32.2 is current and lacks the merged Swift 6 async fix. Compile the real exports first, then pin a release, an audited commit or a patch. | Fact check |
| 18 | Offboarding: documented formats plus an independent recovery tool that reads a Sweetbriar store without the app. | Codex (round 1), Devin |

## Where the reviewers differed, and my call

- **Composer probes.** Devin wants them moved to month 2; Codex wants them
  early. A bounded probe stays in Foundations: CJK marked text, RTL, undo
  across formatting and the Markdown → `multipart/alternative` pipeline,
  taking days. Pasted HTML, inline attachments and the external editor
  move to the Remote-effects milestone, when reply and forward are built.
- **Evidence-free default at the D10 gate.** Devin proposes narrow IMAP;
  Codex proposes one demanded provider. If no cohort evidence exists at
  the gate, the first backend is **the owner's own primary provider**, plus
  Dovecot fixtures. The owner is the first daily user, and dogfooding is
  evidence.
- **Commit-latency trigger for D17.** Devin proposed p95 50 ms / p99
  200 ms; Codex proposed p95 100 ms / p99 250 ms. I register Codex's
  numbers as the switch trigger, because they match the 100 ms
  durable-intent budget in [03-performance](../03-performance.md). Devin's
  numbers become the engineering target.

## The public promise (revised again)

Round 2 showed that the round-1 wording conflicted with caches and
user-chosen purges. The revised promise:

> Once Sweetbriar shows "Saved", what you wrote survives crashes and power
> loss. Mail in your archive is never deleted or overwritten except by an
> action you can undo, or by a purge or retention policy you chose. Caches
> are labelled as caches. Backups have a stated recovery point, and a
> restore never sends, moves or deletes anything on its own.

It doesn't promise recovery after the primary store and every backup are
gone.

## Delivery: evidence milestones, not a calendar

Codex's milestone table replaces the month-by-month plan. Dates follow
staffing (D20). Month 2's read-only sync remains the hard gate; import
polish must not consume it (Devin).

| Milestone | Deliverable | Evidence before moving on |
|---|---|---|
| **Foundations** | Store publication and backup skeleton under fault injection. Module architecture check. Behavioral case specs for IMAP and Graph. Signed shell with list and hostile-HTML reader probes and the bounded composer probe. Own-tenant Graph probe. Approvals started; recruiting under way. | Durable acknowledgements. Bounded rendering. Own-app authorization results. |
| **Local archive** | EML + `.mbox` import with a reconciliation report. Conservative reader (sender layout by default). FTS5 exact search. EML export. Local draft revisions. Coordinated backup. | Reconciled inventory; restart recovery; clean-machine restore of mail, drafts, settings and annotations within the declared recovery point. **Gate:** first live backend (D10). |
| **Live reading** | One provider: resumable backfill, explicit coverage, the account health surface. | Interrupted pages and expired cursors never imply deletion. |
| **Remote effects** | Flags, supported filing, reply/forward, submission, inspectable Out Box. | Operation-specific crash/drop evidence; external-client conflicts; unknown outcomes never retried; restore quarantine. |
| **Pilot expansion** | Saved views, notes and tags, retrieval polish, wider Reading defaults where the corpus passes. | Weekly reuse; judged search tasks; maintenance capacity; paid demand. |

## D17 pre-registration (before the engine study's numbers)

Merged from both round-2 reviews.

**Prior:** bundled SQLite for authoritative state; FTS5 for derived search.
These are two separate decisions. Pin a tested build (≥ 3.51.3; 3.54.0 is
the latest, released 2026-10-09; exclude the withdrawn 3.52.0) and verify
its source id at runtime.

**Experiment contract.**
- **Hardware:**
  - an 8 GiB Apple-silicon Mac on macOS 27/APFS, plus the macOS 26 floor;
  - a named Ubuntu/ext4 machine.
- **Data:** consented distributions at 100k and 1M messages, with
  duplicates, multiple memberships, long threads, sparse bodies and
  private metadata.
- **Load:**
  - 200-row windows during a three-account backfill;
  - 1,000 metadata changes/s with bursts of 10,000;
  - interleaved draft saves, checkpoints and backups;
  - an idle run.
- **Runs:** at least five randomized runs, warm and cold, reporting
  p50/p95/p99 with confidence intervals.
- **Tuning budget:** ten engineering days per track; equal durability and
  semantics for every candidate.

**Hard gates for any authoritative engine, SQLite included:**
- zero lost acknowledged transactions across every publication/commit cut
  and ≥ 10,000 seeded fault runs (short writes, ENOSPC, flush errors,
  dropped unflushed writes);
- zero semantic violations in the conformance corpus;
- zero automatic resubmissions or restored outbound effects;
- recovery to a useful mailbox in ≤ 2 s at 100k and ≤ 10 s at 1M;
- 24 interruption/restore scenarios within a 5-minute backup recovery
  point;
- interrupted migrations keep a recoverable generation.

`kill -9` tests don't count as power-loss evidence.

**SQLite is replaced only if a candidate passes every hard gate and:**
1. a product-relevant SQLite durability or isolation defect reproduces on
   the fixed build while the candidate passes. A bug in our own
   publication protocol doesn't count; or
2. after tuning, durable commits exceed p95 100 ms or p99 250 ms, or list
   windows exceed p95 100 ms or p99 200 ms, and the candidate meets the
   budgets while improving the failing p99 by ≥ 30%; or
3. SQLite cannot meet a required corruption-recovery or recovery-point
   outcome that the candidate demonstrates; or
4. the owner adopts a requirement SQLite structurally lacks, such as
   sustained independent writers or a hosted service. That is a scope
   change, not a benchmark win.

Every switch allows at most a 10% regression elsewhere and requires
qualified packaging, upgrades, backup and export.

**FTS5 is replaced** by an engine that passes the search gates:
- exact matches 100%;
- p95 ≤ 100 ms and p99 ≤ 250 ms during sync;
- new content searchable within 2 s (p95);
- rebuild of 1M messages ≤ 2 h;
- index RSS ≤ 512 MiB and index size ≤ 20% of the corpus.

It must also show one of:
- nDCG@10 +0.05 (or task success +10 points) with a paired 95% interval
  and no exact, filter or CJK regression;
- FTS5 missing a latency, freshness or identifier/CJK requirement after
  tuning, while the candidate meets it and cuts the failing tail by
  ≥ 30%;
- FTS5 breaching the resource budget, while the candidate cuts the
  offending measure by ≥ 40%.

Placement is measured separately: FTS5 inside the authority vs a separate
derived FTS5 database.

## What is settled, and what is open

The settled design (D1–D16, D17's prior, D22–D29 starting points) and the
open questions are listed in [00-summary](../00-summary.md). Both reviewers'
round-2 sections 7 agree with that list.
