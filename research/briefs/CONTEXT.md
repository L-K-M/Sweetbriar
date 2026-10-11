# Research assignment: Sweetbriar email client

## Project context

Sweetbriar is a new email client project (started 2026-10-11). Goals:

- Modern, fast, native on each platform. First target: macOS, fully native and
  modern. Later: Linux/Ubuntu with a UI that feels native there.
- A shared core written in Rust (sync, storage, MIME, search, account
  backends), with a native UI on top: Swift (AppKit and/or SwiftUI) on macOS,
  a native toolkit on Linux. Core code must be reusable across platforms.
- It must be fast and responsive, guarantee data safety (never lose mail,
  drafts or settings), offer best-in-class search, support non-standard
  sources (Exchange/Microsoft 365, Proton, Gmail API, JMAP, ...), and bring
  back beloved features of classic clients (Eudora, Mailsmith, Claris Emailer,
  Musashi, ...).
- Long-term project. Depth and correctness matter more than speed.

Today is 2026-10-11. Your training data may be stale. Verify anything
time-sensitive (deprecations, API retirements, library status and versions,
pricing, policies, OS versions) against current sources on the web, and give
the date of each such fact.

## Rules

- Research on the web (search and fetch pages). Do not rely on memory where a
  source can be checked. If HTTPS fetching of a site fails, try plain HTTP,
  the Wayback Machine (web.archive.org), or a search engine cache.
- Cite sources inline as Markdown links. Every non-obvious factual claim
  needs a source. Never invent URLs, quotes, version numbers, dates or
  features. If you could not verify something, mark it "(unverified)".
- Separate facts from opinions. Give your recommendation, and the strongest
  argument against it.
- Be concrete: name crates, APIs, RFCs, numbers, file formats, real products.
- Only write files inside the current directory. Do not use git. Do not
  install software system-wide.

## Report structure

1. TL;DR (5-10 bullets)
2. Findings, organized by the questions below
3. Options compared (tables where useful)
4. Recommendations for Sweetbriar, with trade-offs and the strongest
   counter-argument
5. Risks and open questions
6. Sources (URLs you actually consulted)

Depth is welcome: 3,000-8,000 words is fine if the material warrants it.
