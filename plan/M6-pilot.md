# M6 — Pilot complete

**Outcome:** the classic workflows, complete account colors with the
owner's appearance requests (M6.6) and qualified performance,
durability and accessibility. The pilot's
definition of done ([plan/README](README.md#definition-of-done-for-the-pilot))
is met.

### M6.1 Saved views, tags and notes — M, linux + macos-ci

**M6.1a (core) — M, linux.** `upsert_saved_view`, `delete_saved_view`,
`saved_views()`, the `View(id)` window spec, `add_tag`, `remove_tag`
and `set_note`, through `sb-store`, `sb-engine` and `sb-service`.
- **Spec:** 02 (`saved_views`, `tags`, `message_tags`, `notes`), 06,
  07 (Views, Messages).
- **Depends on:** the local archive only (M1.6, M1.8); the "never
  sent" trace waits for a remote backend (M2+).
- **Acceptance:** a view's results equal its query; tags and notes
  survive occurrence vanishing and restore; export includes them (09,
  capture format 1); commands are idempotent under replay.

**M6.1b1 (FFI) — L, linux.** `upsert_saved_view`, `delete_saved_view`,
`saved_views()`, the `View(id)` window spec, `add_tag`, `remove_tag`
and `set_note` exported through `sb-ffi` with DTOs mirroring the
service's, plus the regenerated bindings under
`apps/macos/SweetbriarCore/`.
- **Spec:** 02, 07 (Views, Messages, Threading, cancellation and FFI
  rules).
- **Depends on:** M6.1a.
- **Acceptance:** each export round-trips over a synthetic archive
  and replays idempotently under a repeated key (the sb-ffi tests);
  `cargo xtask ffi --bindings-only` regenerates the committed bindings
  and `cargo xtask check-ffi` passes.

**M6.1b2 (macOS UI) — M, macos-ci.** "Save search as view", sidebar
views, a tag picker and a reader note editor.
- **Spec:** 02, 07, 08 (reader header, sidebar).
- **Depends on:** M6.1b1; the "never sent" trace also needs a remote
  backend (M2+).
- **Acceptance:** saving a search lists its rows under the new view;
  the picker applies and removes a tag; the note editor saves and
  clears the note; nothing is sent (trace).

### M6.2 Account colors complete — M, macos-ci + owner

- **Goal:** every P0 and P1 color surface from research 15 is
  implemented.
- **Spec:** research 15 (Where the color appears); 08.
- **Depends on:** M4.4, M6.1.
- **Deliverables:**
  - account cues in search results, the Out Box,
    the move destination picker and the health view;
  - account-prefixed window titles;
  - the account name in notification text (notifications exist since
    M2.7);
  - Appearance settings (Standard, Strong, Minimal) with previews.
- **Acceptance:**
  - an audit test lists every P0 and P1 surface and checks it renders an
    account cue with a label;
  - **owner:** recognition check with all their accounts (the local
    archive plus Proton identities).

### M6.3 Reading view experiment — M, linux + macos-ci + owner

- **Goal:** an optional Reading view for supported structures, behind a
  toggle (off by default).
- **Spec:** research 12; 08 (Reader: Reading view).
- **Depends on:** M1.5.
- **Deliverables:**
  - **M6.3a (core):** `sb-mime::document`, a versioned semantic
    document model for plain personal mail and simple HTML (paragraphs,
    headings, emphasis, explicit line breaks, preformatted blocks,
    quotes, lists, links, images), with source-part references; a
    positive structure detector, with sender layout otherwise. This
    step.
  - **M6.3b1 (query and corpus tool):** the reading-document query
    with its service DTOs, plus `cargo xtask reading-corpus`, a local
    tool that runs the model over the owner's own mail and prints
    counts only.
  - **M6.3b2 (FFI and sender memory):** the FFI export and bindings;
    the `reader.reading_view_senders` setting and its bridge model.
    Split from M6.3b1 2026-11-15: together they measure about 1,620
    hand-written lines before the Swift toggle.
  - **M6.3b3 (renderer and reader toggle):** the native renderer of
    the semantic document and the reader's Reading view toggle over
    it, keeping both renditions and recording the sender. Split from
    M6.3b2 2026-11-16: with the renderer, M6.3b2 measures about 1,530
    hand-written lines.
  - **Owner corpus review:** the owner runs the corpus tool on their
    own mail and reviews the flagged critical failures. Results summary
    only goes in `plan/qualification/M6-reading.md`.
- **Acceptance:**
  - **M6.3a:** fixtures with tables, receipts or complex layout are
    classified unsupported; plain text with quotes and lists and simple
    HTML round-trip into the expected document; nodes keep source-part
    references; the hostile corpus still passes;
  - **M6.3b1:** the query answers the semantic document for supported
    structures and `Unsupported` otherwise, over the reader body query's
    lookup and errors; the corpus tool prints counts only;
  - **M6.3b2:** each DTO crosses the FFI field for field; the sender
    list keeps the registry's canonical form and refuses a full list;
  - **M6.3b3:** each node kind renders; the toggle is instant and
    remembered per sender;
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
  - **owner:** the spec/08 budgets met at p95 with the real mailbox, and
    the spec/06 search targets (cached exact search p95 ≤ 100 ms and
    p99 ≤ 250 ms during sync, new mail searchable within 2 s at p95,
    index rebuild ≤ 15 min), with mailbox size and sync load recorded;
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

### M6.6 Account appearance requests — L, linux + macos-ci + owner

Owner request, 2026-11-24 (research 15 "Window tint survey"): a color
wash over the whole compose window as the default, Send named for the
account, message windows in the receiving account's tint, a reply line
naming the account that received the original, an in-app account logo,
and Settings controls for the account's letter and per-address colors.
Lanes: M6.6a and M6.6b are independent core steps; M6.6c stacks on
M6.6a, and M6.6d on M6.6a and M6.6b.

**M6.6a (core) — M, linux + macos-ci.** The Color Wash tokens, the
`wash` setting and the reply's account.
- **Spec:** 02 (Settings registry), 07 (`appearance()`, `draft()`), 08
  (Color Wash mode, Send as the account, Reply line).
- **Depends on:** M1.10, M4.4.
- **Deliverables:** `derive_tokens` adds each appearance's wash stops
  and the marker's ink, exported through `sb-service` and `sb-ffi`;
  `appearance.compose_cue` accepts `wash` and defaults to it; `draft()`
  carries `reply_account_id`; regenerated bindings; the app parses
  `wash` (drawn as Strong until M6.6c), so main never meets an unknown
  value.
- **Acceptance:**
  - for every palette color and a sweep of custom colors (black, white,
    the sRGB primaries and secondaries, near-grays), the ink reaches
    7:1 against the stops' channel-wise minimum in light and maximum in
    dark, and the marker's ink reaches 4.5:1 against the marker;
  - the tokens are deterministic across runs;
  - `settings()` reports `wash` when unset and keeps a stored
    `standard`; `set_setting` accepts `wash`;
  - a reply's and a reply-all's draft report the answered
    occurrence's account, a local-archive one included; a forward and a
    new message report none;
  - `cargo xtask check-ffi` passes; macOS CI's app tests parse `wash`.

**M6.6b (core) — M, linux.** Account logos.
- **Spec:** 02 ("Account logos", migration 37), 07
  (`set_account_logo`, `appearance()`).
- **Depends on:** M1.10.
- **Deliverables:** migration 37; the store's checked writer and
  reader; the engine and service command with receipts; `appearance()`
  carrying each logo and its revision; the FFI export and bindings.
- **Acceptance:**
  - a synthetic 128 × 128 PNG round-trips through `appearance()`;
  - a wrong signature, a wrong or corrupted `IHDR`, other dimensions, a
    missing `IEND`, an empty value and 65,537 bytes are each
    `InvalidInput`, and such a stored value is `Corrupt`;
  - a stale revision is `Conflict`; an unchanged write writes nothing;
    clearing keeps the row with a new revision; each changed write
    advances `appearance_generation` once; a replayed command
    returns its receipt;
  - a logo survives backup and restore; the schema drift test passes.

**M6.6c (macOS UI) — L, macos-ci.** Color Wash in compose windows,
Send as the account, the reply line, the Color Wash preview.
- **Spec:** 08 (Color Wash mode, Send as the account, Reply line, Mode
  previews).
- **Depends on:** M6.6a.
- **Acceptance (app-hosted tests):**
  - Color Wash draws the strip fill behind the header and the gradient
    behind the editor, which draws no background; injected Increase
    Contrast and Reduce Transparency render Strong;
  - for every palette color, in light and dark, body and status text
    reach 4.5:1 on both rendered stops, and status text uses the label
    color; banners keep the window background;
  - Send's title and accessibility label name the account in Color
    Wash and Strong, and the plain Send stays in Standard and Minimal
    and without a route;
  - switching From changes the fill, the wash and Send in one
    transition;
  - the reply line shows for a reply, a reply-all and a local-archive
    reply, and not for a forward or a new message;
  - Settings shows the Color Wash preview.

**M6.6d (macOS UI) — L, macos-ci + owner.** Message window tint, the
account logo and the Settings letter, logo and address controls.
- **Spec:** 08 (Message window "Tint"; Logo; Settings → Appearance,
  per account; Sidebar; Account chips).
- **Depends on:** M6.6a, M6.6b.
- **Acceptance:**
  - a message window takes the receiving account's fill in Color Wash
    and Strong, Increase Contrast and Reduce Transparency included, and
    stays neutral in Standard and Minimal and without the account's
    appearance;
  - the letter field trims, limits to two grapheme clusters and clears
    back to the automatic letter;
  - the logo pipeline refuses a file over 20 MB before decoding,
    writes a 128 × 128 PNG within the bound, refuses a synthetic noise
    image with "Choose a simpler image." and removes a logo;
  - the logo shows with its plate and ring in the strip, swatches,
    sidebar, chips and previews; list rows keep the monogram;
    accessibility labels are unchanged; an undecodable stored logo
    shows the monogram and the note; an identity's symbol override
    wins;
  - a per-address color writes through `set_identity_appearance`, and
    a `Conflict` reloads without overwriting;
  - **owner:** the M6.2 recognition check, repeated in Color Wash with
    the owner's accounts.

### M6.7 Recipient check — L, linux + macos-ci + owner

Owner request, 2026-11-24 (research 15 "Recipient fencing"): block
sending from an account to an address that has written only to other
accounts, with a lasting per-address allowance. Stacks on M6.6c for
the compose banners' surroundings; the core step can start at once.

**M6.7a (core) — L, linux.** Account senders, allowances, the check
and the refusal.
- **Spec:** 02 ("Account senders and recipient allowances", migration
  38), 05 ("Recipient check"), 07 (`recipient_fence`,
  `recipient_allowances`, `allow_recipient`,
  `remove_recipient_allowance`, `send_draft`).
- **Depends on:** M4.3.
- **Deliverables:** migration 38 with its backfill; account-sender rows
  written by the received-address sweep; the query and commands with
  receipts through `sb-engine`, `sb-service` and `sb-ffi`;
  `send_draft`'s refusal; regenerated bindings. If the measured diff exceeds the
  limit, split the FFI export into its own step.
- **Acceptance:**
  - an address that wrote only to B is blocked from A in To, Cc and
    Bcc, for a new message, a reply and a forward; one that wrote to A,
    or to no account, passes;
  - before the first pass after migration 38 completes the check
    reports not ready and blocks nothing, even with B's rows recorded;
  - the local archive's mail and A's own sent mail add no history;
  - `send_draft` refuses a blocked revision with `Conflict` naming
    `recipient_fence`, whatever the `acks`, and sends after
    `allow_recipient`, a route change or removing the recipient from
    the draft;
  - allowances replay idempotently, survive a derived rebuild and
    backup and restore, and both commands refuse the local archive;
  - migration 38's backfill matches the incremental rows on a seeded
    archive, and a vanished occurrence keeps its row.

**M6.7b (macOS UI) — M, macos-ci + owner.** The banners, the
allowance flow and Settings → Sending's list.
- **Spec:** 08 ("Recipient check").
- **Depends on:** M6.7a, M6.6c.
- **Acceptance (app-hosted tests):**
  - a blocked recipient shows its banner and warning token, and Send is
    disabled with the reason; a check that isn't ready shows its note
    and blocks nothing;
  - Use <account> switches From and clears the banner; an address that
    wrote to two other accounts shows both chips and offers both;
  - Allow asks first; Cancel changes nothing; Allow clears the banner
    and the allowance appears in Settings, where Remove brings the
    block back;
  - a `Conflict` from Send for `recipient_fence` shows the banner;
  - **owner:** sending from one Proton address to a correspondent of
    another is blocked, and allowed after Allow (needs a second sending
    account; recorded as pending until one exists).

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
