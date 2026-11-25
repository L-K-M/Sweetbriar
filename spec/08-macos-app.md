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
| **Message window** | A standalone reader for one message (Message window, below) |
| **Compose window** | One per draft |
| **Settings** | SwiftUI: accounts (rename, pause and resume, disconnect, sign in again, notification previews; M2.7), appearance, sending safety, backups |
| **Activity / account health** | SwiftUI: per-account state and condition with "Sign in again" where it applies, coverage and, for a Proton account, "Proton messages synced" with each mailbox's count against Proton's (04-sync-proton "Coverage"), breaker events with Accept and Keep, parked and quarantined intents, the Recovery list, and the Out Box (M2.7; the Out Box with M4.4, intent actions with M3.1) |
| **Import report** | Results of an import job (09) |

State restoration uses AppKit window restoration. A compose window
restores its draft by `draft_id` (M1.9c2d): only the id is encoded,
never content. Restoration waits for the archive to open and admits the
window like any compose open (one window per draft, below). A draft
that is no longer `editing` or `queued`, an archive that can't open, or
a quit or restore in progress restores no window and reports nothing:
restoration is a convenience, not a promise.

### Main window chrome

The main window is laid out as Mail's on macOS 26:

- **Full-height sidebar.** The content runs under a unified toolbar
  (`.fullSizeContentView`), so the sidebar's glass reaches the top of
  the window. The toolbar's sidebar button and a tracking separator sit
  over the sidebar; the other items follow the message list.
- **Title and subtitle.** The title names what the list shows (the
  chosen mailbox or view, "Drafts (local)", "Out Box" or "Search"); the
  subtitle gives its account and its count once exact, with the unread
  count where the sidebar shows one ("Work · 240 messages, 3 unread"),
  or a search's result count. The window keeps the accessibility
  identifier `Sweetbriar` whatever its title.
- **Toolbar.** New Message; Reply, Reply All and Forward; Archive, Move
  to Trash and Move to…; a Labels menu with Add Label… and Remove
  Label…; Mark as Read and Star; then the search field. Each item is a
  command the menus also carry, under the same identifier, and is
  enabled exactly when that command is: a group validates each of its
  items. Read and Star show what a click does ("Mark as Unread",
  "Unstar"). The toolbar is customizable, its configuration and the
  window's frame and split positions are saved, and a narrow window
  collapses the search field to a button and moves the items that don't
  fit into the overflow menu.
- **Integrity banner** sits under the toolbar as a titlebar accessory,
  takes no height while there is nothing to report, and never sets the
  window's width.

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
Below it, the pane states 10's encryption at rest: Sweetbriar doesn't
encrypt the archive or its backups and relies on FileVault, so a backup
on another disk needs that disk encrypted.

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

## Proton sign-in (M2.14)

File → Add Proton Account… opens a sheet driving 07's sign-in
conversation; the Bridge account sheet is no longer offered.

- **Before the first sign-in,** the sheet says plainly: Sweetbriar talks
  to Proton directly; Proton doesn't officially support third-party
  apps, may ask you to verify that you are human, and could block this
  access; your password is used to sign in and is never stored.
- **Steps:** display name, username and password; then, as Proton asks,
  a six-digit code field (with paste), the mailbox password, or human
  verification. Each step shows Proton's refusal inline and keeps what
  was typed except secrets. Cancel ends the conversation
  (`cancel_proton_sign_in`).
- **Human verification** opens a child sheet with a non-persistent
  `WKWebView` limited as spec/10 says. It closes when Proton hands back
  the token, and the conversation continues.
- **A security-key-only account** gets the refusal's explanation and a
  link to Proton's 2FA settings page in the default browser.
- **Signing in again** (from the account's health row, "Sign in again",
  and the account's sidebar context menu, "Sign In Again…") opens the
  same sheet for that account, with the username fixed.
- **Verifying during sync** (04-sync-proton "Human verification during
  sync"): while the engine holds Proton's challenge, the account's
  health row offers "Verify…", which opens the human-verification sheet
  on its own, as a sheet of the Activity window. The token it closes
  with goes to `verify_proton_account`; closing it without one changes
  nothing. Without a held challenge the row offers "Try Again", which
  calls `verify_proton_account` without a token. A refusal shows on the
  row until the next attempt, and only while the account still asks to
  be verified.
- Password fields are `NSSecureTextField`s; their contents go to the
  Swift bridge (`SweetbriarBridge`, not Proton Bridge) as bytes and are
  cleared when the step is sent.

## Sidebar

- **Unified section:** Inbox (`Unified(inbox)`), Flagged (`Flagged`),
  Drafts (local) (`drafts()`), Out Box (`out_box()`), then saved views
  (07). Each account's own synced Drafts mailbox appears under the
  account.
- **One section per account,** with its colored monogram (or logo,
  "Identity strip and account colors") and name; its
  mailboxes are below: system folders, then folders (nested), then
  labels, as 04-sync-proton maps them.
- **Built with** an `NSOutlineView`, source-list style. Each entry
  leads with an SF Symbol in the accent color, as Mail's: its role's
  (Inbox `tray`, Sent `paperplane`, Drafts `doc`, Trash `trash`, Junk
  `xmark.bin`, Archive `archivebox`, All Mail `tray.full`, Starred
  `star`), else its kind's (folder `folder`, label `tag`); Flagged
  `flag`, Out Box `tray.and.arrow.up`, a saved view
  `folder.badge.gearshape`. An entry that opens nothing is dimmed,
  symbol included.
- **Sidebar sections** can collapse independently and remember their
  state across launches. Choosing a mailbox by command expands its
  section. The unread count sits in a capsule and stays in the row's
  accessibility label.
- **Mailboxes** come from `mailboxes(account_id)` (07), with unread
  counts. Imported mailboxes are flat, named after their sources; no
  role or hierarchy is read into a file name. IMAP hierarchy uses the
  server's delimiter (M2). A mailbox that isn't selectable opens no
  window.
- **Saved views** are listed in the unified section in `saved_views()`
  order, after Drafts (local) and Out Box. Choosing one opens a
  `View(id)` window (07). Its context menu offers Rename and Delete;
  Delete asks first. Rename keeps the view's stored query and sort.
- **Until their steps land,** Out Box is shown disabled with a note,
  and the reader is a placeholder (M1.5). Before M1.10's appearance
  service, accounts show a neutral symbol and their name; no color is
  derived from an id.

## Drafts (local)

M1.9c2d. Choosing Drafts (local) shows the local drafts list in the
list pane instead of a message list; the reader shows nothing while it
is up. Choosing any other entry brings its message list back.

- **Rows** are `drafts()` in the core's order (newest update first,
  `editing` and `queued` only): the subject or "No Subject", the To
  then Cc recipients by name or address (Bcc is not shown), "Queued"
  for a queued draft, and the update time.
- **From:** each row shows its sending identity as an identity chip
  (Account chips, with the identity's monogram and colors) labelled
  like the identity strip: the account, the name and the full address.
  A draft without an identity says "No sending identity". One whose
  identity the compose catalog doesn't name, or that couldn't be read,
  says "Sending identity unavailable", neutral, never a guessed address.
- **Opening:** double-click or Return opens the row's draft. There is
  one compose window per draft: opening a draft whose window is open,
  or already opening, brings that window forward and opens nothing
  else. Opening is a compose action, admitted like New Message (refused
  during quit or restore and while archive writes are unavailable). A
  draft that has since been sent or discarded opens nothing; the list
  reloads.
- **Refresh:** the list reloads when chosen, when a compose window opens
  or closes, and on Retry. One reload runs at a time; a reload asked
  for meanwhile runs once after it. A failed reload keeps the previous
  rows and shows the error with Retry.
- **Old archive:** the list retires with the rest of the main window's
  content on restore (above). A result that arrives after retirement is
  dropped and a retired row opens nothing; the rebuilt window lists the
  restored archive's drafts.

## Out Box

M4.4c. Choosing Out Box shows the Out Box in the list pane, as Drafts
(local) does; the reader shows nothing while it is up.

- **Rows** are `out_box()` (07): the live attempts, then a History
  group, each in the core's order. A row shows the frozen subject or
  "No Subject", the To then Cc addresses (Bcc is not shown; the frozen
  recipients carry no names), the state ("Sending in N s" while `held`, "Queued",
  "Account changed; confirm sending with the new settings" (05),
  "Sending", "Outcome unknown", "Failed",
  "Sent", "Cancelled", and for a resolved unknown "Marked as sent",
  "Sent again" or "Kept as draft") and the last update time.
- **From:** each row shows the route the attempt froze, on its own line
  apart from the state: an identity chip (Account chips) labelled with
  the account and the frozen sending address, from `out_box()`, not the
  identity's current address. Without the identity's appearance the
  chip is neutral; the address still shows.
- **Details:** the selected row's details show below the list: each
  recipient with its address field and state, and "Not end-to-end
  encrypted" for one Proton was asked to send in clear (05 step 1:
  an external address without keys), Proton's code and message
  (`final_reply`), the Proton evidence observed since launch, the
  Proton draft it left in Proton's Drafts (05 "Drafts on the server"),
  and after `accepted` the soft warning when the Sent copy has not
  appeared (05). Evidence is shown, never acted on by the shell.
- **Actions** act on the selected row and are buttons in the details
  and items in the row's context menu; each is offered only in the
  state that admits it:
  - `held` or `queued`: **Cancel Send** (`cancel_send`, 05 step 2);
  - `queued` waiting for confirmation: **Confirm and Send**
    (`confirm_send_route`);
  - unresolved `outcome_unknown`: **Mark as Sent**, **Send Again…** and
    **Keep as Draft…** (`resolve_unknown_send`, 05 step 6). Send Again
    asks first with the duplicate warning; Keep as Draft asks first
    with the warning that sending the same revision again may duplicate
    it. A resolution can't be changed;
  - an attempt with `rejected_*` recipients: **Retry Rejected
    Recipients** (`retry_rejected_recipients`). Only generic accounts
    have them (05), so Proton attempts never offer it.

  Each action runs under one idempotency key per user action, one at a
  time, until the reload after it lands; a call the core refuses or
  that fails shows its message under the details, and the list
  reloads. An action the pane refuses itself (archive writes are
  unavailable, like the composer's, another action is running, or the
  row doesn't admit it) calls nothing and shows nothing.
- **Refresh:** the list reloads when chosen, after each action, when a
  compose window closes, and on Retry; while it is shown and has live
  rows it also reloads every 2 s, so states move without input. One
  reload runs at a time, as for Drafts (local); a failed reload keeps
  the previous rows and shows the error with Retry.
- **Old archive:** it retires with the main window's content on
  restore, as Drafts (local) does; a retired row offers no action.

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
    For a single message, the context menu also offers Open in New
    Window, Reply, Reply All and Forward on that message.
- **Keyboard:**
  - arrows, Return to open the message in its own window, Space to
    page the reader (Message window, below);
  - `⌘⌫` delete to trash, `⌃⌘A` archive (Keyboard filing, below);
  - `⇧⌘U` mark read or unread, `⇧⌘L` star or unstar (M3.2). Each
    toggles the selection: when any selected row is unread (unstarred)
    it marks every row read (starred), otherwise unread (unstarred).
    The Message menu and the toolbar carry both commands. They act
    through `set_flags` (07), one call per account of the selection,
    and are disabled when the selection holds a local-archive row. A
    call's local effect commits with it, so its rows change at once; a
    refused or failed call shows its error and changes nothing, while
    the other accounts' calls still run and keep their effect. A
    mailbox window's whole selection of more than 1,000 rows passes
    its `SelectionToken`, and `selection_flags` (07) says whether any
    of its rows is unread (unstarred). Any other selection over 1,000
    rows leaves both commands disabled;
  - type-to-file (below);
  - every command is also in a menu.
- **Performance budget:** switching mailbox shows the first rows within
  100 ms at p95. Scrolling a 500k-row mailbox spends < 5 ms of main-thread
  work per update. Selection feedback takes under 50 ms at p95.

## Search (M1.6)

- The main window's search field is the toolbar's search item; Edit →
  Find → Search Mail (⌥⌘F) expands it and moves the keyboard into it.
  The strip below the toolbar (scope, sort, Save as View…, chips and
  the parse status) shows only while a search is under way. Escape
  clears the field, closes the search and hands the keyboard back to
  the list it returns to.
- The search field uses the core parser/formatter (06, 07).
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
- The search strip's **Save as View…** command names the current query
  in a sheet and saves it with the current sort through
  `upsert_saved_view`; the new view is selected in the sidebar. A saved
  view stores its query and sort, not the search's scope: `View(id)`
  runs the stored query over all accounts (07 Views). The name follows
  spec/02's bound; a failed save shows its error and changes nothing.

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
  - The toggle is remembered per sender: turning it on adds the
    message's sender address, lowercased, to `reader.reading_view_senders`
    (02), and turning it off removes it. Their later messages open in
    the Reading view when the core reports a supported document, and in
    sender layout otherwise. A full list (1,000 senders) refuses another
    with an inline note; the toggle still applies to the open message.
  - Switching is instant: while a message is shown, both its reading
    document and its sender-layout rendition are kept, so toggling
    fetches nothing. Links in the Reading view follow the same click
    rule and warnings as in sender layout.
  - In the pilot, images in the Reading view show as their alt text
    (or "Image" when there is none). Inline images show in sender
    layout; remote ones stay blocked in both.
  - Per-category defaults wait for the fidelity corpus (D16).
  - "Sender layout" and "View source" are always one click away.
- **Marking read (M3.2):** when the reader shows one unread message of
  a Proton account, it marks that occurrence read through `set_flags`
  as `reader.mark_read` (02) says: at once, after that many seconds
  while the same message stays shown, or never (`manual`). Another
  selection, a multi-row selection, a closed pane or marking the shown
  message unread cancels a pending mark. A message marked unread while
  it is shown stays unread until it is shown again. Settings → Reading chooses the setting:
  Immediately, After 2, 5 or 10 seconds, or Manually.
- **Empty pane:** with no message or several selected, the reader
  shows "No message selected" or "N messages selected" centered in the
  pane, as Mail's does; loading and error notes stay above the body.
- **Header area:**
  - the subject in bold, then from (the sender's name in bold), to, cc
    and the date where present, their labels and addresses in the
    secondary color; addresses always show beside names;
  - the "Received in <account>" account chip (Account chips), from
    the selected occurrence's account;
  - a tag picker and a multi-line note editor, borderless with an
    "Add a note" placeholder while empty. The picker applies and
    removes the message's tags through `add_tag`/`remove_tag` on the
    selected occurrence; typing a new name creates the tag, within
    spec/02's name bound. The note is saved on end-editing through
    `set_note`; empty text clears it. A failed write shows its error and
    leaves the previous tags and note.
- **Links** show their real destination on hover. A warning appears
  when:
  - the link text's domain differs from the target's;
  - the target is a raw IP address;
  - the target URL contains user info (`user@host`).
  - IDN confusable checks come later.

## Message window

A message window is the reader for one message on its own, with its own
`WKWebView` (Reader above).

- **Opening:** Return, ⌘O (Message → Open in New Window,
  `ch.lkmc.Sweetbriar.cmd.openMessage`) or a double-click on a row
  opens the row's message. Return and ⌘O need exactly one selected
  message row and do nothing otherwise; a double-click opens the row it
  hit. Drafts and the Out Box keep their own Return (above).
- **One window per occurrence:** opening an occurrence that already
  has a window brings that window to the front. A new window cascades
  from the last one opened, so it doesn't hide it.
- **Contents:** the main window's reader over that occurrence: the
  header area with its chip, tags and note, the reading-view controls
  and the link warnings. The title is the subject, or "No Subject".
  It marks the message read as the main reader does (Reader, "Marking
  read"), and closing it cancels a pending mark.
- **Tint** (M6.6d; owner decision 2026-11-24): in Color Wash and Strong
  modes (Composer, "Identity strip and account colors"), the title bar
  and the reader's header area take the receiving account's strip
  fill, with their labels in its strip foreground. The message body
  keeps the reader's own surface, since mail draws its own colors.
  Standard and Minimal leave the window neutral. The fill is opaque
  and solved for contrast, so, as in a Strong compose window, Increase
  Contrast adds the outline and Reduce Transparency changes nothing.
  When the account's appearance is unavailable the window stays
  neutral. The header's chip still names the account in text.
- **Commands:** while a message window is key, Reply, Reply All and
  Forward act on its message. The list's commands (mark read or
  unread, star, filing) belong to the main window and are unavailable
  there.
- **Paging:** in the main window, Space in the message list scrolls the
  reader down a page and ⇧Space up a page; in a message window, Space
  pages its reader.
- **Old archive:** message windows close when the main window's
  content retires (a restore, or a closed core); a result that arrives
  afterwards is dropped. Window restoration doesn't reopen them.

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
    A blank New Message starts with the keyboard in To.
  - A labeled Subject field.
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
  - **Keep Both (M1.9c2cd):** a conflicted window offers Keep Both. It
    settles begun attachment publications, then saves the window's
    content as the first revision of a new draft (same reply source; a
    forward can't be copied yet and is refused with an explanation),
    opens that draft in its own window and closes this one without
    another save. The draft the other window saved is unchanged. The
    copy has no base, so its stored references and spent handles are
    republished from this draft's revisions that store them; one with no
    known source refuses the copy before anything is written. A refusal
    before the copy's create began leaves the window editable. Once the
    create began, the window stays frozen with its text and the failure,
    and only an explicit Keep Both retries, with the same keys and
    content, so a lost receipt never makes a second copy. Keep Both is
    unavailable while archive writes are.
  - A save whose answer failed after it began may have committed. Its
    exact intent (key, base and content) is retried before any different
    save: a replay answers the lost receipt, otherwise that older content
    commits first. A conflict, or another refusal the core answers before
    writing (invalid input, not found, store version), means it never
    committed; so does a failure before the save began, such as
    republication. Those record no such intent. A same-intent retry sends
    the recorded content unchanged. One published handle appears at most
    once in the content.
  - M1.9c2a first wires subject and plain/Markdown body editing. Existing
    recipients, route and attachments survive those edits unchanged;
    their editable controls, Drafts navigation and complete native UI
    acceptance follow in c2b–c2e. Sending remains unavailable until M4.
- **Attachments** are dragged in or picked. Each is published to the
  object store before the revision that references it.
  - A file streams through `publish_draft_attachment_file` and joins the
    content the editor holds when it finishes, so edits made meanwhile
    survive. Dismissing an unbegun publication commits nothing; a begun
    one's unwanted result is dropped unreferenced. A failure stays visible
    until dismissed. Close and restore fence new publications and await
    begun native commands.
  - Removal, undo and redo edit the content only. A handle an
    acknowledged save spent returns as its stored reference, never as a
    handle to spend again. At save time a stored reference the base
    revision lacks is republished from a revision that stores it
    (`republish_draft_attachment`), so undo after a saved removal is not
    a conflict. References compare by UTF-8 bytes; same-byte attachments
    stay independent entries.
  - **Native controls (M1.9c2cc):** Attach… opens a file sheet (several
    files at once, no folders); files dropped on the editor attach the
    same way and never insert their paths as text. Each attachment has a
    row with its name and a Remove button; each unfinished publication a
    row saying it is attaching, and each failed one a row with the
    reason and Dismiss. Attaching and removing are undoable edits on the
    window's undo stack ("Undo Attach File", "Undo Remove Attachment"),
    with redo; an undo whose attachment is no longer at its place does
    nothing. New attachments are admitted only while the editor is
    editable and archive writes are available. Close settles begun
    publications before its final save, so a finished file is saved,
    then fences and drains them; Close Without Saving fences first and
    drops unwanted results unreferenced.
- **Paste (M4.4e1):** ⌘V always pastes plain text; the editor never
  takes rich text. When the pasteboard holds HTML (`public.html`) and
  the focused body editor is an editable Markdown draft, Edit → Paste as
  Markdown (⌥⌘V) is enabled; otherwise it is disabled.
  - It captures the HTML once and asks the core for its Markdown
    (07 `markdown_from_html`), then shows a sheet with a read-only
    preview of the Markdown source, styled as the composer styles it.
    When the conversion was lossy, the sheet says what was lost ("Some
    formatting can't be kept: images, text styles and colors."), one
    phrase per loss in the core's order. Paste inserts the source at
    the selection as one undoable edit ("Undo Paste as Markdown");
    Cancel changes nothing. A later pasteboard change does
    not alter an open sheet.
  - HTML the core refuses (too large, 07) shows the reason in the sheet
    with only Cancel; ⌘V still pastes its plain text.
- **Inline images (M4.4e2–e3):** in a Markdown draft, Edit → Insert
  Image… picks PNG, JPEG or GIF files, and pasting image data (PNG or
  JPEG as is, TIFF converted to PNG) inserts it. Each image is published
  like a picked file (above) and joins the content as an attachment with
  a new Content-ID (a lowercase UUID followed by `@sweetbriar.invalid`,
  bare, 05), and an image reference to it, `![name](cid:…)` (05), is
  inserted at the selection; the text and the attachment are one
  undoable edit ("Undo Insert Image"). The name is the file name without
  its extension, or "Image" for pasted data, escaped like Markdown text
  so nothing in it can break the reference. The source stays visible:
  the editor shows the reference, not the picture. The image's
  attachment row says "Inline". Removing that row keeps the reference
  text, which then sends as its alt text in the text part and is dropped
  from the HTML (05). Dropped files still attach as ordinary
  attachments. In a plain draft Insert Image… is disabled and pasted
  image data is ignored.
- **External editor (M4.4e4):** Edit → Edit in External Editor (⇧⌘E)
  hands the body to the default app for its type, `.md` for a Markdown
  draft and `.txt` for a plain one.
  - The shell writes the body source as UTF-8 to a new private file
    (10) and opens it in that app. While it is out, the body editor is
    read-only under a banner, "Editing in <app>. Changes you save there
    appear here.", with Done; recipients, subject and attachments stay
    editable. A file no app can open leaves the body editable, shows the
    reason, and is deleted with its directory.
  - The shell watches the file's directory, since editors save by
    replacing the file, and reads it once changes have been quiet for
    250 ms, which usually keeps an in-place save from being read
    half-written; the final read on Done or close (below) replaces
    whatever a mid-write read caught. Each change it
    reads replaces the whole body as one undoable edit ("Undo External
    Edit") and goes through autosave as any edit. A file that isn't
    valid UTF-8 or is over 16 MiB changes nothing and the banner shows
    the reason until a later save fixes it.
  - Done, closing the window or quitting waits for the same 250 ms of
    quiet (at most 2 s), reads the file once more, applies a change,
    deletes the file and its directory, and then follows the usual close
    and flush rules. When that read is invalid, nothing is deleted: Done
    keeps the edit open with the reason in the banner, and closing or
    quitting first asks whether to discard the external edits
    ("Discard External Edits" or Cancel). Close Without Saving drops
    the external edits with the other unsaved changes. Files left by a
    crash are deleted at the next launch, unread.
- **Sending** shows the Out Box entry. During the undo window (default
  7 s, set in settings) **Undo Send** cancels it.
- **Composer probes (M0):** CJK marked text, right-to-left text, and undo
  across formatting. Pasted HTML, inline images and the external editor
  came in M4.4e.

### Identity strip and account colors

Implements [15-account-colors](../research/15-account-colors.md):

- An `NSTitlebarAccessoryViewController` with `layoutAttribute = .bottom`
  hosts a custom-drawn `IdentityStripView`.
- **Size:** at least 32 pt tall, growing with Dynamic Type-like text
  size.
- **`fullScreenMinHeight`** is set to the strip's height, so it stays
  visible in full screen.
- **Contents:** monogram (or logo, below), account name and the full
  From address, as in
  "**W** Work · Alex Example <alex@company.example>". Wrap rather than
  truncate long labels, growing the strip to fit. The whole strip is one
  accessibility element with that label. An unavailable route has an
  explicit neutral label, not a guessed address or color.
- **Colors** come from tokens the core derives (06 of research 15):
  - opaque restrained fill and saturated monogram;
  - foreground chosen per surface for ≥ 4.5:1 contrast;
  - separate light, dark and high-contrast variants.
  The core's light/dark bundles contain an opaque strip, marker,
  foreground and high-contrast outline, and since M6.6a the Color
  Wash pair (below) and the marker's ink. That ink is black or white,
  whichever contrasts more with the marker; one of the two always
  reaches 4.5:1 against it. On the final 8-bit sRGB values,
  foreground reaches 4.5:1 and marker/outline 3:1 against the bundle's
  strip. The shell must also verify contrast on its actual surfaces,
  including selected rows; a strip ratio alone does not qualify the UI.
- **The From control** is an `NSPopUpButton`: a swatch in each item's
  image and full addresses in titles. Provisional identities are marked.
  The swatch is the strip in small: its fill and outline, with the
  identity's monogram in the marker color. Without a monogram it shows
  a marker square.
- **The window title** starts with the sending account, so the Window
  menu, Mission Control and Stage Manager show it: "Work · Lunch on
  Friday · alex@company.example". Without a resolved identity it is the
  subject, or "New Message".
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
- **Color Wash mode** (the `appearance.compose_cue` default; owner
  decision 2026-11-24, research 15 "Window tint survey") tints the whole
  window, M6.6c:
  - The title bar, the strip and the header rows (From, To, Cc, Bcc,
    Subject and the reply line) sit on the opaque strip fill. Their
    labels use the strip foreground. Fields keep their own backgrounds
    and native editing colors. Banners, such as the cross-account
    warning, keep the window background, so a warning never takes an
    account's color (Cross-account check).
  - Below the header, a vertical gradient from the wash's top stop to
    its bottom stop fills the rest of the window, behind the editor and
    the controls. The editor and its scroll view draw no background of
    their own, so the text sits on the wash.
  - **The wash stops** come from the core (M6.6a): fixed OKLCH
    lightness with a capped chroma in the account's hue, pale in light
    and deep in dark, far from the mid-luminance band where neither
    black nor white reaches 7:1, the top stop deeper than the bottom
    one. Every color of the gradient lies, channel by channel, between
    its stops, so the core checks the appearance's text ink (black in
    light, white in dark) against the stops' channel-wise minimum in
    light and maximum in dark. That ink reaches at least 7:1 (WCAG
    1.4.6), and a hue that would miss it gets neutral stops.
  - **Text on the wash** uses the text and label colors, which that
    check covers. Status text that Standard draws in the secondary
    label color, below 4.5:1 even on the plain background, uses the
    label color here. Markdown syntax marks keep their tertiary color,
    decorative as on the plain background. The shell's tests check body
    and status text on both rendered stops at 4.5:1 or more.
  - Native selection and the insertion point are unchanged; the account
    color never replaces them.
  - **Fallbacks:** with Increase Contrast or Reduce Transparency on, the
    window renders as Strong mode, with the editor back on the text
    background. Dark Mode uses the dark stops. An unavailable route
    shows the neutral window, as in Standard.
  - Switching From changes the fill, the wash and the Send button in
    the strip's transition (crossfade, or instant with Reduce Motion).
- **Send as the account** (Color Wash and Strong modes, M6.6c): Send
  reads "Send as <account name>", filled with the marker color, its
  label in the marker's ink. A long name truncates in the middle; the
  accessibility label is "Send as <account name>, <full address>".
  Without a resolved identity it is the plain, disabled Send. Standard
  and Minimal keep the plain Send.
- **Strong mode** (Settings → Appearance) tints the upper window via
  `backgroundColor`, `titlebarAppearsTransparent` and
  `.fullSizeContentView`. The editor stays neutral. The existing
  `appearance.compose_cue` setting is a typed Color Wash/Standard/
  Strong/Minimal preference (02); missing or malformed answers are explicit errors, never a
  guessed mode.
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
- **Minimal mode** (research 15's Minimal marker) reduces only the color:
  - the strip keeps its size, monogram, account name, full From address,
    accessibility label and full-screen height;
  - its fill is the opaque neutral window background and the window
    isn't tinted, so the monogram is the only colored part;
  - its text uses the system label color. The monogram keeps its
    account marker, outlined in the system label color wherever the
    marker is under 3:1 against that background (the account's outline
    token is solved against its strip, not this one);
  - an unavailable route and every warning look as they do in Standard.
- **Mode previews:** Settings → Appearance shows Color Wash, Standard,
  Strong and Minimal each with a preview of a compose window's top, drawn from the
  same presentation as the real strip with the selected account's
  tokens, or the neutral route when there is no account. A preview
  never takes focus. Its mode's accessibility label says in words what
  the mode colors.
- **Cross-account check:** replying through a different account than the
  one that received the original shows an inline banner with **Use
  <account>** and **Keep <account>**. An unacknowledged mismatch is
  confirmed at Send. Validation runs in the core (05).
  - The banner and the confirmation name the account that received the
    original and the sending identity, with its account and full
    address.
  - The banner leads with a warning symbol in the label color, and the
    confirmation is a warning alert, so neither looks like any account's
    color.
- **Recipient check** (M6.7b; 05): a blocked recipient gets a banner
  for each, in the cross-account banner's warning style: "<address> has
  written to <account chips> but never to <sending account>. Sweetbriar
  won't send to it from <sending account>." It shows one chip for each
  account the address has written to.
  - **Use <account>** switches From to that account's default identity,
    as the cross-account banner does, when that account can send. With
    several such accounts, it is a menu of them.
  - **Allow for <sending account>…** asks first, in a warning alert:
    "Allow sending to <address> from <sending account>? Sweetbriar
    stops blocking this address for <sending account>; Settings →
    Sending lists it." Allow calls `allow_recipient`; Cancel changes
    nothing.
  - The recipient's token shows a warning symbol, never an account
    color. Send stays disabled while any recipient is blocked, and its
    tooltip and accessibility help name the reason.
  - Until the check is ready (02), the status line says "The recipient
    check starts once Sweetbriar has read your existing mail." in the
    label color.
  - The window rereads `recipient_fence` whenever the recipients or the
    route change, after an allowance, when mail arrives, and every 30
    seconds while it has recipients, since sender rows and readiness
    change when a pass publishes (02), which no compose event marks. A
    `Conflict` from Send for `recipient_fence` shows the banner.
  - **Settings → Sending** lists the allowances by account, each with
    Remove, which calls `remove_recipient_allowance`.
- **Reply line** (M6.6c): a reply or reply-all shows, under its header
  in every mode, "Reply to a message received in" and the receiving
  account's chip (`reply_account_id`, 07), whether or not the accounts
  differ, so the original's account is always next to the sending one.
  A mismatch still adds the cross-account banner. A reply to a
  local-archive message shows the local archive's chip and no banner
  (05). New messages and forwards have no reply line. The line is one
  accessibility element: "Reply to a message received in <account>".
- **Logo** (M6.6d): an account's logo (02 "Account logos") replaces its
  monogram in the strip, the From menu's swatches, the sidebar, account
  chips and the Settings previews. List rows keep the monogram, which
  stays legible at row size.
  - It is drawn at the monogram's size, clipped to a rounded square on
    an opaque plate of the window background, and ringed in the marker
    color: 1 pt, or 2 pt with Increase Contrast or Differentiate
    Without Color. The ring keeps the account color visible around
    any logo.
  - It is decorative: every accessibility label keeps the account's
    name, and the name stays visible as text.
  - An identity with its own symbol override shows that symbol rather
    than the account's logo.
  - A stored logo the shell can't decode shows the monogram and a note
    in Settings, never an empty mark.
- **Settings → Appearance, per account** (M6.6d), next to the color:
  - **Letter:** the account's own monogram, one or two characters
    (grapheme clusters, surrounding whitespace trimmed); empty restores
    the automatic letter. It is the account's `symbol` (02).
  - **Logo:** Choose Image… opens an `NSOpenPanel` for images; files
    over 20 MB are refused before decoding. ImageIO decodes the file,
    which is scaled to fill 128 × 128 pixels, cropped to the center and
    encoded as PNG. One still over 65,536 bytes is refused with
    "Choose a simpler image." Remove clears it.
  - **Addresses:** each of the account's identities with "Use account
    color" (the default) or its own swatch or custom color, written
    through `set_identity_appearance`.
  - Every write takes the observed revision. A `Conflict` reloads the
    values and says so, never overwriting another window's change.
- **Window tabs** get an `NSWindowTab.accessoryView` swatch (P2, later).
- **Accessibility settings honored:**
  - `accessibilityDisplayShouldDifferentiateWithoutColor`: stronger
    monogram, symbol and outline;
  - `…ShouldIncreaseContrast` and `…ShouldReduceTransparency`;
  - Dark Mode.

## Account chips

An account chip names the account a surface acts for (research 15's P1
list): the reader's "Received in <account>", each account's row in
Settings → Accounts and in Activity, and the move picker's
destinations.

- It shows the account's monogram in the marker color, or its logo
  (Identity strip), and its label in the foreground color, on the strip
  fill: the identity strip's tokens
  for the current appearance. Increase Contrast and Differentiate
  Without Color add the outline.
- The label is text, so the chip never relies on color alone. The chip
  is one accessibility element with that label.
- Without the account's appearance the chip is neutral: the label alone,
  in the secondary label color.
- An identity chip is the same chip with the identity's own monogram and
  colors (its overrides, else its account's), where a surface shows a
  sending identity rather than an account.

## Keyboard filing ("type to file")

`⇧⌘M` (Move to…) opens a filing palette: an `NSPanel` with a search field over
mailboxes, ranked by recent use per account. Return moves the selection,
and the undo toast offers **Undo** for 10 s.

- **Commands (M5.2):** the Message menu, the list's context menu and
  the toolbar carry Archive (`⌃⌘A`), Move to Trash (`⌘⌫`), Move to…
  (`⇧⌘M`), Add Label… and Remove Label…. They act only in the key main
  window, while its list pane shows messages. While text is being
  edited, in the search field or elsewhere, they are disabled and
  `⌘⌫` edits the text: it never files mail.
- **The selection** is captured when the command is invoked. Archive
  and Move to Trash run one call per account, so a refused account
  changes nothing while the others keep their effect. Archive moves to
  the account's Archive folder. Move to… and the label commands need
  one account, since a message never moves between accounts. A mailbox
  window's whole selection of more than 1,000 rows passes its
  `SelectionToken`. Any other selection over 1,000 rows is refused
  with an explanation, as is one the list changed under.
- **The palette** lists the one account's filing destinations (04: its
  folders except Sent and Drafts, and not the mailbox being shown) or,
  for the label commands, its labels, headed by the account's monogram
  and name. With an empty field the mailboxes in
  `filing.recent_mailboxes` (02) come first, newest first, then the
  core's order. Typing keeps that order and shows the names containing
  every typed word, ignoring case and diacritics. Up and Down choose,
  Return files and Escape changes nothing. Both return focus to the
  list. Choosing records the mailbox once filing succeeded. A failed
  recording leaves the previous ranking and doesn't undo the filing.
- **Dragging** rows to a folder of their account in the sidebar moves
  them; to one of its labels, adds the label. Any other drop changes
  nothing and, on a mailbox, says why.
- **The toast** names what was done and for how many rows, and is
  announced to accessibility clients. After a move, Trash or Archive
  its Undo, also Edit → Undo in the list, calls `undo` with every
  token the action got. It is offered for 10 s or until the next
  filing action replaces it. Adding or removing a label offers no
  Undo.
- **Refusals** the core gives, and the shell's own above, are shown as
  an explanation in a sheet. An action is never reported done when it
  was refused.
- **Afterwards,** when every selected row left the list, nothing is
  selected, so nothing is marked read. The next arrow key continues
  from the first one's rank.

## Menus and commands

- **Every action is a command** with a stable identifier, in the
  `ch.lkmc.Sweetbriar.cmd.*` namespace, wired to the menus and the responder
  chain. A command keeps its identifier in every menu that offers it,
  context menus included, and no two commands share one. AppKit's own
  commands (About, Services, Hide, Hide Others, Show All, Quit, Undo,
  Redo, Cut, Copy, Paste, Delete, Select All, the Find, Spelling and
  Grammar, Substitutions, Transformations and Speech items, Close, the
  sidebar and toolbar items, Enter Full Screen, Minimize, Zoom and Bring
  All to Front) keep AppKit's actions and carry no identifier.
- **The menu bar** follows Mail's layout, so its commands and shortcuts
  are where a Mac user looks for them:
  - **File:** New Message (⌘N), Close (⌘W), Save (⌘S), Attach Files…
    (⇧⌘A), Add Proton Account… and Import… (⇧⌘I);
  - **Edit:** the standard editing items, Paste as Markdown… (⌥⌘V),
    Insert Image…, Edit in External Editor (⇧⌘E), and Find: Search Mail
    (⌥⌘F) into the main window's search field, then Find, Find and
    Replace, Find Next and Previous through the focused text view's find
    bar (the composer, plain-text and Reading-view bodies);
  - **View:** Show or Hide Sidebar (⌃⌘S), Message Source (⌥⌘U), Reading
    View (⌥⌘R), the toolbar items and Enter Full Screen;
  - **Mailbox:** Inbox, Flagged, Drafts (local) and Out Box (⌘1–⌘4),
    chosen as a sidebar click chooses them;
  - **Message:** Send (⇧⌘D), Open in New Window (⌘O), Reply (⌘R), Reply
    All (⇧⌘R), Forward (⇧⌘F), then the flag and filing commands;
  - **Format:** Make Plain Text or Make Markdown (⇧⌘T), Bold, Italic;
  - **Window:** Minimize, Zoom, Message Viewer (⌘0, the main window
    again once it was closed, as Mail's), Activity (⌥⌘0), Bring All to
    Front and AppKit's window list; then Debug and Help.

  Save, Attach Files…, Send and the format command act on the key
  compose window and are enabled exactly when its own controls are.
  Message Source and Reading View act on the key main or message
  window's reader, checked while that rendition shows.
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

### Latency log (M6.4)

- **Opt-in:** off by default. Debug → **Record Interaction Latency**
  turns it on and off, and is stored in user defaults like List
  Implementation.
- **The log:** while it is on, each measured interaction appends one
  JSON line to `diagnostics/latency.jsonl` in the cache root: the
  interaction's name, its milliseconds, when it ended and the app
  version. It holds no message, address, subject, id or search text.
  At 10,000 lines the oldest are dropped first.
- **Spans** run on the monotonic clock, each to the first frame that
  shows the result. A shown result whose frame hasn't come within a
  second (a hidden window) ends at that second, so a stall counts as
  a miss:
  - `launch`: from the process's start to the main window's list
    showing its rows. A launch is warm unless it is the first since
    the Mac started;
  - `open_message`: from a single selection in a list to the reader
    showing that message's body, when the body was already on this
    Mac. An open that waits for Proton to fetch the body isn't
    recorded;
  - `archive_visible` and `archive_durable`, `delete_visible` and
    `delete_durable`: from the command to the row leaving the list,
    and to the core's receipt for its intent. Archive and Move to
    Trash count, from the keyboard, a menu, the toolbar or a context
    menu, when every row they targeted was filed; a drag or Move to…
    doesn't. A row that stays listed (a label's list, or a Search it
    still matches) records no visible span;
  - `search`: from the last keystroke to the results for that query.
    A later keystroke replaces the span;
  - `open_compose`: from New Message, Reply, Reply All or Forward to
    the compose window's editor taking keystrokes.
- **The report:** Debug → **Latency Report…** shows each interaction's
  count, p50, p95 and maximum beside its budget, and whether the p95
  meets it, with a Copy button. Warm launches alone count toward the
  launch budget. Debug → **Clear Latency Log** empties the log.
- **Failures:** a log that can't be written stays on, and the report
  says how many entries were lost and why. Nothing is retried in a
  loop.
