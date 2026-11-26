# Sweetbriar — analysis backlog

Consolidated on 2026-11-26 from the full review of `origin/main` at
`19fde48e` (Rust ~298k lines, Swift ~60k lines) plus a second pass over
the reader and list hot paths. This file is the shovel-ready backlog:
every open item names the file and line that carries it, and each entry
is written so an LLM can pick it up cold. Extended at `366f29bc` with
a third pass that shipped typed attachment rows and notification
click-through. Completed work and submitted patches are recorded below.

## Completed on main

- **Help menu content.** Help → Keyboard Shortcuts… now opens a shared
  reference window with the app's keyboard commands. [Menu entry](apps/macos/Sweetbriar/MainMenu.swift#L458), [Shortcut reference](apps/macos/Sweetbriar/KeyboardShortcutHelp.swift)

## Submitted patches

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
- **Dock unread badge** (second pass, PR #575). `SidebarModel.inboxUnread`
  sums every account's Inbox mailbox once; `MainWindowController` mirrors
  it on `NSApp.dockTile`, refreshes after generation-driven sidebar
  reloads, and clears it when content retires. Tests cover the sum, the
  role filter, reload updates and the badge text.
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
`MessageRowContent.accessibilityLabel` omits tags entirely. A message
with four or more tags therefore reads as three in the list and none to
VoiceOver, while the reader shows them all. Draw a trailing `+N` chip
after the visible chips (keeping the fixed-size pool so the hand layout
is unchanged) and add the remaining count to the accessibility label.

## B14. A self-built payload is encoded with `try!`

`apps/macos/SweetbriarBridge/HumanVerification.swift:46` encodes a
dictionary the same file builds, so it cannot fail today; the pattern
is fragile if the payload ever gains a non-JSON value. Throw or record
an internal error instead.

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

## P13. A cold sidebar reads every account serially

`SidebarModel.readOnce()` awaits `mailboxes(accountId:)` one account at
a time, so the first sidebar paint pays N independent round trips
serially. Read them with a task group, preserving the account order, and
keep the reload's cancellation and coalescing behaviour. Related
per-row costs are in P10 (`accounts.first(where:)` per configured row,
and the date rule's `Calendar.current` work per row).

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
- The subscription's failure policy is undefined: `NewMailNotifier.pull()`
  stops for the rest of the session on any stream error, so a transient
  Proton or network failure silences later mail even though the event
  loop recovers. Choose a bounded retry policy or record why stopping is
  final, and test it.

## F17. Find does not work in an HTML message

`Edit → Find → Find…` reaches the focused text view, so while the
sanitized HTML web view is first responder the command is inert, which
is how most mail is read. Drive `WKWebView.find(_:configuration:)` from
a small find bar or a reader-level Find handler. Plain-text and Reading
views already work through their scroll views' find bars.

## F18. No unified inbox total or progress treatment

The Unified section has no header total, so the aggregate unread count
lives only on each account's Inbox row; sync and filing surface through
the integrity banner and transient toasts. A visible unified total and
a subtle toolbar/sidebar progress treatment would make the shell
scannable. Check secondary-text contrast in Color Wash first.

## F19. Send Later and Snooze

Larger, post-pilot. Both fit the durable-intent architecture: a scheduled
send is an Out Box attempt with a `not_before` time; snooze is a durable
flag plus later re-insertion. The existing Undo Send hold shows the
outbox state machine can carry a delay. Write the spec before any UI.

## F20. A saved draft cannot be discarded from the UI

`DraftModel.discard()`, the FFI `discardDraft` and the core
`discard_draft` command all exist, but no menu or context command
calls them. Once a draft has been saved, the only way to remove it is
Close Without Saving before the first save; afterwards it accumulates
forever, and a failed send leaves clutter. Spec/05 step 8 defines the
semantics: refused `Conflict` while an attempt is live, revisions kept.

Add a Drafts-list context command (identifier
`ch.lkmc.Sweetbriar.cmd.discardDraft`) plus a Delete key path, a
confirmation naming the draft, one retained idempotency key per draft,
routing through `ArchiveWriteTasks` so a restore retires it, and an
error path that keeps the rows. Decide what a still-open compose window
does when its draft is discarded elsewhere. Revise spec/08's Drafts
(local) section first. Test discarding removes the row, a live attempt
refuses, a replay is idempotent, a read-only archive refuses, and a
failed reload keeps rows.

## F21. Remember recent searches

Small. Keep recent queries locally (a settings row or UserDefaults),
offer them from the search field's recipe menu and allow clearing them.
Search text is sensitive, so state the retention and keep clearing
obvious; never put it in the opt-in latency log, which is documented to
hold no query text.

## F22. Remember each mailbox's scroll position and selection

Small. The id index makes restoring a rank cheap. Persist the last
visible rank and selected occurrence ids per mailbox and restore them
when the mailbox is chosen again. Needs a storage location and a spec
sentence; cover a mailbox that shrank, a deleted message and a restore.

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

Medium priority. Add a Mailbox menu command and a keyboard palette using current sidebar entries and the filing palette's behavior. Rank exact, prefix, then recent matches. Show account and full path to disambiguate duplicates. Return navigates and Escape restores focus. Preserve the four existing unified shortcuts. Test keyboard-only travel across duplicate names in two accounts and smoke-test sidebar selection. [Filing palette](apps/macos/Sweetbriar/Filing/FilingPalette.swift), [Mailbox menu](apps/macos/Sweetbriar/MainMenu.swift#L436)

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
