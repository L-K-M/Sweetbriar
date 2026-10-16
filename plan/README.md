# Sweetbriar implementation plan

How the pilot defined in [spec/00-overview](../spec/00-overview.md) gets
built, step by step. **AI agents implement; the owner reviews.** Every step is one pull request with acceptance checks.

**Status:** v0.1, 2026-10-12.

## Milestones

```
 M0 Foundations ──► M1 Local archive ──► M2 Proton read-only ──► M3 Flags ──► M4 Send ──► M5 Filing ──► M6 Pilot complete
 repo, CI, store,     import, list,        Bridge account,        read/star     drafts→       archive,      views, tags, notes,
 FFI, app shell,      reader, search,      sync, bodies,          via intents   Out Box→      move, trash,  colors complete,
 prototypes           export, backup,      health, real-Bridge                  SMTP          label, undo   qualification
                      local drafts         CI lane
```

| Milestone | Outcome the owner can see | Plan |
|---|---|---|
| **M0** | An app that launches from CI artifacts and talks to the core; the list and composer prototypes decided | [M0-foundations](M0-foundations.md) |
| **M1** | Import an Apple Mail export, browse, read safely, search, export, back up, restore; write and save local drafts | [M1-local-archive](M1-local-archive.md) |
| **M2** | The Proton account syncing read-only through Bridge, with coverage and account health | [M2-proton-read-only](M2-proton-read-only.md) |
| **M3** | Read/unread and star synced to Proton | [M3-flags](M3-flags.md) |
| **M4** | Compose and send through Bridge from the right identity, with an inspectable Out Box | [M4-send](M4-send.md) |
| **M5** | Archive, move, trash and add-label, with undo | [M5-filing](M5-filing.md) |
| **M6** | Saved views, tags, notes, complete account colors; budgets met; the pilot done | [M6-pilot](M6-pilot.md) |

**Order:** each milestone depends on the previous one. Within a
milestone, steps run in the listed order unless a step says it can run in
parallel. **Read-only before mutations**: the order follows the research's build
order (local durability → read-only sync → reversible effects →
submission), adapted to Bridge. Flags, the first reversible effect,
come first; send comes before filing because daily use needs it more.

## Step format

Every step has:

- **Goal:** what exists afterwards, in one or two sentences.
- **Spec:** the sections it implements. The step must match them.
- **Depends on:** earlier steps.
- **Deliverables:** files, modules and commands.
- **Acceptance:** checks that must pass. Each is a command, a test name
  or a manual check. A step is done only when all pass in CI (and, if
  listed, on the owner's Mac).
- **Runs on:** `linux` (agent and Linux CI), `macos-ci` (only in macOS
  CI) or `owner` (manual on the owner's Mac).
- **Size:** S (under ~300 changed lines), M (~300–800), L (~800–1,500).
  Anything larger is split before starting.

## Rules for agents

1. **One step per PR.** The title is "M1.3: List windows and
   subscriptions". The body lists the acceptance checks with their
   results.
2. **Before opening a PR,** run `cargo xtask ci` (fmt, clippy with
   `-D warnings`, tests, layering check, cargo-deny) and make it green.
   For PRs touching `apps/` or FFI exports, regenerate bindings
   (`cargo xtask ffi --bindings-only`) and commit them.
3. **The spec wins.**
   - If the code needs to differ from the spec, change the spec first in
     a separate PR with the reason, or in the same PR as its first
     commit, clearly marked.
   - Never silently deviate. Never weaken an acceptance check to make it
     pass.
4. **Never weaken safety rules to make a test pass:** no deleting
   authoritative rows, no automatic resend, no EXPUNGE in the Proton
   policy, no silent certificate acceptance, no secrets in logs.
5. **Tests come with the code.**
   - Every **must** in the implemented spec sections has a test, or is
     listed as a manual check in the step.
   - Bug fixes start with a failing test.
6. **Dependencies:** pin exact versions from
   [spec/01](../spec/01-architecture.md#crates-and-versions). A new
   dependency must pass `cargo deny`, and gets a line in the PR body
   saying why.
7. **No owner data in the repository.** It is public. Fixtures are
   synthetic or freely licensed.
8. **Keep PRs reviewable:** the size limits above, no unrelated
   refactors, and the module layout from spec/01.

## Review loop

Each PR follows the owner's standing loop:
1. Open the PR.
2. Reviews run. The repository has no GitHub review bots, so helper
   reviews by Codex and Devin are posted as PR comments.
3. Address the findings holistically.
4. Repeat until there are no blocking findings and the `ci` and `macos`
   checks are green, then merge.

`main` has no branch protection (owner decision, 2026-10-12): agents
check CI themselves before merging.

## Owner tasks

These are human steps on the owner's side. Each appears in the milestone
that needs it.

| # | Task | Needed by |
|---|---|---|
| O1 | *Dropped (2026-10-12): no branch protection on `main`* | — |
| O2 | *Decided (2026-10-12): "Unlicense OR 0BSD"; `LICENSE-0BSD` added* | — |
| O3 | Run the CI-built app on your Mac (`xattr -dr com.apple.quarantine Sweetbriar.app`) and report problems | From M0.9 |
| O4 | Judge the list prototype (and run its benchmark: Debug → Run List Benchmark) and the composer probe on your Mac (checklists in M0) | M0.10, M0.11 |
| O5 | Export a few Apple Mail mailboxes (Mailbox → Export Mailbox) for the import qualification | M1.Q |
| O6 | Install Proton Mail Bridge 3.27.1+ (paid Proton plan with Mail), sign in, export its TLS certificate | M2 |
| O7 | Run the per-milestone qualification checklists against your real Proton account | M2.Q onward |
| O8 | Optional: enrol in the Apple Developer Program, for signed builds, Keychain access groups, notarization and later distribution. Ad-hoc builds suffice for the pilot except where M2.3 qualifies the data-protection Keychain (see there) | M2.3 / M6 |
| O9 | Run the list benchmark's store-backed mode (Debug → Run List Benchmark) over an imported mailbox on your Mac | M1.4 |

## Definition of done for the pilot

All of these on the owner's Mac:

1. **Daily use.** The owner uses Sweetbriar daily for Proton mail for two
   weeks, reading, searching, replying, sending and filing, without
   falling back to another client for core tasks.
2. **Restore.** A restore drill recovers mail, drafts and metadata within
   the recovery point, from a backup generation, into a fresh macOS user
   account. Nothing is sent or moved by the restore.
3. **Faults.** Crash and fault-injection suites are green in CI. A manual
   power-cut test (pull power during sync and during a draft save)
   preserves every acknowledged save.
4. **Budgets.** The performance budgets in spec/08 are met at p95 on the
   owner's Mac with their real mailbox size.
5. **Accessibility.** The gates in spec/08 pass.
6. **Bridge.** The Bridge qualification checklists for M2–M5 are
   complete, with dated results committed under `plan/qualification/`.
