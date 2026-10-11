# Adversarial review: Sweetbriar foundational recommendations (round 1)

You are GPT-6.1-Sol, asked to be the strongest possible critic and
collaborator. Claude (Opus 5.5) led a research effort for **Sweetbriar**, a
new email client: native and fast on macOS first (Swift UI), native on
Linux later, shared Rust core, "never lose data", best-in-class search,
non-standard sources (Exchange/M365, Proton, Gmail API, JMAP), and revival of
what made classic clients beloved (Eudora, Mailsmith, Claris Emailer,
Musashi). Long-term project. Today is 2026-10-11 (macOS 27 "Golden Gate"
is current; Exchange Online EWS is being phased out since 2026-10-01).

The repository snapshot in the current directory contains:

- `research/debate/00-position-paper.md` — Claude's recommendation for
  each foundational decision D1–D15, with its strongest counter-argument.
  **This is what you are reviewing.**
- `research/0*.md`, `research/11-classic-clients.md`, `research/classics/`
  — curated syntheses Claude wrote from 30+ helper reports.
- `research/raw/` — the underlying helper reports (some by you, earlier,
  in separate sessions), with sources.
- `research/notes/working-notes.md` — running notes, conflicts, facts
  Claude verified by hand.

## Your task

Read the position paper first, then whatever curated documents and raw
reports you need. You may use web search to verify facts.

Then write `review.md` in the current directory with:

1. **Verdict table:** for each of D1–D15: agree / agree with changes /
   disagree, and one line why.
2. **Where Claude is wrong.** For every decision you disagree with or would
   change: your alternative, the evidence, and what would change your mind.
   Be specific and concrete (crates, APIs, numbers, sources). Steelman the
   option Claude rejected.
3. **Factual errors or stale claims** anywhere in the position paper or
   curated docs, with corrections and sources.
4. **Missing foundational decisions** not in D1–D15 (e.g. product licence,
   pricing/business model, account onboarding, multi-device sync of
   Sweetbriar's own metadata, encryption at rest, telemetry, accessibility,
   localization, testing infrastructure, team size assumptions).
5. **Risk ranking:** the five decisions that hurt most if wrong and are
   hardest to reverse, and how to de-risk each early.
6. **First vertical slice:** what the first 3-6 months of building should
   cut through, and what should explicitly wait. Challenge Claude's
   implied ordering where you disagree (e.g. is Graph/Exchange or
   generic IMAP first? Is the deterministic-simulation harness worth it
   before the first user?).
7. **Where you and Claude agree but both could be wrong** (shared blind
   spots).

Be direct and opinionated; disagreement is the point. Prefer depth over
politeness. Do not modify any files other than `review.md`. Your final
message must be the complete review in Markdown.
