# Sweetbriar research

Foundation research for Sweetbriar: a fast, native email client with a
shared Rust core, a Swift UI on macOS first, and a native Linux UI later.

Research began 2026-10-11. Time-sensitive facts carry the date they were
checked.

## Start here

- [00-summary.md](00-summary.md): the answers to the owner's questions,
  what is settled, the decisions only the owner can make, and what is
  still open.
- [debate/](debate/): Claude's position paper, two adversarial review
  rounds by GPT-6.1-Sol and Devin, Claude's response, and the
  [resolution](debate/04-resolution.md).

## Curated documents

| # | Topic | Question it answers |
|---|---|---|
| [01](01-architecture.md) | Architecture | How to build a Rust core under native UIs: layers, boundary, async, process model, packaging |
| [02](02-rust-ecosystem.md) | Rust ecosystem | Which crates to reuse, what to build, licence traps |
| [03](03-performance.md) | Performance | Latency budgets, sync protocols, store design at 1M messages, measurement |
| [04](04-data-safety.md) | Data safety | How to never lose mail, drafts, settings or sends; invariants and tests |
| [05](05-search.md) | Search | Engines, query language, index consistency, federation, semantic search |
| [06](06-account-backends.md) | Account backends | Exchange/M365 (Graph), Gmail, Proton, JMAP, iCloud and others; order and approvals |
| [07](07-standards.md) | Standards | Autoconfig, OAuth, MIME edge cases, threading, sending, keywords; launch checklist |
| [08](08-macos-ui.md) | macOS UI | AppKit vs SwiftUI, list, reader, composer, system integration, distribution |
| [09](09-linux.md) | Linux UI | GTK4/libadwaita vs Qt, WebKitGTK, portals, packaging |
| [10](10-landscape.md) | Landscape | The 2026 competition, user complaints, gaps, AI, positioning |
| [11](11-classic-clients.md) | Classic clients | What made Eudora, Mailsmith, Claris Emailer, Musashi and others beloved; ranked revival catalog |
| [12](12-markdown-first.md) | Markdown-first mail | Should Sweetbriar normalize incoming mail into one rendering and compose in Markdown? (owner's proposal) |
| [13](13-database-engine.md) | Database engine | SQLite, PostgreSQL, DuckDB or another engine for the local store? SQLite's quirks and how to neutralize them (owner's questions) |

Per-client deep dives: [classics/eudora.md](classics/eudora.md),
[classics/mailsmith.md](classics/mailsmith.md),
[classics/claris-emailer.md](classics/claris-emailer.md),
[classics/musashi.md](classics/musashi.md).

## Method

```
  briefs (questions)                   raw reports              curated docs
 ┌──────────────────┐   fan-out   ┌────────────────────┐   ┌──────────────────┐
 │ one brief per    │ ──────────► │ 2+ independent     │──►│ synthesis per    │
 │ topic + shared   │  codex,     │ researchers per    │   │ topic, claims    │
 │ context & rules  │  devin,     │ topic, each with   │   │ cross-checked,   │
 └──────────────────┘  glm, mimo  │ cited sources      │   │ conflicts noted  │
                                  └────────────────────┘   └────────┬─────────┘
                                                                    │
                       primary sources (manuals, source             ▼
                       code, original app archives) ──────► adversarial review
                                                            (GPT-6.1-Sol, Devin)
                                                                    │
                                                                    ▼
                                                            resolution + summary
```

- **Independent researchers.** Most topics were researched by two or more
  helper agents on different models: Codex GPT-6.1-Sol, Devin SWE-2 Max,
  GLM-5.3, and MiMo until its free quota ran out. Their reports are kept
  verbatim in [raw/](raw/), with a provenance header.
- **Primary sources for the classic clients:**
  - the Eudora Mac source code (CHM, BSD), audited by codex
  - the Eudora 6.2, Mailsmith 2.x and Claris Emailer manuals
  - the original Musashi distribution archives, extracted with a StuffIt 5
    decoder codex wrote for the purpose, and their English and Japanese
    manuals and resource forks

  Copyrighted manuals and binaries are cited, not committed.
- **Curated documents** merge the raw reports, resolve contradictions, and
  say what is still unverified. Claude checked high-impact facts by hand:
  - EWS retirement and Apple Mail's dependence on it
  - macOS 27 being current and Apple-silicon-only
  - the SQLite WAL-reset bug
  - the SMTP AUTH timeline
  - RFC 9979
  - Gmail CONDSTORE and the end of third-party Send-as
  - Thunderbird's Graph support
  - App Intents' mail domain
  - AppKit Observation tracking
  - Graph's 5,000-message filtered-delta cap and Microsoft's consent policy

  A separate fact check verified 18 more recent claims against primary
  sources ([raw/fact-check-1/codex.md](raw/fact-check-1/codex.md)).
- **Running notes** with every conflict and verification are in
  [notes/working-notes.md](notes/working-notes.md). The foundational
  decisions are listed in [notes/decision-agenda.md](notes/decision-agenda.md).

## Layout

- `raw/<topic>/<agent>.md`: helper reports, verbatim. `<agent>-notes/` and
  `<agent>-artifacts/` hold their working notes, scripts and data.
- `briefs/`: the exact questions each helper received.
- `notes/`: working notes and the decision agenda.
- `debate/`: the adversarial review rounds.
