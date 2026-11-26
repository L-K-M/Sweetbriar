# Sweetbriar: review and backlog

Produced 2026-11-26 from a full read of `origin/main` at `19fde48e`
(macOS app, Swift bridge, Rust core hot paths). The codebase is
mature and heavily tested; the open items below are therefore mostly
polish, accessibility, missing user-facing features and small edge
cases, not structural defects. Each item carries file/line evidence
and a sketch of the fix so a future agent can pick it up directly.

**How to use this document.** Items marked **done** shipped in this
pass; do not redo them. Items marked **spec-first** need a spec change
before code, per `plan/README.md`. Everything else is shovel-ready in
the order listed within its section. Confidence is **verified** (read
and traced), **likely** (read, not run), or **idea** (design
proposal).

## Done in this pass

- **Tag overflow (`PR #572`).**
  Message rows drew at most three tag chips and silently dropped the
  rest (`AppKitPrototypeList.swift`), while the SwiftUI prototype drew
  every tag. Both lists now share `MessageRowMetrics.visibleTagCount`
  and `MessageRowContent.visibleTags`/`hiddenTagCount`; the AppKit cell
  draws a `+N` chip when room allows and hides it rather than overlap
  tags on narrow rows. VoiceOver hears the collapsed count. Tests:
  `MessageRowPresentationTests`, `narrowRowsKeepEveryChipInsideTheRow`.
- **Keyboard shortcut reference (`PR #574`).** Help had no content
  beyond AppKit's search field. Added Help → Keyboard Shortcuts…
  (`MainMenuBuilder.ItemID.showShortcuts`) opening one shared
  `ShortcutHelpWindowController` whose `ShortcutCatalog` mirrors the
  menu bar, grouped by menu. Tests: `ShortcutHelpTests`.
- **First-run welcome (`PR #577`).** A fresh archive showed an empty
  Unified Inbox with no route forward. `SidebarModel.isFreshArchive`
  detects "no account beyond the implicit local archive and no imported
  mailboxes"; `MessageListViewController` then covers exactly that
  empty inbox with `WelcomeEmptyState`, offering the existing Add
  Proton Account and Import commands. Any other window, search, error
  or row hides it. Its headline is deliberately "No messages" so the
  existing XCUITest empty-state expectations keep matching.

## Bugs and polish worth fixing next

### N1. Notifications cannot be clicked and have no actions

- **Where:** `apps/macos/Sweetbriar/NewMailNotifier.swift`
  (`UNUserNotificationCenterPoster.post`; no category, no delegate)
  and `AppDelegate.swift` (no `UNUserNotificationCenterDelegate`).
- **What:** notification `userInfo` carries only `accountId` and
  `count`; clicking does nothing; there are no Mark Read/Reply/View
  buttons. `NewMail.messageIds` already exists
  (`SweetbriarBridge/NewMail.swift`), so a single-message arrival can
  name its message.
- **Fix:** install a delegate; make the default click reveal the main
  window and select the account's Inbox (or the message when the core
  can resolve the id); add a small action set if time allows. Keep the
  existing redaction rule.
- **Effort:** M. **Confidence:** verified.

### N2. Find does not work in an HTML message

- **Where:** `MainMenu.swift` `StandardTextMenus.find()` reaches the
  focused text view; the HTML reader is a plain `WKWebView`
  (`Reader/HTMLReaderView.swift`) and the comment notes it is excluded.
- **What:** most real mail is HTML, so ⌘F fails exactly where users
  expect it. Plain-text and Reading views work through their find bars.
- **Fix:** drive `WKWebView.find(_:configuration:)` behind a reader
  find bar with Next/Previous, or forward the Find menu action to a
  reader-level handler. Keep generation-ticket checks so a late find
  result cannot touch the next message.
- **Effort:** M. **Confidence:** verified.

### N3. Print is absent

- **Where:** File menu in `MainMenu.swift`; no `print` implementation
  anywhere under `apps/macos/Sweetbriar`.
- **Fix:** File → Print (⌘P) over the shown message. Reuse the plain
  text for plain/Reading renditions; render or snapshot the web view
  for sender-layout HTML. Offer the list selection too if cheap.
- **Effort:** M. **Confidence:** verified.

### N4. No Quick Look for attachments

- **Where:** `Reader/ReaderViewController.swift` attachment buttons
  call `openAttachment` → `NSWorkspace.open`;
  `SweetbriarBridge/ReaderModel.openAttachment` returns a quarantined
  file kept until core shutdown.
- **Fix:** handle Space/`QLPreviewPanel` with that URL, retaining the
  lease until the panel closes; keep the existing Open command as the
  fallback.
- **Effort:** M. **Confidence:** likely.

### N5. List typography ignores text-size preferences

- **Where:** fixed `13/12/11/10` pt fonts in
  `MessageRowCellView` (`AppKitPrototypeList.swift`),
  `ComposerTextView.swift:117`, and the reader header
  (`ReaderViewController.swift:541`). System text-size/Dynamic Type has
  no effect.
- **Fix options:** adopt `preferredFont(forTextStyle:)` with a scaled
  row pitch, or add an explicit list density/font-size preference.
  `MessageRowMetrics.pitch` drives the window model's placeholder
  geometry, so font and pitch must change together.
- **Effort:** M-L. **Confidence:** verified.

### N6. Row layout asks `intrinsicContentSize` during scrolling

- **Where:** `MessageRowCellView.layoutSubviews()` computes
  `accountField.intrinsicContentSize.width` per laid-out row when the
  account label shows (unified inbox and Search).
- **Fix:** measure with the latency log's scroll span first; if it
  shows, precompute the width in `configure(content:account:)`.
- **Effort:** S after measurement. **Confidence:** likely.

## Missing user-facing features

### F1. Signatures cannot be edited

`signature_source` exists end to end (`crates/sb-service/src/identities.rs`,
`upsert_identity`), but no command crosses the FFI boundary and the app
has no signature editor. **Spec-first/shovel-ready split:** export the
identity commands through `sb-ffi` + bindings, then add Settings →
Accounts → Signature per identity. Without it a user is stuck with a
synced signature.

### F2. Conversation/thread view is unreachable

`WindowSpec.thread(Int64)` exists (`Types.swift`, `CoreBridge.swift`)
and the sidebar even names its symbol, but no UI constructs a thread
window. **Shovel-ready:** a context-menu "Show Conversation" and/or a
list toggle that opens/sets the thread spec; core work is done.

### F3. Sort control for mailboxes

Window ordering is fixed to `sort_date` descending
(`crates/sb-store/src/lists.rs`); Search alone has relevance/date.
**Spec-first:** extend `WindowSpec`/list queries with sort keys and
backing indexes, then expose a list sort menu.

### F4. Trackpad gestures and drag affordances

`NSTableView` row actions are not implemented (no swipe to
Archive/Trash), sidebar drops are not spring-loaded, and rows expose
no hover quick actions. Filing already batches per account, so swipe is
mostly presentation.

### F5. Mark all as read

`set_flags` is capped at 1,000 ids; the shell has whole-window
selection tokens for large selections. A mailbox-wide mark-read needs a
dedicated preconditioned core intent, not a shipped id list.
**Spec-first.**

### F6. Send Later and Snooze

Both fit the durable-intent architecture (a scheduled outbox attempt;
a durable flag plus later re-insertion) but are absent from spec and
plan. Post-pilot, spec-first.

### F7. Per-mailbox notification controls

`notifications.previews` is global and every arrival posts. Mail users
expect per-account/per-mailbox muting and Focus-aware behaviour.
Needs settings plus routing rules.

### F8. Scripting surfaces

AppleScript, App Intents and a CLI are explicitly *later*. The menu
commands already carry stable `ch.lkmc.Sweetbriar.cmd.*` identifiers,
so App Intents are the natural next layer.

## Performance notes

The core hot paths are healthy: keyset paging with no `OFFSET`, the
`d_list_rows` covering projection with partial flagged/representative
indexes, a materialized window index, and a three-page cache with at
most two reads in flight (`ListWindowModel.swift`). Measure before
changing:

- **P1:** N6 above (row `intrinsicContentSize`).
- **P2:** `MainWindowController.listSaw` re-reads the whole sidebar on
  every newer list generation. During sync that can be frequent; the
  model coalesces, but consider debouncing or dirty flags when only
  counts change.
- **P3:** the reader intentionally holds both renditions for instant
  toggling; it is the dominant memory cost for huge HTML messages.

## Visual and aesthetic opportunities

- **A1. Icon and brand surface.** `media-sources/icon.png` is the
  master and the generated set is committed. A dedicated design pass
  should check 16/32/64 pt legibility, not just 1024, and the Dock /
  Finder / notification presence.
- **A2. List density.** One fixed 56 pt pitch with three text zones.
  Comfortable/Compact (tied to N5) helps smaller displays and makes the
  app feel considered. Scale pitch and placeholder geometry together.
- **A3. Consistent empty states.** Drafts, Out Box and the reader have
  empty states; unified inbox, zero-hit search and first-run differ.
  A shared component (symbol, title, secondary line, optional action)
  would lift the app. The first-run welcome above is one instance.
- **A4. Toolbar and chrome progress.** The toolbar follows Mail well.
  Consider a visible sync/filing progress treatment beyond the
  integrity banner, and a unified-inbox account total in the sidebar
  header. Preserve the Color Wash contrast guarantees if touching
  status colors.
- **A5. Delight candidates.** Markdown preview beside the composer
  source (live styled output via `MarkdownStyler`), word count, Quick
  Look (N4), and message statistics. Keep the keyboard cheat sheet
  linked from Help.

## Order suggested for a future pass

1. N1 notification click/actions.
2. N2 find in HTML.
3. N4 Quick Look.
4. N3 print.
5. N5/N6 typography and row layout measurement.
6. F1 signatures, F2 threads.

Spec-first items (F3, F5, F6, F7) start with a spec change.
