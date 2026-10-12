# M6 — Pilot complete

**Outcome:** the classic workflows, complete account colors and
qualified performance, durability and accessibility. The pilot's
definition of done ([plan/README](README.md#definition-of-done-for-the-pilot))
is met.

### M6.1 Saved views, tags and notes — M, linux + macos-ci

- **Goal:** saved views (smart mailboxes) from any query, plus private
  tags and notes.
- **Spec:** 02 (`saved_views`, `tags`, `message_tags`, `notes`), 06
  (Query AST), 07 (Views, Messages commands).
- **Depends on:** M5 complete.
- **Deliverables:**
  - in the core: `upsert_saved_view`, `delete_saved_view`, the `View`
    window spec, tag and note commands;
  - macOS:
    - "Save search as view";
    - views in the sidebar;
    - a tag picker;
    - a note editor in the reader header.
- **Acceptance:**
  - a view's results equal those of its query;
  - tags and notes survive the message vanishing from the server, and
    restore;
  - they are never sent to the server (trace);
  - export includes them in the manifest.

### M6.2 Account colors complete — M, macos-ci + owner

- **Goal:** every P0 and P1 color surface from research 15 is
  implemented.
- **Spec:** research 15 (Where the color appears); 08.
- **Depends on:** M4.4, M6.1.
- **Deliverables:**
  - account cues in mixed-account threads, search results, the Out Box,
    the move destination picker and the health view;
  - account-prefixed window titles;
  - the account name in notification text;
  - Appearance settings (Standard, Strong, Minimal) with previews.
- **Acceptance:**
  - an audit test lists every P0 and P1 surface and checks it renders an
    account cue with a label;
  - **owner:** recognition check with all their accounts (the local
    archive plus Proton identities).

### M6.3 Reading view experiment — M, linux + macos-ci

- **Goal:** an optional Reading view for supported structures, behind a
  toggle (off by default).
- **Spec:** research 12; 08 (Reader: Reading view).
- **Depends on:** M1.5.
- **Deliverables:**
  - `sb-mime::document`: a semantic document model for plain personal
    mail and simple HTML (paragraphs, quotes, lists, links, images),
    with source-part references;
  - a positive structure detector, with sender layout otherwise;
  - the reader toggle with per-sender memory;
  - local corpus tooling that runs on the owner's Mac against their own
    mail and is never committed.
- **Acceptance:**
  - fixtures with tables, receipts or complex layout are classified
    unsupported;
  - the toggle is instant;
  - **owner:** runs the corpus tool on 200+ of their own messages and
    reviews the flagged critical failures. Results summary only goes in
    `plan/qualification/M6-reading.md`.

### M6.4 Performance, durability and accessibility qualification — M, owner + macos-ci

- **Goal:** measure the pilot against the budgets and gates on the
  owner's Mac.
- **Spec:** 08 (Performance budgets, Accessibility gates), 11 (Fault
  injection: power loss), 03.
- **Depends on:** M6.1–M6.3.
- **Deliverables:**
  - the opt-in local latency log with a percentile report;
  - an accessibility audit script (Accessibility Inspector checklist);
  - the power-cut test procedure;
  - the nightly power-loss simulation on both layers: the `Fs` seam for
    objects, and the test SQLite VFS for the database and WAL (11).
- **Acceptance:**
  - **owner:** budgets met at p95 with the real mailbox;
  - VoiceOver completes the gate tasks;
  - pulling power during sync and during a draft save preserves every
    acknowledged save;
  - the nightly power-loss simulation is green.

### M6.5 Signed builds — S/M, macos-ci (optional for the pilot)

- **Goal:** Developer ID signing and notarization in `release.yml`, once
  the owner has enrolled (O8).
- **Spec:** research D12; [raw/plan-build-ci](../research/raw/plan-build-ci/devin.md)
  §2.4.
- **Depends on:** O8.
- **Deliverables:**
  - secrets: P12, password, App Store Connect API key;
  - a keychain setup step;
  - hardened runtime;
  - `notarytool --wait` and stapling of the app and the DMG;
  - the data-protection Keychain re-qualified with the signed build
    (M2.3 fallback removed).
- **Acceptance:**
  - a tagged release produces a notarized, stapled DMG that opens
    without Gatekeeper warnings on the owner's Mac;
  - the Keychain uses the protected store.
- Sparkle updates wait until there's a second user.

### Pilot exit

The pilot is done when every item in the definition of done is checked
and recorded in `plan/qualification/pilot.md`.

**Next decisions after the pilot** (not planned here):
- the Linux GTK app;
- a second backend, Gmail or Graph, by demand, starting the OAuth
  approvals early;
- server Drafts;
- rules;
- distribution to other users;
- D19 (business model) and D21 (scope).
