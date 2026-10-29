# Search benchmark Time Profiler run: 30888e0, main-profile

Built commit `30888e0f5dc6020896f1af5094d64a23256ac830` ("Review first-pass
chunks with StepFun step-5-preview"), Release, on a Mac mini M1
(Macmini9,1), macOS 26.5.2 (25F84), 60 Hz display, 100,000-message
synthetic archive. Recorded 2026-10-29 with `xcrun xctrace record
--template 'Time Profiler' --launch` over `-runSearchBenchmark`. The
report is `20261029T212655Z-30888e0-main-profile-profiled.json`
(`ok: true`, 50 of 50 samples).

The profiler distorts the timings: this run's p50 is 2,503 ms against
1,304 ms in the timed run, and round 0's exact queries took 13.7 s and
22.8 s. Read the numbers below for where time goes, not for how long a
search takes.

## How this was produced

- `xctrace` reported "Fatal logging system error: The log archive is
  corrupt or incomplete" on both profiled attempts. The trace has no
  `com.sweetbriar` signposts, so the "apply" points weren't available.
  The search phase and each query were located from the stacks instead
  (see the script's docstring). The timed keystroke's `post` closure,
  "closure #6 in `ListBenchmarkRunner.runSearch`", forms exactly 50
  clusters, which match the report's 50 samples in order.
- xctrace left part of Sweetbriar's frames unsymbolicated. The trace
  was re-symbolicated with `xctrace symbolicate --dsym` against the
  build's dSYMs. The script then resolves the remaining bare addresses
  in the Sweetbriar and SweetbriarBridge binaries with `atos`, using
  each binary's load address from the export. Those frames carry an
  `[atos 0x…]` tag. Most are `<deduplicated_symbol>`, code the linker
  folded, so their identity is lost. Rust v0 symbols are demangled
  with the `rustc-demangle` crate (filter source at the end).
- Time Profiler samples running threads only (1 ms, user stacks). Time
  a thread spends blocked or descheduled leaves no samples, so on-CPU
  ms can be lower than wall time.
- The first profiled attempt didn't count. Sweetbriar launched behind
  Finder and was brought to the front about 2 minutes in, so it failed
  with "the window wasn't key and the app active" and "the run lost
  the foreground". Its report is kept as
  `20261029T212655Z-30888e0-main-profile-profiled-invalid1.json`. The
  profile below comes from the second attempt, which was activated as
  soon as its process appeared (about 14 s after `xctrace` started).

## Findings

1. **The main thread isn't what stalls the search.** It is on CPU
   for 11.3% of the search phase (51.7 s of 457.5 s), and the Rust
   core never runs on it (Rust core area 7 ms). In the timed run the
   report's `firstPage` stage tracks the total almost exactly (p50
   1,289 ms against 1,304 ms keystroke-to-rows). The wait is the
   core's opening page on the tokio workers.
2. **The core's time is SQLite page reads.** On the workers,
   `sqlite3_step` holds 96% of samples, `getPageNormal` 73% and
   `unixRead` (`pread`) 55%. Pages come from the OS file cache through
   syscalls rather than from SQLite's page cache. The callers are
   `sb_store::query::counts::paging::read_search_relevance_page` (43.5%
   of worker time), `paging::search_ids` (18.4%) and
   `paging::read_page` (13.8%).
3. **The sidebar recounts during the search phase.**
   `sb_store::sidebar::mailboxes` takes 23.5% of worker time. On the
   main thread, `MainWindowController.reloadSidebar()` →
   `SidebarViewController.reloadSections()` takes 3.2 s inclusive
   (6.2%), including `NSOutlineView expandItem:` in the post-query
   bursts. That's worker contention while the search's own page
   loads.
4. **Main-thread time in the search phase** (inclusive, % of main busy):
   Core Animation commit 51.7%, with layout inside it (`NSView`
   layout 24.7%, `NSTableView` 24.1%); `AppKitPrototypeList` 11.9%
   (`tableView(_:viewFor:row:)` 7.6%; `MessageRowCellView` init and
   configure 7.1%); `MessageListViewController.show`/`install`/`detach`
   11.5% (each search swaps the list: `detach` removes the old views
   and `reloadData` purges row views); `SearchFieldController` 8.1%
   (`applyModel` 5.7%, of which `NSButton` creation 3.1%);
   `ListWindowModel`
   4.3%; `FrameBatcher` 1.1%; row formatters 0.2%.
5. **Post-query bursts** (keystroke to max(rendered, opened) + 50 ms,
   19.8 s on CPU over 50 windows) look the same. The heaviest distinct
   paths: autorelease-pool pops after main-actor jobs (4.8%),
   `AppKitPrototypeList.tableView(_:viewFor:row:)` making row views
   during `NSTableView` layout (4.5%), the sidebar's outline expansion
   (1.8%), XPC work deferred onto the main thread (1.7%), and
   `MessageListViewController.detach()` → `removeFromSuperview` (1.7%).
6. **The timed keystroke's `insertText` runs synchronously.** It
   includes `controlTextDidChange` → `SearchFieldModel.updateText` and
   text checking, and is on CPU for 3 to 59 ms. In round 0 its first
   and last samples are 300 to 835 ms apart, with only 15 to 59 ms on
   CPU, so the main thread was off CPU inside the call. With seven
   busy tokio workers and the profiler on an 8-core M1, preemption is
   the likely cause. This template can't confirm it because it
   records no context switches.

---

## Generated analysis: main thread, search phase

Samples: 121930 total, 65449 on the main thread (1 ms sampling, running threads only).
Timed keystrokes found: 50; report samples: 50.

Search phase: 150.111 s to 607.581 s of the trace (457.5 s wall); main thread busy 51656 ms (11.3% of wall).

### Top 40 frames by self time (main thread, search phase)

| # | ms | % of main busy | frame |
|---:|---:|---:|---|
| 1 | 3015 | 5.8 | `objc_msgSend` |
| 2 | 995 | 1.9 | `<deduplicated_symbol>` |
| 3 | 942 | 1.8 | `getMethodNoSuper_nolock(objc_class*, objc_selector*)` |
| 4 | 905 | 1.8 | `__CFStringHash` |
| 5 | 803 | 1.6 | `<no backtrace>` |
| 6 | 802 | 1.6 | `_xzm_xzone_malloc_tiny` |
| 7 | 622 | 1.2 | `_platform_strcmp$VARIANT$Base` |
| 8 | 565 | 1.1 | `objc_retain` |
| 9 | 527 | 1.0 | `protocol_conformsToProtocol_nolock(protocol_t*, protocol_t*)` |
| 10 | 408 | 0.8 | `list_array_tt<unsigned long, protocol_list_t, RawPtr>::iteratorImpl<false>::skipEmptyLists()` |
| 11 | 388 | 0.8 | `CFEqual` |
| 12 | 380 | 0.7 | `__CF_IS_OBJC` |
| 13 | 378 | 0.7 | `_platform_memcmp` |
| 14 | 371 | 0.7 | `objc_class::demangledName(bool)` |
| 15 | 366 | 0.7 | `objc_release` |
| 16 | 322 | 0.6 | `_class_conformsToProtocol_unrealized_nolock(objc_class*, Protocol*)` |
| 17 | 318 | 0.6 | `__CFSearchStringROM` |
| 18 | 300 | 0.6 | `weak_entry_for_referent` |
| 19 | 255 | 0.5 | `_platform_memmove` |
| 20 | 245 | 0.5 | `___CFBasicHashFindBucket_Linear` |
| 21 | 231 | 0.4 | `__kdebug_trace64` |
| 22 | 225 | 0.4 | `_xzm_xzone_malloc_from_freelist_chunk` |
| 23 | 216 | 0.4 | `ItemVariationStore::ValueForDeltaSet(unsigned short, unsigned short, unsigned long, short const*, double const*) const` |
| 24 | 204 | 0.4 | `_platform_strlen` |
| 25 | 193 | 0.4 | `swift_retain` |
| 26 | 190 | 0.4 | `_xzm_free` |
| 27 | 179 | 0.3 | `objc::StringHashTable::tryGetIndex(char const*) const` |
| 28 | 166 | 0.3 | `objc_autorelease` |
| 29 | 163 | 0.3 | `search_method_list(method_list_t const*, objc_selector*)` |
| 30 | 154 | 0.3 | `mach_msg2_trap` |
| 31 | 133 | 0.3 | `list_array_tt<unsigned long, protocol_list_t, RawPtr>::iteratorImpl<false>::iteratorImpl(list_array_tt<unsigned long, protocol_list_t, RawPtr>::ListIterator,...` |
| 32 | 125 | 0.2 | `_xzm_xzone_malloc_freelist_outlined` |
| 33 | 124 | 0.2 | `os_unfair_lock_lock` |
| 34 | 117 | 0.2 | `_Block_copy` |
| 35 | 112 | 0.2 | `AutoreleasePoolPage::add(objc_object*)` |
| 36 | 111 | 0.2 | `objc_loadWeakRetained` |
| 37 | 109 | 0.2 | `objc_msgSendSuper2` |
| 38 | 108 | 0.2 | `bool objc::DenseMapBase<objc::DenseMap<void const*, objc::ObjcAssociation, objc::DenseMapValueInfo<objc::ObjcAssociation>, objc::DenseMapInfo<void const*>, o...` |
| 39 | 107 | 0.2 | `getMethodFromRelativeList(relative_list_list_t<method_list_t>*, objc_selector*)` |
| 40 | 107 | 0.2 | `swift_allocObject` |

### Top 40 frames by inclusive time (main thread, search phase)

| # | ms | % of main busy | frame |
|---:|---:|---:|---|
| 1 | 35839 | 69.4 | `-[NSApplication run]` |
| 2 | 35839 | 69.4 | `start` |
| 3 | 35839 | 69.4 | `main [atos 0x10028175b]` |
| 4 | 35827 | 69.4 | `-[NSApplication(NSEventRouting) _nextEventMatchingEventMask:untilDate:inMode:dequeue:]` |
| 5 | 35827 | 69.4 | `-[NSApplication(NSEventRouting) nextEventMatchingMask:untilDate:inMode:dequeue:]` |
| 6 | 35791 | 69.3 | `_DPSNextEvent` |
| 7 | 35790 | 69.3 | `_DPSBlockUntilNextEventMatchingListInMode` |
| 8 | 35789 | 69.3 | `_BlockUntilNextEventMatchingListInMode` |
| 9 | 35788 | 69.3 | `ReceiveNextEventCommon` |
| 10 | 35751 | 69.2 | `RunCurrentEventLoopInMode` |
| 11 | 35696 | 69.1 | `_CFRunLoopRunSpecificWithOptions` |
| 12 | 34128 | 66.1 | `__CFRunLoopRun` |
| 13 | 27513 | 53.3 | `UC::DriverCore::continueProcessing()` |
| 14 | 27266 | 52.8 | `stepTransactionFlush` |
| 15 | 26716 | 51.7 | `CA::Transaction::commit()` |
| 16 | 26629 | 51.6 | `CA::Transaction::flush_as_runloop_observer(bool)` |
| 17 | 22229 | 43.0 | `__CFRunLoopDoSources0` |
| 18 | 22211 | 43.0 | `__CFRunLoopDoSource0` |
| 19 | 22211 | 43.0 | `__CFRUNLOOP_IS_CALLING_OUT_TO_A_SOURCE0_PERFORM_FUNCTION__` |
| 20 | 19002 | 36.8 | `0x19469f4a4` |
| 21 | 18993 | 36.8 | `0x19469f47c` |
| 22 | 17964 | 34.8 | `CA::Transaction::run_commit_handlers(CATransactionPhase)` |
| 23 | 17826 | 34.5 | `NSDisplayCycleFlush` |
| 24 | 17777 | 34.4 | `NSDisplayCycleObserverInvoke` |
| 25 | 15014 | 29.1 | `completeTaskWithClosure(swift::AsyncContext*, swift::SwiftError*)` |
| 26 | 14492 | 28.1 | `NSPerformVisuallyAtomicChange` |
| 27 | 14086 | 27.3 | `<deduplicated_symbol> [atos 0x100287389]` |
| 28 | 14085 | 27.3 | `<deduplicated_symbol> [atos 0x100330fc9]` |
| 29 | 12763 | 24.7 | `-[NSView layoutSubtreeIfNeeded]` |
| 30 | 12756 | 24.7 | `-[NSView _layoutSubtreeIfNeededAndAllowTemporaryEngine:]` |
| 31 | 12752 | 24.7 | `__56-[NSView _layoutSubtreeIfNeededAndAllowTemporaryEngine:]_block_invoke` |
| 32 | 11945 | 23.1 | `-[NSView _layoutSubtreeWithOldSize:]` |
| 33 | 11945 | 23.1 | `__36-[NSView _layoutSubtreeWithOldSize:]_block_invoke` |
| 34 | 11789 | 22.8 | `_NSViewLayout` |
| 35 | 11769 | 22.8 | `___NSViewLayout_block_invoke` |
| 36 | 11652 | 22.6 | `0x194664d5c` |
| 37 | 11649 | 22.6 | `0x194664178` |
| 38 | 11644 | 22.5 | `-[NSWindow(NSConstraintBasedLayoutInternal) layoutIfNeeded]` |
| 39 | 11462 | 22.2 | `-[NSWindow(NSConstraintBasedLayoutInternal) _layoutViewTree]` |
| 40 | 11325 | 21.9 | `__NSWindowGetDisplayCycleObserverForLayout_block_invoke` |

### Top 40 Sweetbriar frames by inclusive time (main thread, search phase)

Frames from the Sweetbriar and SweetbriarBridge binaries (the Rust core is linked into SweetbriarBridge).

| # | ms | % of main busy | frame |
|---:|---:|---:|---|
| 1 | 35839 | 69.4 | `main [atos 0x10028175b]` |
| 2 | 14086 | 27.3 | `<deduplicated_symbol> [atos 0x100287389]` |
| 3 | 14085 | 27.3 | `<deduplicated_symbol> [atos 0x100330fc9]` |
| 4 | 6265 | 12.1 | `<deduplicated_symbol> [atos 0x100286e55]` |
| 5 | 4416 | 8.5 | `closure #1 in MessageListViewController.show(_:) [atos 0x10031a801]` |
| 6 | 3906 | 7.6 | `@objc AppKitPrototypeList.tableView(_:viewFor:row:) [atos 0x100288c17]` |
| 7 | 3428 | 6.6 | `closure #1 in AppDelegate.startBenchmark(mode:reportPath:)` |
| 8 | 3413 | 6.6 | `MessageListViewController.install(_:ticket:)` |
| 9 | 3185 | 6.2 | `closure #1 in MainWindowController.reloadSidebar() [atos 0x100309bc7]` |
| 10 | 3014 | 5.8 | `specialized AppKitPrototypeList.tableView(_:viewFor:row:) [atos 0x10028bdcf]` |
| 11 | 2969 | 5.7 | `SearchFieldController.applyModel()` |
| 12 | 2809 | 5.4 | `closure #1 in closure #2 in implicit closure #1 in SearchFieldController.observeModel()` |
| 13 | 2602 | 5.0 | `ListBenchmarkRunner.runSearch(controller:)` |
| 14 | 2504 | 4.8 | `SidebarViewController.reloadSections() [atos 0x100331457]` |
| 15 | 1627 | 3.1 | `@nonobjc NSButton.__allocating_init(title:target:action:)` |
| 16 | 1315 | 2.5 | `closure #4 in MessageListViewController.init(bridge:sidebar:) [atos 0x10031910f]` |
| 17 | 1313 | 2.5 | `SearchFieldModel.updateText(_:composition:)` |
| 18 | 1297 | 2.5 | `closure #2 in AppKitPrototypeList.init(model:formatter:accountMarker:selectionLoss:onIndexChange:) [atos 0x100287aab]` |
| 19 | 1290 | 2.5 | `ListWindowModel.open()` |
| 20 | 1258 | 2.4 | `specialized reset #1 (scope:) in ListBenchmarkRunner.runSearch(controller:) [atos 0x1002ff953]` |
| 21 | 1217 | 2.4 | `SearchFieldController.navigated(to:)` |
| 22 | 1048 | 2.0 | `MessageListViewController.detach()` |
| 23 | 860 | 1.7 | `specialized reset #1 (scope:) in ListBenchmarkRunner.runSearch(controller:) [atos 0x100300b6b]` |
| 24 | 759 | 1.5 | `closure #6 in ListBenchmarkRunner.runSearch(controller:)` |
| 25 | 665 | 1.3 | `closure #3 in MainWindowController.install(_:) [atos 0x1003096bf]` |
| 26 | 637 | 1.2 | `SearchFieldController.refreshScopeMenu()` |
| 27 | 585 | 1.1 | `specialized closure #1 in FrameBatcher.post(_:) [atos 0x1016e3ff7]` |
| 28 | 582 | 1.1 | `closure #1 in ListWindowModel.queue(_:)` |
| 29 | 580 | 1.1 | `specialized FrameBatcher.enqueue(_:) [atos 0x1016d5da7]` |
| 30 | 576 | 1.1 | `SidebarViewController.reloadSections() [atos 0x100331403]` |
| 31 | 539 | 1.0 | `closure #1 in AppKitPrototypeList.init(model:formatter:accountMarker:selectionLoss:onIndexChange:)` |
| 32 | 539 | 1.0 | `ListWindowModel.deliver(_:) [atos 0x1016e2e8b]` |
| 33 | 528 | 1.0 | `AppKitPrototypeList.pagesDidLand(_:)` |
| 34 | 527 | 1.0 | `AppKitPrototypeList.init(model:formatter:accountMarker:selectionLoss:onIndexChange:)` |
| 35 | 507 | 1.0 | `PlainTextReaderView.text.setter` |
| 36 | 500 | 1.0 | `@objc BadgeView.init(frame:) [atos 0x10028b77f]` |
| 37 | 500 | 1.0 | `BadgeView.init(frame:)` |
| 38 | 496 | 1.0 | `MessageRowCellView.init(showsAccountMarker:) [atos 0x10028a1c7]` |
| 39 | 475 | 0.9 | `@objc BenchmarkTickSampler.tickFired(_:) [atos 0x1002b6ff7]` |
| 40 | 474 | 0.9 | `specialized AppKitPrototypeList.tableView(_:viewFor:row:) [atos 0x10028c04f]` |

### Top 40 Sweetbriar frames by attributed self time (main thread, search phase)

Each sample is charged to its leaf-most Sweetbriar frame, so system work a Sweetbriar call causes counts against that call.

| # | ms | % of main busy | frame |
|---:|---:|---:|---|
| 1 | 31120 | 60.2 | `main [atos 0x10028175b]` |
| 2 | 2051 | 4.0 | `SidebarViewController.reloadSections() [atos 0x100331457]` |
| 3 | 1565 | 3.0 | `@nonobjc NSButton.__allocating_init(title:target:action:)` |
| 4 | 1294 | 2.5 | `closure #2 in AppKitPrototypeList.init(model:formatter:accountMarker:selectionLoss:onIndexChange:) [atos 0x100287aab]` |
| 5 | 1016 | 2.0 | `MessageListViewController.detach()` |
| 6 | 987 | 1.9 | `SearchFieldController.navigated(to:)` |
| 7 | 860 | 1.7 | `specialized reset #1 (scope:) in ListBenchmarkRunner.runSearch(controller:) [atos 0x100300b6b]` |
| 8 | 701 | 1.4 | `closure #6 in ListBenchmarkRunner.runSearch(controller:)` |
| 9 | 626 | 1.2 | `SearchFieldController.refreshScopeMenu()` |
| 10 | 568 | 1.1 | `SidebarViewController.reloadSections() [atos 0x100331403]` |
| 11 | 500 | 1.0 | `BadgeView.init(frame:)` |
| 12 | 496 | 1.0 | `MessageRowCellView.init(showsAccountMarker:) [atos 0x10028a1c7]` |
| 13 | 451 | 0.9 | `PlainTextReaderView.text.setter` |
| 14 | 392 | 0.8 | `ListBenchmarkRunner.runSearch(controller:)` |
| 15 | 362 | 0.7 | `MessageRowCellView.init(showsAccountMarker:) [atos 0x10028a1e7]` |
| 16 | 313 | 0.6 | `MessageRowFormatter.dateString(_:now:) [atos 0x10031e5b7]` |
| 17 | 294 | 0.6 | `AppKitPrototypeList.buildTable()` |
| 18 | 269 | 0.5 | `specialized static RunContext.displayID(_:) [atos 0x1002fdb93]` |
| 19 | 253 | 0.5 | `MessageRowCellView.init(showsAccountMarker:) [atos 0x100289a3f]` |
| 20 | 217 | 0.4 | `MessageRowCellView.configure(content:)` |
| 21 | 200 | 0.4 | `@nonobjc NSTableColumn.init(identifier:)` |
| 22 | 190 | 0.4 | `@nonobjc NSScrollView.init()` |
| 23 | 187 | 0.4 | `@objc MessageRowCellView.isFlipped.getter [atos 0x10028b50b]` |
| 24 | 164 | 0.3 | `closure #1 in SearchFieldController.rebuildChips()` |
| 25 | 152 | 0.3 | `SearchFieldController.rebuildChips()` |
| 26 | 151 | 0.3 | `MessageRowCellView.init(showsAccountMarker:) [atos 0x100289b23]` |
| 27 | 148 | 0.3 | `MessageRowCellView.init(showsAccountMarker:) [atos 0x10028a2af]` |
| 28 | 147 | 0.3 | `MessageRowCellView.init(showsAccountMarker:) [atos 0x10028a2cf]` |
| 29 | 138 | 0.3 | `MessageListViewController.install(_:ticket:)` |
| 30 | 134 | 0.3 | `@nonobjc NSTableView.init()` |
| 31 | 133 | 0.3 | `@nonobjc NSMenuItem.init(title:action:keyEquivalent:)` |
| 32 | 130 | 0.3 | `AppKitPrototypeList.pagesDidLand(_:)` |
| 33 | 124 | 0.2 | `SidebarEntryCellView.configure(_:)` |
| 34 | 121 | 0.2 | `closure #1 in MessageRowCellView.applyLayerColors()` |
| 35 | 120 | 0.2 | `specialized SearchCoverageNote.init(coverage:excludedUnknown:locale:timeZone:) [atos 0x1016f8697]` |
| 36 | 105 | 0.2 | `MessageRowFormatter.dateString(_:now:) [atos 0x10031e667]` |
| 37 | 91 | 0.2 | `SearchFieldController.refreshStatus()` |
| 38 | 86 | 0.2 | `@nonobjc NSTextField.__allocating_init(labelWithString:)` |
| 39 | 85 | 0.2 | `@objc MessageRowCellView.isFlipped.getter [atos 0x10028b4ff]` |
| 40 | 77 | 0.1 | `SearchFieldController.applyModel()` |

### Named areas, main thread, search phase

Inclusive: a sample counts for every area with a matching frame on its stack.

| area | ms | % |
|---|---:|---:|
| AppKitPrototypeList | 6155 | 11.9 |
| MessageRowCellView | 3685 | 7.1 |
| Row presentation / formatters | 110 | 0.2 |
| ListWindowModel | 2234 | 4.3 |
| FrameBatcher | 594 | 1.1 |
| MessageListViewController | 5950 | 11.5 |
| SearchFieldController | 4172 | 8.1 |
| SearchFieldModel | 1583 | 3.1 |
| Benchmark harness (runner, tick sampler) | 4059 | 7.9 |
| -[NSTableView reloadData] | 1741 | 3.4 |
| NSTableView (any) | 12427 | 24.1 |
| NSView layout (layoutSubtreeIfNeeded) | 12763 | 24.7 |
| NSView display (displayIfNeeded / drawRect) | 2068 | 4.0 |
| NSTextView insertText | 177 | 0.3 |
| NSTextCheckingController | 541 | 1.0 |
| Core Animation commit (CA::Transaction::commit) | 26716 | 51.7 |
| NSDisplayCycleFlush | 17826 | 34.5 |
| Swift concurrency job (completeTaskWithClosure) | 15014 | 29.1 |
| Rust core (sb_*) | 7 | 0.0 |

### The timed keystroke itself

Main-thread samples inside the timed keystroke's `post` closure (`-[NSTextView insertText:]` of the last character, synchronous). `span` runs from its first to its last sample; on-CPU time below `span` means the main thread was off CPU inside the call.

| sample | rendered ms | span ms | on-CPU ms |
|---|---:|---:|---:|
| exact-reference r0 | 22775 | 835 | 57 |
| exact-address r0 | 13653 | 709 | 45 |
| phrase r0 | 4642 | 366 | 59 |
| scoped-filter r0 | 3206 | 486 | 32 |
| negative-body r0 | 3901 | 399 | 15 |
| exact-reference r1 | 3257 | 300 | 24 |
| exact-address r1 | 3110 | 90 | 16 |
| phrase r1 | 3557 | 332 | 23 |
| scoped-filter r1 | 2800 | 134 | 16 |
| negative-body r1 | 2631 | 226 | 12 |
| exact-reference r2 | 2148 | 100 | 10 |
| exact-address r2 | 3258 | 86 | 9 |
| phrase r2 | 2469 | 35 | 5 |
| scoped-filter r2 | 1878 | 132 | 28 |
| negative-body r2 | 2664 | 227 | 9 |
| exact-reference r3 | 3018 | 176 | 20 |
| exact-address r3 | 2503 | 280 | 19 |
| phrase r3 | 2986 | 65 | 12 |
| scoped-filter r3 | 2093 | 8 | 3 |
| negative-body r3 | 2689 | 136 | 16 |
| exact-reference r4 | 2289 | 79 | 15 |
| exact-address r4 | 2998 | 33 | 20 |
| phrase r4 | 2541 | 30 | 14 |
| scoped-filter r4 | 1612 | 62 | 5 |
| negative-body r4 | 2857 | 77 | 6 |
| exact-reference r5 | 1568 | 108 | 14 |
| exact-address r5 | 330 | 51 | 6 |
| phrase r5 | 2966 | 57 | 10 |
| scoped-filter r5 | 1829 | 18 | 15 |
| negative-body r5 | 2054 | 25 | 12 |
| exact-reference r6 | 1906 | 49 | 13 |
| exact-address r6 | 2728 | 84 | 8 |
| phrase r6 | 2882 | 129 | 14 |
| scoped-filter r6 | 1739 | 96 | 16 |
| negative-body r6 | 1610 | 25 | 12 |
| exact-reference r7 | 3045 | 29 | 11 |
| exact-address r7 | 1411 | 56 | 9 |
| phrase r7 | 2896 | 53 | 9 |
| scoped-filter r7 | 1604 | 51 | 12 |
| negative-body r7 | 2416 | 21 | 8 |
| exact-reference r8 | 2443 | 330 | 25 |
| exact-address r8 | 2441 | 80 | 10 |
| phrase r8 | 2239 | 15 | 11 |
| scoped-filter r8 | 1204 | 110 | 10 |
| negative-body r8 | 2282 | 26 | 15 |
| exact-reference r9 | 2039 | 320 | 20 |
| exact-address r9 | 2533 | 21 | 6 |
| phrase r9 | 2498 | 16 | 5 |
| scoped-filter r9 | 863 | 61 | 5 |
| negative-body r9 | 2866 | 22 | 10 |

### Main-thread busy ms per 100 ms bucket after each timed keystroke

Column k is [k*100, (k+1)*100) ms after the keystroke; `rendered` is the report's keystroke-to-rows time for that sample.

```
sample                    rendered   0   1   2   3   4   5   6   7   8   9  10  11  12  13  14  15  16  17  18  19  20  21  22  23  24  25  26  27  28  29  30  31  32  33  34  35  36  37  38  39
exact-reference r0           22775   9   3   6   6   9  10   1  13   8   9   9   9   6   8  19  13   5   6   7   4   1   5  11  14   7   7  12  10   7   3   9   2   8  14  11   6   5   6   2   5
exact-address r0             13653   5   2  10   6  13   4   6   7   4   5   6   7   7  15  13   5   3   4   7   7   7   7  19  19   6   6  15  13  12   8  12  12   9   7  13   7  18   7   6  12
phrase r0                     4642  13  11  12  36  30  35  36  40  17   2   3   6   3   1   1   1   1   2   3   0   4   3   2   2   1   3   1   5  26  34   9  28  11  16  13  13  18  15   9  11
scoped-filter r0              3206   9   7   6   7   9   6   8   3  11  17   5   9  10   7   8   8  17  21  12  18   8   7  11   6  12  11   4   9  21  12   4  22  24  28   5   1  11   8   5   4
negative-body r0              3901   1   3   5   6   6   5   4  11   3   5  13  14   4   5   5   6  15   6   8  17   6  26  17  13   9   6   5   5   8   8   1   8  11   2  22  11   5   7  17   0
exact-reference r1            3257  10   6   8   7   3  12   9   6  12   8  10   7   7   4   4   9   4   2   2   4   3  13   5  17  28  20  16  11  11  13  17  28  10   5   4   8   3  11  10  19
exact-address r1              3110  19  22   7  10   5   8   5   6   6   9   4   9  21   7   9  31  11  25  11  31  34  10   7   6  18  17  11   8  24  34  12  11   8  23  13  18  21   9   6   8
phrase r1                     3557   5   7   8   9  13  10   6  17  21  13  11   9  11   7  15  10   3   6  11  10   6   8   0   3   0   0   3   7   3   5  23   9   6   6  13  12   0   0   0   0
scoped-filter r1              2800  13  24  18  10  11  29  17   7   6   5  16   4   5   5  13  13  26   9   8  10  11  10  14   9  30   7  27  14   1  21  33   4  15  13   9   7  29  33  18   9
negative-body r1              2631   4   5  10   8   2  12   5   5  12  21   3   1   1   0   1   2   2   0   0   0   0  11  31  15  15  17   0   0   0   1   0   0   0   1   1   0   7  14  14  13
exact-reference r2            2148   9  13   9   3   7  10  14  11  14   8   9  15  15  14  14   2  11  14  20  11   7  23  11  18  10   6  15  17  23  16  18   7   3   7  10   6  29   2  27  20
exact-address r2              3258  14  24  10  16  18   2   8   5   5   6   6   5   4   6  25   3   6   6   4   6   6  12  27  39  13  19  16  22   4  21  25  24  24   5   7  10  13   6   6  10
phrase r2                     2469  15  15   7  19   6   6  13   7   3   2   1   1   1   2   2   6   5   9  18   9   7   7   5   8  10   2   2  16  29  16   7  12  11   6  11   9   7  15  10   9
scoped-filter r2              1878  16  19   8   4   4  18   9  23  12  20  17  25  15  13  12   6  23  37  17   9   7   7  36   6  12   2   3   2   2   2   2   2   2   2   2   2  28  29  27   9
negative-body r2              2664   6   2   2  11   6   9  10  37   0   1   5  13  13  11   8  12   0   2   0   1   4  16  26   1   5  14   1   1   1   0   1   5  14  21  27  20  16  25  38  23
exact-reference r3            3018  10  15   7   7   6   4   2   7  17  11   7  21  28  11  12  29  28  21   8   0   1   0   2   1   1   2   1   2  13  12  25  10  19   4  17   6  19  30  16   7
exact-address r3              2503   6   9   8   8   6  19   8  11   9   6  21  16  15  28  18   8  13  18   7   1   9   9  17  15  13  25  47  33  28  12  18   6  16  25  15   7   7   8   9   7
phrase r3                     2986  16  10  12  13  12  12  15  28   8   5   0   1   0   0   0   1   1   0   2   0   3   4  15   5   7  25  14  14  10   6  20   0   0   1  10  22  38  23  31  46
scoped-filter r3              2093  18   8   5   4  16   7  10   7  20   8   9   8  11  10  21  25  34  44   9  23  16  18   0   0   0   2   1   2   1   2   3   2   1   1   1   0   0   6  22  22
negative-body r3              2689   5  22  29  30   8   8   8   9   0   0   0   1   1   1   2   0   0   1   1   2   8   6  30   3  25   9  11   0   0   0   0   1   1   0   1   1   0  26  14  21
exact-reference r4            2289  16   6  11  14  26  12   9   0   0   0  15  20   7  18  12   1   1   2  13  18  16  36   9   7  20  29  16  13  31  21  43  54  34  21  44  21  15  24   4   9
exact-address r4              2998  28  15  15  12  12  13  41   8   0   4   5  13   4   7   9  12  25  19  29  16  14  16   8  13  23  16   6  21  23  22  19  19  28  19  13  37  17  15   6  15
phrase r4                     2541  29  13  15   8  22   3   0   0   2   0   0   1   1   0   8   3  11  10   9   6   8  46   9   8   8   3  46  10   4  17  21  25   6  11  20  20   5   2  17   3
scoped-filter r4              1612  19   5  38  16   3   5  15   9   6  12  20  25  14  19  38  24  29   2   3   4   2   1   3   1   5   4   2   4   3   3   3   4   2   5   2   2   2   3   3   2
negative-body r4              2857   6   5  23  20  27  25   7   3   0   2   3   3   3   2   2   1   2   1   0   1   2   3   7  16  13  21  26  45  21   0   0   2   0   2   2   2   3   4   3   7
exact-reference r5            1568  12  11   7  16  10   3   6   6   3   7  11   8   8  15   3  20  11  15  20  11  11  49  17  10  21   8  17  21  17  14  24   4  50  31  29  45  12  20   4  18
exact-address r5               330  18  16  13   9  30  10   7   7   6   7  14  13  24  16  10   9  28  11  35  47  27  23   1   0   7  11  12  26  14   7  31  45  42  32  23   1   2   1   0   0
phrase r5                     2966  17   7  30  12   6  41  39  41  27  24   0   2   1   0   0   0   1   1   0   0   1  12   3  21  30   4  13  11   6   7   0   0   0  23  39  33  29  48  42  54
scoped-filter r5              1829  35  17  29  31  19  17  45  40  18  27  30  73  32   8  32  24  18  12  27   5   0   0   1   0   0   1   1   1   0   1   0   2   1   0   3   4   3   8   5   3
negative-body r5              2054  28  19  34   9   5   0   1   0   1   0   3  21   0   0   0   1   4  12  41  19   9   0   0   0   0   0   0   0   1   1   1  16  13  11  26  12  14  19  10  27
exact-reference r6            1906  24  20  29  12   5  10   5   2   0   0   0  18  12  10  14  12   9  15  29  17  17  24   9  34  58  29   9  18  15  11  10   6   3   2   8  26  18   7  30  21
exact-address r6              2728  10  13   3   8   4  17   4   7  17   9   8   7  13  21   7   6   6  10  16  27  53  22  25  17  20  38  16  10   3  24  20   7  37  26  12   2   1   8  11   9
phrase r6                     2882  11   9  12  14   5  10  14  27  24  15  14  36  35  19  36   4   1   0   3   5   1   1   1   0   1   0   1  56  35   1   0   2   1   0   0   1   2   2   4   2
scoped-filter r6              1739  16  15  13   8  13   9   3   4  23  19  15  30  24   2  17  16  29  41   9   0   1   0   0   2   0   2   2   2   0   1   0   0   1   0  21  36  47  10  34  37
negative-body r6              1610  19   8   9   7   3   6   9  12   0   0   1   1   1   1   0   7  12  22  22   5  14   1   0   3   1   0   0   1   8  18  19  31  27  16  40  28  12  41  52   7
exact-reference r7            3045  20  15  14   2  12   0   0   1   1   0   0   1   2   0   0   3   8   8   4   7   6   6   9   6  12  17  20  24  20  18  26  35  11  37  10   7  58  24  13  16
exact-address r7              1411  15  11  18  24  38   6  14  11   8  14  18   8  24  36  35   9  39  28  14  10   4   8  26  20  14  18  19  16  15   3   2  13  22  20  11  54   2  13  13  25
phrase r7                     2896  26  18  13  54   2  11  15  21  20  20  13   6  23   9   2   1   2   2   3   2   2   1   1   2  35  35   4  12  14   2   2   2   1   1   1   1   3   0   6  21
scoped-filter r7              1604  23  14  38  21  32  14  32  28  31  21  35  17  29  17   9  33  38   3  15   1   0   1   1   1   1   1   0   0   0   2   1   0   1   0   1   1   1   8  10   8
negative-body r7              2416  18  14  10  13   5   1   2   1   2   5   2   2  13  13   9   3   1   1   2   4   1   0  52  17   0   0   0   0   1   2   3   1   0   0   2   7  13  47  23  28
exact-reference r8            2443   4   5  11  14  17   5  10  10   7   8   8  11   5   8  15   2  10  24  12   7   4   7   6  18  20  27  11   8   6   7   5  20  10  17  30  25  17   9   6  12
exact-address r8              2441  12  19   3   2  11   6  15  23  15  22  16  10  13  17   1  22  13  31  10  23   6  18  12  14  12   5   2  16  48  20   5  10  15  11  31  42  26   5  15   3
phrase r8                     2239  45  12  14  16   8  12  14  13   0   1   0   0   1   0   1   0   0   0   0   0  14  42  36   2   1   3   0   2   0   0   0   1   1   1   4  20  45  41  20  35
scoped-filter r8              1204   4  22  10  14  11  36  18  21  32  21   7  49   5   2   2   2   2   2   2   2   2   2   3   1   1   1   2  11  24  29  40  15  11   9  22  46  16  17   5  16
negative-body r8              2282  26  13  14  29  10   1   1   2   1  16   2   3   1   0   1   2   1   1   1   5  33  39  34   3   3   2   3   2   1   4   3   2   0   2   2   3   2   3   2   1
exact-reference r9            2039   4   4  10  10   5   9  11   6  18   1   7   8  15   5  12   7  13  12  12  16  16   9  20  19   5   7   6   9  24  37  10   9   7  25  16   6  15  21  26  21
exact-address r9              2533  18  18   9  16  21  16  23  30  10  41  12   7  24   8  37  35  23  17  15  24  24  23  33  54  72  20  42  18   9  43  39  16   2   0   6  13  33  14   0   1
phrase r9                     2498  22  28   8   0  10   3   2   0   0   0  14  18  18  30   0   3   3   5   1   1  12   9  22   7  23   0   0   2   2   0   0   2   1  23  17   7  28  32  21  10
scoped-filter r9               863  11  28   9  13  12  11  23  44  35  13   9   8  17   4   3   2   4   1   3   2   3   2   5   2   4  11  16  29   8   7  17  23  31  20  11   3  36  38  34  21
negative-body r9              2866  27  36  33  11   5   4   2   2   2   3   2   2   3   5  18   4   6   3   5   5   4   3   2  21   8  11  43  14   8   1   1   2   1   2   2   1   3  10  47  13
mean                                15  13  14  13  12  11  12  13  10   9   9  12  11   9  11   9  11  11  10   9   9  12  13  10  13  10  11  12  12  11  12  11  11  11  13  13  15  15  15  14
```

### Main-thread busy ms per 100 ms bucket, whole search phase

One line per second of trace time, ten buckets per line; `*` marks a second holding a timed keystroke.

```
   150.1s     3   1   4  12   7   3  11  14  10   6
   151.1s     7  11  15  12  12   8   6  10  11   1
   152.1s     7   2   8  11  10   6   5  12   9   9
   153.1s     7  19   7  11   6   5   6  11   8   6
   154.1s     6   6   5  10  10   4  14   1   0   2
   155.1s *   9   3   6   6   9  10   1  13   8   9
   156.1s     9   9   6   8  19  13   5   6   7   4
   157.1s     1   5  11  14   7   7  12  10   7   3
   158.1s     9   2   8  14  11   6   5   6   2   5
   159.1s    10  10   9   7   9   9  14  12   7  12
   160.1s    14   7  12   1   8   3  16   4   4   8
   161.1s    13   7   8   8   4   8   7  12  21  21
   162.1s     4  14  13   8   7   5  16  16   6  16
   163.1s     8   9  11   5  13  11   4   4   7   7
   164.1s     6   8   9  10   6   1   2   5   3   3
   165.1s     8   4   7   5   7   6   6   8   9   9
   166.1s     6   6   7   6   5   6   5   5   2   7
   167.1s     5   8   6   6   6   8   4   7   4   6
   168.1s     4   8   8  10   9   5   4  12   9   5
   169.1s     6   7   8   2   8   5  11  12   3   6
   170.1s     6   5   4   3   7   7   9   8  13   5
   171.1s     4   6   1   6   5  16   8   9  13   7
   172.1s     0   2   0   2   0   1  12   0   4   6
   173.1s     5  10   3   9   5   7   5   4   9   4
   174.1s     6  10   9   9   4   7   6   9   6   7
   175.1s     5   6   5   7  13   6   4   4   5   2
   176.1s     5   8   8   6   7   4  10  19  15   3
   177.1s     6  12   4   6   3  12   7   4   8   7
   178.1s     8   3   9   3   7   6   2   7  10   7
   179.1s     9   6  11   8   2   3   7   3   7   4
   180.1s    11   6   4  10   5  15   7   3   3   8
   181.1s     4   4  11   4   4   9  12   4   5   4
   182.1s     4  13  17   3   4   6  11   8   6   3
   183.1s     6   6   4   7   6   8   7   5   8   6
   184.1s     6   4   7   8   3   7   8   7  10   5
   185.1s     9   3   4   8   4   7  14   5   6   8
   186.1s    12  11   6  12  11  11   9   9  11  11
   187.1s    12  15   9   5   8   6   8   5  10   9
   188.1s     2   5   9   2   6  11   6  13   6   6
   189.1s    11   8   1  11  20   5   9  14   7   4
   190.1s    11   7   6   6  10  17  13   5  13  23
   191.1s    11   9  15   8   5   8   8   6   6   8
   192.1s     8  18   9   9  10  11   8   7   9   8
   193.1s     6   8  12  14  17  21  13  21  12  11
   194.1s    13  17   5   8   6   9   5   3   4   7
   195.1s    10  12   9   7  18  12   5   4   5   8
   196.1s    24  18  23   7   8  10  12  10  10  10
   197.1s     8   5  14   6   4   7  13  14  10  10
   198.1s     5  25   8   9   5   2   8   4   9   6
   199.1s     3   3   5   9   5   4  11   1   2  12
   200.1s     9   5   3  10   4  11   5   4  10  10
   201.1s     7   0   2   3  15   7  13   7  10   3
   202.1s    12   6   8  10   8  11   7  14  10   9
   203.1s     8   6   8   5   4   8   6   3   7   5
   204.1s     6   6   9  12  12   5   8   5   7  11
   205.1s    10  11  10  11   7   5   8   8   7   4
   206.1s     6   7   6   9   6   4   8  12  16  10
   207.1s     7  18  14  17   8   9  11   3   7  11
   208.1s    11   8   4   8  10   9  20   4   5  17
   209.1s    10   8  16  11  14   8   5   4   5  13
   210.1s     7   4   4   6   7  10   6  17   9  14
   211.1s     5   5   8   4   6   5   4   3   4   8
   212.1s *  10   7   1   6   1   5   3   8   5  11
   213.1s     7   4   9   5   5   5   4   9  10  18
   214.1s     7   3   2   7   6   8   6  17  18   9
   215.1s    10   9  15  10  11  12  11  11   6  12
   216.1s     8  19   8   6   8   6   7  15   6   8
   217.1s    14  10  10  12  10   9   8   8  14  11
   218.1s    14  17  12  14   8  13   6   6  22  10
   219.1s    18  11  10   9  13  10  12  11  20  13
   220.1s     6  11   8  17  10  13  13   9   7   6
   221.1s    14   6   9   7  11   5   8  12  27   9
   222.1s    10  10   6  19  12   5   4  10  12   6
   223.1s    15  22  25  15   8  14   8  10   3  10
   224.1s     7   6  10  12   6  16  10  11  13   7
   225.1s    10   7   5   4  28   9  16  12  19  22
   226.1s    18  11   7   3   9   9  17  10   1   7
   227.1s     7   8  17   5  14  32   6  13  10   7
   228.1s     4   5   4   3  12   8  10  10   8   2
   229.1s     5   6   3   5   7  15   3  22  23   8
   230.1s    11   3   5   6   6   1   4   7  11   8
   231.1s     1  12   6  10  22   8   6   9   8   8
   232.1s     2   9  10   5   2   8  12   7   4   8
   233.1s    13   6   7   6   5   9   7   6   7   8
   234.1s     5   6   4   3   9  11  14  10  11   7
   235.1s     8  13  11   6  10  12   8   2   7   5
   236.1s    25   8   4  11  16  15   6  10   8  24
   237.1s     6   8   9   4   6   2  29   5   6   9
   238.1s     6  12   9   6   9  10   5  30  20   5
   239.1s *   4  11   2  19  11   9  12  42  30  38
   240.1s    32  39  12   2   7   2   3   1   1   1
   241.1s     1   2   3   0   4   3   2   2   1   3
   242.1s     1   5  32  28  10  27  12  16  17   8
   243.1s    24  14   5  19  22  19  13   6  11   7
   244.1s     2   2   3   2   4   4   4   9  14  16
   245.1s    29  45   5   7  26  25   9  12   6  29
   246.1s     8   4   8  11  10   6  40  25   9   9
   247.1s    17   4   6   9   3   7   6   9   7   8
   248.1s    15   8   7  10   8   6   5   8   9   8
   249.1s     4   2   6  12   8   8   7   6   2   7
   250.1s     4   8   8   7   8   8  11  12  10  13
   251.1s     7  11  12  11   6   7   5   6  14  10
   252.1s     9   6  11   7   8   5   5   3   9   7
   253.1s    17  19  10  17  23  24   9  12  10  16
   254.1s     7   9  20  12   4  16  20   7  11  15
   255.1s     8  21   6  11   6   6   9  16   8  11
   256.1s    23  45  21  10  10  12  13   4  10  10
   257.1s    16   5  15  10   8   7   5  12   6   6
   258.1s *   7  11  11   7   0   1   7   7   7   7
   259.1s     7   9   6   7   5  11  17   4  10   8
   260.1s    10   7   6  24  16  14  16   6   8  11
   261.1s     6  12  10   5  12  18  11   6  25  32
   262.1s    15   5   2  10   8   5   5   1   9   7
   263.1s     8  14  15   6  11  11  22   7   9  17
   264.1s     9   6   8   8   7  14  12  14  21  11
   265.1s     3   7  17   8   8  10   5   5   2   2
   266.1s     6   5  24  17  16  12  10   8  12  13
   267.1s     4   4  11  13   9  14  26   3   9  13
   268.1s    11   4   5   4   3   6   3   2   7   5
   269.1s     5   7  19  10   4   9  12  15  14  23
   270.1s     8   9  16   4  13   8   9   5  11   6
   271.1s     5   7  14  17  25  11   9  17  12   6
   272.1s    43  16  14   4   5  15  13   5   8   8
   273.1s     5  10  10   8   8  16  14   9  10  11
   274.1s     2   9  26   6   8   7   9  12  10   2
   275.1s     4   4   2   2   4   3   3   2   2   6
   276.1s     7   4   6   5   3   9   3   1   5   5
   277.1s     3   6   4   5  31  21  10  21   9   4
   278.1s *  36  11   9  13   1   2   5   5   5   6
   279.1s     4   8   8   4   3  19  10   3   4   5
   280.1s    10  12   7   8  16  26  16  13   9   8
   281.1s     6   4  10   5   4   6   7   8   2  27
   282.1s     5   7  16   5   0  14  14  16  16  28
   283.1s    12  15   7   4  12   8  23  10   5  14
   284.1s     7  10   8   8  10  18   6   7  12  14
   285.1s     8   5  20   7  27   8   5  34   8  15
   286.1s     1   8   7   4   9   2   4  14   8  14
   287.1s     5  13   5   7   5   7  10   7  13  17
   288.1s    22   3   4  15   8  12   4   6   7  12
   289.1s    12  10  15   7  16  11  32   7  14  18
   290.1s     4   6   9   8  15  12  12   8   8   1
   291.1s     5   4   5   4   5   4   3   8  19   3
   292.1s     5   4   8  12   8   7   6   6  12   4
   293.1s *   9   5   0   1   7   9   7   7   4  10
   294.1s    10   7  10   8  10   8   8   4   2   9
   295.1s     6   2   2   4   3  10   8  14  30  19
   296.1s    17   7  16  11  16  24  15   7   1   9
   297.1s     5   9  11  18   6  10  11   5   7   5
   298.1s     5   7   3   9  12  16  23  16  24  39
   299.1s    16   7  10  22   4   2  11  15  14  20
   300.1s    14  13  11   4  49   6   8   7   6  39
   301.1s    15  17  14  28   8  13  16   6   7   5
   302.1s    11  12  13  21  10   9   8  26  18  11
   303.1s *   5   8  10  23  13  12   7  13  21  15
   304.1s     9   7   6   7   4   8   8   5   6  17
   305.1s    12   9  10  33  21  14  33  27  14   9
   306.1s     4  18  17  10   8  19  37  14  13   5
   307.1s    21  15  18  19  14   5   9  23  18  10
   308.1s     7   7   6   6   3   5   7   5  30  27
   309.1s    36  19  11   9  22  17  38  13  10  32
   310.1s    30  22   3   4   4   8  22  15  10  11
   311.1s    10  11  22  11   7   6   7  11  10  12
   312.1s    28  18  21  22  20  50  14  21   8  18
   313.1s *  23  12   4   0   1   5   7   8   9  13
   314.1s     9   7  14  24  12  12   9  11   7  15
   315.1s    10   3   6  11  10   6   7   1   3   0
   316.1s     0   3   7   3   5  23   9   5   7  13
   317.1s    12   0   0   0   0   9  23  29  21  21
   318.1s    17   6   8  14  11  35  14   6  11   4
   319.1s    10   4   1   6   7  33  18   5   7   9
   320.1s     4   6  15   5   8   8  17  17   8   8
   321.1s    11   3   8   7  12  11  24  21   6   7
   322.1s    18  20  14  13  15  16  10  16  31  10
   323.1s     5   5  10   9  13   9  32   9   9   8
   324.1s     4   9  16  12   5   7   4   5  20   4
   325.1s *   9   1   0   0  11  25  19  10  11  27
   326.1s    19   6   7   5  15   4   6   5  13  13
   327.1s    25  10   8  10  11  10  13   9  28  10
   328.1s    26  15   1  20  33   5  15  10  12   6
   329.1s    28  35  15  11   8   3   4  13   8   7
   330.1s     8  21   8  18  12  14  18  15   7   6
   331.1s    42   8  36  13  14  17  10   7   7  15
   332.1s     4   1   0   1   0   1   0   0   1   3
   333.1s *   9  31  19  12  18   0   1   1   4   4
   334.1s    11   3   7  11   6   4   9  25   3   1
   335.1s     1   0   1   2   2   0   0   0   0  10
   336.1s    31  13  17  18   0   0   0   1   0   0
   337.1s     0   1   1   0   7  12  16   4  39  33
   338.1s    27  28  40  39  17  57  17  26  17  13
   339.1s    13   9   7  17   6   7  12  12   4   8
   340.1s     2   9  14   5  20  17  15  10   0   0
   341.1s *  10  14   9   5   4  10  11  14  11  10
   342.1s     7  20  10  18   9   8   4  21  16  14
   343.1s     6  25   8  18  11   7   7  18  30  15
   344.1s    17  10   4   3   9   8  11  23  12  30
   345.1s    13  16  12  26  13  13  16  33  19   6
   346.1s    19  22   3   9   5  16   4  14  10   9
   347.1s    12  15  20  11  13  23  12  13  15  24
   348.1s *  14  32   7  13  24   3   6   5   4   7
   349.1s     8   4   3   6  19  11   5   4   8   5
   350.1s     7   4  31  30  21  18  13  20  13  18
   351.1s    20  22  21  17   7   8  16   4   9   9
   352.1s     9  10  18   7   4   8  20  28  19  27
   353.1s    28  20  14   7   5  11   7   3  12   7
   354.1s     6   9  15   5  23  19  27  21  40  26
   355.1s *  26   9   8   5  12  13  18   6   3  14
   356.1s    17   6  21   5   7  11   6   3   2   2
   357.1s     0   1   2   2   6   9   6  20   8   8
   358.1s     4   7   6  10   3   4  19  23  19   7
   359.1s    13   7   6  13   7  13  14   6   9   5
   360.1s     7  29   6   8   3   7   3  32   8   3
   361.1s    32   9  27  24   9  20  20   5   8  10
   362.1s *   8  10  25   8   4   3  13  15  22   6
   363.1s    24  17  19  22  11  10  11  23  28  20
   364.1s    14   8   6  26  16  13   3   3   2   2
   365.1s     2   2   2   2   2   2   2  23  16  25
   366.1s    28   6   0   3  21  15   8  15   9   6
   367.1s    43   7   8  11  21  16  20  20  13  15
   368.1s     7  13  12  27  19  12  17  30  28  12
   369.1s     8  12  14  12   2   7   3   7   6   0
   370.1s     1   6   3   8   2   0   1   3   4   9
   371.1s     6  11  31   8   6  12  13   5  12  15
   372.1s *   4   3  16  43   3  11   5   3   1   8
   373.1s     6  13   1  38   8   1   3   8  18   8
   374.1s     9  16   0   1   1   0   2  12  31   3
   375.1s     4   8   8   0   1   1   0   1  10  26
   376.1s    25  15  16  31  30  33  10   7  24  10
   377.1s    11   4   4  10  18  16   3   6   2   4
   378.1s     3   7  28   2   8  18  28   0  15  42
   379.1s     9  26   6   8  18   1   0   3   1   3
   380.1s *   7  10  14   0   0   2   8  15   7   7
   381.1s     6   4   2   7  17  11   7  23  26  12
   382.1s    11  29  28  23   6   0   1   0   2   1
   383.1s     1   2   1   2  13  12  25  11  18   4
   384.1s    17   6  20  29  16   7  12   6   1  31
   385.1s    12  10   7  22  19  22  20  31  23  24
   386.1s    29  12  16  12  19  13  19   7  20  20
   387.1s *  13  15  11  14  15   6   9  10   8   5
   388.1s    16  12  11   9   6  16  17  18  28  15
   389.1s    10  15  15  10   1   8  10  14  18  10
   390.1s    26  41  29  39  13  18   5  15  27  14
   391.1s     6   9   7   9   6  11   7  13  25  44
   392.1s *  22  18  16  10  10  15  15  12  13  28
   393.1s     9   6   0   1   0   0   0   0   2   0
   394.1s     2   0   2   4  15   5   8  11  28   8
   395.1s    13   9  16   4   0   1   2  23  32  34
   396.1s *  22  51  45  29  59  24   3   0   1  18
   397.1s     8   5   4  16   7   9   8  20   6  11
   398.1s     7  12   9  22  23  31  49   5  26  15
   399.1s    20   0   0   0   2   1   2   1   2   3
   400.1s     2   1   1   1   0   0   5  23  22  17
   401.1s    14  11   9   4   1  48   8  29   9   7
   402.1s    32   3   7  17   8   7  10  21  51  30
   403.1s    28   5   4   2  10  10   0   0   1   1
   404.1s     2   0   1   2   0   1   2  15   4  29
   405.1s *   6   8  17  25   5   1   6  21  25  27
   406.1s    16   6   6  12   1   0   0   1   1   1
   407.1s     2   0   0   1   0   3   5   9  27   5
   408.1s    24   7  15   0   0   0   0   1   1   0
   409.1s     1   0   1  23  12  23   8  15  30  13
   410.1s    18  13   7  10  10   9  36  11  11  12
   411.1s    15   6  16  15  15   8   7  11  35  25
   412.1s     8  13  16  17  11  11   5  17  16  18
   413.1s     6   6   8   8   3  12  10  13   6   4
   414.1s    11  13  13  14  10  19  11   8  21  11
   415.1s    24  28   8   6  25   9  13  12  22  23
   416.1s    11   5   2   2   3   1   0   0   0   0
   417.1s *   7  17  22   1   0  13   7   4  17  23
   418.1s    18   9   4   0   0   4  23  10  19  16
   419.1s     1   1   0   7  23   9  39  14   4  16
   420.1s    26  21  15  15  31  26  64  42  15  40
   421.1s    37  12  23  11   5   6   7  12  12   9
   422.1s     9  10  16   5  11   8  10  13   0   5
   423.1s     9  15   9   6  15   8   9  13  15   7
   424.1s     9  10  12   8  14  13   6  14  11  15
   425.1s     7  14  11  17   9  15  27  21   6   5
   426.1s *  10   3   6   6   8  39  16  11  11  15
   427.1s     7  32  23   7   2   4   9  11   2   5
   428.1s    17  10  25  16  33  14  10  20   8  14
   429.1s    23  13  18  11  24  29  10  30  19  15
   430.1s    41  10  23   7  16   6   7   6  11   8
   431.1s    17  25  11  17  13   8   5   8   9  11
   432.1s    21  25   4   3  24  22   8  24  14  23
   433.1s *  14  13  20  18   2  31   6  16  10  17
   434.1s    11   1   0   1   1   0   1   1   0   1
   435.1s    10   7  11  10   7   5  33  26   7   9
   436.1s     3  31  19   6  10  27  24  10   8  17
   437.1s    19  13   3   8  12  16   6  42  16   5
   438.1s     8   8  21  18  18  20  16   6  29   4
   439.1s *  25  17  19   5  17   7   7  16  30  19
   440.1s     8   5   9  15   5  10  19  18  20  14
   441.1s    45  24  18  17   3   4   2   1   3   1
   442.1s     5   4   2   3   4   3   3   3   3   4
   443.1s     2   3   2   3   3   2   2   2   3   5
   444.1s     2  13  15  15   6  18  38  25  19  45
   445.1s    13   9  44  16  14   6  11   9   7  16
   446.1s    23   5  14  16  51  38  10   9  11  11
   447.1s    13  24  17  14  38  33  20  18  24  14
   448.1s     8  11   9  10   6   2   7   2   3   2
   449.1s     3   2   2   2   3   4   2   9   8  10
   450.1s *  23  11  11   7  16   1   0   0   3   5
   451.1s     7  22  23  27  23   5   2   1   2   4
   452.1s     2   2   3   2   1   1   1   1   1   1
   453.1s     3   9  16  14  26  24  55   5   0   1
   454.1s     1   1   2   2   2   3   4   3   8   3
   455.1s     2   2  22   8   6  34  39  17  27  20
   456.1s    18  14  14  17  19  12  11   3  52  16
   457.1s    23  34   6   9   3   7   4  11   3   1
   458.1s     2   5   6   8   8   8  14  21  15  16
   459.1s *  16   7  22  17   2   2   2  13  11   6
   460.1s    15  10   5   4   6   3   8  11   7  10
   461.1s    14   3  20  12  14  21  15   6  48  19
   462.1s     8  21  11  17  18  18  17  20  11  49
   463.1s *  29  30  41  11  21   4  21  17   9  17
   464.1s    22   9   8   8   3  11  12  20  17  17
   465.1s     7  13  28  10  53  28  27  21   0   4
   466.1s *   6  14   7  30  12   6  41  40  40  27
   467.1s    24   0   2   1   0   0   0   1   1   0
   468.1s     0   2  11   4  20  30   5  12  11   8
   469.1s     5   0   0   1  27  34  33  32  48  40
   470.1s *  54  14   9  11   9  16  26  23  19  31
   471.1s    17  23  46  36  23  29  37  66  22  14
   472.1s    39  17  15  20  21   0   0   1   0   0
   473.1s     0   1   2   0   0   1   1   1   1   0
   474.1s     4   4   3   8   5   3   0   0  12  20
   475.1s     8  28  21   6  23  11  46  10  11  15
   476.1s     5  22   9   0   1   4  17  11   1   2
   477.1s     5   6   5  17  12   0   6  12  42  26
   478.1s *   0   0  20  11  35  17  10   2   0   1
   479.1s     0   1   2   2  20   0   0   0   3   3
   480.1s    25  38  15   2   0   0   0   0   0   0
   481.1s     0   2   0   5  15  14   8  31  12  17
   482.1s    10  14  31  14  11  24  25  17  10  35
   483.1s    25  51  31  42  30  15  12   4   1   3
   484.1s     9   2  14   1   0  11  15  18  14   8
   485.1s *   8  19   3   3   0   9  16  32  25   5
   486.1s     6   8   4   2   0   0   8  19   7  13
   487.1s    11  12   9  19  25  20  17  25   7  43
   488.1s    54  23  14  20   7   9  11   5   3   1
   489.1s    20  22  12  10  38  21  48  16  16  21
   490.1s *  14  13  20   7  13   4   7   6  15   4
   491.1s    10  17   8   6  11   9  25   3   6   6
   492.1s    18  10  29  56  19  21  18  21  38  15
   493.1s     9   3  29  15  10  45  15  12   2   2
   494.1s *   9  11  11  11  13   4  15  11  39  11
   495.1s    14  33  35  19  21  32   2   0   2   3
   496.1s     3   2   1   0   0   1   0  20  64   9
   497.1s     0   0   3   0   0   0   2   2   4   1
   498.1s     3   4   9  37  20  29  20  12  12  32
   499.1s *  30  21  47  23  33  25  11   2   6  15
   500.1s    14  15   8   9  12   4   4  23  19  14
   501.1s    27  26   4  15  18  26  40  13   0   1
   502.1s     0   0   2   0   2   2   2   0   1   0
   503.1s     0   1   0  16  35  50  11  33  40  23
   504.1s     9   9  40  22  32  39  15  11  14   4
   505.1s     1  11   5   8  15   6   9   7  16   7
   506.1s *  10  15  16   4  11  49   3   0   0  17
   507.1s    10  10   5   4   5  11  12   0   0   1
   508.1s     1   0   2   0   4  12  21  21   9  15
   509.1s     1   0   3   1   0   0   0   7  12  23
   510.1s    32  20  24  29  36  17  14  77   7   9
   511.1s    13  25  23  11  13  10   8  22  63  24
   512.1s    34   8  20  30  45  17  18   8  15  23
   513.1s *   2   1   1   0  26   3  11   3  17   6
   514.1s    13  15   3   9   0   0   1   1   0   1
   515.1s     1   1   0   0   8   7   8   4   4   7
   516.1s    10   6   9  11  24  23  18  14  33  15
   517.1s    31  21  31   8  22  49  19  10  24  45
   518.1s *  11  23  19   8  12  12   6  12  19  13
   519.1s    11  17  34  26   8  16  10  10  14  15
   520.1s    12  20  47  23  12  42  23  17   6   8
   521.1s     4  33  16  10  20  21  22   6   3   2
   522.1s *  13  25  18  26  38   2  14  20  26  12
   523.1s    16  12  16  15   7   1   2   2   2   3
   524.1s     2   1   1   2   5  50  18   6  12  11
   525.1s     2   2   2   1   0   2   2   1   1  13
   526.1s    14  27  31   8  17  49  66  30  14   8
   527.1s *   5  18  15   9   6   2  17  13  25  28
   528.1s    19  27  32  22  24  24  25  34  26  27
   529.1s    28  14  36  13   6  47  24   5  14   0
   530.1s     0   1   1   1   1   1   0   0   0   3
   531.1s     0   0   1   1   1   0   2   9   9  17
   532.1s     4   9  26  10  21  14  14   8   5  12
   533.1s    15  23   9  15  14  13   9  22   3   4
   534.1s     9  44   6  10   8  10  20  26  17  27
   535.1s    16  31  30  27  10  24  10  13  13  36
   536.1s    13   9   7   7   1   0   4   0   1   0
   537.1s     1   1   0   1   0   3   5   7   4  34
   538.1s *  24   0   0  10  19  10  12   9   0   2
   539.1s     2   2   3   3   2   7  12  15   4   1
   540.1s     1   1   4   2   1  23  37   9   0   0
   541.1s     0   0   2   3   1   1   0   0   6   7
   542.1s    35  38  14  28  13   8   8   4   3   8
   543.1s     5   3   5   8   4  18   6  15  11   3
   544.1s    12   2   7   6  20  11  10  32  36  25
   545.1s    35  30  11   6   2  26   3   2   2   0
   546.1s     0   0   1   9  14  13  17   8   3   0
   547.1s *   0   3   6  10  16  17   5   7  11   8
   548.1s     6  10  10   6   9  14   3   8  20  18
   549.1s     7   3   8   4  17  20  19  22   8   5
   550.1s     6   6  15  15  17  16  34  21   7   8
   551.1s *  13  12  31   9  27   6  14  30  15   2
   552.1s    15  15   3   5   8   9  12  26  26  10
   553.1s    17  10  13  14   1  25  19  26   6  23
   554.1s     8  21  15   8  10   7   3  21  47  13
   555.1s     5  13  12  12  48  29  22   5  15   3
   556.1s *  12  22   4  27  44  30  17   2  16  33
   557.1s    13  18  12  11   7  25   1   1   0   0
   558.1s     0   1   0   1   0   0   0   0   1  25
   559.1s    49  18   2   0   3   1   1   0   0   1
   560.1s     0   1   1  13  23  49  36  23  22  14
   561.1s     4  16   6  15   5   8   9   9   3   4
   562.1s    10   4   1  10  14  34   2   6  23   5
   563.1s    21  19  28  39  27   5   8  11   5   4
   564.1s *  13   7   6   2   2   6  19  11  13  12
   565.1s    37  16  25  28  23   8  48   4   2   2
   566.1s     2   2   2   2   2   2   1   3   1   1
   567.1s     1   2  11  27  27  42  12  14   7  23
   568.1s    45  15  18   7  13   7  24   2   9  12
   569.1s    13  21  11   6   7  26   2  16  10   8
   570.1s *   5   2  26  24   2   3   7  22  13  16
   571.1s    27  10   1   2   1   1  16   2   3   1
   572.1s     0   2   2   1   0   1   5  41  31  36
   573.1s     2   3   2   3   1   2   4   2   2   1
   574.1s     1   3   3   3   2   1   2   4   2   1
   575.1s     2   2   2   5  28  32  10  34  17  25
   576.1s    40  13  10  14  20  19  25  30  14  22
   577.1s    37   5   9  40  16  11   5  11  23  33
   578.1s     5  23   7   9   4  15   9  17   7  29
   579.1s    15  17   2   2  19  12  14   3   2   0
   580.1s *   5   3   6   9   9   8   9   8   9  13
   581.1s     5   6  14   7  10   8  13   8  17  14
   582.1s    14   9  11  33   6   5   6   7  10  50
   583.1s    11  11   8  12  20  14  16   2  35  19
   584.1s *  24  12  16  14  30  23  21   7  18  16
   585.1s    20  25  25  11  38  18  11  19   9  30
   586.1s    41  19  23  10  20  31  23  23  54  68
   587.1s    39  40  14  14  18  64  16   3   0   2
   588.1s *  17  21  26   0   1  12   0   2   0   0
   589.1s     2  16  18  33  11   1   4   5   2   0
   590.1s     9   8   7  24  12  14   0   1   2   1
   591.1s     0   1   2  11  25   8  21  20  34  16
   592.1s     7  11   7   7  11  16   5   1  14  16
   593.1s    16   9   5  15   6   5   4  12  18   8
   594.1s     9  16  10   2  17  28  18  42  18   6
   595.1s *  17   7   5  23  19  11  11  10  11  42
   596.1s    45  20   8   6  12  14   3   3   3   2
   597.1s     2   3   2   3   4   3   2  12   9  15
   598.1s    29   6  11  28  20  31  13   3   9  54
   599.1s    32  26  19  10  10   7   5  11   4  13
   600.1s    23  55  24  30  29  20   4   2   3   3
   601.1s     4   3   1   3   3   2   2   0   1   3
   602.1s *   3   2   2   8  46   6   2   4  23  28
   603.1s    43  14   6   3   2   2   2   3   2   2
   604.1s     3   4   9  13   6   4   5   5   4   3
   605.1s     2  10  19   7  33  25  12   1   0   2
   606.1s     1   2   2   2   2   5  43  22   6  33
   607.1s    10  18   3   4  10
```

### Post-query bursts

Window per sample: keystroke to max(rendered, opened) + 50 ms. Main thread busy 19771 ms over 50 windows.

#### Named areas in the bursts

| area | ms | % |
|---|---:|---:|
| AppKitPrototypeList | 2705 | 13.7 |
| MessageRowCellView | 1311 | 6.6 |
| Row presentation / formatters | 39 | 0.2 |
| ListWindowModel | 1299 | 6.6 |
| FrameBatcher | 288 | 1.5 |
| MessageListViewController | 2550 | 12.9 |
| SearchFieldController | 2013 | 10.2 |
| SearchFieldModel | 321 | 1.6 |
| Benchmark harness (runner, tick sampler) | 1250 | 6.3 |
| -[NSTableView reloadData] | 1053 | 5.3 |
| NSTableView (any) | 5053 | 25.6 |
| NSView layout (layoutSubtreeIfNeeded) | 4516 | 22.8 |
| NSView display (displayIfNeeded / drawRect) | 793 | 4.0 |
| NSTextView insertText | 96 | 0.5 |
| NSTextCheckingController | 280 | 1.4 |
| Core Animation commit (CA::Transaction::commit) | 9499 | 48.0 |
| NSDisplayCycleFlush | 6523 | 33.0 |
| Swift concurrency job (completeTaskWithClosure) | 6978 | 35.3 |
| Rust core (sb_*) | 1 | 0.0 |

#### Heaviest main-thread call paths in the bursts

Distinct paths of the top-down call tree, each ending where no callee keeps 1% of burst time; the weight is the path's inclusive time. Leaf first, at most 30 frames (the run-loop frames nearest the root are cut when longer).

##### 1. 948 ms (4.8% of burst busy)

```
AutoreleasePoolPage::releaseUntil(objc_object**)
objc_autoreleasePoolPop
swift::runJobInEstablishedExecutorContext(swift::Job*)
swift_job_runImpl(swift::Job*, swift::SerialExecutorRef)
_dispatch_main_queue_drain.cold.6
_dispatch_main_queue_drain
_dispatch_main_queue_callback_4CF
__CFRUNLOOP_IS_SERVICING_THE_MAIN_DISPATCH_QUEUE__
__CFRunLoopRun
_CFRunLoopRunSpecificWithOptions
RunCurrentEventLoopInMode
ReceiveNextEventCommon
_BlockUntilNextEventMatchingListInMode
_DPSBlockUntilNextEventMatchingListInMode
_DPSNextEvent
-[NSApplication(NSEventRouting) _nextEventMatchingEventMask:untilDate:inMode:dequeue:]
-[NSApplication(NSEventRouting) nextEventMatchingMask:untilDate:inMode:dequeue:]
-[NSApplication run]
main [atos 0x10028175b]
start
```

##### 2. 888 ms (4.5% of burst busy)

```
specialized AppKitPrototypeList.tableView(_:viewFor:row:) [atos 0x10028bdcf]
@objc AppKitPrototypeList.tableView(_:viewFor:row:) [atos 0x100288c17]
-[NSTableView(NSTableViewViewBased) _delegate_viewForTableColumn:row:]
-[NSTableView(NSTableViewViewBased) makeViewForTableColumn:row:]
-[NSTableRowData _addViewToRowView:atColumn:row:]
-[NSTableRowData _initializeRowView:atRow:]
-[NSTableRowData _preparedRowViewForRow:storageHandler:]
-[NSTableRowData _addRowViewForVisibleRow:withPriorView:]
-[NSTableRowData _addRowViewForVisibleRow:]
-[NSTableRowData _updateVisibleRowEntries]
-[NSTableRowData updateVisibleRowViews]
-[NSTableView layout]
0x19469f47c
0x19469f4a4
0x194664178
0x194664d5c
___NSViewLayout_block_invoke
NSPerformVisuallyAtomicChange
_NSViewLayout
__36-[NSView _layoutSubtreeWithOldSize:]_block_invoke
NSPerformVisuallyAtomicChange
-[NSView _layoutSubtreeWithOldSize:]
__36-[NSView _layoutSubtreeWithOldSize:]_block_invoke
NSPerformVisuallyAtomicChange
-[NSView _layoutSubtreeWithOldSize:]
__36-[NSView _layoutSubtreeWithOldSize:]_block_invoke
NSPerformVisuallyAtomicChange
-[NSView _layoutSubtreeWithOldSize:]
__36-[NSView _layoutSubtreeWithOldSize:]_block_invoke
NSPerformVisuallyAtomicChange
... 46 more frames to the root
```

##### 3. 355 ms (1.8% of burst busy)

```
-[NSOutlineView _updateDisclosureButtonForRowView:forRow:removeIfNotAvailable:updatePosition:inDidAddRowView:]
-[NSOutlineView didAddRowView:forRow:]
-[NSTableRowData _preparedRowViewForRow:storageHandler:]
-[NSTableRowData _addRowViewForVisibleRow:withPriorView:]
-[NSTableRowData _updateVisibleViewsBasedOnUpdateItems]
-[NSTableRowData endUpdates]
-[NSOutlineView _batchExpandItemsWithItemEntries:expandChildren:]
-[NSOutlineView expandItem:expandChildren:]
SidebarViewController.reloadSections() [atos 0x100331457]
closure #1 in MainWindowController.reloadSidebar() [atos 0x100309bc7]
<deduplicated_symbol> [atos 0x100287389]
<deduplicated_symbol> [atos 0x100330fc9]
<deduplicated_symbol> [atos 0x100287389]
completeTaskWithClosure(swift::AsyncContext*, swift::SwiftError*)
```

##### 4. 330 ms (1.7% of burst busy)

```
__invoking___
-[NSInvocation invoke]
__deferNSXPCInvocationOntoMainThread_block_invoke
__deferBlockOntoMainThread_block_invoke_2
invocation function for block in wrapBlockWithVoucher(void () block_pointer)
_ZL24deferredBlockOpportunity_block_invoke_2
__CFRUNLOOP_IS_CALLING_OUT_TO_A_BLOCK__
__CFRunLoopDoBlocks
__CFRunLoopRun
_CFRunLoopRunSpecificWithOptions
RunCurrentEventLoopInMode
ReceiveNextEventCommon
_BlockUntilNextEventMatchingListInMode
_DPSBlockUntilNextEventMatchingListInMode
_DPSNextEvent
-[NSApplication(NSEventRouting) _nextEventMatchingEventMask:untilDate:inMode:dequeue:]
-[NSApplication(NSEventRouting) nextEventMatchingMask:untilDate:inMode:dequeue:]
-[NSApplication run]
main [atos 0x10028175b]
start
```

##### 5. 327 ms (1.7% of burst busy)

```
-[NSView _setWindow:]
-[NSView removeFromSuperview]
MessageListViewController.detach()
MessageListViewController.install(_:ticket:)
closure #1 in MessageListViewController.show(_:) [atos 0x10031a801]
<deduplicated_symbol> [atos 0x100287389]
<deduplicated_symbol> [atos 0x100330fc9]
<deduplicated_symbol> [atos 0x100287389]
completeTaskWithClosure(swift::AsyncContext*, swift::SwiftError*)
```

##### 6. 305 ms (1.5% of burst busy)

```
<no backtrace>
```

##### 7. 291 ms (1.5% of burst busy)

```
-[NSImage _usingBestRepresentationForRect:context:hints:body:]
-[NSImage CGImageForProposedRect:context:hints:]
-[_NSSimpleImageView updateLayer]
0x19469f47c
0x19469f4a4
0x1946659dc
0x1946665c0
_NSViewUpdateLayer
+[NSAppearance _performWithCurrentAppearance:usingBlock:]
__29-[NSViewBackingLayer display]_block_invoke
-[NSFocusStack performWithFocusView:inWindow:usingBlock:]
-[NSViewBackingLayer display]
CA::Layer::display_if_needed(CA::Transaction*)
CA::Context::commit_transaction(CA::Transaction*, double, double*)
CA::Transaction::commit()
CA::Transaction::flush_as_runloop_observer(bool)
stepTransactionFlush
UC::DriverCore::continueProcessing()
__CFRUNLOOP_IS_CALLING_OUT_TO_A_SOURCE0_PERFORM_FUNCTION__
__CFRunLoopDoSource0
__CFRunLoopDoSources0
__CFRunLoopRun
_CFRunLoopRunSpecificWithOptions
RunCurrentEventLoopInMode
ReceiveNextEventCommon
_BlockUntilNextEventMatchingListInMode
_DPSBlockUntilNextEventMatchingListInMode
_DPSNextEvent
-[NSApplication(NSEventRouting) _nextEventMatchingEventMask:untilDate:inMode:dequeue:]
-[NSApplication(NSEventRouting) nextEventMatchingMask:untilDate:inMode:dequeue:]
... 3 more frames to the root
```

##### 8. 274 ms (1.4% of burst busy)

```
__56-[NSView _layoutSubtreeIfNeededAndAllowTemporaryEngine:]_block_invoke
NSPerformVisuallyAtomicChange
-[NSView _layoutSubtreeIfNeededAndAllowTemporaryEngine:]
NSPerformVisuallyAtomicChange
-[NSView layoutSubtreeIfNeeded]
-[NSView _setDefaultKeyViewLoop]
-[NSTableRowData _updateKeyViewLoopForRowView:]
-[NSTableRowData _initializeRowView:atRow:]
-[NSTableRowData _preparedRowViewForRow:storageHandler:]
-[NSTableRowData _addRowViewForVisibleRow:withPriorView:]
-[NSTableRowData _addRowViewForVisibleRow:]
-[NSTableRowData _updateVisibleRowEntries]
-[NSTableRowData updateVisibleRowViews]
-[NSTableView layout]
0x19469f47c
0x19469f4a4
0x194664178
0x194664d5c
___NSViewLayout_block_invoke
NSPerformVisuallyAtomicChange
_NSViewLayout
__36-[NSView _layoutSubtreeWithOldSize:]_block_invoke
NSPerformVisuallyAtomicChange
-[NSView _layoutSubtreeWithOldSize:]
__36-[NSView _layoutSubtreeWithOldSize:]_block_invoke
NSPerformVisuallyAtomicChange
-[NSView _layoutSubtreeWithOldSize:]
__36-[NSView _layoutSubtreeWithOldSize:]_block_invoke
NSPerformVisuallyAtomicChange
-[NSView _layoutSubtreeWithOldSize:]
... 48 more frames to the root
```

##### 9. 273 ms (1.4% of burst busy)

```
0x1944ce9a8
0x1944becfc
0x1944c09a4
closure #1 in ViewBodyAccessor.updateBody(of:changed:)
ViewBodyAccessor.updateBody(of:changed:)
protocol witness for BodyAccessor.updateBody(of:changed:) in conformance ViewBodyAccessor<A>
closure #1 in StaticBody.updateValue()
StaticBody.updateValue()
partial apply for implicit closure #1 in closure #1 in closure #1 in Attribute.init<A>(_:)
AG::Graph::UpdateStack::update()
AG::Graph::update_attribute(AG::data::ptr<AG::Node>, unsigned int)
AG::Graph::input_value_ref_slow(AG::data::ptr<AG::Node>, AG::AttributeID, unsigned int, unsigned int, AGSwiftMetadata const*, unsigned char&, long)
AGGraphGetValue
DynamicViewList.updateValue()
partial apply for implicit closure #1 in closure #1 in closure #1 in Attribute.init<A>(_:)
AG::Graph::UpdateStack::update()
AG::Graph::update_attribute(AG::data::ptr<AG::Node>, unsigned int)
AG::Graph::input_value_ref_slow(AG::data::ptr<AG::Node>, AG::AttributeID, unsigned int, unsigned int, AGSwiftMetadata const*, unsigned char&, long)
AGGraphGetValue
specialized implicit closure #1 in closure #1 in closure #1 in Attribute.init<A>(_:)
AG::Graph::UpdateStack::update()
AG::Graph::update_attribute(AG::data::ptr<AG::Node>, unsigned int)
AG::Graph::input_value_ref_slow(AG::data::ptr<AG::Node>, AG::AttributeID, unsigned int, unsigned int, AGSwiftMetadata const*, unsigned char&, long)
AGGraphGetValue
specialized DynamicContainerInfo.updateItems(disableTransitions:)
specialized DynamicContainerInfo.updateValue()
specialized implicit closure #1 in closure #1 in closure #1 in Attribute.init<A>(_:)
AG::Graph::UpdateStack::update()
AG::Graph::update_attribute(AG::data::ptr<AG::Node>, unsigned int)
AG::Graph::input_value_ref_slow(AG::data::ptr<AG::Node>, AG::AttributeID, unsigned int, unsigned int, AGSwiftMetadata const*, unsigned char&, long)
... 57 more frames to the root
```

##### 10. 262 ms (1.3% of burst busy)

```
-[NSTextView(NSSharing) didChangeText]
-[NSTextView _insertText:replacementRange:]
closure #6 in ListBenchmarkRunner.runSearch(controller:)
closure #1 in AppDelegate.startBenchmark(mode:reportPath:)
<deduplicated_symbol> [atos 0x100286e55]
<deduplicated_symbol> [atos 0x100330fc9]
<deduplicated_symbol> [atos 0x100287389]
completeTaskWithClosure(swift::AsyncContext*, swift::SwiftError*)
```

##### 11. 255 ms (1.3% of burst busy)

```
specialized implicit closure #1 in closure #1 in closure #1 in Attribute.init<A>(_:)
AG::Graph::UpdateStack::update()
AG::Subgraph::update(unsigned int)
specialized GraphHost.runTransaction(_:do:id:)
ViewGraph.updateOutputs(at:)
closure #1 in ViewGraphRootValueUpdater.render(interval:updateDisplayList:targetTimestamp:)
ViewGraphRootValueUpdater.render(interval:updateDisplayList:targetTimestamp:)
0x1945680b8
0x19472aa78
0x19472aaa0
+[NSAnimationContext runAnimationGroup:]
0x1945654a8
0x1945585ec
0x194558640
0x19469f47c
0x19469f4a4
0x194664178
0x194664d5c
___NSViewLayout_block_invoke
NSPerformVisuallyAtomicChange
_NSViewLayout
__36-[NSView _layoutSubtreeWithOldSize:]_block_invoke
NSPerformVisuallyAtomicChange
-[NSView _layoutSubtreeWithOldSize:]
__36-[NSView _layoutSubtreeWithOldSize:]_block_invoke
NSPerformVisuallyAtomicChange
-[NSView _layoutSubtreeWithOldSize:]
__36-[NSView _layoutSubtreeWithOldSize:]_block_invoke
NSPerformVisuallyAtomicChange
-[NSView _layoutSubtreeWithOldSize:]
... 54 more frames to the root
```

##### 12. 251 ms (1.3% of burst busy)

```
__57-[NSTableRowData reloadViewsForRowIndexes:columnIndexes:]_block_invoke
__75-[NSTableRowData enumerateAvailableRowViewsIncludingOrphanRows:usingBlock:]_block_invoke
-[NSTableRowData enumerateAvailableRowViewsIncludingOrphanRows:usingBlock:]
-[NSTableRowData reloadViewsForRowIndexes:columnIndexes:]
-[NSTableView reloadDataForRowIndexes:columnIndexes:]
AppKitPrototypeList.pagesDidLand(_:)
closure #1 in AppKitPrototypeList.init(model:formatter:accountMarker:selectionLoss:onIndexChange:)
ListWindowModel.deliver(_:) [atos 0x1016e2e8b]
closure #1 in ListWindowModel.queue(_:)
specialized FrameBatcher.enqueue(_:) [atos 0x1016d5da7]
specialized closure #1 in FrameBatcher.post(_:) [atos 0x1016e3ff7]
<deduplicated_symbol>
<deduplicated_symbol>
<deduplicated_symbol>
completeTaskWithClosure(swift::AsyncContext*, swift::SwiftError*)
```

### CPU time by thread over the search phase (top 12)

| ms | thread |
|---:|---|
| 51656 | Main Thread (0x401eb8b) (Sweetbriar, pid: 61206) |
| 9024 | tokio-rt-worker (0x402012f) (Sweetbriar, pid: 61206) |
| 8500 | tokio-rt-worker (0x402098d) (Sweetbriar, pid: 61206) |
| 8119 | tokio-rt-worker (0x4020e00) (Sweetbriar, pid: 61206) |
| 7077 | tokio-rt-worker (0x402012c) (Sweetbriar, pid: 61206) |
| 6993 | tokio-rt-worker (0x4020e01) (Sweetbriar, pid: 61206) |
| 5462 | tokio-rt-worker (0x40218b5) (Sweetbriar, pid: 61206) |
| 2164 | tokio-rt-worker (0x402200d) (Sweetbriar, pid: 61206) |
| 732 | Sweetbriar (0x4021899) (Sweetbriar, pid: 61206) |
| 715 | Sweetbriar (0x4021898) (Sweetbriar, pid: 61206) |
| 663 | Sweetbriar (0x40220bf) (Sweetbriar, pid: 61206) |
| 655 | Sweetbriar (0x4021e59) (Sweetbriar, pid: 61206) |

### Top 40 Sweetbriar frames on the tokio workers (inclusive, search phase)

For context: the core's work off the main thread (47598 ms on all workers).

| ms | % of worker time | frame |
|---:|---:|---|
| 47591 | 100.0 | `<std::sys::thread::unix::Thread>::new::thread_start [atos 0x101dacc07]` |
| 47590 | 100.0 | `<std::thread::lifecycle::spawn_unchecked<<tokio::runtime::blocking::pool::Spawner>::spawn_thread::{closure#0}, ()>::{closure#1} as core::ops::function::FnOnc...` |
| 47546 | 99.9 | `std::sys::backtrace::__rust_begin_short_backtrace::<<tokio::runtime::blocking::pool::Spawner>::spawn_thread::{closure#0}, ()> [atos 0x101d63027]` |
| 45705 | 96.0 | `sqlite3_step` |
| 34736 | 73.0 | `getPageNormal` |
| 34641 | 72.8 | `getAndInitPage [atos 0x101c28e67]` |
| 30575 | 64.2 | `moveToChild` |
| 26391 | 55.4 | `readDbPage [atos 0x101c0030f]` |
| 26343 | 55.3 | `unixRead [atos 0x101bf211f]` |
| 20976 | 44.1 | `<tokio::runtime::task::harness::Harness<tokio::runtime::blocking::task::BlockingTask<<sb_engine::engine::Engine>::read_through<(sb_store::query::counts::cove...` |
| 20949 | 44.0 | `<tokio::runtime::task::core::Core<tokio::runtime::blocking::task::BlockingTask<<sb_engine::engine::Engine>::read_through<(sb_store::query::counts::coverage::...` |
| 20949 | 44.0 | `<tokio::runtime::blocking::task::BlockingTask<<sb_engine::engine::Engine>::read_through<(sb_store::query::counts::coverage::StoredSearchCoverage, sb_store::l...` |
| 20938 | 44.0 | `<sb_store::readers::Readers>::read::<(sb_store::query::counts::coverage::StoredSearchCoverage, sb_store::lists::PageRead<sb_store::query::counts::paging::Sea...` |
| 20706 | 43.5 | `sb_store::query::counts::paging::read_search_relevance_page` |
| 17004 | 35.7 | `<rusqlite::row::Rows as fallible_streaming_iterator::FallibleStreamingIterator>::advance [atos 0x101aeb25f]` |
| 16035 | 33.7 | `sqlite3BtreeTableMoveto` |
| 15666 | 32.9 | `sqlite3VdbeExec [atos 0x101c12a17]` |
| 14648 | 30.8 | `sqlite3VdbeExec [atos 0x101c1374b]` |
| 13702 | 28.8 | `sqlite3BtreeNext` |
| 12746 | 26.8 | `<rusqlite::row::Rows>::get_expected_row [atos 0x101a85767]` |
| 11237 | 23.6 | `<tokio::runtime::task::harness::Harness<tokio::runtime::blocking::task::BlockingTask<<sb_engine::engine::Engine>::read_through<core::option::Option<sb_store:...` |
| 11234 | 23.6 | `<rusqlite::row::Rows as fallible_streaming_iterator::FallibleStreamingIterator>::advance [atos 0x101af7ed7]` |
| 11228 | 23.6 | `<tokio::runtime::task::core::Core<tokio::runtime::blocking::task::BlockingTask<<sb_engine::engine::Engine>::read_through<core::option::Option<sb_store::sideb...` |
| 11227 | 23.6 | `<tokio::runtime::blocking::task::BlockingTask<<sb_engine::engine::Engine>::read_through<core::option::Option<sb_store::sidebar::MailboxList>, <sb_engine::eng...` |
| 11219 | 23.6 | `<sb_store::readers::Readers>::read::<core::option::Option<sb_store::sidebar::MailboxList>, <sb_engine::engine::Engine>::mailboxes::{closure#0}::{closure#0}::...` |
| 11182 | 23.5 | `sb_store::sidebar::mailboxes [atos 0x101af2bdb]` |
| 9764 | 20.5 | `<tokio::runtime::task::harness::Harness<tokio::runtime::blocking::task::BlockingTask<<sb_engine::engine::Engine>::read_through<(sb_store::query::counts::cove...` |
| 9758 | 20.5 | `<tokio::runtime::task::core::Core<tokio::runtime::blocking::task::BlockingTask<<sb_engine::engine::Engine>::read_through<(sb_store::query::counts::coverage::...` |
| 9750 | 20.5 | `<tokio::runtime::blocking::task::BlockingTask<<sb_engine::engine::Engine>::read_through<(sb_store::query::counts::coverage::StoredSearchCoverage, sb_store::l...` |
| 9602 | 20.2 | `<sb_store::readers::Readers>::read::<(sb_store::query::counts::coverage::StoredSearchCoverage, sb_store::lists::PageRead<(u64, alloc::vec::Vec<i64>)>), <sb_e...` |
| 8771 | 18.4 | `sb_store::query::counts::paging::search_ids [atos 0x101ac1967]` |
| 8771 | 18.4 | `<sb_store::query::counts::Compiled>::read::<alloc::vec::Vec<i64>, sb_store::query::counts::paging::search_ids::{closure#1}>` |
| 8721 | 18.3 | `<core::iter::adapters::GenericShunt<rusqlite::row::MappedRows<sb_store::query::counts::paging::search_ids::{closure#1}::{closure#0}>, core::result::Result<co...` |
| 8694 | 18.3 | `<alloc::vec::Vec<i64> as alloc::vec::spec_from_iter::SpecFromIter<i64, core::iter::adapters::GenericShunt<rusqlite::row::MappedRows<sb_store::query::counts::...` |
| 8035 | 16.9 | `pcache1Fetch` |
| 6569 | 13.8 | `sb_store::query::counts::paging::read_page [atos 0x101ac4517]` |
| 6512 | 13.7 | `<sb_store::query::counts::Compiled>::read::<i64, sb_store::query::counts::paging::read_page::{closure#7}> [atos 0x101aa236f]` |
| 6493 | 13.6 | `sqlite3BtreeIndexMoveto` |
| 6488 | 13.6 | `<rusqlite::statement::Statement>::query_row::<i64, rusqlite::params::ParamsFromIter<&[rusqlite::types::value::Value]>, sb_store::query::counts::paging::read_...` |
| 6292 | 13.2 | `sqlite3VdbeExec [atos 0x101c1233f]` |

## Analysis script

Run as `python3 analyze_profile.py sb-time-profile.xml search-benchmark-profiled.json --demangler <rust-demangle filter> --dsyms build/DerivedData/Build/Products/Release`.

```python
#!/usr/bin/env python3
"""Analyse an `xctrace export` of a Time Profiler run of Sweetbriar's
search benchmark (-runSearchBenchmark).

Usage: analyze_profile.py <time-profile.xml> <report.json>
                          [--demangler <rust demangler>] [--dsyms <dir>]

The export reuses repeated nodes through id/ref back-references; every
node with an id is resolved once and cached, and a ref always points
backwards. Frames inside a backtrace are leaf first. Time Profiler
records running threads only, so time a thread spends blocked or
descheduled leaves no samples.

Frames xctrace leaves as bare addresses inside Sweetbriar's own binaries
(mostly the linker's deduplicated symbols) are resolved with atos
against the dSYMs in --dsyms, using each binary's load address from the
export. Rust v0 symbols go through --demangler (a filter that reads one
symbol per line, e.g. one built on the rustc-demangle crate).

The search phase is located from the stacks. Each sample resets the
field (reset #1 in runSearch), types the query's prefix, settles, then
types the last character inside ListBenchmarkRunner.postInput(_:),
whose `post` closure is "closure #6 in runSearch". That closure runs
only for the timed keystroke and is sampled every time, so each cluster
of it marks one timed keystroke. The report's samples (round-major,
query order) give each keystroke's render and open times; the burst
that follows a query is [keystroke, keystroke + max(rendered, opened)
+ 50 ms].
"""

import bisect
import collections
import json
import os
import subprocess
import sys
import xml.etree.ElementTree as ET

NS_PER_MS = 1_000_000
BUCKET_MS = 100
BURST_TAIL_MS = 50
TOP_FRAMES = 40
TOP_PATHS = 12
TOP_WORKER_FRAMES = 40
PATH_DEPTH = 30
PATH_THRESHOLD = 0.01  # a heavy path ends where no child keeps 1% of burst time
ALIGNED_BUCKETS = 40  # 4 s after each timed keystroke
KEYSTROKE_GAP_MS = 1000

FINAL_MARKS = ("ListBenchmarkRunner.postInput(_:)",
               "closure #6 in ListBenchmarkRunner.runSearch(controller:)")
PHASE_MARKS = ("reset #1 (scope:) in ListBenchmarkRunner.runSearch",
               "settle #1 (after:replacing:) in ListBenchmarkRunner.runSearch")
RUN_SEARCH = "ListBenchmarkRunner.runSearch(controller:)"
OWN_BINARIES = ("Sweetbriar", "SweetbriarBridge")

# Named areas: inclusive main-thread time of any frame matching a key.
AREAS = [
    ("AppKitPrototypeList", ("AppKitPrototypeList",)),
    ("MessageRowCellView", ("MessageRowCellView",)),
    ("Row presentation / formatters", ("MessageRowPresentation", "RowPresentation",
                                        "DateFormatter", "Formatter.string", "formatted(")),
    ("ListWindowModel", ("ListWindowModel",)),
    ("FrameBatcher", ("FrameBatcher",)),
    ("MessageListViewController", ("MessageListViewController",)),
    ("SearchFieldController", ("SearchFieldController",)),
    ("SearchFieldModel", ("SearchFieldModel",)),
    ("Benchmark harness (runner, tick sampler)", ("ListBenchmarkRunner", "BenchmarkTickSampler")),
    ("-[NSTableView reloadData]", ("-[NSTableView reloadData]",)),
    ("NSTableView (any)", ("NSTableView", "NSTableRowData")),
    ("NSView layout (layoutSubtreeIfNeeded)", ("-[NSView layoutSubtreeIfNeeded]",)),
    ("NSView display (displayIfNeeded / drawRect)", ("displayIfNeeded", "drawRect:", "_drawRect:")),
    ("NSTextView insertText", ("-[NSTextView insertText:replacementRange:]",)),
    ("NSTextCheckingController", ("NSTextCheckingController",)),
    ("Core Animation commit (CA::Transaction::commit)", ("CA::Transaction::commit()",)),
    ("NSDisplayCycleFlush", ("NSDisplayCycleFlush",)),
    ("Swift concurrency job (completeTaskWithClosure)", ("completeTaskWithClosure",)),
    ("Rust core (sb_*)", ("sb_engine::", "sb_store::", "sb_ffi::", "sb_search", "sb_mime::")),
]


def load(path):
    """Rows plus, per frame name, the binary it lives in, and each
    binary's load address and path."""
    cache = {}
    frame_binary = {}
    binaries = {}

    def resolve(el):
        ref = el.get("ref")
        if ref is not None:
            return cache[ref]
        tag = el.tag
        if tag == "backtrace":
            value = tuple(resolve(f) for f in el if f.tag == "frame")
        elif tag == "tagged-backtrace":
            bt = el.find("backtrace")
            value = resolve(bt) if bt is not None else ()
        elif tag == "frame":
            value = el.get("name") or el.get("addr") or "?"
            b = el.find("binary")
            if b is not None:
                frame_binary.setdefault(value, resolve(b))
        elif tag == "binary":
            value = el.get("name") or "?"
            if el.get("load-addr"):
                binaries.setdefault(value, (int(el.get("load-addr"), 16), el.get("path")))
        elif tag in ("sample-time", "weight"):
            value = int(el.text)
        else:
            value = el.get("fmt") or (el.text or "")
            for child in el:
                resolve(child)  # cache nested ids (tid, pid, ...)
        if el.get("id") is not None:
            cache[el.get("id")] = value
        return value

    rows = []
    for _, el in ET.iterparse(path, events=("end",)):
        if el.tag != "row":
            continue
        rec = {"stack": ()}
        for child in el:
            v = resolve(child)
            if child.tag == "sample-time":
                rec["t"] = v
            elif child.tag == "thread":
                rec["thread"] = v
            elif child.tag == "weight":
                rec["w"] = v
            elif child.tag in ("tagged-backtrace", "backtrace"):
                rec["stack"] = v
        rows.append(rec)
        el.clear()
    return rows, frame_binary, binaries


def demangle(names, tool):
    if not tool:
        return {n: n for n in names}
    names = sorted(names)
    out = subprocess.run([tool], input="\n".join(names) + "\n",
                         capture_output=True, text=True, check=True).stdout
    return dict(zip(names, out.splitlines()))


def atos(names, frame_binary, binaries, dsyms):
    out = {}
    if not dsyms:
        return out
    for binary in OWN_BINARIES:
        if binary not in binaries:
            continue
        base, _ = binaries[binary]
        bundle = "app" if binary == "Sweetbriar" else "framework"
        dwarf = os.path.join(dsyms, f"{binary}.{bundle}.dSYM", "Contents", "Resources", "DWARF", binary)
        addrs = [n for n in names if n.startswith("0x") and frame_binary.get(n) == binary]
        if not addrs or not os.path.exists(dwarf):
            continue
        res = subprocess.run(["atos", "-o", dwarf, "-arch", "arm64", "-l", hex(base)] + addrs,
                             capture_output=True, text=True, check=True).stdout.splitlines()
        for a, line in zip(addrs, res):
            if not line.startswith("0x"):
                out[a] = line.split(" (in ")[0]
    return out


def is_final(stack):
    return any(m in stack for m in FINAL_MARKS)


def clusters(times, gap_ns):
    out = []
    for t in sorted(times):
        if out and t - out[-1][-1] <= gap_ns:
            out[-1].append(t)
        else:
            out.append([t])
    return [(c[0], c[-1]) for c in out]


def ms(ns):
    return ns / NS_PER_MS


def cell(text, width=160):
    text = text.replace("|", "\\|")
    return text if len(text) <= width else text[:width - 3] + "..."


def heavy_paths(stacks, total):
    """Frontier nodes of the top-down call tree: nodes holding at least
    PATH_THRESHOLD of `total` none of whose children do. Each is one
    distinct heavy path, with its inclusive weight."""
    root = {}
    for stack, w in stacks:
        node = root
        for f in reversed(stack):
            entry = node.setdefault(f, [0, {}])
            entry[0] += w
            node = entry[1]
    limit = PATH_THRESHOLD * total
    found = []

    def walk(node, path):
        for f, (w, kids) in node.items():
            if w < limit:
                continue
            here = path + [f]
            if any(kw >= limit for kw, _ in kids.values()):
                walk(kids, here)
            else:
                found.append((w, here))
    walk(root, [])
    found.sort(key=lambda x: -x[0])
    return found


def main():
    args = sys.argv[1:]
    opts = {}
    for flag in ("--demangler", "--dsyms"):
        if flag in args:
            i = args.index(flag)
            opts[flag] = args[i + 1]
            del args[i:i + 2]
    xml_path, report_path = args
    rows, frame_binary, binaries = load(xml_path)
    report = json.load(open(report_path))
    samples = report["search"]["samples"]

    main_rows = sorted((r for r in rows if r.get("thread", "").startswith("Main Thread")),
                       key=lambda r: r["t"])
    times = [r["t"] for r in main_rows]

    finals = clusters([r["t"] for r in main_rows if is_final(r["stack"])],
                      KEYSTROKE_GAP_MS * NS_PER_MS)
    phase_marks = [r["t"] for r in main_rows
                   if any(m in f for f in r["stack"] for m in PHASE_MARKS)]
    search_rows = [r["t"] for r in main_rows if RUN_SEARCH in r["stack"]]

    print("# Main-thread profile: search phase\n")
    print(f"Samples: {len(rows)} total, {len(main_rows)} on the main thread "
          f"(1 ms sampling, running threads only).")
    print(f"Timed keystrokes found: {len(finals)}; report samples: {len(samples)}.\n")
    if len(finals) != len(samples):
        print("**The keystroke count doesn't match the report: the per-sample "
              "alignment below pairs them in order and may be off.**\n")
    if not finals:
        print("No timed keystrokes found: the search phase can't be located.")
        return

    # The phase opens with the first sample's reset, before its prefix.
    starts = [t for t in phase_marks if t < finals[0][0]]
    phase_start = starts[0] if starts else finals[0][0] - 5000 * NS_PER_MS
    phase_end = max(search_rows) if search_rows else finals[-1][1]
    phase = [r for r in main_rows if phase_start <= r["t"] <= phase_end]
    busy = sum(r["w"] for r in phase)
    wall = phase_end - phase_start
    print(f"Search phase: {ms(phase_start)/1000:.3f} s to {ms(phase_end)/1000:.3f} s of the trace "
          f"({ms(wall)/1000:.1f} s wall); main thread busy {ms(busy):.0f} ms "
          f"({100*busy/wall:.1f}% of wall).\n")

    names = {f for r in rows if phase_start <= r["t"] <= phase_end for f in r["stack"]}
    # atos first (it can return mangled Rust names), then demangle.
    raw_sym = {n: n for n in names}
    resolved = atos(names, frame_binary, binaries, opts.get("--dsyms"))
    raw_sym.update(resolved)
    pretty = demangle(set(raw_sym.values()), opts.get("--demangler"))
    nice = {n: pretty[raw_sym[n]] + (f" [atos {n}]" if n in resolved else "") for n in names}

    def own(raw):
        return frame_binary.get(raw) in OWN_BINARIES

    self_t, incl_t, own_self, own_incl = (collections.Counter() for _ in range(4))
    for r in phase:
        if not r["stack"]:
            self_t["<no backtrace>"] += r["w"]
            continue
        self_t[nice[r["stack"][0]]] += r["w"]
        for f in set(r["stack"]):
            incl_t[nice[f]] += r["w"]
            if own(f):
                own_incl[nice[f]] += r["w"]
        leaf_own = next((f for f in r["stack"] if own(f)), None)
        if leaf_own:
            own_self[nice[leaf_own]] += r["w"]

    def table(title, counter, denom, note=""):
        print(f"## {title}\n")
        if note:
            print(note + "\n")
        print("| # | ms | % of main busy | frame |\n|---:|---:|---:|---|")
        for i, (f, w) in enumerate(counter.most_common(TOP_FRAMES), 1):
            print(f"| {i} | {ms(w):.0f} | {100*w/denom:.1f} | `{cell(f)}` |")
        print()

    table(f"Top {TOP_FRAMES} frames by self time (main thread, search phase)", self_t, busy)
    table(f"Top {TOP_FRAMES} frames by inclusive time (main thread, search phase)", incl_t, busy)
    table(f"Top {TOP_FRAMES} Sweetbriar frames by inclusive time (main thread, search phase)",
          own_incl, busy, "Frames from the Sweetbriar and SweetbriarBridge binaries "
          "(the Rust core is linked into SweetbriarBridge).")
    table(f"Top {TOP_FRAMES} Sweetbriar frames by attributed self time (main thread, search phase)",
          own_self, busy, "Each sample is charged to its leaf-most Sweetbriar frame, so system "
          "work a Sweetbriar call causes counts against that call.")

    def areas(rows_in, denom):
        print("| area | ms | % |\n|---|---:|---:|")
        for label, keys in AREAS:
            w = sum(r["w"] for r in rows_in
                    if any(k in nice.get(f, f) for f in r["stack"] for k in keys))
            print(f"| {label} | {ms(w):.0f} | {100*w/max(denom, 1):.1f} |")
        print()

    print("## Named areas, main thread, search phase\n")
    print("Inclusive: a sample counts for every area with a matching frame on its stack.\n")
    areas(phase, busy)

    # The timed keystroke's own synchronous work.
    print("## The timed keystroke itself\n")
    print("Main-thread samples inside the timed keystroke's `post` closure "
          "(`-[NSTextView insertText:]` of the last character, synchronous). "
          "`span` runs from its first to its last sample; on-CPU time below "
          "`span` means the main thread was off CPU inside the call.\n")
    print("| sample | rendered ms | span ms | on-CPU ms |\n|---|---:|---:|---:|")
    for (t0, t1), s in zip(finals, samples):
        inside = sum(r["w"] for r in main_rows if t0 <= r["t"] <= t1 and is_final(r["stack"]))
        rendered = s.get("milliseconds")
        rtxt = f"{rendered:.0f}" if rendered is not None else "n/a"
        print(f"| {s['query']} r{s['round']} | {rtxt} | {ms(t1 - t0):.0f} | {ms(inside):.0f} |")
    print()

    bucket_ns = BUCKET_MS * NS_PER_MS
    pairs = list(zip(finals, samples))
    print(f"## Main-thread busy ms per {BUCKET_MS} ms bucket after each timed keystroke\n")
    print(f"Column k is [k*{BUCKET_MS}, (k+1)*{BUCKET_MS}) ms after the keystroke; "
          f"`rendered` is the report's keystroke-to-rows time for that sample.\n")
    print("```")
    print("sample                    rendered " + " ".join(f"{k:>3}" for k in range(ALIGNED_BUCKETS)))
    mean = [0.0] * ALIGNED_BUCKETS
    for (t0, _), s in pairs:
        lo = bisect.bisect_left(times, t0)
        hi = bisect.bisect_left(times, t0 + ALIGNED_BUCKETS * bucket_ns)
        b = [0.0] * ALIGNED_BUCKETS
        for r in main_rows[lo:hi]:
            b[(r["t"] - t0) // bucket_ns] += ms(r["w"])
        for k in range(ALIGNED_BUCKETS):
            mean[k] += b[k] / len(pairs)
        rendered = s.get("milliseconds")
        rtxt = f"{rendered:8.0f}" if rendered is not None else "     n/a"
        print(f"{s['query'] + ' r' + str(s['round']):<25} {rtxt} " + " ".join(f"{v:>3.0f}" for v in b))
    print(f"{'mean':<25} {'':>8} " + " ".join(f"{v:>3.0f}" for v in mean))
    print("```\n")

    print(f"## Main-thread busy ms per {BUCKET_MS} ms bucket, whole search phase\n")
    print("One line per second of trace time, ten buckets per line; "
          "`*` marks a second holding a timed keystroke.\n")
    print("```")
    nb = int(wall // bucket_ns) + 1
    series = [0.0] * nb
    for r in phase:
        series[(r["t"] - phase_start) // bucket_ns] += ms(r["w"])
    marks = {int((t0 - phase_start) // NS_PER_MS // 1000) for t0, _ in finals}
    for sec in range(0, nb, 10):
        star = "*" if sec // 10 in marks else " "
        print(f"{ms(phase_start)/1000 + sec/10:8.1f}s {star} "
              + " ".join(f"{v:>3.0f}" for v in series[sec:sec + 10]))
    print("```\n")

    # The bursts that follow each query.
    burst_rows = []
    for (t0, _), s in pairs:
        span = max(s.get("milliseconds") or 0, s.get("openedMilliseconds") or 0) + BURST_TAIL_MS
        lo = bisect.bisect_left(times, t0)
        hi = bisect.bisect_right(times, t0 + span * NS_PER_MS)
        burst_rows.extend(main_rows[lo:hi])
    burst_total = sum(r["w"] for r in burst_rows)
    print("## Post-query bursts\n")
    print(f"Window per sample: keystroke to max(rendered, opened) + {BURST_TAIL_MS} ms. "
          f"Main thread busy {ms(burst_total):.0f} ms over {len(pairs)} windows.\n")
    print("### Named areas in the bursts\n")
    areas(burst_rows, burst_total)

    print("### Heaviest main-thread call paths in the bursts\n")
    print(f"Distinct paths of the top-down call tree, each ending where no callee keeps "
          f"{PATH_THRESHOLD:.0%} of burst time; the weight is the path's inclusive time. "
          f"Leaf first, at most {PATH_DEPTH} frames (the run-loop frames nearest the root "
          f"are cut when longer).\n")
    paths = heavy_paths([(tuple(nice.get(f, f) for f in r["stack"]) or ("<no backtrace>",), r["w"])
                         for r in burst_rows], burst_total)
    for i, (w, path) in enumerate(paths[:TOP_PATHS], 1):
        print(f"#### {i}. {ms(w):.0f} ms ({100*w/burst_total:.1f}% of burst busy)\n")
        print("```")
        shown = list(reversed(path))[:PATH_DEPTH]
        for f in shown:
            print(f if len(f) <= 200 else f[:197] + "...")
        if len(path) > PATH_DEPTH:
            print(f"... {len(path) - PATH_DEPTH} more frames to the root")
        print("```\n")

    # Other threads in the phase, for context.
    thr = collections.Counter()
    workers = collections.Counter()
    worker_total = 0
    for r in rows:
        if not (phase_start <= r["t"] <= phase_end):
            continue
        thr[r.get("thread", "?")] += r["w"]
        if "tokio-rt-worker" in r.get("thread", ""):
            worker_total += r["w"]
            for f in {nice.get(f, f) for f in r["stack"] if own(f)}:
                workers[f] += r["w"]
    print("## CPU time by thread over the search phase (top 12)\n")
    print("| ms | thread |\n|---:|---|")
    for t, w in thr.most_common(12):
        print(f"| {ms(w):.0f} | {t} |")
    print()
    print(f"## Top {TOP_WORKER_FRAMES} Sweetbriar frames on the tokio workers (inclusive, search phase)\n")
    print(f"For context: the core's work off the main thread ({ms(worker_total):.0f} ms on all workers).\n")
    print("| ms | % of worker time | frame |\n|---:|---:|---|")
    for f, w in workers.most_common(TOP_WORKER_FRAMES):
        print(f"| {ms(w):.0f} | {100*w/max(worker_total, 1):.1f} | `{cell(f)}` |")


if __name__ == "__main__":
    main()
```

## Rust demangle filter

`Cargo.toml` depends on `rustc-demangle = "=0.1.28"`; `src/main.rs`:

```rust
use std::io::{self, BufRead, Write};
fn main() {
    let stdin = io::stdin();
    let mut out = io::BufWriter::new(io::stdout());
    for line in stdin.lock().lines() {
        let line = line.unwrap();
        writeln!(out, "{:#}", rustc_demangle::demangle(&line)).unwrap();
    }
}
```
