# Pull-request review: Sweetbriar foundation research (PR #1)

You are reviewing pull request #1 of the Sweetbriar repository: a
documentation-only PR adding `research/` (curated research documents,
verbatim helper reports in `raw/`, an adversarial debate in `debate/`).
The snapshot is in `./research/`. Today is 2026-10-11.

Focus on the files a reader actually acts on:

- `research/00-summary.md` (the owner's entry point)
- `research/README.md`
- `research/01-…13-*.md` curated documents
- `research/debate/04-resolution.md`
- `research/notes/decision-agenda.md`

`raw/` files are verbatim helper output; do not review their content,
only whether curated documents represent them faithfully.

Look for, in order of importance:

1. **Blocking:** factual errors in curated docs or the summary; places
   where the summary or a curated doc contradicts the debate resolution
   (`debate/04-resolution.md`) or another curated doc; recommendations an
   implementer could follow into data loss; misrepresentation of a raw
   report or benchmark number.
2. **Non-blocking:** unclear wording, stale statements, broken links,
   missing citations, inconsistent numbers.

Do not re-open design decisions settled in the debate unless you find a
factual error that undermines one. Do not propose new research.

Write `report.md` with:

- A verdict line: `VERDICT: APPROVE` (no blocking findings) or
  `VERDICT: CHANGES REQUESTED`.
- A table of findings: severity (blocking / non-blocking), file:line,
  the problem, and the exact fix. Verify each finding against the files
  before listing it; omit anything you can't point to.

Keep it under 1,500 words. Do not modify any files other than `report.md`.
