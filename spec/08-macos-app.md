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
    in 01.

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

- **Unified section:** Inbox, Flagged, Drafts, Out Box, then saved views.
- **One section per account,** with its colored monogram and name; its
  mailboxes are below, folders and labels grouped as Bridge exposes them
  (04).
- **Built with** an `NSOutlineView`, source-list style.

## Message list

- **Built with** `NSTableView` (view-based), backed by a `ListWindowModel`
  over a core window (07). The choice between AppKit and SwiftUI was
  settled by a prototype in M0; this spec assumes AppKit unless the
  prototype says otherwise.
- **Rows:**
  - sender, subject, snippet, date;
  - unread, flagged, attachment and tag markers;
  - the account marker, a 4 pt colored rail plus monogram, in unified
    and search views.
- **Selection:**
  - native selection colors; the account color never replaces them;
  - selection is held by `occurrence_id`;
  - for bulk actions on more than 1,000 rows, the list passes a
    `SelectionToken`.
- **Keyboard:**
  - arrows, Return to open, Space to page the reader;
  - `⌘⌫` delete to trash, `⌃⌘A` archive;
  - type-to-file (below);
  - every command is also in a menu.
- **Performance budget:** switching mailbox shows the first rows within
  100 ms at p95. Scrolling a 500k-row mailbox spends < 5 ms of main-thread
  work per update.

## Reader

- **HTML:** sanitized sender layout, shown in **one reused `WKWebView`**:
  - JavaScript off;
  - `WKContentRuleList` blocks every network load;
  - a custom `WKURLSchemeHandler` for `sweetbriar-cid:` serves only
    resources from the current message's lease;
  - a navigation generation counter drops responses for a previously
    shown message.
- **Remote content** stays blocked by default. "Load remote content"
  applies per message or per sender; the choice is stored in settings,
  never inferred.
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
- **Links** show their real destination on hover. A mismatch between the
  link text's domain and its target domain shows a warning.

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
  come in M3.

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

`⌃⌘F` opens a filing palette: an `NSPanel` with a search field over
mailboxes, ranked by recent use per account. Return moves the selection,
and the undo toast offers **Undo** for 10 s.

## Menus and commands

- **Every action is a command** with a stable identifier, in the
  `com.sweetbriar.cmd.*` namespace, wired to the menus and the responder
  chain.
- **Versioning:** the persisted intent and query formats are versioned
  now (D14). The scripting surfaces (AppleScript, App Intents, a CLI) are
  *later* and will attach to these commands.

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
