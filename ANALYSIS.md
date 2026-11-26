# Sweetbriar — analysis backlog

Consolidated on 2026-11-26 from the full review of `origin/main` at
`19fde48e` (Rust ~298k lines, Swift ~60k lines) plus a second pass over
the reader and list hot paths. This file is the shovel-ready backlog:
every open item names the file and line that carries it, and each entry
is written so an LLM can pick it up cold. Items already shipped live
under "Done" below — do not re-pick them.

## Done (shipped)

- **Tag draft followed the selection to another message.** The reader's
  tag field never cleared on selection change, so an unsubmitted draft
  typed on message A would land on message B. Fixed in PR #576
  (`mac/reader-tag-draft-20261126`): `ReaderViewController` tracks the
  message the draft belongs to, clears on selection change
  (occurrence-keyed, eagerly in `resetSelection`) and when the shown
  message differs; a metadata-only refresh of the same message keeps
  it. Regression test `tagPickerDraftStaysWithItsMessage`.
- **`setVisible` re-ran all visibility work on every scroll
  notification, including retrying failed pages on no-op reports**
  (old B6). Fixed in PR #586 (`mac/list-visible-guard-20261126`):
  `ListWindowModel.setVisible` returns early when the range is
  unchanged, so `failedPages` survives until the range actually moves.
  Test `aRepeatedVisibleRangeDoesNotRetryAFailedPage`.

## Verified non-bugs (do not re-report)

- The search strip's visibility is checked live and behaves per spec.
- `ListWindowModel` fires `onIndexChanged` twice on open (provisional
  prefix, then exact index); both fire are needed so the list redraws
  when provisional ids are replaced by the full index.

## How the review was done

- Read the macOS app shell, list, sidebar, reader, composer, search and
  settings by hand.
- Read the Rust store query layer, engine windows/bodies, and `sb-mime`
  render/rendition paths by hand.
- Ran four bounded read-only helper passes (LongCat 2.5) over the store
  query layer, the list/bridge, `sb-mime`/reader and search, then
  verified each reported finding against the source before recording it.
- Every claim below was checked against the code at the stated line.
  Nothing is inferred from naming alone.

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

## B13. The Help menu is empty

`MainMenu.swift` `makeHelpMenu()` registers an `NSMenu` with no items.
AppKit adds its menu-search field to the registered menu, so the user
sees a "Help" menu whose only content is an empty search box.

## B14. Audit the reader's stale-render interleaving

`ReaderViewController.render()` (ReaderViewController.swift:437) clears
the pane and re-renders synchronously while body loads complete
asynchronously into `HTMLReaderView.show(_:ticket:)`
(HTMLReaderView.swift:181). The ticket guards (`clear(ticket:)` at
:170, `currentTicket()`) look correct, but a fast A→B→A navigation
where A's slow load lands last deserves a targeted test: confirm the
last `show` wins and no intermediate content flashes. Low priority —
verify before changing.

## B15. Verify Reading-view ordered-list numbering

`crates/sb-mime/src/document.rs:531` (`build_list`) parses `start`,
`reversed` and per-`li` `value`; `ReadingRenderer.swift:72-85` computes
`fallback = reversed ? start - index : start + index` and prefers
`item.number`. A second review pass flagged wrong numbering in the
Reading view for lists with `start`/`reversed`/`value`. Confirm with a
fixture (nested and reversed lists) before changing; the Rust side has
`ReadingRendererTests`/`document` tests to extend.

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

## V3. The Help menu has no content (see B13)

A "Help" menu containing only AppKit's empty menu-search field looks
unfinished in an otherwise complete menu bar.

## V4. Empty states are plain secondary-label text

`ReaderViewController.swift` `placeholder` is a centred
`NSTextField(labelWithString:)` with `.secondaryLabelColor` and
`.title2`. The reader's "No message selected" and "N messages selected"
states, the sidebar's empty states and `AccountsSettingsView`'s
"No accounts available." are all bare text. Apple's own pattern
(`ContentUnavailableView`) is already used in `AppearanceSettingsWindow`
for the closed-archive cases, so the app has the vocabulary but not the
coverage.

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

## F3. No thread/conversation view

`WindowSpec` has a `.thread` case and `MessageRowPresentation` knows
about it, but there is no UI to expand a conversation. Mail's
conversation view is what most people live in.

## F4. No Quick Look for attachments

`ReaderViewController` lists attachments and `openAttachment` hands back
a file URL, but pressing Space on an attachment does not open Quick
Look, and there is no "Reveal in Finder" or "Save As…".

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

## F14. No spell-check / grammar indicator control in the composer

`ComposerTextView` is a plain `NSTextView` subclass;
`isContinuousSpellCheckingEnabled` is not set, and there is no
Format → Spelling and Grammar submenu.

## F15. Spec-deferred items worth scheduling explicitly

Deferred by spec rather than forgotten: remote-content blocking UI in
the reader, IDN/homoglyph warnings on link hover, per-tab window
swatches, "Save As…" for attachments (see F4), and a jump-to-mailbox
command palette. Each needs a spec line before implementation.

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

## D8. A first-run checklist that ends in a real import

File → Import… exists and works, but a new user lands on an empty
Unified Inbox with a centred "No message selected". A one-screen
checklist ("Add your Proton account", "Import an Apple Mail export",
"Learn the keyboard") would make the first five minutes land.

## D9. Signpost-free latency surfacing

The app already records a latency log behind Debug. Surfacing a
discreet "slow" pill in the window subtitle when a p95 span exceeds its
budget (spec/08 "Performance budgets") would let the owner notice a
regression without opening a report.

## D10. Sound and haptic restraint

The app posts no sounds at all. Mail's send "whoosh" and the new-mail
alert are part of why it feels finished. A single, quiet, optional send
confirmation sound (and nothing else) would be enough.

---

# Part 6 — What I would implement, in order

Ranked by (user-visible impact × confidence) / diff size.

| # | Entry | Why first |
|---|---|---|
| 1 | B1 — stop the full table reload on sidebar reload | largest single stutter source, small diff, pure Swift |
| 2 | B2 — HashMap cid resolution | clear quadratic bug, small diff, existing test surface |
| 3 | P1 — render the body once | 2x on every message open, spec/08's 100 ms budget |
| 4 | B3 — cache the body lease | removes re-render on re-selection and source toggling |
| 5 | B7 — visible-only page reload | removes off-screen cell churn (`setVisible` guard shipped in PR #586) |
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
benchmark-only items (P11, `windows.rs` synthetic fill). Already
shipped and therefore absent from the table: the reader tag-draft fix
(PR #576) and the `setVisible` no-op guard (PR #586).
