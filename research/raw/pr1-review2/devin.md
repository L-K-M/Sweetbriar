<!-- provenance: helper=devin model=swe-2-max started=2026-10-11T21:41:36+00:00 exit=0 brief=research/briefs/ -->

# PR #1 review, round 2 — Sweetbriar foundation research

VERDICT: APPROVE

Scope covered: `00-summary.md`, `README.md`, curated docs `01`–`13`,
`debate/04-resolution.md`, `notes/decision-agenda.md`; `raw/` used only to
judge faithful representation. The snapshot is not a git repo, so the
round-1 fixes were verified by checking the current text against the
round-1/round-2 review records and the fact-check's "correct statement"
blocks.

## Round-1 fixes: verified correct

- **No-small-object-cutoff** — `03-performance.md:161-165` and
  `04-data-safety.md` now defer the file-vs-BLOB cutoff and cold
  compression until measured; BLOBs remain a switch behind the
  object-store contract. No stale recipe remains.
- **Occurrence identity** — `06-account-backends.md` and `00-summary.md`
  (D2) use occurrences; Message-ID never authorizes merging; Gmail labels
  are not folders.
- **Object-store qualifications** — `04-data-safety.md` carries the
  debate-approved publication (rename + directory fsync), DB-reference
  ordering, GC and backup wording.
- **DavMail** — `06-account-backends.md:69` matches the fact-check's
  correct statement verbatim (Graph first shipped in 6.8.0, 2026-06-12;
  7.0.0 strengthens it).
- **Thunderbird Graph, Apple Mail EWS** — scoped correctly in `06` and
  `10` (Graph shipped in 154, email only; Apple still on EWS, Graph in a
  future update).
- **PostgreSQL/Turso framing** — `13-database-engine.md` no longer claims
  a proven Postgres safety reduction; the Turso-date disagreement is
  reported; redb's absence is explained (empty stub wheel).
- **Markdown security** — `12-markdown-first.md` states Markdown is not a
  security boundary; normalized reading is gated on a fidelity corpus.
- All 18 `raw/fact-check-1` claims appear correctly applied (Gmail
  quotas, Send-as sunset, Thunderbird Graph, DavMail, UniFFI, etc.).

## New problems introduced by the fixes

None found. Two apparent conflicts checked out in the curated docs'
favor: `05-search.md`'s Tantivy 0.26.2/"0.27 unreleased" is correct
(crates.io shows 0.26.2 as latest; GLM's raw was the wrong one), and
`09-linux.md`'s GTK 4.22 is correct (gtk.org lists 4.22.5 stable;
codex's raw claiming 4.24.1 was the wrong one).

## Spot-checks passed

- Benchmarks in `13`: point lookups ~13 µs, keyset ~0.05 ms, sustained
  reader p95 0.399 ms, writer commit p95 6.108 ms, DuckDB lookups
  0.49–0.60 ms, LMDB 1.2 µs, OFFSET 80 ms vs cursor 0.08 ms — all match
  the raw artifacts, with environment caveats preserved.
- Quotas/limits: Graph 10,000/10 min, 4 concurrent, 150 MB/5 min,
  `$search` ≤1,000 date-sorted, filtered-delta 5,000 cap, subscription
  <7 days; Gmail 6,000/1,200,000/80,000,000, `messages.get` 20,
  IMAP 2,500/500 MB — all match raws.
- Timelines: EWS (blocking 2026-10-01, dead 2027-04-01), SMTP AUTH
  (re-timed 2026-01-27, off-by-default end Dec 2026, removal TBA H2
  2027), Gmail Send-as (Jan 2027) — consistent and correct.
- `03-performance.md:63` "IMAP4rev2 ... not QRESYNC" verified against
  RFC 9051 Appendix F (QRESYNC is recommended, not incorporated).
- Historical: Engst delta 121,077 (970,283 − 849,206), Geary 46.0
  (May 2024), MailMate Store exit 2012, f=f default-off 2021, Akonadi
  MR !311 — all match raws.
- Internal links resolve (`classics/`, `debate/03-review-*`, `raw/`
  paths, `notes/`). Decision agenda D1–D29 consistent with `00-summary`.
- No recommendations that could cause data loss; intent/quarantine/
  breaker/send-uncertainty guards are in place wherever remote effects
  are discussed.

## Findings

| Severity | Location | Problem | Exact fix |
|---|---|---|---|
| Non-blocking | `research/03-performance.md:149` | "SQLite 3.40" drops the patch version; the benchmark actually ran 3.40.1 (`raw/performance/codex.md` line ~192: "Python 3.11.2/SQLite 3.40.1"). Other docs cite patch versions (3.51.3, 3.53.4). | Change "SQLite 3.40 on Linux" to "SQLite 3.40.1 on Linux". |
