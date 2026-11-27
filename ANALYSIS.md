# Sweetbriar — analysis backlog

Consolidated on 2026-11-26 from the full review of `origin/main` at
`19fde48e` (Rust ~298k lines, Swift ~60k lines) plus a second pass over
the reader and list hot paths. This file is the shovel-ready backlog:
every open item names the file and line that carries it, and each entry
is written so an LLM can pick it up cold. Extended at `366f29bc` with
a third pass that shipped typed attachment rows and notification
click-through. Completed work and submitted patches are recorded below.
Extended again from a fourth pass focused on row presentation and the
first-run surface: message-row tag collapse (PR #572) and the first-run
welcome island (PR #577) are recorded under their original entries.
A further pass (`friendly-wasp`) added the small correctness, layout,
hygiene and product entries below; its four fixes (PRs #578, #581,
#585 and #590) are merged.
A further independent review, by opencode, added Part 8; its nine
changes, PRs #573, #582, #583, #584, #587, #591, #595, #596 and #597,
are merged.
A fifth pass (`friendly-wasp`, 2026-11-26/27) added Part 10 and the
"Completed in this pass" section: eight fixes (PRs #599, #600, #601,
#607, #608, #609, #610 and #611) covering a reused row cell's account
marker, the sidebar's observer and reveal behaviour, a list's sticky
read error, the toolbar search field outside the message list, the
reader's note commit on close, a row hover fill, tag chips that
overflowed their cell, and a search cursor minted per row instead of
per page edge.

## Completed on main

- **Dock unread badge** (second pass, PR #575). `SidebarModel.inboxUnread`
  sums every account's Inbox mailbox once; `MainWindowController` mirrors
  it on `NSApp.dockTile`, refreshes after generation-driven sidebar
  reloads, and clears it when content retires. Tests cover the sum, the
  role filter, reload updates and the badge text.

- **Second review (opencode), landed.** `scripts/build.sh` finds rustup's
  tools outside `CARGO_HOME` (#573); the unit-test host opens a private
  archive, never the user's (#582); the Reading view memory resets after
  a restore (#583); `Info.plist` matches `project.yml` (#584); long
  recipient lists collapse behind **Show All Recipients** (#587);
  `scripts/bundle-notices.sh` is shellchecked in CI (#591); a panicking
  writer observer no longer hangs its write or ends the writer (#595);
  the Proton events cursor gets the same path check as every other id
  (#596); a backup reference that is not a hash fails `Corrupt` before
  it names a directory (#597). Details and what remains are in Part 8.

- **Help menu content.** Help → Keyboard Shortcuts… now opens a shared
  reference window with the app's keyboard commands. [Menu entry](apps/macos/Sweetbriar/MainMenu.swift#L458), [Shortcut reference](apps/macos/Sweetbriar/KeyboardShortcutHelp.swift)
- **First-run welcome** (PR #577). A fresh archive shows a welcome
  island over exactly the empty Unified Inbox (`SidebarModel.isFreshArchive`,
  no account beyond the local one and no imported mailboxes): the
  headline is "No messages", the body explains local-first storage, and
  Add Proton Account… / Import Mail… reuse the existing commands. Any
  other window, search, error or real row hides it. The headline keeps
  the XCUITest empty-state expectations matching.
- **Reader blocked-content copy** (further pass, PR #578). The reader's
  remote-content line pluralizes ("1 remote content item blocked")
  instead of reading "1 remote content items blocked" at a count of one.
- **Allowance error precedence** (further pass, PR #585). Settings →
  Sending keeps a failed allowance removal apart from a failed list
  load and reports the removal failure when both happen, instead of
  overwriting one with the other.
- **Row date column** (further pass, PR #590). Message rows reserve the
  measured width of their formatted trailing date (clamped 32-84 pt)
  instead of a fixed 84 pt, returning roughly 50 pt to the subject and
  snippet on today's time-only rows while long dates keep the old
  maximum.
- **Reader rule-list install** (further pass, PR #581). `HTMLReaderView`
  installs its shared `WKContentRuleList` once per web view instead of
  once per message. GLM found no actionable issues; its one minor note
  about a hypothetical replaced web view was answered with an explicit
  one-web-view invariant and a test comment about the SDK's lack of
  rule-list read-back.

## Completed in this pass

- **Row account marker restore** (friendly-wasp pass, PR #599). A reused
  message-row cell kept the rail and monogram hidden from a row whose
  account had no appearance yet: the appearance subscription answers
  after the list has opened, so the first cells a mixed-account window
  makes are configured without one and nothing brought the marker back.
  `MessageRowCellView.configure` now assigns both flags from the row's
  own state and exposes `showsMarker` for tests. `observeAppearance`
  answers with `reloadData`, which reuses views rather than making new
  ones, so the bug was invisible in a fresh window and permanent in a
  reused one.
- **Sidebar lifecycle, reveal and count presentation** (friendly-wasp
  pass, PR #600). Four defects: the sidebar registered an accessibility
  observer in `loadView` and never removed it, so a restore dropped the
  controller while it was still registered and a later Increase
  Contrast change posted to a dead object; choosing a mailbox by
  command expanded its section but never scrolled the row into view, so
  a deep mailbox changed the list with no visible confirmation; account
  ids were matched with `==`, which treats canonically equivalent
  spellings as equal, while the account chips match them byte for byte;
  and the summed health-row count was interpolated directly while every
  other count the presentation shows goes through its locale formatter.
  The "Archive closed" banner also lacked the padding its other states
  apply, so it touched both window edges.
- **A list's read error clears when the read succeeds** (friendly-wasp
  pass, PR #601). `ListWindowModel.lastError` was written on every page,
  subscription and empty-status failure and never cleared, so one
  transient failure was permanent: the list kept "Couldn't read this
  list: …" over healthy rows and `onCountChange` reported nil, so the
  window's subtitle lost its count for good. The error now clears when
  the failed pages land and when a fresh index installs, and the list's
  failure status carries a **Try Again** button, so the retry no longer
  depends on the user happening to scroll. While a new choice is
  opening, `model` is still the previous list, so `updateStatus`
  reported the old count under the new title — "Search · 240 results"
  straight after leaving a 240-row mailbox.
- **Toolbar search field outside the message list** (friendly-wasp pass,
  PR #608). The toolbar's search field is the message list's and stayed
  enabled while Drafts (local) or the Out Box filled the list pane, so
  typing in it searched a hidden list: no visible result, no title
  change, no error. The field is now admitted with the pane and the
  toolbar's search interaction is ended before the field is disabled.
- **Reader note commit on close** (friendly-wasp pass, PR #609). The
  note editor saved only on `resignFirstResponder`, and neither
  `ReaderViewController.close()` nor `MessageWindowController.windowWillClose`
  ended editing or committed `noteDraft`. AppKit does not promise to
  resign the first responder when a window closes, so a note typed into
  the reader and then dismissed was lost. `closeAndWait` now commits and
  awaits the save before the teardown, because a task created inside
  `close` does not start until `close` has already closed the model.
- **Row hover fill** (friendly-wasp pass, PR #607). The list is a
  `.plain` NSTableView with no tracking area, so a row gave no response
  until it was clicked. `HoverRowView` supplies a layer-backed fill that
  fades in over 120 ms while the pointer is over an unselected row,
  dequeued through the table's recycle queue, with its color resolved
  inside the effective appearance and a raised alpha under Increase
  Contrast. A row that appears under a stationary pointer primes its
  state, and the selection's own highlight replaces the fill rather
  than stacking on it.
- **Row tag chips stay inside the cell** (friendly-wasp pass, PR #610).
  The chips were laid out right-to-left with no floor, so three long
  labels walked past the text column, drew over the rail and outside
  the cell, and reduced the snippet to zero width. They were also
  framed one point into the gap between rows.
- **Search cursor minted for a page's edges** (friendly-wasp pass,
  PR #611). `read_page` built a cursor for every hit rather than just
  the page's first and last rows, and `Binding::mint` serialises the
  binding's `spec`, which holds the full canonical query JSON: a
  maximum-size query made each cursor about 16 KB, so a 1,000-row page
  allocated about 16 MB of strings on every page read and every
  keystroke. `StoredSearchRow` now carries the cursor's key and
  `SearchPage` carries the binding it mints against.

## Submitted patches

- **Saved-view naming.** Save stays disabled until the name contains
  non-whitespace text. [Saved-view PR](https://github.com/L-K-M/Sweetbriar/pull/606)
- **Import variant choices.** Long names truncate without hiding the
  picker; the tooltip keeps the full name and path, and the picker
  announces its mailbox. [Import PR](https://github.com/L-K-M/Sweetbriar/pull/605)
- **Backup sizes.** Ordinary sizes use readable units, with exact byte
  counts available through accessibility and the tooltip. [Backup PR](https://github.com/L-K-M/Sweetbriar/pull/604)
- **Literal Markdown punctuation.** Entering Markdown mode disables
  smart quotes, dashes and text replacement; entering plain mode applies
  current system preferences. [Composer PR](https://github.com/L-K-M/Sweetbriar/pull/602)
- **Out Box row height.** Attempt rows use the configured 74-point height
  while the History heading remains compact. The larger-text sizing work
  remains in V9. [Out Box PR](https://github.com/L-K-M/Sweetbriar/pull/603)
- **Proton sync after a failed session restore.** Active accounts retry
  session restoration after an offline launch or a failed refresh rather
  than leaving the sync worker stopped. Synthetic tests cover both paths
  and timeout classification. [Proton retry PR](https://github.com/L-K-M/Sweetbriar/pull/598)

- **Message-row tag collapse** (fourth pass, PR #572). Rows drew at
  most three tag chips and silently dropped the rest, so a row
  under-reported its state and disagreed with the SwiftUI prototype.
  Both lists now share `MessageRowMetrics.visibleTagCount` and
  `MessageRowContent.visibleTags`/`hiddenTagCount`; the AppKit cell
  draws a `+N` chip when it fits and hides it rather than overlap tags
  on narrow rows, and VoiceOver hears the collapsed count. Tests:
  `tags cap at the visible count...`, `narrowRowsKeepEveryChipInsideTheRow`.
  [Tag collapse PR](https://github.com/L-K-M/Sweetbriar/pull/572)
- **Reader attachments as typed rows** (third pass, PR #580). Each
  attachment gets a full-width row with a media-type symbol, truncating
  name, trailing size, hover wash, Return/Space activation and a
  VoiceOver press action; opening still goes through the core lease.
  GLM's two review rounds found a stale hover draw, a missing press
  action, a possible zero-width stack and a focus-ring ordering bug;
  all were fixed before this entry was written.
  [Attachment rows PR](https://github.com/L-K-M/Sweetbriar/pull/580)
- **Notification click-through** (third pass, PR #592).
  `AppDelegate` adopts `UNUserNotificationCenterDelegate`, promises
  banners while Sweetbriar is frontmost, and a default-action tap
  reveals the account's Inbox role, falling back to its first
  selectable mailbox. A tap during a cold launch is remembered until
  the sidebar opens. The router and destination choice are pure and
  tested. [Notification open PR](https://github.com/L-K-M/Sweetbriar/pull/592)
- **Reader rendition interleaving.** Parked HTML preparation hid the
  current Reading view on navigation or restarted WebKit after a
  rendition toggle. The regression tests cover a late selection load,
  a parked HTML preparation across navigation, and a toggle within
  the same selection. Independent preparation and selection checks
  reject stale completions, including when a message is reselected.
  [Reader rendition PR](https://github.com/L-K-M/Sweetbriar/pull/588)
- **Reading view list numbering.** Reversed lists now default to
  the item count, explicit numbers reset the sequence, and the
  renderer handles the integer bounds without trapping. Rust and
  native renderer tests cover defaults and explicit numbering in both
  directions. Existing nested-list indentation coverage remains
  relevant to the rendered layout.
  [Ordered-list numbering PR](https://github.com/L-K-M/Sweetbriar/pull/593)
- **Account logos after restore.** The logo cache checks the stored
  PNG when an archive reuses a revision. The From menu redraws its
  swatch when the decoded image changes. The regression uses two
  different PNGs under the same account ID and revision.
  [Logo cache PR](https://github.com/L-K-M/Sweetbriar/pull/589)
- **Tag draft followed the selection to another message.** The reader's
  tag field never cleared on selection change, so an unsubmitted draft
  typed on message A would land on message B. Tracked in PR #576
  (`mac/reader-tag-draft-20261126`): `ReaderViewController` tracks the
  message the draft belongs to, clears on selection change
  (occurrence-keyed, eagerly in `resetSelection`) and when the shown
  message differs; a metadata-only refresh of the same message keeps
  it. Regression test `tagPickerDraftStaysWithItsMessage`.
- **`setVisible` re-ran all visibility work on every scroll
  notification, including retrying failed pages on no-op reports**
  (old B6). Tracked in PR #586 (`mac/list-visible-guard-20261126`):
  `ListWindowModel.setVisible` returns early when the range is
  unchanged, so `failedPages` survives until the range actually moves.
  Test `aRepeatedVisibleRangeDoesNotRetryAFailedPage`.
- **Trackpad swipe actions** (second pass, PR #579). Trailing swipe
  offers Archive and Move to Trash; leading swipe offers Mark as
  Read/Unread and Star/Unstar. It reuses the context menu's titles,
  admission and click-target rule, so a selected row acts on the whole
  selection. The list-appearance suite covers command set, order, style
  and target.

## Verified non-bugs (do not re-report)

- The search strip's visibility is checked live and behaves per spec.
- `ListWindowModel` fires `onIndexChanged` twice on open (provisional
  prefix, then exact index); both fire are needed so the list redraws
  when provisional ids are replaced by the full index.
- A new-mail subscription does not fail on a transient Proton network
  error: `next` ends on engine shutdown or a concurrent wait, and the
  notifier is its sole caller. Reconnecting the stream on shutdown would
  spin on a dead engine. [Subscription](crates/sb-engine/src/windows/subscribe.rs#L337)
- Appearance's account controls are disabled as a group when the archive
  is read-only; the compose-cue controls receive the same write state.
  [Appearance settings](apps/macos/Sweetbriar/AppearanceSettingsWindow.swift)
- A server's IDLE command refusal ends the IMAP watcher and the scheduler
  falls back to polling; it does not loop on that refusal. Transport
  errors can retry, so keep that case separate.
  [IMAP watcher](crates/sb-engine/src/account_sync/idle.rs)
- Sidebar health rows use static condition titles, not relative times.
  The Activity window's relative-time detail has its own refresh tick.
  [Sidebar health](apps/macos/Sweetbriar/SidebarViewController.swift)

## How the review was done

- Read the macOS app shell, list, sidebar, reader, composer, search and
  settings by hand.
- Read the Rust store query layer, engine windows/bodies, and `sb-mime`
  render/rendition paths by hand.
- Ran four bounded read-only helper passes (LongCat 2.5) over the store
  query layer, the list/bridge, `sb-mime`/reader and search, then
  verified each reported finding against the source before recording it.
- A Dark Mode walkthrough at `241250fb` covered fresh roots, import
  of a two-message synthetic mbox, reader selection and a local
  draft. Narrow panes, other appearances and real-account qualification
  remain open.
- The findings cite source locations and distinguish observations
  from proposals.
- The third pass built the Debug app locally (Xcode 27.2, macOS
  27.0.1, Rust 1.99.0) and ran it once against throwaway roots; its new
  app-hosted tests passed. On this Mac `CARGO_HOME` must point at the
  real registry path or `askama`'s absolute include paths break.
- Paseo worktrees on this machine are not always exclusive: another
  agent can switch branches or commit in the same directory, so check
  `git reflog` and `git status` before assuming a clean tree, and use a
  private worktree for multi-branch work. App-hosted UI tests that
  assert `NSApplication.shared.isActive` fail spuriously when another
  process holds the foreground; they are not a code signal.
- The further (`friendly-wasp`) pass worked in a private git worktree,
  built the app with Xcode 27.2, and let each of its four fixes run the
  repository's full macOS gate (Rust tests, bindings, XcodeGen,
  `xcodebuild build test`, the XCUITest flows and both benchmarks).

---

# Part 1 — Bugs

## B1. Any sync invalidation rebuilds the whole message list

`AppKitPrototypeList.observeAppearance()` wraps **two** reads in one
`withObservationTracking` block:

```swift
withObservationTracking {
    _ = appearanceModel.appearance
    observeAccountLabels()      // reads `sidebar.sections`
} onChange: { … tableView.reloadData() … }
```

`observeAccountLabels` is `MessageListViewController.swift:332`:
`{ [weak sidebar] in _ = sidebar?.sections }`. So the block subscribes to
`SidebarModel.sections`, an `@Observable` array that `reloadSections()`
replaces whenever the sidebar re-reads.

The sidebar re-reads on every `listSaw(generation)`
(`MainWindowController.swift:578`), i.e. on **every list index
generation change**: new mail, a flag change, a read receipt, a Proton
sync. Each of those calls `outlineView.reloadData()`, which assigns a new
`nodes` array and a new `model.sections`, which fires the observation,
which does a **full `NSTableView.reloadData()` plus a selection restore**
on the message list.

Cost: every background sync event tears down and reconfigures every
visible row, re-measures text, rebuilds badges and re-resolves layer
colors. On a busy Proton account this is a continuous low-grade stutter,
and it is invisible to the existing benchmarks, which measure scrolling
and search, not idle repaints.

Fix: keep appearance observation and account-label observation separate.
Observe only `appearanceModel.appearance` here, and let the list take an
explicit `reloadRows()` call (or an `[accountId: String]` label map that
is rebuilt only when a label actually changes).

## B2. cid resolution is quadratic in a message's resource count

`crates/sb-mime/src/rendition.rs:333` resolves each `cid:` reference
with `self.input.resources().iter().position(|r| r.content_id() == id)`
and then checks membership with `self.used.contains(&i)` — a `Vec`
linear scan. A newsletter with N inline images and N references does
`N × (N + N)` work, and `Inventory` is rebuilt per render, so there is no
carry-over between the placeholder render and the real render in
`publish_body` (see P1): `2 × N²`.

Fix: build a `HashMap<content_id, usize>` once in `Inventory::new` and
track used indices in a `HashSet`.

## B3. Re-opening the same message re-renders it from scratch

`crates/sb-engine/src/bodies.rs:100` (`Engine::open_body`) goes straight
to `resources.publish_body(message_id, preferred)` on every call. The
`Leases` registry only tracks cleanup; nothing is looked up. So:

- re-selecting a row that is already shown,
- pressing Cmd-O on the message that is already shown,
- toggling View → Message Source and back,
- arrowing down and back up between two messages,

each re-acquire the message bytes, re-run `render_input` (a full MIME
parse), re-run the renderer, and re-publish the files. Arrow-key
navigation in a mailbox pays this twice per row.

Fix: index leases by `(message_id, BodyKind)` and hand back the existing
`PublishedBody` while it is live, invalidating on a new acquisition.

## B4. `complete_fields` parses JSON once per candidate row per leaf

`crates/sb-store/src/query/counts.rs:504` runs
`serde_json::from_str::<Vec<String>>(mask)` inside the
`sb_query_complete_fields` UDF. The UDF is invoked from a correlated
subquery for every candidate row and every FTS leaf. The stored value in
the normal case is exactly `[]`, so the overwhelmingly common path is a
full `serde_json` parse of a two-byte string, per row, per leaf.

`crates/sb-store/src/query/counts.rs:482` (`address_value`) has the same
shape: it decodes header address JSON and runs IDNA/UTS46 mapping for
every candidate row with a header row.

Fix: short-circuit `mask == "[]"` before serde; keep the callback only
for header rows without precomputed keys.

## B5. A fresh in-memory FTS5 database is opened per search keystroke

`crates/sb-store/src/query/text.rs:111` (`TextValidator::open`) creates
an in-memory SQLite connection and two FTS5 virtual tables.
`Query::parse_text` (`crates/sb-store/src/query/ast.rs:240`) passes
`oracle: None`, so the lazily-opened validator is discarded at the end of
every parse. `Engine::parse_search`
(`crates/sb-engine/src/search.rs:218`) is the only production caller and
runs on every accepted keystroke.

The `oracle: Option<TextValidator>` parameter that `syntax::parse` already
threads through exists precisely so a validator can be reused, but the
public entry point never supplies one.

Fix: keep one validator per parse-site (thread-local, or held by the
engine) and reset only its content between parses.

## B7. Delivered pages reload rows that are not on screen

`AppKitPrototypeList.swift:247` (`pagesDidLand`) inserts all 200 row
indexes of every delivered page into the reload set. A page delivered
while the user has scrolled elsewhere reconfigures cells AppKit prepared
just off screen, synchronously inside the delivery callback.

Fix: intersect the delivered pages with `visibleRanks` plus one prepared
screen before `reloadData(forRowIndexes:)`.

## B8. The reverse id index is rebuilt wholesale after every index fetch

`ListWindowModel.swift:514` (`index(of:)`) lazily builds
`ids.enumerated().map { … }.reduce(into: [:])` — a dictionary over the
entire window — and clears it at every `install` (`:454`). With a row
selected, `reconcileSelection` (`:807`) and `restoreSelection` trigger a
fresh O(n) main-thread dictionary construction after **every** index
change, including the ones B1 already makes too frequent.

Fix: maintain the map incrementally across generations, or stamp it with
a generation and rebuild it off the main actor.

## B9. A dropped selection serialises one core round trip per missing id

`ListWindowModel.swift:819`: after each index fetch `reconcileSelection`
issues one sequential `readWindow` per missing selected id while
`holdingReads` blocks all page reads. A large selection that leaves the
window (e.g. after a filing) stalls newly visible rows behind N serial
requests.

Fix: run the lookups concurrently or in one batched read, bound their
count, and release the read hold earlier.

## B10. `updateText` re-parses and rebuilds the search window for an unchanged query

`SearchFieldModel.swift:85` has no unchanged-text guard. Selecting a
clause for editing (`editClause` re-calls it with the same canonical
text) and pressing Return with unchanged text invalidate the current
request, relaunch the parser, re-present the identical query, and
`MessageListViewController.swift:323` tears down and rebuilds the whole
`AppKitPrototypeList` — table, columns, observers, constraints —
synchronously on the main actor.

Fix: return early when the bytes equal the committed text and no
composition is marked; keep one table view and swap its model when both
the old and new specs are Search.

## B11. `open()` fires two full reloads during launch

`ListWindowModel.swift:442` fires `onIndexChanged()` for the provisional
prefix and again at `:423` once the exact index arrives, so opening a
window does two `reloadData()` calls and two selection restores.

## B12. `SearchFieldModel` makes one `formatSearch` round trip per AST node

`SearchFieldModel.swift:247`: after a successful parse it formats the
whole query and then **one call per AST node**, plus a
`JSONSerialization` encode per node on the main actor, before chips and
canonical text appear. Each of those is a separate engine hop.

## B13. Message rows silently drop tags past three

`AppKitPrototypeList.swift` caps the row cell at `tagChipCount = 3` and
hides any further chips without an overflow indicator, and
`MessageRowContent.accessibilityLabel` omits tags and attachments
entirely. A message with four or more tags therefore reads as three in
the list and none to VoiceOver, while the paperclip is visual-only and
the reader shows the full state. Draw a trailing `+N` chip after the
visible chips (keeping the fixed-size pool so the hand layout is
unchanged) and add the remaining tag count and attachment presence to
the accessibility label. Keep the established sender, subject and date
order first, and audit verbosity for a row with many tags.

**Implementation note (PR #572):** both lists now share
`MessageRowMetrics.visibleTagCount`; the AppKit cell renders
`content.visibleTags` plus a `+N` chip when it fits between the content
origin and the trailing group, otherwise it shows the tags alone rather
than overlap. `accessibilityOverflowSuffix` appends ", N more tags" in
both lists. Tags are unique per message (`message_tags`'s primary key),
so the prototype's `ForEach(id: \.self)` stays stable.

## B14. A self-built payload is encoded with `try!`

`apps/macos/SweetbriarBridge/HumanVerification.swift:46` encodes a
dictionary the same file builds, so it cannot fail today; the pattern
is fragile if the payload ever gains a non-JSON value. Throw or record
an internal error instead.

## B15. Settings snapshots are posted into a discard sink

`CoreBridge.settings()` posts every snapshot into the caller-owned
`FrameBatcher` for "`@MainActor` observers", but the production
`AppDelegate` constructs that batcher with `{ _ in }` and no observer
consumes it. A future settings writer can therefore appear to propagate
while nothing in the app hears the update. Either wire the batcher to
the appearance/reading settings models it was meant to update, or make
the absence explicit by removing the dead publication path and having
consumers read settings directly. Add a test at the chosen boundary so
this cannot silently regress.

## B16. Search parse-span conversion can trap on a mid-scalar offset

`apps/macos/SweetbriarBridge/SearchQuery.swift`'s `utf16Range` indexes
`text.utf8` with `bytes.index(_:offsetBy:)`, which traps when the
core's span is not on a UTF-8 scalar boundary. The comment promises
scalar-boundary offsets and the core tests cover EOF/over-limit spans,
so this is hardening rather than a known crash: convert with a
nearest-boundary scan or return nil, and add a unit test with a span
inside a multi-byte character.

## B17. Row cells register accessibility observers they never remove

`MessageRowCellView.init`
(`apps/macos/Sweetbriar/AppKitPrototypeList.swift`) observes
`NSWorkspace.accessibilityDisplayOptionsDidChangeNotification` and no
`deinit` removes it. Cells live as long as the table's reuse pool.
Verify with a test that closes and deallocates a window's table while
registered; modern AppKit generally zeroing-weakens selector observers,
so if it is safe, record that conclusion here rather than re-reporting
it.

---

# Part 2 — Performance

## P1. Every message open renders the body twice

`crates/sb-resources/src/bodies.rs:230` renders once with the placeholder
token `"0".repeat(64)` to measure sizes, drops the `Rendition`, then
renders **again** with the reservation's real token:

```rust
let rendition = render(&input, &"0".repeat(64), preferred);   // measure
… let sizes = vec![rendition.content().len(), manifest.len(), …];
let mut files = cache.reserve(&sizes)?;
drop(rendition); drop(manifest);
let rendition = render(&input, files.token(), preferred);      // publish
```

The comment is correct that the token length is fixed, but that only
means the two renders are identical in size — not that they are cheap.
Every message open pays two full MIME parses, two sanitisation passes,
two manifest builds and two HTML serialisations.

Fix: render once into owned buffers using a 64-byte placeholder token,
then substitute the real token with a byte-level replace over the
already-rendered HTML and manifest. A single `memchr`-driven pass over
the output is orders of magnitude cheaper than a second render.

## P2. `open_body` has no lease cache (see B3)

## P3. Every attachment is transfer-decoded just to learn its length

`crates/sb-mime/src/reader.rs:453` decodes every attachment to fill
`AttachmentDescriptor::decoded_length`, including attachments the user
never opens. Base64 and quoted-printable bodies are fully decoded and
dropped.

`crates/sb-mime/src/reader.rs:497` (`render_input`) eagerly decodes every
CID part in the related scope into an owned `Vec`, referenced or not. A
message with several large images holds all of them — up to 25 MB each —
for the whole render.

`crates/sb-resources/src/bodies.rs:163` (`publish_attachment`) parses the
message twice: `extract_attachment` re-admits, walks, parses and
collects, then `render_input` repeats admission and parsing.

## P4. The reading document is rebuilt from scratch on every open

`crates/sb-mime/src/document.rs:315` (`mail_document`) runs its own
`render_input → detect → rewrite_images → sanitize_run → tree` pipeline
concurrently with the body rendition's equivalent parse and sanitise
work. `DOCUMENT_VERSION` is exported as the intended cache key but no
cache exists.

Fix: derive the document from the already-sanitised body HTML and
memoize per message + acquisition + `DOCUMENT_VERSION`.

## P5. Image bytes are copied twice during a render

`crates/sb-mime/src/rendition.rs:362` copies referenced image bytes out
of `RenderInput.resources` into `Manifest.resources` with `to_vec`, so
peak image memory doubles while input and manifest coexist, then doubles
again when the file is written. Fix: move/drain resources out of an
owned `RenderInput`, or share with `Arc<[u8]>`.

An HTML-only message requested as plain text also copies referenced CID
bytes during the first pass and then clears them. Avoid minting those
bytes for that rendition while retaining CID validation and warnings;
test that the HTML rendition still publishes the same resources.

## P6. Relevance searches score and materialise every hit

`crates/sb-store/src/query/counts/paging.rs:669`: a relevance page with
more than one positive leaf builds
`s{i} AS MATERIALIZED (SELECT rowid, bm25(...) FROM d_search_fts WHERE MATCH ?)`
per leaf, scoring and materialising each phrase's entire match set into
an ephemeral table on every page read and every keystroke, then joining
it. A common term costs 100k+ scored and stored rows per leaf per page
while the page needs only its limit plus a tie run.

`crates/sb-store/src/query/counts/paging.rs:396` (`search_ids`) has the
same shape: it builds the whole Relevance-sorted id index from `hits()`
even when the plan has a single driving FTS leaf that `read_page`
already streams cheaply in rank order. Every accepted keystroke pays a
full scored scan plus a sort of up to 500k hits.

`crates/sb-store/src/query/counts/paging.rs:686` re-evaluates each
leaf's full `check` (the `sb_query_complete_fields` UDF plus a
correlated FTS MATCH) in the SELECT list after the WHERE clause already
evaluated the same predicate, doubling FTS and UDF work per hit row.

Fix: score only the documents the page actually fetches, and carry the
check outcome through the materialised score CTE.

## P7. Statements are re-prepared and re-compiled on every page

`crates/sb-store/src/query/counts.rs:230` (`Compiled::read`) uses
`tx.prepare(sql)`, which bypasses the prepared-statement cache
configured at `store.rs:1568`. Every page recompiles multi-kilobyte
UNION/INTERSECT/CTE statements. `crates/sb-store/src/query/counts/paging.rs:323`
rebuilds an IN-list that `lists.rs:563` also prepares uncached.

`crates/sb-store/src/query/counts.rs:228` (`register`) re-creates both
scalar functions for every statement execution inside `Compiled::read`.

`crates/sb-store/src/query/counts.rs:268` (`compile`) re-queries
`rebuild::latest_rebuild`, `index::search_generation` and
`index::target_version` on every count, page and id-index read, then
`drive::guards` runs two range scans over
`d_search_docs_by_version` with a `NOT EXISTS` per row.

`crates/sb-store/src/query/counts/paging.rs:237`: every page read calls
`plan.check_bindings`, which `search_snapshot` already ran on the same
transaction.

Fix: `prepare_cached` for stable shapes, register once per transaction,
read the constant metadata once into the SearchMemo snapshot.

## P8. Coverage scans run two full aggregate passes

`crates/sb-store/src/query/counts/coverage.rs:247` runs a
`COUNT(DISTINCT …)` pass over the queue joined to every scoped row, then
a second full aggregate pass over all scoped candidates containing two
correlated `NOT EXISTS` subqueries evaluated per row. It is memoized per
scope and generation, but the first search after any write pays both.

## P9. Every cursor embeds the whole canonical query JSON

`crates/sb-store/src/query/counts/paging.rs:920` (`Binding::read`)
reparses `query.canonical_json()`, rebuilds the spec `Value` and does
two `store_meta` lookups per page; `paging.rs:933` embeds the full
canonical query JSON in every row cursor, so a maximum-size query
copies roughly 16 KB into each of up to 200 cursors per delivered page.

## P10. Per-row formatting and measurement in the list

- `AppKitPrototypeList.swift:359` calls `formatter.present($0)` per
  visible row on every reload and every cell reuse, with no cache: the
  date rule, the VoiceOver label, the monogram and the color hash are
  recomputed for unchanged rows.
- `AppKitPrototypeList.swift:358` linearly scans
  `appearance?.appearance?.accounts` per row per configure.
- `MessageListViewController.swift:328` calls
  `Self.accountLabel(id, sections:)` per row, which filters and sorts all
  sections and then calls `SidebarViewController.accountLabel`, which
  builds grouping dictionaries — even in single-account windows where
  the marker is hidden.
- `AppKitPrototypeList.swift:705` `configure` always sets
  `needsLayout = true`, forcing `layoutSubviews` (with synchronous
  `NSString` text measurement at `:788` and `:825`) for every visible
  cell on every reload.
- `AppKitPrototypeList.swift:678` `applyLayerColors()` resolves the
  effective appearance, reads two `NSWorkspace` accessibility flags,
  creates `CGColor`s and assigns five layer colors plus BadgeView
  fill/title/font/outline on **every** reused cell; those `didSet`s call
  `needsDisplay`, so monograms and tag chips redraw on every reuse.
- `AppKitPrototypeList.swift:877` `BadgeView.draw` builds an
  `NSBezierPath` and measures its title with `NSString` on every dirty
  rect.
- `AppKitPrototypeList.swift:566` `contentSubviews` is a computed
  property that allocates a fresh array on every access, and
  `configure` iterates it on every cell reuse.
- `AppKitPrototypeList.swift:394` `tableViewSelectionDidChange` maps
  every selected index into a `Set<Int64>` and compares it to the
  previous set even when one index changed; Cmd-A over a large window
  costs an O(selection) enumeration and equality test per change.
- `ListWindowModel.swift:1176` validates each landed page with
  `window.rows.map(\.occurrenceId).elementsEqual(…)`, allocating a
  200-element array per page per frame.
- `AppKitPrototypeList.swift:269` `restoreSelection()` walks every
  selected id through `model.index(of:)` on every index change and every
  model-driven selection change.
- `MessageListViewController.swift:472` `observe(_:)` re-registers
  `withObservationTracking` and schedules a `Task` on every
  `lastError`/`searchStatus` change; for Search, `publishStatus` changes
  `searchStatus` on many page landings, rebuilding the coverage strip
  while scrolling.
- `LatencyRecorder.swift:129` `nextFrame(of:)` creates a new
  `DisplayLinkClock(view:)` per span end, adding and invalidating a
  `CADisplayLink` plus a `TaskGroup` and a one-second sleep task.
- `crates/sb-engine/src/windows.rs:808` `Windows::ids` builds
  `(0..n).map(…).collect()` for the synthetic source, up to 1,000,000
  `i64` (8 MB) per `window_ids` call, while spec/07 budgets 4 MB for
  500k. Debug-only today, but it is a public API.
- `crates/sb-store/src/query/dates.rs:128` `load_zone` linear-scans
  `db.available()` for every date operand.
- `crates/sb-store/src/store.rs:271` `generations_to_verify` loads every
  due generation id into an unbounded `Vec` with no `LIMIT`.

## P11. `search_ids` collects every matching id unbounded

`crates/sb-store/src/query/counts/paging.rs:379` collects every matching
occurrence id into a `Vec<i64>` with no bound and, for relevance, orders
the whole relation by the CTE-computed score. A broad query over a large
archive materialises and sorts hundreds of thousands of ids per call.

## P12. `sb_query_address` runs IDNA mapping per candidate row

See B4. `crates/sb-store/src/query/counts.rs:482`.

## P13. A cold sidebar reads every account serially

`SidebarModel.readOnce()` awaits `mailboxes(accountId:)` one account at
a time, so the first sidebar paint pays N independent round trips
serially. Read them with a task group, preserving the account order, and
keep the reload's cancellation and coalescing behaviour. Related
per-row costs are in P10 (`accounts.first(where:)` per configured row,
and the date rule's `Calendar.current` work per row).

## P14. Row date formatting mutates one shared `DateFormatter`

`MessageRowFormatter.dateString`
(`apps/macos/Sweetbriar/MessageRowPresentation.swift`) flips a cached
`DateFormatter`'s `dateFormat` per row and consults `Calendar.current`.
It is main-actor confined today, so it is not a current stutter source,
but any move of row formatting off the main actor turns it into a data
race. Use three cached formatters (today / this year / older) or
`Date.FormatStyle`, plus a cached calendar.

---

# Part 3 — Visual and layout issues

## V1. The message list rebuilds on background sync (see B1)

This is listed again here because the visible symptom is a layout
problem, not just a CPU problem: rows flicker, the scroll position can
jump, and selection rings re-draw.

## V2. Row layout measures text synchronously on every pass

`AppKitPrototypeList.swift:741`/`:748` — both `layout()` and
`resizeSubviews` can invoke `layoutSubviews` in one resize, and
`layoutSubviews` measures text for each visible row
(`accountField.intrinsicContentSize.width` at `:788`,
`chip.fittingWidth()` at `:825`). A window resize therefore re-measures
every visible row twice.

## V4. Make first-run and empty states actionable

`ReaderViewController.swift` `placeholder` is a centred
`NSTextField(labelWithString:)` with `.secondaryLabelColor` and
`.title2`. The reader's "No message selected" and "N messages selected"
states, the sidebar's empty states and `AccountsSettingsView`'s
"No accounts available." are all bare text. Apple's own pattern
(`ContentUnavailableView`) is already used in `AppearanceSettingsWindow`
for the closed-archive cases, so the app has the vocabulary but not the
coverage.

Medium priority. The live first-launch window showed both "No messages" and "No
message selected," with neither import nor sign-in available in either pane. Put
Import Mail or Add Proton Account in the empty archive flow. Distinguish an
empty mailbox, a reader without a selection, and a zero-result query with
incomplete coverage. Use one primary action and concise text. Test clean roots,
an empty mailbox beside a populated mailbox, and an incomplete zero-hit search.
[Current list
states](apps/macos/Sweetbriar/MessageListViewController.swift#L565)

A short first-run checklist could also link the keyboard reference and
guide the user to a completed import. Keep those links secondary to
the primary action.

Use one shared empty-state view — large tertiary symbol, title and one
optional hint — for the list panes, reader, Drafts and Out Box. Keep
"Opening…" and failure text separate: `MessageListViewController`'s
`statusLabel` currently mixes loading, errors and emptiness.

**Landed in part (PR #577):** the first-run case is now a SwiftUI island
in the list pane (`WelcomeEmptyState`), shown only when
`SidebarModel.isFreshArchive` holds, the Unified Inbox is shown, there
are no rows and no error/search. Its headline is exactly "No messages"
so the existing XCUITests that wait for that string still match; the
body carries the welcome copy and the two commands. The remaining
reader/empty-mailbox/zero-hit consistency work above is still open.

## V5. The app icon set is thin

`Assets.xcassets/AppIcon.appiconset` carries 16, 32, 64, 128, 256, 512
and 1024 px PNGs, generated from `media-sources/icon.png`. The master
is a 1024 px raster, so a 16 px Dock/Menu Bar rendering and a 1024 px
Finder icon both come from the same source. A dedicated 16/32 px master
or a vector master would sharpen the small end.

## V6. `Info.plist` declares almost nothing

No `CFBundleDocumentTypes` (dragging an `.mbox` or `.eml` onto the Dock
icon does nothing), no `NSUserNotificationAlertStyle`, no
`CFBundleURLTypes` for `mailto:`. The app is not a `mailto:` handler and
cannot be chosen as the default mail client, which is fine for a pilot,
but it is worth writing down.

## V7. Settings is a `TabView` with five tabs of unequal weight

`AppearanceSettingsWindow` builds one `TabView` with Appearance,
Accounts, Reading, Sending and Backups. The Appearance tab carries the
whole per-account color editor; Accounts carries rename/pause/
disconnect plus notification previews plus a Diagnostics button. A
narrow Settings window squeezes both. A 780 × 680 default is workable
but the tab order puts the heaviest surface first.

## V8. The main window's minimum size is very tight

`MainWindowController.minimumContentSize` is 800 × 420 with sidebar
minimum 180, list 320 and reader 280. 180 + 320 + 280 = 780, so the
reader gets 20 pt at the minimum width. A three-pane Mail layout at
800 pt is cramped but legal; the reader pane is effectively unusable
there.

## V9. Row heights cannot follow larger text

Message rows hand-layout at a fixed 56 pt pitch
(`MessageRowMetrics.pitch`) with fixed fonts and y positions, and the
Drafts and Out Box rows hard-code 74 pt. There is no Dynamic Type path:
a user who raises the system text size gets clipped or overlapping
labels. Treat this as its own step with measured text extents and a
re-baselined list benchmark rather than a tweak to the row constants.

## V10. The note placeholder is drawn by hand

`NoteTextView.draw`
(`apps/macos/Sweetbriar/Reader/ReaderMetadataViews.swift`) draws
"Add a note" at a computed origin on every draw. Any text-container
geometry change (Dynamic Type, insets) can misalign it, and it
duplicates what a real placeholder mechanism provides. Use an overlay
field or an attributed placeholder, and pin alignment with a large-text
test.

## V11. Native renditions may not show link destinations on hover

spec/08 says links show their real destination on hover. The HTML web
view gets this from WebKit; `ReadingView` and `PlainTextReaderView`
attach `.link` attributes but implement no tooltip tracking. Verify in
the running app first; if missing, add a mouse-moved character lookup
with `addToolTipRect` for link ranges, shared by both native views and
honoring Reduce Motion. Related: F15's IDN warnings.

---

# Part 4 — Missing features and quality-of-life gaps

## F1. No keyboard navigation between messages with the reader focused

`AppKitPrototypeList` handles arrows, Return and Space while the table
has focus, but with focus in the reader (the common case after opening a
message) there is no Down/Up to move to the next message. Mail does
this, and it is the single most-used navigation gesture after clicking.

## F2. No unread-only / filtering affordance on the list

There is no "Show only unread" toggle, no per-mailbox filter chip and no
quick "Mark all as read" for a mailbox. The Unified Inbox with a Proton
account carrying thousands of unread messages has no fast path.

A bulk "Mark all as read" is deliberately not on this backlog: a mass
Proton mutation is exactly the case the breaker guards exist for, so it
needs its own spec and safety analysis rather than a convenience flag.

## F3. No thread/conversation view

`WindowSpec` has a `.thread` case and `MessageRowPresentation` knows
about it, but the main window has no way to expand a conversation.

Exploratory. Use the existing thread window type and reader message's thread ID
to show a compact read-only trail across Inbox, Sent and archive mail. Expose
unread state, account and whether a link is inferred. Keep header-linked replies
distinguishable from subject-only matches. Start with duplicate copies, missing
parents, unrelated same-subject mail and multiple accounts. Treat a threaded
message list as a separate decision. [Window
types](apps/macos/SweetbriarBridge/Types.swift#L392)

## F4. Attachment saving and preview controls

**Landed in part:** PR #580 gives every attachment a typed, full-width
row with name, size, hover and keyboard/VoiceOver activation. Saving
and previewing remain:

`ReaderViewController` lists attachments and `openAttachment` hands back
a file URL, but pressing Space on an attachment does not open Quick
Look, and there is no "Reveal in Finder" or "Save As…".

Medium priority. Reuse checked attachment acquisition and copy to a destination
the user chooses. Surface unavailable or damaged parts in the reader. Test
cancellation, destination collisions, long Unicode names, and navigation while
acquiring a file. Evaluate Quick Look separately for safe behavior across
supported attachment types. [Attachment
handling](apps/macos/Sweetbriar/Reader/ReaderViewController.swift#L638)

## F5. No drag-and-drop of messages out of the app

`MessageListDrag` supports dragging **within** the app (to a mailbox,
to a folder, to Scheduled). Dragging a message to the Finder or to
another mail client writes nothing.

## F6. No export of the current selection

spec/09 covers import and export; the app has Import… but no
Export… command in the File menu.

## F7. No "Search in this mailbox" scope chip

`SearchFieldController` has a scope popup, but there is no one-click
"search within the mailbox I am in" from the list, and no way to scope a
search to the currently selected message's sender or account.

## F8. No swipe actions on list rows

macOS Mail's swipe-to-archive/delete is absent. Given the app already
tracks drag gestures and filing intents, this is a natural addition.

## F9. No per-account notification muting

`AccountsSettingsView` has one global "notification previews" toggle.
There is no per-account mute, no per-mailbox mute and no
do-not-disturb schedule.

## F10. No compose window position memory

Compose windows are centred; their frames are not autosaved, so a
second compose window opens exactly on top of the first.

## F11. No reader print support

There is no Print… command in the File menu and no `NSPrintOperation`
use in the reader. Printing an email is a routine need.

## F12. No "Mark as Read" delay setting

The reader marks a shown Proton message read through `MarkReadTimer`.
There is no user control over the delay (Mail offers none/1s/5s), and no
way to keep a message unread after reading it.

## F13. No signature support

Nothing in the composer or the settings writes a signature per identity.
For a daily driver this is a hard blocker.
Separately consider reusable draft templates after specifying how identity,
recipients, attachments and private source text are retained.

## F14. No spell-check / grammar indicator control in the composer

`ComposerTextView` is a plain `NSTextView` subclass;
`isContinuousSpellCheckingEnabled` is not set, and there is no
Format → Spelling and Grammar submenu.

## F15. Spec-deferred items worth scheduling explicitly

Deferred by spec rather than forgotten: remote-content blocking UI in
the reader, IDN/homoglyph warnings on link hover, per-tab window
swatches, "Save As…" for attachments (see F4), and a jump-to-mailbox
command palette. Each needs a spec line before implementation.

## F16. Notification clicks do nothing and there are no actions

**Landed:** PR #592 installs a delegate; a tap opens the account's
Inbox (falling back to its first selectable mailbox), including a tap
that arrives while the archive is still opening. What remains:

- No Mark Read / Reply notification categories, and no per-account
  muting (F9). Add categories only with core commands behind them and
  keep the redaction rules: no content beyond the text already approved
  by `NewMailNotificationText`.

## F17. Find does not work in an HTML message

`Edit → Find → Find…` reaches the focused text view, so while the
sanitized HTML web view is first responder the command is inert, which
is how most mail is read. Drive `WKWebView.find(_:configuration:)` from
a small find bar or a reader-level Find handler. Plain-text and Reading
views already work through their scroll views' find bars.

## F18. No unified inbox total or progress treatment

The Unified section has no header total, though the Dock badge now shows
the Inbox unread total. Sync and filing surface through the integrity
banner and transient toasts. A visible unified sidebar total and
a subtle toolbar/sidebar progress treatment would make the shell
scannable. Check secondary-text contrast in Color Wash first.

## F19. Send Later and Snooze

Larger, post-pilot. Both fit the durable-intent architecture: a scheduled
send is an Out Box attempt with a `not_before` time; snooze is a durable
flag plus later re-insertion. The existing Undo Send hold shows the
outbox state machine can carry a delay; offer a scheduled date and time
beside that affordance. Write the spec before any UI.

## F20. A saved draft cannot be discarded from the UI

`DraftModel.discard()`, the FFI `discardDraft` and the core
`discard_draft` command all exist, but no menu or context command
calls them. Closing a compose window leaves its draft in place, including
a new untouched blank draft; no UI command removes it. A failed send can
also leave clutter. Spec/05 step 8 defines the semantics: refused
`Conflict` while an attempt is live, revisions kept.

Add a Drafts-list context command (identifier
`ch.lkmc.Sweetbriar.cmd.discardDraft`) plus a Delete key path, a
confirmation naming the draft, one retained idempotency key per draft,
routing through `ArchiveWriteTasks` so a restore retires it, and an
error path that keeps the rows. Decide what a still-open compose window
does when its draft is discarded elsewhere. Revise spec/08's Drafts
(local) section first. Test discarding removes the row, a live attempt
refuses, a replay is idempotent, a read-only archive refuses, and a
failed reload keeps rows. Separately decide whether a never-edited blank
new draft should be discarded on close: distinguish it from replies,
forwards and any draft with saved content, and revise the close contract
before changing it. Test both close paths and protect nonblank drafts.

## F21. Remember recent searches

Small. Keep recent queries locally (a settings row or UserDefaults),
offer them from the search field's recipe menu and allow clearing them.
Search text is sensitive, so state the retention and keep clearing
obvious; never put it in the opt-in latency log, which is documented to
hold no query text.

## F22. Remember each mailbox's scroll position and selection

The id index makes restoring a rank cheap. Keep the last visible rank or
scroll anchor and selected occurrence IDs per mailbox, restoring them
after its first page lands. Decide whether this is session-only or
persists across launches. `MessageListViewController.install` currently
recreates the table and model for each choice; retaining them per mailbox
is one possible design, subject to a bound on memory. Part 1's B10 also
asks the table to survive Search changes. Add a spec sentence and test a
shrunken mailbox, deleted message, restore and sidebar navigation.
[List installation](apps/macos/Sweetbriar/MessageListViewController.swift#L295)

## F23. Sort control for mailboxes

Window ordering is fixed to `sort_date` descending
(`crates/sb-store/src/lists.rs`); only Search offers relevance/date, and
`date_asc` is reserved in spec/07.
Mail users expect date/sender/subject/size and ascending/descending.
**Spec-first:** extend `WindowSpec`/the list queries with sort keys and
backing indexes, then expose a list header/menu control. Keyset paging
and cursors must be re-derived for the new order. An incremental first
choice could offer newest and oldest first, remembered per mailbox.

## F24. Spring-loaded sidebar destinations

Dragging rows onto a sidebar mailbox works, but holding a drag over a
collapsed folder does not open it after a delay (spring loading), which
slows deep filing. Implement hover timers over the sidebar's outline
view, or disclosure-on-drag. Keep the existing drop-target rule (same
account, not the shown mailbox) and its refusal text.

## F25. Mark all as read

`set_flags` caps at 1,000 ids and the shell has whole-window selection
tokens for large selections; a mailbox-wide mark-read needs a dedicated
preconditioned core intent over the mailbox rather than a shipped id
list. **Spec-first.** Include a confirmation naming the mailbox and
count, undo through the normal filing undo path, and a refusal path for
a read-only archive.

## F26. Menu-bar extra with per-account unread counts

A small status item and menu listing accounts and their unread counts
would suit people who keep the main window closed. Reuse the sidebar's
counts and respect notification settings; it complements the submitted
dock badge (PR #575).

## F27. Saved-view icons or colors

Saved views are all gear folders in the sidebar. A stable symbol or
color per view would make them scannable. Store the choice with the
view's record and spec it first; rename/delete must keep working.

## F28. Search syntax help

The parser supports a query language, but nothing teaches it. A
popover from the search field listing supported operators with examples
is presentation-only. Keep chip editing, grouping and negation semantics
unchanged.

## F29. Message permalinks and deep links

Copy a `sweetbriar:message/<id>` link for the shown message, useful from
Notes. Needs a URL scheme, a resolver, and an explicit missing-message
state; decide whether a link may open another archive safely.

---

# Part 5 — Novel, delightful and quirky ideas

## D1. A "Calm Inbox" opening view

A first-run/opt-in view that shows only messages from humans (no
newsletters, no automated senders), with everything else behind a
"Everything else" row. The engine already classifies automated senders
for the recipient fence (`sb-store`'s account senders and recipient
allowances), so the signal exists. One toggle in Settings → Reading.

## D2. An "Out Box" that reads like a sent-items log

The Out Box already exists. Make it a small timeline with the intent's
state, the retry schedule and a per-row "why it hasn't sent" note. The
data is all there in the intent lifecycle; the presentation would make
the app feel trustworthy in a way no screenshot can fake.

## D3. A "Coverage" ring in the sidebar health row

The sidebar's health row is a line of text. A tiny ring showing the
fraction of Proton messages actually acquired, drawn in the account's
color, would turn the abstract "coverage" number into something you can
read at a glance.

## D4. Keyboard-first "command strip" over the list

When the list has focus, a single-line overlay shows the shortcuts for
the current selection's available commands (Archive Ctrl-Cmd-A, Trash
Cmd-Delete, Star Shift-Cmd-L…). It fades in on the first arrow key of a
session and out after a few seconds. Cheap, and it teaches the app.

## D5. Per-mailbox accent hues derived from the account color

Account colors exist. Deriving each mailbox's sidebar tint as a hue
rotation of its account color, at low saturation, would give the
sidebar structure without adding a new color system.

## D6. A "Reading view" progress and estimated read time

The Reading view renders a semantic document. Adding a word count and
"3 min read" under the subject costs one pass over the document nodes
and is genuinely useful for long mail.

## D7. Attachment "preview strip" in the reader header

Instead of a vertical list of attachment rows, a horizontal strip of
thumbnail tiles under the subject, with the count. Images already have
bytes in the manifest; `NSImage` thumbnails are cheap.

## D9. Signpost-free latency surfacing

The app already records a latency log behind Debug. Surfacing a
discreet "slow" pill in the window subtitle when a p95 span exceeds its
budget (spec/08 "Performance budgets") would let the owner notice a
regression without opening a report.

## D10. Sound and haptic restraint

The app posts no sounds at all. Mail's send "whoosh" and the new-mail
alert are part of why it feels finished. A single, quiet, optional send
confirmation sound (and nothing else) would be enough.

## D11. Hover quick actions on message rows

Rows have no hover affordance. A tracking-area overlay offering Reply,
Archive and Flag would fit the fast, keyboard-heavy personality; filing
already handles one call per account and the row actions from PR #579
provide the same command set. Keep it clear of selection, drag,
context-click targeting and the row's hand layout, and keep every action
reachable from the context menu, swipe and keyboard.

## D12. Composer Markdown preview beside the source

The composer styles Markdown live (`MarkdownStyler`) but always shows
source. A toggleable split preview — styled output in a read-only text
view next to or below the editor — would make Markdown-first composing
feel safe without hiding the source. Keep it presentation-only (never
stored), undo-aware, and off by default for plain-text drafts.

## D13. Sender rhythm

Show the times of day a sender usually writes, derived locally from the
archive, in the reader header ("usually mornings"). Purely local,
privacy-safe, and a small delight. Never send or store it outside the
archive.

## D14. Quiet-hours triage view

A menu extra that gathers mail that arrived overnight into one temporary
view with Archive / Reply / Keep as three large keys. The saved-view and
filing machinery already covers the pieces; this is an arrangement, not
a new model.

## D15. Message postcard in the Reading view

Render the sender's most recent message as a small quoted card at the
bottom of the Reading view, reusing `ReadingRenderer`. It makes one
sender's context visible without leaving the message.

## D16. Typing-first compose

For quick replies, reduce chrome: subject optional until send,
recipients shown as tokens only when needed, and the body focused first.
Keep every field reachable and the existing safety checks intact.

## D17. Local focus score

A diagnostic-only measure (response time, thread depth) in Activity,
never sent, to explain why a mailbox feels heavy. Keep it clearly
labeled as an estimate.

## D18. "Where did that go?" filing timeline

A one-key timeline of recent filing actions with jump-back, built on the
undo toast and intents. Turns the ten-second undo reflex into a
browsable safety net; the durable intent records already carry what is
needed.

## D19. Animated account wash on sidebar selection

Mirror the composer's crossfade when the list changes account, under
Reduce Motion. Small, but it makes the account color system feel
connected across surfaces.

---

# Part 6 — What I would implement, in order

Ranked by (user-visible impact × confidence) / diff size.

| # | Entry | Why first |
|---|---|---|
| 1 | B1 — stop the full table reload on sidebar reload | largest single stutter source, small diff, pure Swift |
| 2 | B2 — HashMap cid resolution | clear quadratic bug, small diff, existing test surface |
| 3 | P1 — render the body once | 2x on every message open, spec/08's 100 ms budget |
| 4 | B3 — cache the body lease | removes re-render on re-selection and source toggling |
| 5 | B7 — visible-only page reload | removes off-screen cell churn (`setVisible` guard tracked in PR #586) |
| 6 | B4 + B5 + P7 — search hot path | the 100 ms search budget |
| 7 | B8 + B9 — reverse index and dropped-selection lookups | removes main-thread stalls |
| 8 | B10 + B12 — search field round trips | per-keystroke latency |
| 9 | P3 + P4 + P5 — render once, decode lazily, share image bytes | memory and CPU on image-heavy mail |
| 10 | V4 + F1 + F4 — reader empty states, Down/Up from the reader, Quick Look | visible polish |
| 11 | F13 — signatures | daily-driver blocker |
| 12 | D1 — Calm Inbox | the delightful one |

Entries deliberately left out of the implementation pass: everything in
Part 4 that needs a spec change first (F2, F3, F6, F9, F15), everything
that needs a design decision from the owner (D2, D3, D10), and the
benchmark-only items (P11, `windows.rs` synthetic fill). The tag-draft
fix and `setVisible` no-op guard have separate submitted patches.

---

# Part 7 — Parallel curated pass

A second pass over `19fde48e` recorded the additional entries below.
See V4 for first run, F3 for conversation trails, and F4 for attachments.

## Correctness and interaction

### Restore the preceding selection when clearing Search

High priority. The field remembers the previous window spec, but opening it creates an empty selection. Capture occurrence IDs when Search first replaces the list. Restore surviving IDs when the matching list is ready and use a bounded lookup for a scroll anchor outside the first page. Test Escape and the clear button, a selected message removed during Search, intervening sidebar navigation, and returning keyboard focus to the list. [Search contract](spec/08-macos-app.md#search-m16), [Search field model](apps/macos/SweetbriarBridge/SearchFieldModel.swift#L85), [List install](apps/macos/Sweetbriar/MessageListViewController.swift#L295)

### Refresh tags and notes without reloading the reader body

High priority. The model preserves the body during metadata refresh, but the controller calls its full render after either write, redoing TextKit styling or starting another WebKit navigation. Refresh only metadata for the current message. Test a note edit and tag add/remove while scrolled deep into HTML and native bodies. Preserve scroll position and selected text, surface write errors, and discard late UI completions from another selection. [Metadata writes](apps/macos/Sweetbriar/Reader/ReaderViewController.swift#L593), [Model refresh](apps/macos/SweetbriarBridge/ReaderModel.swift#L202)

### Apply Reading view sender preferences in click order

Medium priority. Membership is checked before the settings write returns. A toggle off can see the old membership and return while a toggle on is still pending. Two senders can also start from the same array. Queue edits in the shared memory and apply each against the last committed set. Preserve immediate rendition switching. With a gated source, test on/off during a pending write, two senders across windows, a write failure, and archive retirement during a write. [Sender preference model](apps/macos/SweetbriarBridge/ReadingViewMemory.swift#L48)

## Native layout and accessibility

### Give the reader body a consistent share of the window

High priority. In the live Dark Mode reader, the tag field and empty note occupied two rows before the rendition controls and body. The header also stacks full addresses, account, attachments and status above the scrolling body. The horizontal tag list also has a width cap without dedicated overflow. Keep subject, sender address and account visible first. Put long recipient lists, numerous attachments, and optional tags and notes behind accessible disclosure. Test 20 attachments, 12 tags and long addresses at the minimum pane width. Require a readable first body screen, a reachable attachment action, and VoiceOver access to every disclosed detail. [Reader layout](apps/macos/Sweetbriar/Reader/ReaderViewController.swift#L132)

Long recipient lists collapse behind Show All Recipients since PR #587 (Part 8); the attachments, tags and notes remain.

### Establish shared density and typography for the three-pane window

Medium priority. Keep native sidebar selection and visible account cues. Prototype a consistent subject/sender hierarchy, alignment and control sizing, and a readable maximum line length in native reader renditions. Compare the same synthetic messages in light and dark appearance and at narrow and wide sizes. Test Increase Contrast, Full Keyboard Access, long names and scan speed. Use measured text extents before changing table row height. [Reader layout](apps/macos/Sweetbriar/Reader/ReaderViewController.swift#L132), [Message-list contract](spec/08-macos-app.md#message-list)

### Give local drafts a calm, purposeful composer state

Medium priority. A fresh local draft shows "Without a sending identity" in its top strip and From control, plus "Color Wash appearance is unavailable for this sending identity" beneath the editor. Present one useful explanation of local saving near From and reserve blocking or appearance messages for the condition they describe. Explore an optional Cc/Bcc disclosure for empty fields and separate the document, attachment and sending controls visually. Test a local-only archive, a transient catalog failure and a valid or provisional identity. Preserve From and send safety cues, keyboard access, editable draft contents and any persisted Cc/Bcc recipients. Revise the composer contract before changing the default field layout. [Composer chrome](apps/macos/Sweetbriar/Composer/ComposeWindowController.swift#L1422), [Composer contract](spec/08-macos-app.md#composer)

### Add a reader text size control

Medium priority. Add standard larger, smaller and reset actions for plain and Reading view over a bounded reader setting. Preserve relative heading scale and pitch for code and source. Validate the large size in the minimum pane and with accessibility tools. Specify separately how the controls apply to sanitized sender HTML. [Plain text](apps/macos/Sweetbriar/Reader/PlainTextReaderView.swift), [Reading renderer](apps/macos/Sweetbriar/Reader/ReadingRenderer.swift)

## Performance and qualification

### Measure reader latency by rendition and decouple optional work

High priority. The model awaits semantic extraction before displaying the body, including source, and native styling runs on the main actor. Profile short plain, long quotes, large HTML and semantic messages with the M6.4 log. Record first body, document-ready, and arrow-navigation p95 separately. If optional parsing dominates, let Sender Layout appear first when the sender has no Reading view preference, with a loading or disabled toggle until ready. Use a parked-document test, retain remembered-sender behavior and verify lease retirement on navigation. [Reader load](apps/macos/SweetbriarBridge/ReaderModel.swift#L237)

### Remeasure Search after the first-page and frame-delivery changes

High priority. Historical app p95 was 145.8 and 150.0 ms for a 100,000-message archive against 100 ms. Later M1.6r4 changes affect the result. Repeat the five query classes on the same fixture and record field, opening-page, delivery and first-frame timings, including refresh rate. If replacing the AppKit table dominates, prototype keeping the table while changing the query model. Preserve handle retirement, the specified 30 ms debounce, and selection semantics. [Recorded benchmark](plan/qualification/M1.6q2.md), [M1.6r4](plan/M1-local-archive.md#L773), [List installation](apps/macos/Sweetbriar/MessageListViewController.swift#L295)

### Profile the composer's full render after each edit

Medium priority. Draft capture compares full buffers and then updates attachments, From, safety banners and title. TextKit also styles Markdown on the main actor. Profile short drafts, long quoted replies, several attachments and marked input. Count recurring rebuilds. If chrome dominates, update only surfaces whose inputs changed. Keep byte-exact source capture, autosave behavior, undo and IME handling. [Capture path](apps/macos/Sweetbriar/Composer/ComposeWindowController.swift#L506), [Composer render](apps/macos/Sweetbriar/Composer/ComposeWindowController.swift#L1422)

### Complete the existing pilot gates

High priority. Record warm launch, cached reader, compose, filing and search percentiles on the current build. Record commit, hardware, archive size, refresh rate and sync state. Run the VoiceOver reading, reply, From choice, filing and Out Box tasks, plus the dark appearance, contrast, transparency, keyboard, RTL and Japanese input gates on synthetic data first. Follow the plan's approval and evidence requirements for real account effects and the separately scheduled power cut. [M6.4 qualification](plan/qualification/M6.4.md)

## Features and product experiments

### Jump to a mailbox or saved view by typing

Medium priority. Add a Mailbox menu command and a keyboard palette using current sidebar entries and the filing palette's behavior. Rank exact, prefix, then recent matches. Show account and full path to disambiguate duplicates. Return navigates and Escape restores focus. Preserve the four existing unified shortcuts. Test keyboard-only travel across duplicate names in two accounts and smoke-test sidebar selection. A later extension could offer every enabled command, using its stable `ch.lkmc.Sweetbriar.cmd.*` identifier and fuzzy matching. [Filing palette](apps/macos/Sweetbriar/Filing/FilingPalette.swift), [Mailbox menu](apps/macos/Sweetbriar/MainMenu.swift#L436)

### Pin favorite mailboxes and saved views

Medium priority. Prototype a short, reordered list using stable sidebar targets, with account cues on duplicates. Reuse it in the jump palette. Cover account renames, disconnected accounts, removed saved views, restoration, and keyboard reordering. Review the sidebar spec before changing its sections. [Sidebar contract](spec/08-macos-app.md#sidebar)

### Preview the outbound Markdown rendition

Medium priority. Prototype a Preview command using the core's outbound renderer for the draft and existing safe rendering controls. Include lists, code, inline attachments and quoted replies. Preserve editor selection and undo when leaving preview, block external resources, and leave draft publication and sending to their existing commands. Compare the preview against the generated outbound text and HTML for the same fixture. Only consider a side-by-side editor after validating the single-pane flow. [Composer contract](spec/08-macos-app.md#composer)

### Reply to a selected passage

Medium priority. Offer Reply with Selection for the native reader renditions. Capture message and selected text together and quote only that passage, using the existing reply target, identity checks and quote handling. Test plain and Markdown quotes, Unicode, the standalone reader window, and ordinary Reply. [Reader view](apps/macos/Sweetbriar/Reader/ReaderViewController.swift), [Composer contract](spec/08-macos-app.md#composer)

### Show saved-view scope before saving

Medium priority. A scoped search saved to a view currently runs over all accounts. Show this resulting scope in the sheet when it differs from the active search. If users want persistent scope, revise the contract and retain the meaning of the older saved views. Test a query with hits in two accounts, a missing mailbox, rename, and backup or restore. [Search and saved-view contract](spec/08-macos-app.md#search-m16)

### Explain compatibility before account setup

Medium priority. Document the direct Proton workflow, local archive behavior, and supported Apple Mail and mbox import paths in the README and first-run guidance. Link backup and import help at the action. Scope and qualify general IMAP/SMTP separately if the pilot expands. [Overview](spec/00-overview.md), [Current File menu](apps/macos/Sweetbriar/MainMenu.swift#L329)

### Copy a passage as a Markdown citation

Exploratory. Add a reader context action that copies the selected passage as a Markdown block quote with its source subject and date. Let the user include the sender address when they need it. Start with native reader text selection and the standard clipboard. Support nested quotes, Unicode and multiline headers, and ensure the metadata cannot change the Markdown structure. Prototype internal links only after choosing a reference that survives restore and provides a useful missing-message state. This gives researchers and writers a quick way to collect quotations in their existing notes app.
---

# Part 8 — Second review (2026-11-26, opencode)

A second pass over `19fde48e`, independent of Parts 1 to 7: the Rust
crates (store, engine, service, FFI, MIME, Proton, resources), the Swift
app and bridge, and the build and CI tooling. It ran the Debug app on a
synthetic archive of six mailboxes (dark mode, 1280 x 840), read the
app's Accessibility tree, and used thirteen read-only helper reviews
(LongCat, StepFun, Devin, Nemotron) whose findings were checked against
the code before one was recorded. Line numbers are those of `19fde48e`.

Entries that duplicate Parts 1 to 7 name the entry they extend instead of
repeating it.

## Changes from this review

| Change | PR | State |
|---|---|---|
| `scripts/build.sh` finds rustup's tools outside `CARGO_HOME` | #573 | merged |
| The unit-test host opens a private archive, never the user's | #582 | merged |
| The Reading view memory resets after a restore | #583 | merged |
| `Info.plist` regenerated from `project.yml` | #584 | merged |
| Long recipient lists collapse behind **Show All Recipients** | #587 | merged |
| `scripts/bundle-notices.sh` is shellchecked in CI | #591 | merged |
| A panicking writer observer no longer hangs its write | #595 | merged |
| The Proton events cursor is checked like every other id | #596 | merged |
| A backup reference that is not a hash fails `Corrupt` | #597 | merged |

Tried and dropped: hiding the sidebar's symbols from VoiceOver with
`setAccessibilityElement(false)`. The symbol stayed in the Accessibility
tree, read back from a running build. The row's cell has to become one
element without children (next section).

## Correctness and interaction

### Make each sidebar and list row one VoiceOver stop

Medium priority. In the Accessibility tree a sidebar row is a cell with
its label ("Inbox, 1 unread") and then three children: the title, the
unread badge, and the symbol as an image whose description is the
symbol's default name ("move" for every folder, "Document page" for
Drafts). A message row likewise exposes its three text fields and its
markers after the row's label. VoiceOver stops on each. Make the cell a
single element with no children and keep its label complete; the badge
is an element on purpose for the UI tests
(`SidebarAppearanceTests.theUnreadBadgeKeepsItsCountAccessible`), so
check which XCUITests read it before changing it, and verify with a
VoiceOver pass. [Sidebar cell](apps/macos/Sweetbriar/SidebarViewController.swift#L789),
[Row cell](apps/macos/Sweetbriar/AppKitPrototypeList.swift#L524)

### Keep Mark Read and Star enabled for a selection whose rows are not cached

Medium priority. `flagTargets` and `flagWindowSelection` validate against
`ListWindowModel.row(at:)`, which answers only for the three cached
pages. A range across pages, or Select All followed by scrolling, leaves
selected ids uncached, both commands disable with no explanation, and
the whole-window path needs `row(at: 0)`, which is gone once page 0 is
evicted. Decide enablement from the selection's ids or its window token,
not from cached rows. Test a shift-click across two pages, Select All in
a long mailbox, then scroll. [Flag targets](apps/macos/Sweetbriar/MainWindowController.swift#L681)

### Invalidate the list's display link when the list closes

Low priority. `MessageListViewController.close()` never calls
`clock?.invalidate()`, and a `CADisplayLink` retains its target, so each
window rebuild leaks a paused link for the life of the process.
`ListPrototypeWindowController.windowWillClose` does invalidate its own.
[Test seam to copy](apps/macos/Tests/SweetbriarTests/PrototypeWindowTests.swift#L54)

### A double-click below the drafts opens the selected draft

Low priority. `DraftsListViewController.openClicked` falls back to
`selectedRow` when `clickedRow` is -1. `MessageTableView` guards the
clicked row; do the same. [Drafts list](apps/macos/Sweetbriar/DraftsListViewController.swift#L143)

### Helper-reported defects to confirm before fixing

Each was reported by a helper and not confirmed by reading every caller.

- `crates/sb-ffi/src/drafts.rs:382`: two equal unpublished attachments
  in one save compare equal, so `into_service` clones one publication
  and the second spend of its reservation fails the save with
  `Conflict`. Confirm with a draft holding the same file twice.
- `crates/sb-mime/src/search_document/html.rs:351`: one HTML token over
  16 KiB (a `data:` image URI, a long comment) trips `PendingToken`, and
  `search_document.rs:149` drops the whole body from the index.
  `paste/dom.rs:456` accepts the same shape by counting separators
  too; port that rule and cover a newsletter with inline images.
- `crates/sb-proton/src/client.rs:174`: `clamp_retry_after` reads only
  delta-seconds. An HTTP-date falls to the 1 s floor, the most
  aggressive reading, under a 429 (read). Parse the date, and answer a
  longer wait, not the floor, to a header that cannot be read.
- `crates/sb-proton/src/client.rs:362`: a human-verification token is
  taken before the request is sent, so a request that never reached
  Proton spends it and the user solves a verification again.
- `crates/sb-proton/src/client.rs:762`: `read_body` buffers a JSON
  reply with no byte bound; `collect_download` has one.
- `crates/sb-proton/src/reconstruct.rs:268`: the decrypted body's
  entity headers are appended unfiltered, so a forged
  `X-Pm-Internal-Id` precedes the real one (line 323).
- `crates/sb-proton/src/send_transport.rs:627`: `send_packages` does
  not check the returned `Sent.id` against the draft id, unlike
  `message()` and `read_draft()`.
- `crates/sb-proton/src/send_transport.rs:713`: attachment file names,
  media types and content ids go into the multipart body verbatim; a
  name with CR or LF corrupts the upload.
- `crates/sb-proton/src/keys.rs:584`: `restore_session` maps
  `KeyUnlock` to `Unavailable`, so a password changed elsewhere looks
  like a network failure.
- `crates/sb-store/src/maintenance.rs:135`: `finish` updates whichever
  run of a job is open; a second run started before the first finishes
  would take the first outcome.
- `crates/sb-store/src/gc.rs:139`: `object_seen` can run
  `attachments_json LIKE '%sha%'` per candidate inside one read
  transaction. The unindexed send-MIME and draft-attachment arms are
  reached only when earlier reference checks do not match, so archive
  size alone overstates the impact. Time a pass with many retained sends
  and draft revisions; consider an index for sent MIME and a keyed
  attachment projection in a future migration.
- `crates/sb-store/src/backups/staging.rs:151`: a data root at a
  filesystem root reaches `expect("a data root has a parent")`;
  `restore_pending::root_name` refuses the same case.
- `crates/sb-store/src/backups/stage_quarantine.rs:225`: when the
  quarantine work and its close both fail, the close error hides the
  first.
- `apps/macos/Sweetbriar/Composer/ComposeAttachmentControls.swift:260`:
  the attachment list is rebuilt on every keystroke; diff first, as
  `renderRecipientFence` does.

- `crates/sb-engine/src/settings.rs:249` and `crates/sb-engine/src/identity.rs`:
  `set_setting` and the identity commands write through
  `resources().write(..)` directly, not through a counted worker, so a
  restore's quiesce may not wait for them (spec/03 "Integrity", spec/08
  "Backups settings"). The most serious lead in this list: confirm
  which writes `blocking.start()` counts and whether one of these can be
  in flight across the swap.
- `crates/sb-proton/src/client.rs`: the human-verification token is a
  client-wide slot shared by every account, so a token obtained for one
  account can ride another account's next authenticated request.
- `crates/sb-store/src/recipients.rs`: the recipient fence is specified
  for To, Cc and Bcc together; the code is reported to gate it on To
  being non-empty (spec/05 "Recipient check").
- `apps/macos/SweetbriarBridge/ListWindowModel.swift`: `filingSelection`
  computes and sorts ranks for a `.window` selection token that needs
  none of them.

### Spec and code disagree in two places

`identities(account_id)` (spec/07 "Other queries") is implemented
nowhere; `compose_catalog` serves the app. The selection snapshots of
spec/07 (`open_selection`, `selection_ids`) are not exported, so
`ExportScopeInput::Selection` has no caller. Decide for each whether the
spec or the code changes.

## Performance

### Do not rebuild the sidebar on every list generation

Medium priority. `MainWindowController.listSaw` sends each list
generation to `SidebarModel.noteGeneration`, then
`SidebarViewController.reloadSections` rebuilds every row object and
calls `reloadData()`. A sync landing fifty changes reloads it fifty
times. This is the other half of Part 1's B1, which reads the
consequence in the list. Reload the counts in place, or debounce to one
reload per burst. [Sidebar reload](apps/macos/Sweetbriar/SidebarViewController.swift#L179)

### Cache the calendar the row formatter uses

Low priority. `MessageRowFormatter.dateString` resolves
`Calendar.current` and makes two comparisons per presented row, on the
list's hot path. One autoupdating calendar beside the cached
`DateFormatter` does. Related: Part 2's P14, on the same function.
[Row formatter](apps/macos/Sweetbriar/MessageRowPresentation.swift#L215)

### Show the header while the body loads

Medium priority. `resetSelection` hides the whole header and shows
"Loading message…" until `model.show` returns. For a body Proton still
has to fetch, the pane shows neither subject nor sender, which the row
already knows. Show them at once, and a progress indicator beside the
note. [Reader load](apps/macos/Sweetbriar/Reader/ReaderViewController.swift#L262)

The synthetic local archive also showed a brief full-pane flash on rapid
selection. Measure a grace period for the placeholder and compare
retaining or crossfading the previous rendition. Never allow actions,
links or CID requests from the old message to operate as the newly
selected one. Test stale loads, rapid reselection, accessibility
announcements and Reduce Motion.

### Give HTML mail a dark canvas when it sets no colors

Medium priority. In Dark Mode an HTML message renders as a white
rectangle across the reader (seen). The sanitizer parses every
declaration, so it can report whether the message sets a text color or
a background. For one that sets neither, inject `color-scheme: dark`
and the system canvas colors; for one that sets either, leave it as it
is. Needs a spec line in 08 "Reader". [Sanitizer](crates/sb-mime/src/render.rs#L286)

## Visual and layout

### Make the tag field and the note quiet until they are used

Medium priority. Above every body the reader stacks a full-width
bordered tag field, a note placeholder and then the rendition controls
(`ReaderViewController.loadView`, `:128`), about 220 pt before the body.
A row of tag chips with a small add button, and a note that is a single
line until clicked, would give the body most of that back. The spec
requires both to be present, not to be large. [Reader layout](apps/macos/Sweetbriar/Reader/ReaderViewController.swift#L128)

### Use one style for the two rendition controls

Low priority. "View Source" is a bordered push button and "Reading
View" a checkbox, greyed out for mail with no reading document (seen).
Two small toggle buttons of one style; keep the
`readingViewToggle` identifier, which tests use. [Rendition controls](apps/macos/Sweetbriar/Reader/ReaderViewController.swift#L153)

### Keep an inline image out of the attachment list

Low priority. The reader's attachment rows are typed since PR #580. An
inline image that the body already shows is still listed as a row, named
"attachment" when the part has no file name (seen), because the
manifest's attachments include the parts with a content id. Mark those
as inline and leave them out, or show them behind a disclosure. Extends
Part 3's F4. [Attachments](apps/macos/Sweetbriar/Reader/ReaderViewController.swift#L638)

### Give the unread dot a fixed gutter

Low priority. An unread row indents its sender past the dot while its
subject and snippet keep the column (Accessibility frames: sender at
x 269, subject at x 257), so the three lines of every unread row are
out of step. Reserve the dot's width for all rows. [Row layout](apps/macos/Sweetbriar/AppKitPrototypeList.swift#L802)

### Use the solved foreground for the tinted header's secondary text

Low priority, accessibility. `renderHeader` passes
`tint.foreground.withAlphaComponent(0.75)` as the secondary color in a
tinted message window. The core solves the foreground for 4.5:1 at full
opacity; blended it no longer meets it. Pass the solved foreground for
both. [Header tint](apps/macos/Sweetbriar/Reader/ReaderViewController.swift#L525)

## Missing features

- **A Dock badge preference.** The Inbox unread badge now exists. Consider
  a setting to turn it off, with a spec line and a test that clearing the
  preference updates an already-visible badge.
- **Read and star for the local archive.** Both commands are disabled
  for local-archive rows ("The local archive's flags are later",
  spec/07 "Flags"), so imported mail can never be marked unread. A core
  step: a local flags table and the same `set_flags` path.
- **Role hints for imported mailboxes.** Roles are not read from file
  names (spec/08 "Sidebar", by decision), so after an Apple Mail export
  import the Unified Inbox is empty while "Local Archive > Inbox" holds
  everything (seen). Offer "Treat as Inbox" for a mailbox named Inbox,
  Sent, Drafts, Trash, Archive or Junk at import, and store the role.
- **Skip a snippet that repeats the subject.** A message whose first
  body line equals its subject shows it twice ("Your receipt from
  Harbor Coffee Co.", seen). In `sb-mime`'s summary, drop that line and
  take the next. Needs a spec line in 07 for the snippet's rule.
- **Actions in the reader.** Reply, Reply All, Forward, Archive and
  Trash act from the toolbar, the menu or the context menu only; a
  small action row under the header would save the trip.

## Ideas

- **Move all from this sender.** A row's context menu offers it, with
  the filing toast's Undo; the selection token path already exists.
- **Sender monograms.** In the reader header and the list, colored by
  the sender's domain and never by an account color; the account colors
  keep their meaning.

## Build and developer experience

- **`xcodebuild test` on a Mac with a development team.** The commands
  in AGENTS.md work on CI because CI passes `CODE_SIGN_IDENTITY=-`. On a
  Mac signed into a development team the test action fails looking for
  a "Mac Development" certificate (seen). Add the flag to the documented
  commands.
- **Say where the tools live.** `cargo` need not be on an agent's
  `PATH`; the pinned toolchain's `bin` under `rustup`'s home has it, and
  `cargo-deny` and `cargo-about` live in `~/.cargo/bin`. One line in
  AGENTS.md on both saves every new agent ten minutes.
- **`scripts/bundle-notices.sh` names a script that is not there.**
  Line 51 runs `scripts/swiftpm-notices.py`; no such file exists. The
  branch would fail the build the day a remote SwiftPM package appears.
  Commit the recipe or remove the branch.
- **`bundle-notices.sh` runs on macOS only.** It is shellchecked on
  Linux now (PR 591) and never executed in CI. A `bash -n` under
  `/bin/bash` 3.2 in the macOS job, as `build.sh` gets, would catch a
  bash 4 construct.
- **PR and release builds use different Xcodes.** `release.yml:24` pins
  `Xcode_26.6.app` and `macos.yml` sets no `DEVELOPER_DIR`. Pin both or
  neither, and keep spec/11's claim true.
- **A plain `xcodebuild` needs `rustc` on `PATH`.** The "Bundle licence
  notices" build phase runs `scripts/bundle-notices.sh`, which calls
  `rustc --print sysroot`; without the toolchain on `PATH` the build
  fails with `rustc: command not found` (seen). `scripts/build.sh` finds
  the tools first; the AGENTS.md `xcodebuild` commands do not.
- **A test host that is killed leaves its private archive behind.**
  The host removes it at quit (PR 582), but `xcodebuild` usually ends
  the host without a quit, so `SweetbriarTests-*` directories collect
  in the temporary directory until the system purges it (six after this
  review's runs). Removing stale ones at the next host launch would do.
- **One test is timing-sensitive under load.**
  `sb-service`'s
  `a_breaker_event_is_listed_and_its_resolution_wakes_the_subscription`
  failed once in three full local gates on a loaded Mac (the condition
  had already left `NotAnswering`) and passed alone each time.
- **Reported by a helper, not confirmed:** CI passes no `--locked`, so a
  `Cargo.lock` drift is invisible; tool binaries are downloaded without
  checksums; the tag and version are compared only after the full Rust
  gate; `check-ffi` prints its success wording when its Swift
  type-check was skipped; the derived app icon has no drift check;
  Dependabot skips `fuzz/` and `ci/go-proton-api`; spec/11 describes
  caches CI does not have; the layering check runs twice per leg.

# Part 9 — Synthetic-mail and core follow-up

A further review of `19fde48e` used a 48-message synthetic archive and
light and dark screenshots of the list, reader, search, composer and
settings. References below were checked against `f196d090`; confirm each
reproduction on the branch used to implement it. Performance proposals
need measurements rather than assuming the estimated gain.

## MIME and reader fidelity

### Preserve block boundaries in HTML-derived text

HTML-only list snippets can join adjacent headings, divs, list items or
table cells into one word; the same `html_to_text` conversion is used
for a plain-text rendition. The full-message and extent snippet paths
differ, so fix and test both together. Use block-aware HTML extraction
that preserves inline adjacency, decodes HTML5 legacy entities, and
retains existing byte and resource limits. Test minified headings,
nested lists and tables, inline spans, Unicode and malformed HTML, as
well as full versus partial input. Decide separately whether old stored
snippets should be rebuilt. Consider preferring authored text over quoted
replies and signatures, with a fallback when no authored text exists.
[Summary](crates/sb-mime/src/lib.rs#L138),
[Extent snippet](crates/sb-mime/src/extract.rs#L142),
[Plain rendition](crates/sb-mime/src/rendition.rs#L218),
[Search text walker](crates/sb-mime/src/search_document/html.rs)

### Distinguish numeric prose from link labels that name a host

The link warning's label-host detector can interpret dotted numeric
prose, such as a version number, as a host and report a misleading
domain mismatch. Reproduce with real link labels before changing the
heuristic. Keep detection of actual IPv4 addresses, domains and IDNs;
test matching and mismatched destinations as well as versions and
prices. Do not suppress a genuine spoofing warning to reduce noise.
[Host detection](crates/sb-mime/src/render.rs#L601)

### Establish a safe base presentation for unstyled HTML mail

The sanitized HTML fragment was observed with WebKit defaults, small
margins and an oversized blocked-image placeholder. Test messages with
no sender styling, explicit colors, inline images and wide tables at
minimum pane width in both appearances. Specify a restrained base font,
spacing and overflow behavior, preserving sender inline styles and the
content-blocking boundary. Coordinate adaptive colors with Part 8's
color-free dark-canvas entry; do not infer safety from an absent color
on only the top-level element. [Sanitizer](crates/sb-mime/src/render.rs),
[HTML reader](apps/macos/Sweetbriar/Reader/HTMLReaderView.swift)

## Composer and interaction

### Add source-preserving Markdown authoring commands

Beyond Bold and Italic, consider link, strikethrough, code, quote and
bulleted or numbered list commands, plus list continuation and an empty
item to end a list. Define shortcuts and formatting behavior in the
composer contract first. Each command must preserve unselected text,
act as one undoable edit, avoid touching marked input and keep TextKit 2
active. Cover selections across paragraphs, existing markers and
Unicode. [Formatting](apps/macos/Sweetbriar/Composer/MarkdownFormatting.swift),
[Editor](apps/macos/Sweetbriar/Composer/ComposerTextView.swift)

### Expose literal plain-text sending

The draft model carries `plainFlowed`, but the review found no control
to select literal non-flowed text, useful for patches and code. Confirm
the current composer path, then specify a plainly named option for
plain mode, with a durable per-draft value and an accessible checked
state. Test reopening a draft and both MIME encodings.
[Composer contract](spec/08-macos-app.md#composer),
[Message format](spec/05-send.md)

### Improve remaining compact states and controls

- Check the unread dot and other row markers against selected-row
  backgrounds: an accent-colored unread dot disappeared on an emphasized
  accent selection in the synthetic review. Use selection-aware colors
  with sufficient contrast, including inactive and high-contrast windows;
  keep unread state available to VoiceOver.
  [Row markers](apps/macos/Sweetbriar/AppKitPrototypeList.swift)
- In a one-account archive, evaluate hiding the repeated account rail and
  monogram in unified and search rows to give the message more width.
  Revise the row contract first; retain identity cues whenever multiple
  accounts can appear and test the transition when accounts are added or
  removed. [Row contract](spec/08-macos-app.md#message-list)
- Message-row VoiceOver labels currently name sender, subject, date and
  flags but omit attachment state. Add an unambiguous attachment cue,
  coordinate with the existing tag-collapse accessibility work, and test
  both lists. [Row label](apps/macos/Sweetbriar/MessageRowPresentation.swift#L179)
- The scope popup was visibly truncated at a narrow search width. Measure
  minimum supported widths and choose a readable scope presentation
  without hiding sort or search accessibility.
- Plain-text quote levels currently rely on colored italic text and
  visible `>` markers. Prototype accessible quote bars and signature
  de-emphasis while keeping copy and selection faithful to source.
- Recipient safety-token warnings wait until field editing ends. This is
  tested and deliberately avoids replacing the token value during editing,
  which would erase native typing history. Explore earlier feedback only
  if focus, marked text and undo remain intact; retain the end-edit test.
  [Recipients](apps/macos/Sweetbriar/Composer/RecipientField.swift)
- A diagnostics save error remains after a later successful save. Clear
  it on success while retaining a useful error on failure; test failure,
  retry and cancel. [Diagnostics save](apps/macos/Sweetbriar/DiagnosticsSheet.swift#L71)
- Date labels should update across midnight and time-zone changes even
  when no mailbox generation changes. Invalidate visible rows on the
  relevant system notifications; test an injected calendar boundary.
- The app-hosted import layout test logged conflicting vertical
  constraints in `SearchCoverageView`: its height was zero while its
  internal stack still required a bottom inset. Reproduce on the main
  window, then make the hidden state compatible with the stack's
  constraints and test both hidden and visible coverage.
  [Search coverage](apps/macos/Sweetbriar/Search/SearchCoverageView.swift)
- Activity describes the local archive as "Not synced since launch" even
  though it does not sync. Distinguish the local archive from an account
  that has not completed its first sync; test both states.
  [Activity wording](apps/macos/SweetbriarBridge/ActivityPresentation.swift)

### Prototype calmer composer and settings chrome

Extend the composer state work in Part 7 with a toolbar for Send, Attach
and format, a menu for less frequent commands, lighter field separators,
and a borderless editor with comfortable insets. Consider a remembered
Cc/Bcc disclosure and compact save-status treatment. Keep all commands,
identity warnings, keyboard access and persisted recipients; compare
small-window, dark-mode and VoiceOver behavior before revising spec/08.
For Settings, compare toolbar-style tabs and grouped forms with the
current fixed window, including accessible account-color swatches and a
clear backup destination control. Preserve identifiers and existing
flows, and avoid truncating the larger panes.
[Composer](apps/macos/Sweetbriar/Composer/ComposeWindowController.swift),
[Settings](apps/macos/Sweetbriar/AppearanceSettingsWindow.swift)

### Keep Search reachable in a narrow toolbar

The synthetic walkthrough showed the toolbar search field collapsing to
a button at the default window width. Measure the supported widths and
toolbar customization behavior, then consider prioritizing Search while
keeping the displaced commands available and keyboard accessible. This
concerns the toolbar item, not the separately verified search strip.
[Toolbar](apps/macos/Sweetbriar/MainToolbar.swift)

## Performance and protocol follow-up

### Reduce statement preparation in batch writes

Header sync and message import execute stable one-row SQL repeatedly
within batches of up to 250. Profile those transactions and, if useful,
prepare or cache statements across each batch as the list writer does.
Preserve transaction boundaries, rollback, ordering and failure tests.
[Header writes](crates/sb-store/src/sync/headers.rs),
[Import writes](crates/sb-store/src/imports/messages.rs)

### Avoid repeated full-window checks during command validation

For a large whole-window selection, `wholeWindowSelection` checks every
ID against the selection. Profile toolbar and menu validation with a
large archive before optimizing; if repeated scans are material, cache
the result per selection and generation or maintain a proven invariant.
Test changes to either and never substitute a count-only check that could
mistake a different set of IDs for the full selection.
[Selection](apps/macos/SweetbriarBridge/ListWindowModel.swift#L559)

### Profile opt-in latency logging and external-edit I/O

The review found synchronous file work on the main actor in the latency
recorder and external editor. Measure a full log rotation and a large
external-edit save before changing either path. If they stall the UI,
move bounded reads and writes off the main actor while preserving event
order, error reporting, last-save-on-close and archive retirement. Test
cancellation, read errors, size limits and a save racing with close.
[Latency log](apps/macos/SweetbriarBridge/LatencyLog.swift),
[External edit](apps/macos/SweetbriarBridge/ExternalEdit.swift)

### Investigate list invalidation recovery on errors

A helper reported that a window subscription failure ends its pull loop
and a failed index refresh may consume the invalidation without a retry.
First establish which failures are reachable while an archive remains
open, including restore and a transient store read, and whether the error
is visible. Add gated tests for each reachable path; if recovery is needed,
use bounded backoff and retire subscriptions safely rather than spinning
on a closed engine. [List model](apps/macos/SweetbriarBridge/ListWindowModel.swift)

### Bound send-evidence memory with history retention

Review the in-memory evidence map and the clone used to read the Out
Box against the retained attempt history. If entries outlive all history,
prune only when a documented retention rule makes them unreachable.
Never discard evidence that spec/07 requires for an accepted or unknown
attempt. Test long sessions, retained history and any prune boundary.
[Send worker](crates/sb-engine/src/proton_send.rs),
[Out Box read](crates/sb-engine/src/send.rs)

### Check synthetic window cursor error precedence

Synthetic window filling appears to check generation before resolving
the cursor, unlike the store path. Add malformed and foreign cursor
cases with an old generation; align the result with the cursor contract
without weakening generation fencing. This path serves prototype and
benchmark windows, so confirm its callers and impact first.
[Synthetic windows](crates/sb-engine/src/windows.rs)

### Remove redundant index status work if the latch permits it

An index-worker turn appears to read status twice, with the first value
discarded. Check why both transactions exist and the failure-latch
invariant before removing one. Retain tests for latched I/O failures,
retry and cancellation. [Index worker](crates/sb-engine/src/index.rs#L331)

### Qualify generic IMAP fallback behavior

The retained generic IMAP path has a whole-mailbox flag sweep that can
exceed its metadata budget, although that sweep currently has no engine
caller. A refused ID command is fatal and a BYE during LOGIN can be
reported as a rejected password. Reproduce each against a synthetic
server and check the IMAP contract before changing behavior: the spec
calls the ID version a best-effort hint but does not explicitly say an
ID refusal is tolerated. Keep this separate from Proton, which uses its
direct API. [IMAP implementation](crates/sb-imap/src),
[IMAP contract](spec/04-sync-imap.md)

## Experiments

- Try a subtle briar-rose empty-inbox illustration with motion disabled
  under Reduce Motion; compare it with the existing first-run welcome.
- Prototype an account-colored compose insertion point only where its
  contrast remains visible in both appearances.
- Explore exporting a conversation as Markdown with stable source
  attribution, and a distraction-reduced composer that keeps all fields
  reachable. Specify privacy, content and export semantics first.
- Explore a collapsible original-message preview while composing a reply,
  and a restrained optional haptic when a drag lands on a valid filing
  target. Keep keyboard feedback equivalent and honor system preferences.
- A tracking-pixel label would need evidence stronger than image size
  or hidden styling alone; retain the factual blocked-content count and
  avoid claiming a sender intended tracking without such evidence.
- Post-pilot Mac integration candidates include App Intents for creating
  a message, searching and opening a mailbox, a Share extension for a
  draft, account-based Focus filters, and opt-in Spotlight indexing of
  headers. Specify authorization, local retention and privacy boundaries
  before indexing or exposing archive content to another process.

---

# Part 10 — Friendly-wasp review pass (2026-11-26)

A second independent pass: five bounded read-only helper reviews (message
list, reader, composer, window chrome/sidebar/toolbar, and the Rust core's
store/engine/service), plus a live run of the Debug app against a fresh
archive seeded by importing a 1,500-message synthetic `.mbox`, driven
through its accessibility tree and screenshotted. Entries below are new;
where an earlier part already covers the ground, the entry says so and adds
only what is new. Landed items are in "Completed in this pass" above.

## Correctness and interaction

- **A note typed in the reader could be discarded when its pane closed.**
  `ReaderMetadataViews.swift:190` saved only on `resignFirstResponder`, and
  neither `ReaderViewController.close()` nor
  `MessageWindowController.windowWillClose` ended editing or committed
  `noteDraft`. Landed in #609; the ordering trap there is worth
  remembering: a task created inside `close` does not start until `close`
  has already run `reads.retire()` and `model.close()`, so the commit has
  to happen — and be awaited — before the teardown.
- **Reading-view sender memory races.** `ReadingViewMemory.swift:53` and
  `:63`: `removeSender` returns early when `senders` does not contain the
  address, so an OFF click that lands while an ON write is in flight
  writes nothing and the ON write persists — the opposite of what the user
  asked. `reload()` has the mirror hazard: a background read that resolves
  after a write replaces `senders` with the stale stored list. Serialize
  the three operations through one task or actor. See also Part 7's
  sender-preference entry, which covers the membership check.
- **`selectedOccurrenceIDs` stays stale until reconciliation finishes.**
  `AppKitPrototypeList.swift:269` (`restoreSelection`) drops ids that are
  absent from the new index from the table but does not update the
  property the command path reads, so `perform(.open)` can open a message
  that has left the list and filing commands refuse with a spurious
  "list changed". Assign the mapped id set in `restoreSelection`.
- **Pages around a selected off-screen row are never requested.**
  `ListWindowModel.applyVisibility` derives its wanted set from the last
  reported visible range only, and `reconcileSelection` throws away the
  row it looked up. So a selection held by occurrence id can point at
  ranks whose pages are not loaded, and `MainWindowController.flagTargets`
  (`:686`) then validates Mark as Read and Star as disabled even though
  spec/08 only disables them for local-archive rows or selections over
  1,000 rows. Make the paging engine selection-aware, or give the flag
  commands an async account query like `filingSelection`'s.
- **Renaming a saved view does not refresh the window title.**
  `SidebarViewController.swift:253` reloads and re-selects with
  `selectingInCode == true`, so no choice is reported and
  `MainWindowController.updateTitle()` is never called. Delete works
  because it ends by choosing the unified inbox. Add a callback after a
  successful rename.
- **Sidebar account names go stale after a Settings rename, pause or
  disconnect.** `AccountsSettingsModel` re-reads accounts and notifies
  nobody, and the core bumps only the account revision on rename, so
  `mailboxes()`' generation does not move. The sidebar heading, the
  window subtitle and the account chips keep the old value until an
  unrelated event or a restart. Wire an `onAccountsChanged` closure to
  `reloadSidebar()`.
- **A message window ignores Space.** `MessageWindowController` implements
  no key handling, so spec/08's "in a message window, Space pages its
  reader" is unimplemented; the only Space mapping is the main window's
  table command. Add `keyDown`/`performKeyEquivalent` on the reader's
  backdrop view, passing modified combinations through.
- **Every tag or note write reloads the whole body rendition.**
  `ReaderViewController.render()` always clears and re-shows the HTML pane
  and re-sets the Reading view's attributed string, even when `setTag` or
  `saveNote` rebuilt the state with the same rendition: the body flickers,
  the scroll offset resets and inline images decode again. Track the last
  shown rendition token and skip the body work when it is unchanged.
- **A failed note save can show its error above a different message.**
  `ReaderViewController.swift:628` has no ticket check, unlike `setTag`
  and `activateAttachment`, so an offline-save error for message A can
  appear over message B.
- **The AppKit row keeps the account-marker indent when the marker is
  hidden.** `AppKitPrototypeList.swift:776` computes `contentX` from
  `showsAccountMarker`, not from the flags `configure` set, so before the
  appearance answers every row indents its text 44 pt too far and then
  jumps left. The SwiftUI row starts at `innerGap` instead. Derive the
  indent from the actual marker visibility. (The marker hiding itself is
  fixed in #599; the indent is not.)
- **`MessageWindows` computes a cascade origin it never applies.**
  `MessageWindowController.swift:184` stores `cascadeTopLeft(from:)` back
  into `cascade` and sets no frame, so the comment describes behaviour the
  code does not have; overlap cascading happens only through AppKit's
  implicit nudge. Apply the point, or delete the dead state.
- **VoiceOver's row label omits tags and attachments.**
  `MessageRowPresentation.swift:179` builds the label from sender,
  subject, date, unread and flagged only, and the drawn chips and the
  paperclip are not accessibility elements, so a VoiceOver user cannot
  tell a message has an attachment or tags. Append ", N tags" and ",
  attachment".
- **Sidebar id lookups used `==` where the chips compare bytes.** Landed in
  #600; the same rule should reach `AccountLogoImages.image(for:in:)` and
  `MessageWindowController.renderTint`, which still compare ids their own
  way.

## Composer

- **A send in flight does not freeze From, Refresh Identities, Attach… or
  Remove.** `ComposeWindowController.swift:536` computes a `busy`-aware
  `editable` for the subject, body, format control, recipient fields and
  Save, but `ComposeAttachmentControls` and `ComposeIdentityControls`
  admit from their own closures, which omit `busy`. The window is half
  frozen and a revision is saved while the attempt is held. File drops go
  through `textView.fileDropHandler`, which bypasses editability too.
  Include `sendModel?.isBusy` in `canChangeIdentity`/`canAttach`, or pass
  the render's `editable` in.
- **Keep Both drops uncommitted recipient input and in-flight inline
  images.** `ComposeWindowController.swift:672` excludes marked text but
  not pending recipient input, and `captureEdit()` never captures
  recipients, so an address typed in the quiet period is absent from the
  copy. The same freeze drops an inline image whose publication finishes
  during `publications.settle()`.
- **A refused close or quit leaves the autosave timer stopped.**
  `ComposeWindowController.swift:800` cancels the debounce and, when the
  flush fails, aborts the close without restarting it, so dirty text is
  never autosaved again until the user types. Restore the timer in both
  abort paths when the draft is still dirty, as the abandon alert's Keep
  Editing branch does.
- **Every render rebuilds all attachment rows and the From popup, and a
  keystroke renders at least twice.** `ComposeAttachmentControls.render()`
  detaches and recreates up to 100 rows and `ComposeIdentityControls.render()`
  re-adds every identity; `captureEdit()` ends with `render()` and the
  observation schedules another. Diff rows by index like
  `renderRecipientFence()` does for banners, and cache the popup's items.
- **A local write-gate refusal is recorded as "may have committed".**
  `DraftModel.swift:187` sets `began = true` before the store call and
  classifies `CoreFailure.unavailable` as unresolved, so a pre-write
  refusal replays a stale intent on the next save. Give the local gate a
  distinct error case that maps to `false`.
- **A failed initial draft load has no retry.** The status advises "Try
  Save again" while Save is disabled and nothing calls `model.load()`
  again. Add a Retry control, and reword the status when content is nil.
- **Pressing Send while an inline image publishes can drop the image.**
  Admission is evaluated when the bytes arrive, after the editor is
  frozen, so the outcome is nondeterministic. Decide admission when the
  publication starts.
- **The per-draft literal plain-text option has no UI.**
  `DraftTypes.swift:369` defaults `plainFlowed: true` and nothing sets it
  false, though spec/05 asks for it. Add a per-draft Literal toggle.
- **Bold and Italic undo entries have no action name.**
  `ComposerTextView.swift:175` never calls `setActionName`, unlike every
  other composer command.
- **Publication Cancel/Dismiss buttons are not labelled per attachment.**
  `ComposeAttachmentControls.swift:277` sets only an identifier, so with
  several publications in flight VoiceOver cannot tell which row a button
  belongs to.

## Performance

- **Relevance searches score and materialise every matching document.**
  `sb-store/src/query/counts/paging.rs:661` builds one materialised CTE per
  positive leaf over the entire FTS match set, and `search_ids` sorts the
  whole relation to produce an id index even when a single driving leaf
  could stream in rank order. A common term scores and stores 100k+ rows
  per leaf while the page needs `limit + tie run`. Score only the
  documents the page fetches, carry the predicate outcome through the
  score CTE instead of re-evaluating `check`, and give `search_ids` a
  streaming reader for the single-leaf case. See also P6 and P11.
- **GC's reference check is O(objects × attempts) and uses a substring
  test.** `sb-store/src/gc.rs:133` runs one `SELECT EXISTS` per object
  over `send_attempts` (no index on `mime_sha256`) and `draft_revisions`
  (`attachments_json LIKE '%sha%'`), so a pass over N objects and D
  attempts is O(N·D), and the LIKE arm is a substring match rather than an
  exact reference. Build the referenced-digest sets once per pass.
- **Search reads re-prepare SQL, re-register UDFs and repeat the binding
  check.** `sb-store/src/query/counts.rs:221` uses `tx.prepare` (bypassing
  the statement cache), `register(tx)` per execution, and
  `check_bindings` runs twice per read; `lists::attach_tags` builds a fresh
  1,000-placeholder `IN (...)` per list page. See P7.
- **`complete_fields` parses JSON and `address_value` decodes addresses per
  candidate row, per leaf.** `sb-store/src/query/counts.rs:482`: the normal
  stored mask is exactly `[]`, so the common path parses a two-byte string
  per row per leaf. Short-circuit `mask == "[]"`. See B4 and P12.
- **A hidden Search list keeps its subscription and page reads running.**
  `MessageListViewController.enterStandalone()` only hides the view; the
  model is neither closed nor paused, so index invalidations during a
  rebuild keep re-fetching and re-reading pages for a hidden table. Add a
  `paused` flag checked in `handle`/`scheduleFetch`/`pump`.
- **Account-label strings are recomputed per configured row.**
  `MessageListViewController.swift:328` filters and sorts the sidebar
  sections per row, and `SidebarViewController.accountLabel` filters and
  sorts again, with up to `accounts.count` disambiguation loops. Build an
  `[accountID: String]` map once per sections/appearance change.
- **`DisplayLinkClock` is never invalidated.**
  `MessageListViewController` creates a clock in `install` and never calls
  `invalidate()` in `close()`, so a `CADisplayLink` retains its target and
  both objects leak for the process lifetime on every retire/restore.
  Mirror `ListPrototypeWindowController`.
- **`evictOverflow` rescans a page map that never shrinks.**
  `ListWindowModel.evictOverflow` reduces, filters and sorts `pages` on
  every delivered frame, and `page(containing:)` inserts a `Page` for
  every rank a row view asks about with nothing removing entries, so
  `pages` grows to one object per page of the window. Maintain a
  loaded-page counter where `page.rows` is set and cleared.
- **`MessageRowFormatter` swaps one `DateFormatter`'s format string per
  row.** `MessageRowPresentation.swift:215` picks a bucket, assigns
  `dateFormatter.dateFormat`, then formats; swapping the pattern makes
  Foundation re-parse it and rebuild the ICU formatter, and it re-reads
  `Calendar.current` per row. Hold three preconfigured formatters, or
  memoize per (bucket, day). See P14.
- **Every reconfigured row re-measures its tag chips and redraws its
  badges.** `AppKitPrototypeList.layoutSubviews` calls
  `chip.fittingWidth()` — a CoreText measurement per chip per row — and
  `BadgeView`'s `title`, `font`, `fillColor` and `titleColor` each do
  `didSet { needsDisplay = true }` unconditionally. Cache the measured
  width and guard each `didSet`.
- **A bounds change lays every cell out twice.** Both `layout()` and
  `resizeSubviews(withOldSize:)` end with `layoutSubviews()`, which
  re-frames all ~25 subviews by hand. Cache the last bounds size and
  return early.
- **The synthetic debug id index allocates 8 MB against a documented
  4 MB.** `sb-engine/src/windows.rs:798` builds `(0..n)` for up to
  1,000,000 rows. Cap the fill at the documented 500k, or correct the
  comment and spec budget.
- **Sidebar counts silently turn a corrupt negative count into zero.**
  `sb-store/src/sidebar.rs:209` maps a negative to 0 rather than
  `Corrupt`, hiding schema damage from the UI and diagnostics.
- **The reading document is fetched and awaited even when viewing
  source.** `ReaderModel.swift:240` starts and awaits
  `readingDocument(messageId:)` unconditionally; source view never uses
  it, so it pays the core round trip and delays showing the raw bytes.

## Visual, layout and accessibility

- **The reader's HTML body is a pure white rectangle in Dark Mode.**
  `HTMLReaderView` sets no background, so WebKit paints white and the
  message reads as a hole in the dark window. Inject a small style that
  makes the canvas follow the effective appearance, and let messages that
  set their own background keep it.
- **The tag and note fields are always on show in the reader header.**
  `ReaderViewController.swift:147` adds the tag picker and the note
  editor unconditionally, so every message shows an empty "Add tag" field
  with an Add button and an "Add a note" field with no button above the
  body. Show the note editor only for a message that has a note or on
  focus, and give the tag picker the same treatment.
- **The reader's body has no maximum measure.** The body stack is pinned to
  the view's edges with 12 pt insets and nothing else, so at a 1,180 pt
  window the plain-text body ran about 636 pt wide — roughly 110
  characters per line. Cap the measure at about 68 characters and centre
  it, the way the reader's placeholder already is.
- **The header expands every To and Cc recipient.**
  `ReaderViewController.swift:551` appends every address to a
  non-scrolling header, so mailing-list mail pushes the body out of the
  pane. Cap the visible recipients behind a disclosure. (Part 8 landed
  the collapse for the Activity window; the reader header is separate.)
- **Reader tag chips past 240 pt are clipped and unreachable.**
  `ReaderMetadataViews.swift:53` caps the chip stack with no wrap, scroll
  or overflow control, so trailing chips cannot be reached by mouse,
  keyboard or VoiceOver. Wrap them, or add a trailing "+N" chip that
  lists every tag.
- **The empty unified window reads as a failure, not a first run.** The
  status line is the plain string "No messages" with no action, and the
  welcome island only covers a fresh archive. After an import the unified
  inbox is empty and `isFreshArchive` is false, so nothing explains where
  the mail went — see the imported-role entry in Part 9.
- **The window opens at its minimum size when no frame is saved.** With
  `-ApplePersistenceIgnoreState` the window came up at 800×420 (its
  declared minimums) rather than the 1,100×700 `defaultContentSize`.
  Apply the default content size when no autosave frame exists.
- **View Source and the rule-list fallback show no loading state.**
  `ReaderViewController.setSource` hides the HTML pane and then awaits the
  fetch, so the body area is blank during a slow acquisition while the
  header, attachment list and remote-content label still reflect the
  previous rendition. Show the status line before awaiting.
- **Reading-view links show no destination on hover.**
  `ReadingRenderer.swift:60` attaches only `.link`; `ReadingView`
  implements only the click handler, so the destination appears only after
  clicking, while the file's header comment claims a hover affordance.
  Expose it through `NSTextViewDelegate`'s tooltip API or a tracking-area
  label. See V11.
- **The two prototype lists disagree about the row's trailing group.** The
  AppKit row lays out date, account label, paperclip, flag, then sender,
  while the SwiftUI row's HStack reads sender, account label, flag,
  paperclip, date, and the date column is fixed at 84 pt in AppKit but
  elastic in SwiftUI — despite the shared header comment claiming the two
  lists are identical. Promote the date width into `MessageRowMetrics` and
  give both lists the same order and frame. (The date *width* itself
  landed in #590; the order and the SwiftUI side did not.)
- **Account rail colours collide and monograms can be digits.**
  `MessageRowPresentation.swift:195` takes `hash % 6` over a six-slot
  palette, so three accounts in a unified list already have about a 50 %
  chance of two sharing a rail colour, and the rail is the only colour
  cue. `monogram(for:)` takes the first letter of the id's last `-`
  component, so an id like `acct-2` renders the digit "2". Assign palette
  slots by account order in the window, and fall back to the display
  name's first letter when the id component is not a letter.

## Novel and delightful

- **Reveal Archive / Trash / Mark Read on row hover.** The hover fill in
  #607 supplies the hook: extend `HoverRowView` with a trailing button
  group that fades in with the fill and routes through the same
  `MessageContextAction` admission the context menu uses. See D11.
- **A "where did my mail go" line for an empty unified window.** Once the
  imported-role entry lands, the empty unified window should name the
  local archive and offer "Show Local Archive" rather than a bare "No
  messages".
- **A reading-progress and estimated-read-time line in the reader.**
  `ReadingView` already renders the core's semantic document, so a word
  count is available for free. See D6.
