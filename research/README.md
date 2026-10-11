# Sweetbriar research

Foundation research for Sweetbriar: a fast, native email client with a
shared Rust core, a Swift UI on macOS first, and a native Linux UI later.

Research began 2026-10-11. Time-sensitive facts carry the date they were
checked.

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
                                                            (GPT-6.1-Sol vs Claude)
```

- Each topic is researched independently by two or more helper agents on
  different models (Codex GPT-6.1-Sol, Devin SWE-2 Max, GLM-5.3, MiMo).
- Classic clients are studied from primary sources where possible: the
  Eudora Mac source code, the Eudora 6.2, Mailsmith 2.x and Claris
  Emailer manuals, and the original Musashi archives.
- Curated documents merge the raw reports, resolve contradictions, and
  flag what remains unverified.
- Key recommendations are challenged by GPT-6.1-Sol, and the debate is kept.

## Layout

- `raw/` — helper reports, verbatim, one directory per topic. Not edited
  except for a provenance header.
- `briefs/` — the exact questions each helper received.
- Curated documents (numbered) — added as synthesis completes.
