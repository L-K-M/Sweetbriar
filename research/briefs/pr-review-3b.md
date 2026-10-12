# Adversarial review, round 2: Sweetbriar PR #3 (specification and implementation plan)

Round 1 findings (Devin, and a partial Codex run) were addressed in the two latest commits; the PR comments list them. Check that the fixes are correct and consistent across all documents, then review everything as below. Report only findings you can point to; do not repeat resolved ones.

You are reviewing pull request #3 of the Sweetbriar repository: the
pilot **specification** (`spec/`, 12 sections) and the step-by-step
**implementation plan** (`plan/`, milestones M0–M6). The snapshot is in
`./` (spec/, plan/, research/). Today is 2026-10-12.

Context: AI agents will implement the plan step by step in a Linux
environment; macOS builds run in GitHub Actions; the owner reviews. The
first live backend is Proton Mail through Proton Bridge. The research
(`research/00-summary.md`, `research/debate/04-resolution.md`, docs
01–15, and `research/raw/plan-*` for Bridge, CI and crates) is the
basis; the spec must be consistent with its settled decisions.

Review for, in order of importance:

1. **Blocking:**
   - contradictions between spec sections, or with settled research
     decisions;
   - designs that could lose data, send twice, act on the wrong
     identity, or violate the Bridge rules (`spec/04`, `05`);
   - schema mistakes (the SQL in `spec/02` and `spec/06` should work in
     SQLite; check constraints, keys and the rules in prose);
   - plan steps that can't be executed as written, have acceptance
     checks that can't be tested, depend on later steps, or miss
     something a later step needs;
   - wrong facts about APIs, crates or platforms.
2. **Non-blocking:** unclear wording, missing detail an implementer
   would need, oversized steps, ordering improvements.

Verify technical claims against primary sources where it matters (crate
docs, Apple/GNOME docs, RFCs, Proton Bridge source) and say what you
checked. Don't reopen settled research decisions unless a factual error
undermines one.

Write `report.md`: a verdict line `VERDICT: APPROVE` or
`VERDICT: CHANGES REQUESTED`, then a findings table (severity, file:line,
problem, exact fix). Under 2,500 words. Do not modify files other than
`report.md`.
