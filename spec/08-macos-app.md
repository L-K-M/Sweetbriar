# 08 — macOS app

Rests on research [08-macos-ui](../research/08-macos-ui.md),
[12-markdown-first](../research/12-markdown-first.md),
[15-account-colors](../research/15-account-colors.md) and D8, D16, D31.

## Platform

- **Target:** macOS 26 minimum, Apple silicon (arm64) only. Built with
  the newest Xcode available on the CI runners. Swift 6 language mode
  with strict concurrency checking.
- **Frameworks:** an AppKit application shell, with SwiftUI islands for
  settings, onboarding, the account health surface and inspectors.
  AppKit owns windows, menus, the responder chain, the message list, the
  reader host and the composer.
- **The bridge to the core:**
  - a Swift package `SweetbriarCore` wraps the UniFFI-generated bindings;
  - a hand-written adapter adds `async`/cancellation and main-actor
    delivery (07);
  - **app code never calls generated bindings directly**: layering, L1
    in 01. Only `SweetbriarBridge` imports `SweetbriarCore` or
    `SweetbriarCoreFFI`; an `xtask check-ffi` lint enforces it.

## Windows

| Window | Contents |
|---|---|
| **Main window** (one or more) | Sidebar \| message list \| reader, as an `NSSplitViewController` |
| **Message window** | A standalone reader for one message |
| **Compose window** | One per draft |
| **Settings** | SwiftUI: accounts, appearance, sending safety, backups |
| **Activity / account health** | SwiftUI: per-account state, coverage, parked intents, breaker events, Out Box |
| **Import report** | Results of an import job (09) |

State restoration uses AppKit window restoration. A compose window
restores its draft by `draft_id`.

## Sidebar

- **Unified section:** Inbox (`Unified(inbox)`), Flagged (`Flagged`),
  Drafts (local) (`drafts()`), Out Box (`out_box()`), then saved views
  (07). Each account's own synced Drafts mailbox appears under the
  account.
- **One section per account,** with its colored monogram and name; its
  mailboxes are below, folders and labels grouped as Bridge exposes them
  (04).
- **Built with** an `NSOutlineView`, source-list style.

## Message list

- **Built with** `NSTableView` (view-based), backed by a `ListWindowModel`
  over a core window (07). M0.10's prototype keeps AppKit, pending
  the owner's Mac (O4), which decides. macOS CI's diagnostic run
  (virtualized M1, 60 Hz, 500,000 rows):
  - cold, SwiftUI `List` failed all 26 decision bars. It blocked the
    main thread for tens of seconds on open, jumps and selection, and
    peaked at 3.97 GB. Its warm run overran the benchmark's budget
    before rendering;
  - cold, AppKit showed its first rows in about 0.4 s, peaked at
    41 MB, and rendered every input within 34 ms at p95. Its warm run,
    after SwiftUI's, passed its checks.

  The choice reopens only if SwiftUI matches AppKit on the owner's Mac
  on every checklist item and benchmark (plan M0.10).
- **`ListWindowModel`** lives in `SweetbriarBridge`, free of AppKit, so
  both prototype lists and their tests share its paging, data and
  delivery cadence; each list adapts it idiomatically and observes only
  the rows it shows.
  - It holds the window's id index (07) and pages in fixed 200-row
    pages. Page `k` starts at rank `s = 200k` and holds `L = min(200, N
    − s)` rows; it is read `Around(ids[s + ⌊L/2⌋])` with limit `L`,
    which returns exactly those rows, the last partial page included.
  - The cache holds at most three pages: the visible ones are pinned,
    the next page in the scroll direction is prefetched, and the rest
    go least recently used first.
  - At most two reads are in flight; a read for a page no longer
    wanted is cancelled through its token, and a completion from an
    older generation or request is dropped.
  - Pages reach the list through the frame batcher (07: at most once
    per frame). A row not yet cached shows as a placeholder of the same
    height. `ResetRequired` rebuilds the model on its existing handle:
    the id index, then the pages. The prototype has no subscriptions
    (M1.3 adds them), so after a fill the Debug menu hands the new
    generation to every open prototype model, which rebuilds.
  - Selection is held by `occurrence_id`, so a 10k-row selection
    fetches no rows.
  - Rows have one height per list: production heights vary with the
    preview mode and font, not per row (research 08).
- **Rows:**
  - sender, subject, snippet, date;
  - unread, flagged, attachment and tag markers;
  - the account marker, a 4 pt colored rail plus monogram, in unified
    and search views.
- **Selection:**
  - native selection colors; the account color never replaces them;
  - selection is held by `occurrence_id`;
  - for bulk actions on more than 1,000 rows, the list passes a
    `SelectionToken`;
  - a context click on a selected row acts on the selection; on an
    unselected row it acts on that row alone and leaves the selection.
- **Keyboard:**
  - arrows, Return to open, Space to page the reader;
  - `⌘⌫` delete to trash, `⌃⌘A` archive;
  - `⇧⌘U` mark read or unread, `⇧⌘L` star or unstar;
  - type-to-file (below);
  - every command is also in a menu.
- **Performance budget:** switching mailbox shows the first rows within
  100 ms at p95. Scrolling a 500k-row mailbox spends < 5 ms of main-thread
  work per update. Selection feedback takes under 50 ms at p95.

## Reader

- **HTML:** sanitized sender layout, shown in **one reused `WKWebView`
  per reading pane** (each main window and message window has its own):
  - a non-persistent data store (`WKWebsiteDataStore.nonPersistent()`);
  - JavaScript off;
  - `WKContentRuleList` blocks every network load. If rule-list
    compilation fails, the reader **fails closed**: it shows plain text
    only;
  - a custom `WKURLSchemeHandler` for `sweetbriar-cid:` serves only
    resources from the current message's lease;
  - a navigation generation counter drops responses for a previously
    shown message.
- **Remote content** (images, CSS, fonts that would load automatically)
  is always blocked in the pilot. A "Load remote content" option, per
  message or per sender, is *later*: it needs a separate rendition and
  rule-list policy.
- **Hyperlinks** stay clickable. A click opens the link in the default
  browser after the warnings below; the reader itself never navigates.
- **Plain text** renders natively in an `NSTextView` (TextKit 2):
  literal, no Markdown interpretation, with quotes styled by depth.
- **Reading view:** a toggle, off by default. It renders the semantic
  document model for supported structures only (12-markdown-first).
  - Per-category defaults wait for the fidelity corpus (D16).
  - "Sender layout" and "View source" are always one click away.
- **Header area:**
  - from, to and date;
  - the "Received in <account>" chip in the account color with its
    label;
  - tags and the note.
- **Links** show their real destination on hover. A warning appears
  when:
  - the link text's domain differs from the target's;
  - the target is a raw IP address;
  - the target URL contains user info (`user@host`).
  - IDN confusable checks come later.

## Composer

- **Built with** an `NSTextView` (TextKit 2) in plain or Markdown mode.
  Markdown gets live styling, with source always visible.
- **Above the editor:**
  - the **identity strip** (below);
  - To, Cc and Bcc token fields, with completion from `seen_addresses`
    and optional macOS Contacts;
  - Subject.
- **Saving:**
  - autosaves 1.5 s after typing stops, and on blur and quit;
  - each save is a new draft revision (02);
  - "Saved" shows only after the receipt.
- **Attachments** are dragged in or picked. Each is published to the
  object store before the revision that references it.
- **Paste:** pasting HTML offers "Paste as Markdown" with a preview and a
  lossy-conversion warning; plain paste is the default.
- **Sending** shows the Out Box entry. During the undo window (default
  7 s, set in settings) **Undo Send** cancels it.
- **Composer probes (M0):** CJK marked text, right-to-left text, and undo
  across formatting. Pasted HTML, inline images and the external editor
  come in M4.

### Identity strip and account colors

Implements [15-account-colors](../research/15-account-colors.md):

- An `NSTitlebarAccessoryViewController` with `layoutAttribute = .bottom`
  hosts a custom-drawn `IdentityStripView`.
- **Size:** at least 32 pt tall, growing with Dynamic Type-like text
  size.
- **`fullScreenMinHeight`** is set to the strip's height, so it stays
  visible in full screen.
- **Contents:** monogram, account name and the full From address, as in
  "**W** Work · Alex Example <alex@company.example>". The whole strip is
  one accessibility element with that label.
- **Colors** come from tokens the core derives (06 of research 15):
  - opaque restrained fill and saturated monogram;
  - foreground chosen per surface for ≥ 4.5:1 contrast;
  - separate light, dark and high-contrast variants.
- **The From control** is an `NSPopUpButton`: a swatch in each item's
  image and full addresses in titles. Provisional identities are marked.
- **Switching From:**
  - updates the strip, title, signature and Sent behaviour in one view
    model transition;
  - crossfades in 150 ms, or instantly with Reduce Motion;
  - posts one accessibility announcement.
- **Strong mode** (Settings → Appearance) tints the upper window via
  `backgroundColor`, `titlebarAppearsTransparent` and
  `.fullSizeContentView`. The editor stays neutral.
- **Cross-account check:** replying through a different account than the
  one that received the original shows an inline banner with **Use
  <account>** and **Keep <account>**. An unacknowledged mismatch is
  confirmed at Send. Validation runs in the core (05).
- **Window tabs** get an `NSWindowTab.accessoryView` swatch (P2, later).
- **Accessibility settings honored:**
  - `accessibilityDisplayShouldDifferentiateWithoutColor`: stronger
    monogram, symbol and outline;
  - `…ShouldIncreaseContrast` and `…ShouldReduceTransparency`;
  - Dark Mode.

## Keyboard filing ("type to file")

`⇧⌘M` (Move to…) opens a filing palette: an `NSPanel` with a search field over
mailboxes, ranked by recent use per account. Return moves the selection,
and the undo toast offers **Undo** for 10 s.

## Menus and commands

- **Every action is a command** with a stable identifier, in the
  `com.sweetbriar.cmd.*` namespace, wired to the menus and the responder
  chain.
- **Versioning:** the persisted intent and query formats are versioned
  now (research debate/04, resolution change 15). The scripting surfaces (AppleScript, App Intents, a CLI) are
  *later* and will attach to these commands.

## Debug menu (pilot)

The pilot carries a **Debug** menu in every build, so owner checklists
run on the CI artifact.
- **Fill 500,000 Synthetic Rows** calls `debug_fill_synthetic` (07).
- **List Prototype** (plan M0.10) opens a window listing
  `DebugSynthetic` through a `ListWindowModel`.
- **List Implementation** toggles that window between AppKit
  `NSTableView` and SwiftUI `List`, and is stored in user defaults.
- **Run List Benchmark** runs the prototype's benchmark (plan M0.10) on
  this Mac, for both implementations, and shows the numbers to copy.
  Launching the app with `-runListBenchmark <report.json>` runs the
  same benchmark, writes the report there and quits (macOS CI uses it).

## Accessibility (pilot gates)

Each must be verified before the milestone closes:
- VoiceOver can complete reading, replying, switching From, filing and
  inspecting the Out Box without a pointer.
- Full keyboard access.
- Correct rendering in Increase Contrast, Reduce Transparency and Dark
  Mode.
- Right-to-left and Japanese input in the composer.

## Performance budgets (pilot)

Measured on the owner's Mac (research 03); each is a p95:

| Interaction | Budget |
|---|---|
| Warm launch | 400 ms |
| Open a cached message | 100 ms |
| Archive or delete | 50 ms visible, with the durable intent within 100 ms |
| Search results after the last keystroke | 100 ms |
| Open compose | 100 ms |

An opt-in local latency log records these. Nothing leaves the machine.
