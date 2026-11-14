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

## Backups settings (M1.8j)

Settings → Backups uses only the Service API through the hand-written
bridge. It lists the recorded generations with their state, dates, size
and latest verification verdict (or “Not checked”), and the retained
restore paths (07). Listing a generation is not verification of its
bytes; every restore verifies again. Backup rows, retained names and
preferences are independent query answers. Publish each whole answer
within the current archive epoch and show failures for its own section;
never turn a failed query into an empty success. In particular, unreadable
or corrupt preferences must not hide a successfully listed recovery
point or prevent its freshly verified restore, even on a read-only core.

Back up now starts a keyed generation and shows its copy's eventual
result. A complete generation with a retention warning remains complete;
a stopped or failed copy is never presented as a usable recovery point.
The destination defaults to the data root's `backups/`, with a choice of
another directory. An unavailable chosen volume is an error, not a reason
to fall back to the default.

Restoring requires explicit confirmation naming the generation and its
date. Explain that it replaces the live archive with that recovery point,
keeps the replaced root intact, pauses every account and quarantines
pending work. Nothing is sent, moved or deleted automatically. Preparation
is cancellable while the live archive stays usable; after requesting a
cancel, keep awaiting the call's outcome. A failed receipt after a swap
is reported as “restored, receipt not recorded”, not as a rollback or a
reason to retry automatically. A closed-core failure requires reopening
the app to settle the restore marker. Open content is invalidated after
a swap; no replaced-store content may be saved back automatically.

Preserved roots and stages remain listed, including after relaunch. Show
their paths for deliberate manual filesystem removal, with a warning that
preserved roots can hold the listed backup generations and a stage may
still be in use. Their names do not prove ownership, completeness or
readiness to restore. Refreshing settings deletes nothing and never reuses
a stage.

A read-only integrity finding is shown in the main window with its detail,
time and evidence-copy path when available, and an action to open Backups.
Subscribe to health before reading it and read again after each event,
including the event publishing the copy path. Back up now and other writes
are unavailable, but listing and restore remain available. After a restore,
read health and Search coverage again; do not leave a stale read-only note.

The shared bridge-owned Backups model (M1.8j5) serializes its commands
and binds confirmation to an immutable complete generation's id/date.
An old verification verdict never substitutes for the restore's fresh
verification. A replay without a copy handle is not a completed backup.
Retained names and errors are reported without filesystem inspection or
cleanup by the model.

A restored or receipt-unrecorded outcome invalidates replaced-store state
exactly once even when the requesting task was cancelled. Stale reads
from before that swap cannot overwrite new state. Re-subscribe to health
and read again after a swap or a reset, accepting lower generations.
Refresh from an uncancelled task after command completion; cancelling a
copy wait says only that waiting stopped, not that the copy stopped.
Before shutting down the shared core, cancel and await the model's calls
and health watcher. Closing Settings alone does not end them.

The shell (M1.8j6a) retires pending opens as well as existing handles
before creating replacement panes. Recreate appearance, sidebar, list,
Search, index controls and reader models after a swap: a restored store's
generations can be lower. A closed-core outcome retires content without
reopening it. The Core-free Composer Probe is not an archive editor.
Restore is unavailable while a Core-backed diagnostic benchmark or
synthetic fill runs; explain the reason instead of allowing that work to
continue across a swap. Core-backed diagnostics and admitted compose opens
or windows exclude each other in both admission orders. Releasing one of
several compose windows does not re-enable diagnostics while another remains.

Settings has Appearance and Backups tabs; the main health banner opens
Backups directly. Both surfaces use the same model even if Settings opens
before the main panes finish installing. While a restore is pending,
remove the appearance editor, not just its save buttons. Recreate that
editor after the awaited shell reset, so neither its model nor its
selected account/revision comes from the replaced store. A read-only or
closed core cannot save appearance or destination changes.

At restore admission, synchronously fence every Core-backed UI writer,
including import, account setup and index controls, before scheduling the
native restore. Cancel unbegun UI calls; keep awaiting begun commands to
their receipts. The confirmation warns that open setup/import dialogs
close; detaching their UI does not itself cancel a durable import job.
A refusal leaves the original archive usable and admits fresh UI actions,
never a delayed write from the retired context. An admitted compose open
or window blocks restore: ask the user to close compose windows first.
Closing must acknowledge their displayed edits, or await the explicitly
confirmed Close Without Saving path below, before releasing admission.
Neither unsaved text nor a delayed draft save may cross a swap.

## Sidebar

- **Unified section:** Inbox (`Unified(inbox)`), Flagged (`Flagged`),
  Drafts (local) (`drafts()`), Out Box (`out_box()`), then saved views
  (07). Each account's own synced Drafts mailbox appears under the
  account.
- **One section per account,** with its colored monogram and name; its
  mailboxes are below, folders and labels grouped as Bridge exposes them
  (04).
- **Built with** an `NSOutlineView`, source-list style.
- **Mailboxes** come from `mailboxes(account_id)` (07), with unread
  counts. Imported mailboxes are flat, named after their sources; no
  role or hierarchy is read into a file name. IMAP hierarchy uses the
  server's delimiter (M2). A mailbox that isn't selectable opens no
  window.
- **Until their steps land,** Drafts (local), Out Box and saved views
  are shown disabled with a note, and the reader is a placeholder
  (M1.5). Before M1.10's appearance service, accounts show a neutral
  symbol and their name; no color is derived from an id.

## Message list

- **Built with** `NSTableView` (view-based), backed by a `ListWindowModel`
  over a core window (07). Decided by M0.10's prototype on the owner's
  Mac (500,000 synthetic rows, a 120 Hz window; PR #16):
  - SwiftUI `List` failed 25 of the 26 cold decision bars. It showed
    its first rows after 92 s, peaked at 3.9 GB, and took 938 ms at p95
    to answer wheel input and 5.8 s for jumps (4 never rendered). It
    lost focus during its stalls, so its failed clicks aren't counted,
    and the warm comparison is empty: its warm run stopped after first
    rows, its window no longer key;
  - AppKit passed the benchmark's checks. It showed its first rows in
    170–207 ms, answered input within 32–42 ms at p95, and gave click
    feedback in under 50 ms.

  With SwiftUI failing the benchmark, the owner's checklist couldn't
  change the outcome; it raised no issue. macOS CI's diagnostic runs
  agreed. AppKit still misses two budgets below: scroll updates took
  6.1–7.3 ms of main-thread CPU at p95, and first rows exceeded 100 ms.
  M1.3 and M1.4 close both (plan).
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
    per frame, the first after an idle frame at once). A row not yet
    cached shows as a placeholder of the same height. `ResetRequired`
    rebuilds the model on its existing handle:
    the id index, then the pages. The prototype has no subscriptions
    (M1.3 adds them), so after a fill the Debug menu hands the new
    generation to every open prototype model, which rebuilds.
  - Selection is held by `occurrence_id`, so a 10k-row selection
    fetches no rows.
  - **Opening a store window** shows its first page at once, as a
    provisional prefix, before `window_ids` completes; rank jumps and
    range selection wait for the exact index. The index and pages
    shown must share a generation.
  - **Invalidation** (07 Subscriptions) refreshes the id index on the
    same handle, then re-reads the pages around the visible and
    selected rows. A page whose rows the new index keeps at their ranks
    stays shown until its re-read lands; other pages show placeholders,
    since the cache holds whole pages only. An
    invalidation during a refresh schedules another. One id fetch and
    one pending `next` run beside the two page reads.
  - **In logical-message windows** a selected id the new index lacks is
    looked up with `Around(old_id)` and limit 1 before it is dropped,
    so the selection follows its message (07); only `NotFound` removes
    it. These lookups are the one exception to 200-row pages.
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

## Search (M1.6)

- The main window's search field uses the core parser/formatter (06, 07).
  Chips represent AST clauses; editing text or chips preserves grouping,
  negation, explicit timezone/clock and ids. Complex Boolean groups remain
  editable text, never flattened into a different query. No Swift parser.
- Scope controls choose All accounts, one account or one mailbox; default
  All accounts. Names are presentation only, ids are sent to the core.
  Sort is Relevance (default) or Date. A scope chip is distinct from an
  `in:` membership clause; show both when both constrain the query.
- Debounce committed text by 30 ms; marked text is not submitted. An invalid
  edit shows an inline error at the returned UTF-8 span (converted to Swift
  string/UTF-16 indices), retaining the previous results labelled with their
  last valid query. Clearing the field closes search and restores the
  preceding mailbox/list selection. A new query/scope/sort takes a request
  ticket, cancels the old request and closes its handle. Late completions
  cannot replace current results; any late-opened handle is closed.
- Use the production message list and reader. Search requests 50 rows for
  the first page, then the normal id index and 200-row pages. Until the
  index arrives, show those first rows without waiting for it. Selection
  follows logical message through invalidation (07); reopen at Top if the
  selected message is no longer a hit. Date order stays strictly chronological.
- Show scoped coverage below results whenever `indexed < total_messages`,
  and unknown exclusions whenever nonzero, including zero-hit searches:
  "312 messages not yet searchable". Expose indexed/partial/failed/pending,
  acquired bodies, headers and queued documents with their denominators;
  partial and failed are not complete. Counts come from the page's Search
  status, never a later snapshot attached to older rows. Queue/rebuild
  progress is visible and updates on coverage-only invalidation. Search
  coverage details provide Rebuild, Cancel and Resume controls through 07's
  commands and durable job id, plus Retry now for an I/O pause. Show phase,
  committed phase count and global pending-document count from rebuild
  progress (not the scoped queue count); cancelled work is labelled
  paused until Resume, including after relaunch. An I/O pause shows its
  actionable error and next retry time rather than a running spinner. Poll
  standalone coverage while reset/enqueue makes Search unavailable, showing
  phase/progress and controls there too. Starting a rebuild is explicit;
  closing a window never cancels the job. When
  Search is unavailable because its extractor target is newer than the
  app supports, show standalone coverage's blocked state and the required
  app update, rather than a queue that appears to run forever. On a
  read-only archive (07's `ReadOnly` index state) say that indexing
  stopped and why, with a stopped rebuild's phase, and offer no index
  control: none can work until a restore.
- Show "Attachment contents are not searched; CJK word search is limited"
  in search coverage details even with complete indexing. Empty scopes show
  zero messages, never a misleading incomplete percentage. No automatic
  server search, fuzzy expansion or language-query rewriting in the pilot.

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
    shown message: every selection change takes a new ticket, and
    every body, resource, link and error completion checks it before
    it shows anything; a stale lease is released.
- **Remote content** (images, CSS, fonts that would load automatically)
  is always blocked in the pilot. A "Load remote content" option, per
  message or per sender, is *later*: it needs a separate rendition and
  rule-list policy.
- **Hyperlinks** stay clickable. A click opens the link in the default
  browser after the warnings below; the reader itself never navigates.
  Only a user-activated click on the current message opens anything;
  redirects, frames, downloads and other schemes are refused, and
  `target=_blank` follows the same rule.
- **Opened attachments** stay in the core's bounded, quarantined lease
  cache until core shutdown. An external application's launch acceptance
  does not mean it has read the file. Selection changes, pane close and
  model destruction therefore release body leases, not attachment files
  already handed out. Stale attachment completions are released before
  handoff. The normal lease quotas (07) still apply; no untracked copies
  or timed eviction are used.
- **Plain text** renders natively in an `NSTextView` (TextKit 2):
  literal, no Markdown interpretation, with quotes styled by depth.
- **Reading view:** a toggle, off by default. It renders the semantic
  document model for supported structures only (12-markdown-first).
  - Per-category defaults wait for the fidelity corpus (D16).
  - "Sender layout" and "View source" are always one click away.
- **Header area:**
  - from, to and date;
  - the "Received in <account>" chip in the account color with its
    label, from the selected occurrence's account. Until M1.10's
    appearance service the chip is neutral, with its label;
  - tags and the note, read-only until the metadata commands.
- **Links** show their real destination on hover. A warning appears
  when:
  - the link text's domain differs from the target's;
  - the target is a raw IP address;
  - the target URL contains user info (`user@host`).
  - IDN confusable checks come later.

## Composer

- **Built with** an `NSTextView` (TextKit 2) in plain or Markdown mode.
  Markdown gets live styling, with source always visible.
  - The styling is presentation: derived from the source on each edit
    and never stored in the text. Undo restores text and styling
    together, and text an input method is composing (marked text) is
    never restyled mid-composition.
  - Formatting commands (⌘B bold, ⌘I italic) insert or remove Markdown
    markers as one undoable edit.
  - Nothing touches TextKit 1 (`layoutManager`), which would drop the
    view into compatibility mode.
- **Above the editor:**
  - the **identity strip** (below);
  - To, Cc and Bcc `NSTokenField` controls, with completion from
    `seen_addresses` and optional macOS Contacts. Native parsing owns
    mailbox syntax; quoted commas and display-name/address spelling are
    preserved. Incomplete, pending or invalid fragments remain visible
    and unsaved, including while an earlier revision is acknowledged.
    They pause autosave and refuse Save, close and quit until committed
    and validated; explicit Close Without Saving remains available.
    Quiet-period validation and native token commitment must not move
    keyboard focus or commit marked text. Async answers are versioned:
    old parsing/completion never replaces later recipient or body edits.
    Blur commits and validates the displayed input before flushing.
  - Subject.
- **Saving:**
  - autosaves 1.5 s after typing stops, and on blur and quit;
  - each save is a new draft revision (02);
  - "Saved" shows only after the receipt. Compare source and field buffers
    by their UTF-8 bytes, not Swift's canonical-equivalence string equality:
    normalization-only edits must reach a new revision, and a clean reload
    must display the exact source bytes it loaded.
  - Close and quit freeze edits, await pending opens and flush. A failure
    or conflict keeps the affected window open and refuses quit, with an
    actionable status and an explicit save retry; never discard unsaved
    text or retry conflicts automatically. Close Without Saving requires
    explicit confirmation: it drops only unsaved editor changes, keeps
    the last saved revision, and does not call `discard_draft`. It must
    await any begun save before closing; an acknowledged save is not
    rolled back. Its confirmation pauses the quiet-period timer and
    must not trigger a blur save itself; Keep Editing restarts the timer.
    Normal close/quit dismisses that owned prompt without discarding and
    flushes instead. This path must remain usable when a read-only core
    refuses saving, so recovery is not trapped behind a compose window.
    A begun save is awaited, not cancelled because its waiter was cancelled.
  - M1.9c2a first wires subject and plain/Markdown body editing. Existing
    recipients, route and attachments survive those edits unchanged;
    their editable controls, Drafts navigation and complete native UI
    acceptance follow in c2b–c2e. Sending remains unavailable until M4.
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
  The core's light/dark bundles contain an opaque strip, marker,
  foreground and high-contrast outline. On the final 8-bit sRGB values,
  foreground reaches 4.5:1 and marker/outline 3:1 against the bundle's
  strip. The shell must also verify contrast on its actual surfaces,
  including selected rows; a strip ratio alone does not qualify the UI.
- **The From control** is an `NSPopUpButton`: a swatch in each item's
  image and full addresses in titles. Provisional identities are marked.
- **Switching From:**
  - updates the strip, title, signature and Sent behaviour in one view
    model transition; sending/Sent effects remain M4;
  - signature source (02: Markdown or plain text) follows the draft's
    selected format, with no implicit conversion or format switch. Never
    guess signature delimiters in saved/user-edited text. Replace or remove
    only an exact insertion this editor owns and which has not been edited
    since. Otherwise offer Keep Text as the safe default or explicit append;
    fence that decision against intervening editor changes. Existing saved
    nil/unavailable routes remain; only a fresh draft without an explicit
    route choice may take its captured account/global default;
  - prepare displayed input before the explicit choice and refuse marked
    or invalid input without changing the route. Native text acceptance
    precedes model mutation, without an event-loop gap; route and body
    share one native undo step, preserving later unrelated fields. Signature
    choice sheets pause autosave and owned-sheet blur saves. Normal close
    cancels an unresolved proposal and saves the existing content; explicit
    abandonment still drains begun work without starting another write;
  - crossfades in 150 ms, or instantly with Reduce Motion;
  - posts one accessibility announcement.
- **Strong mode** (Settings → Appearance) tints the upper window via
  `backgroundColor`, `titlebarAppearsTransparent` and
  `.fullSizeContentView`. The editor stays neutral. The existing
  `appearance.compose_cue` setting is a typed Standard/Strong preference;
  missing or malformed answers are explicit errors, never a guessed mode.
  Publish a user change only after its receipt, retain an explicitly stale
  last value on failure, and prevent an older read from undoing that change.
  Settings writers use current admission and their frozen editor tickets;
  restore/quit retire and await preference work before replacing its owner.
  A synchronous fence prevents new preference owners and writes during
  restore. The restore command waits for the frozen cohort to drain before
  entering the native restore; cancellation during this wait does not issue
  the command. Every concurrent reset/stop waiter joins the cohort's actual
  read/write and initial-load drains. A later freeze or permanent stop
  defeats an older reset; a closed archive or accepted quit cannot revive
  an owner. A restore that keeps the current archive, including refusal,
  replay or request failure, releases its fence after retirement instead of
  leaving preferences permanently unavailable. Refusing compose quit leaves
  preference owners usable.
  Appearance changes must not edit draft source or its undo history.
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
  `ch.lkmc.Sweetbriar.cmd.*` namespace, wired to the menus and the responder
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
  - Its **store mode** (M1.4), the **Run Store Benchmark** item and the
    `-runStoreBenchmark <report.json>` launch argument, runs the
    production sidebar and list over an imported mailbox: 30
    alternating mailbox switches, then the synthetic run's scroll and
    selection sequences. It never writes to
    the store and stops if the list generation moves. Switch timing
    runs from the sidebar click to the first frame showing the new
    rows; only nonempty mailboxes count toward first-row samples.
  - Smaller mailboxes use in-range selection endpoints and report the
    span reached; a 500k scroll or 10k selection result needs a mailbox
    that large. Refresh rate and display come from each run's window;
    a run whose window changes display is discarded.
  - The report keeps functional checks apart from budget verdicts. CI
    numbers are diagnostic; the owner's Mac decides (plan O9).
- **Composer Probe** (plan M0.11) opens a compose window: the composer's
  editor in plain or Markdown mode, without the core.

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
