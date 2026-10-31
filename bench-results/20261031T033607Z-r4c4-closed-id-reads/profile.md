# Time Profiler run: 9f21847, r4c4-closed-id-reads

Built commit `9f21847ee3566910323e3df372d9bb226df27952` ("Stop
closed-window ID scans inside SQLite"), Release, Mac mini M1
(Macmini9,1, 8 GiB), macOS 26.5.2, 60 Hz, 100,000-message synthetic
archive. Recorded 2026-10-31 with `xcrun xctrace record --template
'Time Profiler' --launch` over `-runSearchBenchmark`; the report is
`profiled.json` (`ok: true`, 50 of 50 samples).

The profiler distorts the timings: this run's p95 is 4,548 ms against
553 and 617 ms in the timed runs. Read the numbers below for where time
goes, not for how long a search takes.

## How this was produced

The method is the same as for
`bench-results/20261029T212655Z-30888e0-main-profile-profile.md`, and
that file has the script and the demangler source. The benchmark runner
is unchanged since `30888e0`. 50 timed keystrokes were found, matching
the report's 50 samples.

- `xctrace` again reported "The log archive is corrupt or incomplete"
  and exited 2. It saved the trace, but without signposts.
- The trace was re-symbolicated against the build's dSYMs. The
  remaining Sweetbriar addresses were resolved with `atos` (tagged
  `[atos 0x…]`) and Rust symbols demangled.
- Time Profiler samples running threads only, at 1 ms.
- Sweetbriar was brought to the front about 5 s after `xctrace` launched
  it, and the run didn't lose the foreground. The stopped copy that
  `xctrace --launch` leaves behind was killed after the recording.
- There's no profile of the baseline (`2f961b3`, request
  `20261031T033607Z-r4c4-baseline`, which asked for none). The
  comparisons below are with the `8086cb5` profile
  (`bench-results/20261030T221813Z-r4c3-certain-unknowns/profile.md`),
  whose change `2f961b3` carries.

## Findings

1. **Less work on the workers.** They used 46.7 s of CPU over the
   search phase, against 54.3 s in the `8086cb5` profile.
2. **The window id reads moved.** `sb_store::lists::list_ids`
   (`Engine::window_ids`) took 19.6% of worker time at `8086cb5`. It
   no longer appears in this analysis at all. The search's own id
   reads now run through `Readers::read_scoped` and
   `Engine::search_snapshot` / `search_window_ids`:
   - `paging::search_ids` 30.5%;
   - `paging::read_search_relevance_page` 57.3%;
   - `paging::fetch` 29.5%;
   - `paging::stream` 16.1%;
   - `counts::drive::restrict` 11.0%.
3. **Page reads still come from SQLite's page cache.**
   `pcache1Fetch` holds 53.5% of worker time, and `unixRead` doesn't
   appear in the analysis.
4. **The main thread is on CPU for 7.8% of the search phase** (37.3 s
   of 480.5 s), and the Rust core never runs on it. The split matches
   `8086cb5`:
   - Core Animation commit 49.3%, with layout inside it (`NSView`
     layout 21.2%, `NSTableView` 18.8%);
   - `MessageListViewController` 14.2%;
   - `AppKitPrototypeList` 12.8% (`MessageRowCellView` 7.4%);
   - `SearchFieldController` 8.4%;
   - `ListWindowModel` 5.3%; `FrameBatcher` 0.9%; row formatters 0.2%.
5. **The timed keystroke's `insertText`** is on CPU for 14 ms on
   average. Its first and last samples are at most 779 ms apart.

---

## Generated analysis: main thread, search phase

Samples: 103010 total, 44042 on the main thread (1 ms sampling, running threads only).
Timed keystrokes found: 50; report samples: 50.

Search phase: 53.629 s to 534.085 s of the trace (480.5 s wall); main thread busy 37299 ms (7.8% of wall).

### Top 40 frames by self time (main thread, search phase)

| # | ms | % of main busy | frame |
|---:|---:|---:|---|
| 1 | 2119 | 5.7 | `objc_msgSend` |
| 2 | 1274 | 3.4 | `getMethodNoSuper_nolock(objc_class*, objc_selector*)` |
| 3 | 738 | 2.0 | `<deduplicated_symbol>` |
| 4 | 556 | 1.5 | `_xzm_xzone_malloc_tiny` |
| 5 | 540 | 1.4 | `_platform_strcmp$VARIANT$Base` |
| 6 | 503 | 1.3 | `__CFStringHash` |
| 7 | 453 | 1.2 | `objc_retain` |
| 8 | 395 | 1.1 | `protocol_conformsToProtocol_nolock(protocol_t*, protocol_t*)` |
| 9 | 395 | 1.1 | `mach_msg2_trap` |
| 10 | 300 | 0.8 | `__kdebug_trace64` |
| 11 | 293 | 0.8 | `CFEqual` |
| 12 | 279 | 0.7 | `_class_conformsToProtocol_unrealized_nolock(objc_class*, Protocol*)` |
| 13 | 267 | 0.7 | `list_array_tt<unsigned long, protocol_list_t, RawPtr>::iteratorImpl<false>::skipEmptyLists()` |
| 14 | 257 | 0.7 | `objc_class::demangledName(bool)` |
| 15 | 254 | 0.7 | `__CF_IS_OBJC` |
| 16 | 244 | 0.7 | `_platform_memcmp` |
| 17 | 208 | 0.6 | `objc_release` |
| 18 | 199 | 0.5 | `weak_entry_for_referent` |
| 19 | 188 | 0.5 | `__CFSearchStringROM` |
| 20 | 184 | 0.5 | `objc::StringHashTable::tryGetIndex(char const*) const` |
| 21 | 172 | 0.5 | `___CFBasicHashFindBucket_Linear` |
| 22 | 158 | 0.4 | `swift_retain` |
| 23 | 152 | 0.4 | `_xzm_xzone_malloc_from_freelist_chunk` |
| 24 | 149 | 0.4 | `_xzm_free` |
| 25 | 130 | 0.3 | `ItemVariationStore::ValueForDeltaSet(unsigned short, unsigned short, unsigned long, short const*, double const*) const` |
| 26 | 128 | 0.3 | `_platform_memmove` |
| 27 | 126 | 0.3 | `_platform_strlen` |
| 28 | 126 | 0.3 | `objc_autorelease` |
| 29 | 117 | 0.3 | `objc_loadWeakRetained` |
| 30 | 109 | 0.3 | `<no backtrace>` |
| 31 | 108 | 0.3 | `search_method_list(method_list_t const*, objc_selector*)` |
| 32 | 107 | 0.3 | `list_array_tt<unsigned long, protocol_list_t, RawPtr>::iteratorImpl<false>::iteratorImpl(list_array_tt<unsigned long, protocol_list_t, RawPtr>::ListIterator,...` |
| 33 | 104 | 0.3 | `os_unfair_lock_lock` |
| 34 | 101 | 0.3 | `_xzm_xzone_malloc_freelist_outlined` |
| 35 | 100 | 0.3 | `objc::ObjectHashTable::forEachObject(char const*, void (unsigned long long, unsigned short, bool&) block_pointer) const` |
| 36 | 96 | 0.3 | `CFRetain` |
| 37 | 92 | 0.2 | `__CFStringEqual` |
| 38 | 82 | 0.2 | `__workq_kernreturn` |
| 39 | 82 | 0.2 | `swift_allocObject` |
| 40 | 80 | 0.2 | `_platform_memset` |

### Top 40 frames by inclusive time (main thread, search phase)

| # | ms | % of main busy | frame |
|---:|---:|---:|---|
| 1 | 27840 | 74.6 | `main [atos 0x100a9575b]` |
| 2 | 27840 | 74.6 | `start` |
| 3 | 27840 | 74.6 | `-[NSApplication run]` |
| 4 | 27822 | 74.6 | `-[NSApplication(NSEventRouting) nextEventMatchingMask:untilDate:inMode:dequeue:]` |
| 5 | 27822 | 74.6 | `-[NSApplication(NSEventRouting) _nextEventMatchingEventMask:untilDate:inMode:dequeue:]` |
| 6 | 27784 | 74.5 | `_DPSNextEvent` |
| 7 | 27782 | 74.5 | `_DPSBlockUntilNextEventMatchingListInMode` |
| 8 | 27782 | 74.5 | `_BlockUntilNextEventMatchingListInMode` |
| 9 | 27780 | 74.5 | `ReceiveNextEventCommon` |
| 10 | 27687 | 74.2 | `RunCurrentEventLoopInMode` |
| 11 | 27548 | 73.9 | `_CFRunLoopRunSpecificWithOptions` |
| 12 | 27058 | 72.5 | `__CFRunLoopRun` |
| 13 | 20142 | 54.0 | `UC::DriverCore::continueProcessing()` |
| 14 | 19697 | 52.8 | `stepTransactionFlush` |
| 15 | 18403 | 49.3 | `CA::Transaction::commit()` |
| 16 | 18352 | 49.2 | `CA::Transaction::flush_as_runloop_observer(bool)` |
| 17 | 15379 | 41.2 | `__CFRunLoopDoSources0` |
| 18 | 15337 | 41.1 | `__CFRunLoopDoSource0` |
| 19 | 15333 | 41.1 | `__CFRUNLOOP_IS_CALLING_OUT_TO_A_SOURCE0_PERFORM_FUNCTION__` |
| 20 | 12178 | 32.6 | `0x19469f4a4` |
| 21 | 12157 | 32.6 | `0x19469f47c` |
| 22 | 11575 | 31.0 | `CA::Transaction::run_commit_handlers(CATransactionPhase)` |
| 23 | 11458 | 30.7 | `NSDisplayCycleFlush` |
| 24 | 11412 | 30.6 | `NSDisplayCycleObserverInvoke` |
| 25 | 9349 | 25.1 | `completeTaskWithClosure(swift::AsyncContext*, swift::SwiftError*)` |
| 26 | 8838 | 23.7 | `NSPerformVisuallyAtomicChange` |
| 27 | 8737 | 23.4 | `<deduplicated_symbol> [atos 0x100b44fb9]` |
| 28 | 8737 | 23.4 | `<deduplicated_symbol> [atos 0x100a9b389]` |
| 29 | 7919 | 21.2 | `-[NSView _layoutSubtreeIfNeededAndAllowTemporaryEngine:]` |
| 30 | 7919 | 21.2 | `__56-[NSView _layoutSubtreeIfNeededAndAllowTemporaryEngine:]_block_invoke` |
| 31 | 7919 | 21.2 | `-[NSView layoutSubtreeIfNeeded]` |
| 32 | 7864 | 21.1 | `-[NSWindow(NSConstraintBasedLayoutInternal) layoutIfNeeded]` |
| 33 | 7861 | 21.1 | `__36-[NSView _layoutSubtreeWithOldSize:]_block_invoke` |
| 34 | 7861 | 21.1 | `-[NSView _layoutSubtreeWithOldSize:]` |
| 35 | 7761 | 20.8 | `_NSViewLayout` |
| 36 | 7751 | 20.8 | `___NSViewLayout_block_invoke` |
| 37 | 7717 | 20.7 | `-[NSWindow(NSConstraintBasedLayoutInternal) _layoutViewTree]` |
| 38 | 7696 | 20.6 | `0x194664d5c` |
| 39 | 7695 | 20.6 | `0x194664178` |
| 40 | 7674 | 20.6 | `__NSWindowGetDisplayCycleObserverForLayout_block_invoke` |

### Top 40 Sweetbriar frames by inclusive time (main thread, search phase)

Frames from the Sweetbriar and SweetbriarBridge binaries (the Rust core is linked into SweetbriarBridge).

| # | ms | % of main busy | frame |
|---:|---:|---:|---|
| 1 | 27840 | 74.6 | `main [atos 0x100a9575b]` |
| 2 | 8737 | 23.4 | `<deduplicated_symbol> [atos 0x100b44fb9]` |
| 3 | 8737 | 23.4 | `<deduplicated_symbol> [atos 0x100a9b389]` |
| 4 | 4579 | 12.3 | `<deduplicated_symbol> [atos 0x100a9ae55]` |
| 5 | 3990 | 10.7 | `closure #1 in MessageListViewController.show(_:) [atos 0x100b2e801]` |
| 6 | 3270 | 8.8 | `MessageListViewController.install(_:ticket:)` |
| 7 | 2750 | 7.4 | `@objc AppKitPrototypeList.tableView(_:viewFor:row:) [atos 0x100a9cc17]` |
| 8 | 2614 | 7.0 | `closure #1 in AppDelegate.startBenchmark(mode:reportPath:)` |
| 9 | 2250 | 6.0 | `specialized AppKitPrototypeList.tableView(_:viewFor:row:) [atos 0x100a9fdcf]` |
| 10 | 2077 | 5.6 | `SearchFieldController.applyModel()` |
| 11 | 1945 | 5.2 | `closure #1 in closure #2 in implicit closure #1 in SearchFieldController.observeModel()` |
| 12 | 1870 | 5.0 | `ListBenchmarkRunner.runSearch(controller:)` |
| 13 | 1208 | 3.2 | `closure #2 in AppKitPrototypeList.init(model:formatter:accountMarker:selectionLoss:onIndexChange:) [atos 0x100a9baab]` |
| 14 | 1170 | 3.1 | `ListWindowModel.open()` |
| 15 | 1155 | 3.1 | `@nonobjc NSButton.__allocating_init(title:target:action:)` |
| 16 | 1131 | 3.0 | `SearchFieldModel.updateText(_:composition:)` |
| 17 | 1101 | 3.0 | `closure #4 in MessageListViewController.init(bridge:sidebar:) [atos 0x100b2d10f]` |
| 18 | 1078 | 2.9 | `specialized reset #1 (scope:) in ListBenchmarkRunner.runSearch(controller:) [atos 0x100b13953]` |
| 19 | 1058 | 2.8 | `@objc BenchmarkTickSampler.tickFired(_:) [atos 0x100acaff7]` |
| 20 | 1037 | 2.8 | `SearchFieldController.navigated(to:)` |
| 21 | 1034 | 2.8 | `BenchmarkTickSampler.tickFired(_:)` |
| 22 | 982 | 2.6 | `MessageListViewController.detach()` |
| 23 | 745 | 2.0 | `closure #8 in ListBenchmarkRunner.runStore(controller:)` |
| 24 | 744 | 2.0 | `partial apply for closure #2 in ListBenchmarkRunner.runOnce(implementation:pass:generation:inheritedFailures:)` |
| 25 | 715 | 1.9 | `RunContext.tickDidFire(_:) [atos 0x100af6e1b]` |
| 26 | 683 | 1.8 | `closure #6 in ListBenchmarkRunner.runSearch(controller:)` |
| 27 | 668 | 1.8 | `RunContext.checkDisplay() [atos 0x100af6b27]` |
| 28 | 562 | 1.5 | `specialized static RunContext.displayID(_:) [atos 0x100b11b93]` |
| 29 | 506 | 1.4 | `closure #3 in MainWindowController.install(_:) [atos 0x100b1d6bf]` |
| 30 | 436 | 1.2 | `SearchFieldController.refreshScopeMenu()` |
| 31 | 426 | 1.1 | `ListWindowModel.open() [atos 0x101eb0237]` |
| 32 | 422 | 1.1 | `ListWindowModel.showProvisional(_:)` |
| 33 | 422 | 1.1 | `specialized reset #1 (scope:) in ListBenchmarkRunner.runSearch(controller:) [atos 0x100b14b6b]` |
| 34 | 410 | 1.1 | `AppKitPrototypeList.init(model:formatter:accountMarker:selectionLoss:onIndexChange:)` |
| 35 | 397 | 1.1 | `MessageRowCellView.init(showsAccountMarker:) [atos 0x100a9e1c7]` |
| 36 | 368 | 1.0 | `PlainTextReaderView.text.setter` |
| 37 | 325 | 0.9 | `MessageRowCellView.init(showsAccountMarker:) [atos 0x100a9e1e7]` |
| 38 | 322 | 0.9 | `closure #1 in ListWindowModel.queue(_:)` |
| 39 | 318 | 0.9 | `thunk for @escaping @callee_guaranteed (@guaranteed CFRunLoopObserverRef?, @unowned CFRunLoopActivity) -> () [atos 0x100acacd3]` |
| 40 | 318 | 0.9 | `closure #1 in BenchmarkTickSampler.start(on:)` |

### Top 40 Sweetbriar frames by attributed self time (main thread, search phase)

Each sample is charged to its leaf-most Sweetbriar frame, so system work a Sweetbriar call causes counts against that call.

| # | ms | % of main busy | frame |
|---:|---:|---:|---|
| 1 | 23458 | 62.9 | `main [atos 0x100a9575b]` |
| 2 | 1202 | 3.2 | `closure #2 in AppKitPrototypeList.init(model:formatter:accountMarker:selectionLoss:onIndexChange:) [atos 0x100a9baab]` |
| 3 | 1078 | 2.9 | `@nonobjc NSButton.__allocating_init(title:target:action:)` |
| 4 | 958 | 2.6 | `MessageListViewController.detach()` |
| 5 | 852 | 2.3 | `SearchFieldController.navigated(to:)` |
| 6 | 612 | 1.6 | `closure #6 in ListBenchmarkRunner.runSearch(controller:)` |
| 7 | 561 | 1.5 | `specialized static RunContext.displayID(_:) [atos 0x100b11b93]` |
| 8 | 426 | 1.1 | `SearchFieldController.refreshScopeMenu()` |
| 9 | 422 | 1.1 | `specialized reset #1 (scope:) in ListBenchmarkRunner.runSearch(controller:) [atos 0x100b14b6b]` |
| 10 | 397 | 1.1 | `MessageRowCellView.init(showsAccountMarker:) [atos 0x100a9e1c7]` |
| 11 | 331 | 0.9 | `PlainTextReaderView.text.setter` |
| 12 | 325 | 0.9 | `MessageRowCellView.init(showsAccountMarker:) [atos 0x100a9e1e7]` |
| 13 | 288 | 0.8 | `BadgeView.init(frame:)` |
| 14 | 274 | 0.7 | `ListBenchmarkRunner.runSearch(controller:)` |
| 15 | 201 | 0.5 | `@nonobjc NSScrollView.init()` |
| 16 | 199 | 0.5 | `MessageRowCellView.init(showsAccountMarker:) [atos 0x100a9da3f]` |
| 17 | 195 | 0.5 | `MessageListViewController.updateStatus()` |
| 18 | 165 | 0.4 | `@objc MessageRowCellView.isFlipped.getter [atos 0x100a9f50b]` |
| 19 | 160 | 0.4 | `AppKitPrototypeList.buildTable() [atos 0x100a9b5ab]` |
| 20 | 146 | 0.4 | `MessageRowFormatter.dateString(_:now:) [atos 0x100b325a7]` |
| 21 | 142 | 0.4 | `closure #1 in closure #1 in static SystemProbes.physicalFootprint()` |
| 22 | 138 | 0.4 | `MessageRowCellView.init(showsAccountMarker:) [atos 0x100a9db23]` |
| 23 | 126 | 0.3 | `closure #1 in MessageRowCellView.applyLayerColors()` |
| 24 | 126 | 0.3 | `closure #1 in SearchFieldController.rebuildChips()` |
| 25 | 119 | 0.3 | `MessageListViewController.install(_:ticket:)` |
| 26 | 117 | 0.3 | `MessageRowCellView.configure(content:)` |
| 27 | 105 | 0.3 | `MessageRowCellView.init(showsAccountMarker:) [atos 0x100a9e2cf]` |
| 28 | 99 | 0.3 | `SearchFieldController.rebuildChips()` |
| 29 | 96 | 0.3 | `closure #3 in ListBenchmarkRunner.runSearch(controller:)` |
| 30 | 93 | 0.2 | `@nonobjc NSTableView.init()` |
| 31 | 85 | 0.2 | `specialized SearchCoverageNote.init(coverage:excludedUnknown:locale:timeZone:) [atos 0x101ed07a7]` |
| 32 | 76 | 0.2 | `@nonobjc NSMenuItem.init(title:action:keyEquivalent:)` |
| 33 | 76 | 0.2 | `MessageRowFormatter.dateString(_:now:) [atos 0x100b32657]` |
| 34 | 74 | 0.2 | `@objc ClauseChipButton.init(frame:) [atos 0x100b41aff]` |
| 35 | 73 | 0.2 | `AppKitPrototypeList.pagesDidLand(_:)` |
| 36 | 72 | 0.2 | `closure #1 in BenchmarkTickSampler.start(on:)` |
| 37 | 69 | 0.2 | `AppKitPrototypeList.buildTable() [atos 0x100a9b6d7]` |
| 38 | 63 | 0.2 | `SearchFieldController.refreshStatus()` |
| 39 | 60 | 0.2 | `MessageRowCellView.init(showsAccountMarker:) [atos 0x100a9e2af]` |
| 40 | 60 | 0.2 | `@objc MessageRowCellView.isFlipped.getter [atos 0x100a9f4ff]` |

### Named areas, main thread, search phase

Inclusive: a sample counts for every area with a matching frame on its stack.

| area | ms | % |
|---|---:|---:|
| AppKitPrototypeList | 4786 | 12.8 |
| MessageRowCellView | 2749 | 7.4 |
| Row presentation / formatters | 79 | 0.2 |
| ListWindowModel | 1985 | 5.3 |
| FrameBatcher | 328 | 0.9 |
| MessageListViewController | 5280 | 14.2 |
| SearchFieldController | 3123 | 8.4 |
| SearchFieldModel | 1377 | 3.7 |
| Benchmark harness (runner, tick sampler) | 3984 | 10.7 |
| -[NSTableView reloadData] | 1093 | 2.9 |
| NSTableView (any) | 7014 | 18.8 |
| NSView layout (layoutSubtreeIfNeeded) | 7919 | 21.2 |
| NSView display (displayIfNeeded / drawRect) | 1247 | 3.3 |
| NSTextView insertText | 111 | 0.3 |
| NSTextCheckingController | 358 | 1.0 |
| Core Animation commit (CA::Transaction::commit) | 18403 | 49.3 |
| NSDisplayCycleFlush | 11458 | 30.7 |
| Swift concurrency job (completeTaskWithClosure) | 9349 | 25.1 |
| Rust core (sb_*) | 2 | 0.0 |

### The timed keystroke itself

Main-thread samples inside the timed keystroke's `post` closure (`-[NSTextView insertText:]` of the last character, synchronous). `span` runs from its first to its last sample; on-CPU time below `span` means the main thread was off CPU inside the call.

| sample | rendered ms | span ms | on-CPU ms |
|---|---:|---:|---:|
| exact-reference r0 | 2927 | 779 | 41 |
| exact-address r0 | 4770 | 220 | 32 |
| phrase r0 | 2706 | 37 | 11 |
| scoped-filter r0 | 1656 | 23 | 2 |
| negative-body r0 | 2139 | 23 | 10 |
| exact-reference r1 | 858 | 68 | 22 |
| exact-address r1 | 1424 | 258 | 25 |
| phrase r1 | 2419 | 144 | 9 |
| scoped-filter r1 | 1902 | 54 | 7 |
| negative-body r1 | 3300 | 31 | 12 |
| exact-reference r2 | 1197 | 66 | 16 |
| exact-address r2 | 1290 | 114 | 12 |
| phrase r2 | 2842 | 62 | 21 |
| scoped-filter r2 | 3181 | 50 | 18 |
| negative-body r2 | 3346 | 65 | 10 |
| exact-reference r3 | 1298 | 56 | 18 |
| exact-address r3 | 1619 | 42 | 6 |
| phrase r3 | 1351 | 19 | 11 |
| scoped-filter r3 | 2108 | 47 | 10 |
| negative-body r3 | 3172 | 69 | 13 |
| exact-reference r4 | 737 | 195 | 15 |
| exact-address r4 | 2414 | 65 | 7 |
| phrase r4 | 3172 | 48 | 14 |
| scoped-filter r4 | 1793 | 97 | 13 |
| negative-body r4 | 2331 | 54 | 15 |
| exact-reference r5 | 2213 | 124 | 20 |
| exact-address r5 | 1926 | 53 | 11 |
| phrase r5 | 3243 | 22 | 5 |
| scoped-filter r5 | 3026 | 64 | 16 |
| negative-body r5 | 3599 | 65 | 19 |
| exact-reference r6 | 2297 | 87 | 18 |
| exact-address r6 | 949 | 60 | 11 |
| phrase r6 | 2955 | 0 | 1 |
| scoped-filter r6 | 4121 | 118 | 15 |
| negative-body r6 | 3386 | 73 | 10 |
| exact-reference r7 | 1039 | 19 | 7 |
| exact-address r7 | 1188 | 296 | 28 |
| phrase r7 | 3329 | 2 | 2 |
| scoped-filter r7 | 1982 | 16 | 11 |
| negative-body r7 | 2831 | 70 | 12 |
| exact-reference r8 | 580 | 56 | 10 |
| exact-address r8 | 3288 | 36 | 18 |
| phrase r8 | 3584 | 24 | 11 |
| scoped-filter r8 | 4607 | 30 | 14 |
| negative-body r8 | 4548 | 26 | 5 |
| exact-reference r9 | 1758 | 74 | 13 |
| exact-address r9 | 990 | 32 | 9 |
| phrase r9 | 2737 | 95 | 32 |
| scoped-filter r9 | 3807 | 10 | 10 |
| negative-body r9 | 4538 | 74 | 13 |

### Main-thread busy ms per 100 ms bucket after each timed keystroke

Column k is [k*100, (k+1)*100) ms after the keystroke; `rendered` is the report's keystroke-to-rows time for that sample.

```
sample                    rendered   0   1   2   3   4   5   6   7   8   9  10  11  12  13  14  15  16  17  18  19  20  21  22  23  24  25  26  27  28  29  30  31  32  33  34  35  36  37  38  39
exact-reference r0            2927   2   4   2   4  13   5   5   6   6  14   9  21  13   5  17  13  11   5  16   5   4  16  12  15  26  20  13  21  14   7   0   3   8   7   3  21  11  14  17  13
exact-address r0              4770  15  11  19  27  36  29  33  12   8   6  11  11  13   4   6  18  11  10  33  12   9   8   4   8   9  11  10  13   0   6   9  22   6  10  14   3  13  13  11   9
phrase r0                     2706  36  27   7  10   5   0   0   0   1   1   1   0   0   0   2   6   3  19  12  22   9  16  12  10   6   4  19   2   0   2   0   5  21  34  48  13   8  10  13   8
scoped-filter r0              1656  23   6  12  28  10   7  19  28  42   3   9  25  20  24  31  26  22  25   9   0  18  14  10   0   1   0   0   0   0   0   1   1   2   3   3   2   1   3   2   3
negative-body r0              2139  17  25  16  15   2   1   1   0   1   1   3   1   1   1   0   1   0   1   0  33  30  33   2   1   0   1   1   1   0   0   0   0   1   1   0   1   1   0   3   9
exact-reference r1             858  31   8  29  19   6  14  19  21  22  19   4  22   9  26  24  22  19  15   6  13  30  24  12   9   0   2  18   1  11  18   7  24  12  31  39  16  32   2   1   1
exact-address r1              1424  18   3  15  11  13  23  12  38  39   7  31   0   1   1  10  55  44  38   2   4   8  10   8  14   9  31  31  18  25   9   2   8   8   5   6   8   6   5  17  26
phrase r1                     2419   5   6   9   5   5  18  28  11  18   1   8   1   1   0   0   0   1   2   5  12  12  31  21  22   0   0   1   0   0   1   0   1   0   0   0   1   0   2   1   1
scoped-filter r1              1902  14  21  23   9  11  14  24  16  18  13  19  13  23  18  24   4   4   2   4  48  12   2   2   2   3   1   2   3   2   3   2   2   2   2   2   2   2   3   2   3
negative-body r1              3300  20  14  30   2   2   3   4   2   2   3   1  23  16   3   2   2   3   3   2   2   2   4   2   2   3   1   9  19  35   9  28  14  16   3   0   2   2   2   3   0
exact-reference r2            1197  22   8  27  15   5   7   1   2   1  14  19  19  19  14   7   9   4  20  12  11  37  20   7  16  23  20  29  23  22  23  15  13   3  10  20  14  25  17  20  26
exact-address r2              1290  11  16  17  18  23  14  30  16  20  31  16   0  11  41   8   8  18  17  39  35   3   3   9  21  25  17  15  24  22  20   1   2   2  10  14  45  33   2  16   7
phrase r2                     2842  39  40   4  12  10  11   8  13  16  18  18   5   2   0   2   1   0   0   0   1   0   1   0   0  13  15  30  26  12   0   9   4   0   1   0   0   1   0   1   0
scoped-filter r2              3181  24  20   6  15   7   5   6   8  27  24  31  17  10  18   2   0   0   1   0   0   0   0   0   1   0   0  17  22  26  13   9  10   2   0   0   0   0   0   1   0
negative-body r2              3346  19  20  20   1   0  18   1   1   0   0   1   1   1   0   1   1   0   0   3   6   0   0   1   2   0   1   0   2  12  29  20   9  10  12   0   1   1   0   1   0
exact-reference r3            1298  28  12  28  15   8   1   4  21  12   4   6  14  14  18  19  27  11   7  12  16  33  24   6  10   3   9   5  10  21  13  14  31  22   8  20  14   9  14  18  28
exact-address r3              1619  29  23  12  19   6  16  19  23   7   0   1   0  12  35  61  30  20  18  12  24   6  11  24  44  16  26  27   8   1  11  12  29  12  12   9  28   1   1   2   0
phrase r3                     1351  20  23  17  11  11  27   2   1   2   0   1   1   3  13  36   4  19   2   1   0   0   1   0   1   0   0   1   1   0   1   1   0   0   0   1   1   0   1   1   1
scoped-filter r3              2108  17   6  12  12  13   1   9   3  27  12  17  44  17  22   9  10  18   7  26  33  43   3   2   0   2   0   0   2   1   3   2   2   3   3   2   6   2   2   3   2
negative-body r3              3172  14   5   9  34  20  24  14   2   3   4   2   2   4   3   4   3   3   5   3   4   4   4   4   5   6   7   4   9  22  33  14   9   3   2   2   3   3   3   2   3
exact-reference r4             737   3  16   7  30  39   8  30  17  23  40  38  24  23  18   7   6  12   9   5  25  15  29  22  16  13  26  19  33   5  12  10  11  21  21   8  10  17  14  17  18
exact-address r4              2414   8  29  17  29  15  12   8  10  23  18   8  10  16  16  16  20   8  12  10  18  13  11  19  17  54  14  25  45  17  18  38  41   7  15   9  16  21  17  27  29
phrase r4                     3172  29  17  18  30  15   1   1   1   2   0   1   2   0   2   1   0   3   0   0   0   2   0   1   1   2   2   2   6  18  38  11  11   4   7   8   1   1   1   1   1
scoped-filter r4              1793  14  15  34  14  35  33  27   0   0   1   1   1   1   0   2   1   0   7  44  22   0   0   0   0   0   0   0   0   0   0   1   1   1   0   0   4   0   2   1   0
negative-body r4              2331  17  26  10  20   6   9  16   0   0   1   4   0   0   0   1   0   0   1   1   8  12  28  43   5   1   1   0   0   0   0   0   0   1   1   2   1   1   0   0   0
exact-reference r5            2213  18  23  13  10  26  14  29  18   4   1   3   3   3   1   2   2   3   2  11   9  21  15  27  16  11  14  17  20  21  48  14  20  20  26  20  29  17   2   2   5
exact-address r5              1926  23  21  31  28  20  20   9  31  26   1   3   1   2   2   4   5  17  48  46  14  31  52  23  34  39  21  11   8   6  18  16  20  21   4   3   2   1   2   1   3
phrase r5                     3243  19  19  15  21   4   3   2   1   2   1   3   1   1   2   3   0   3   2   3   3   2   2   1   0   3   5   1   7  16  39  40  35   9   1   2   1   1   4   0   2
scoped-filter r5              3026  16   3  19  11  14  28  15  22  22  25  12  39  23   8   2   2   1   1   0   0   0   1   1   1   0   1   1   0   0  23  32  16   2   2   3   3   0   1   0   1
negative-body r5              3599  30  14  13  15  12  10  28   1   1   0   0   0   0   0   0   1   0   0   1   2   0   0   0   2   1   0   0   1   0   1   8  13  28  20  12  23   0   0   0   1
exact-reference r6            2297  19  10  17  13  12  13  13  14   6   3   4   2   1   1   3   3   2   3   2   2   1   3   9  21  17  20   7  25  21  24  30  22  25  35  11  26  30  16   3   4
exact-address r6               949  16  26  31  19  32  12  24   3   1  33  54   9  40  21  14  43  10   2   6  19  26  20   4   5   2   3   4   2   4   0   2   1   2   1   6   3   4   1   2   2
phrase r6                     2955  21  27   9   7   2   3   4   1   3   3   1   2   2   0   3   6   3   2   2   2   2   2   1   2   1   0  18  36  36  17   0   0   2   3   2   3   2   2   2   2
scoped-filter r6              4121  13  22  26  35  14  21   2  32  15  22  25  17   3   1   2   1   1   1   0   2   0   0   0   0   1   2   0   0   1   0   3  10   3   0   0   1   2   8  22  41
negative-body r6              3386  14  16  18  19   1   0   0   2   0   0   0   0   2   0   1   0   0   1   3   1   2   1   2   1   1   1   0   0   2  16  38  11  26  16   1   1   0   1   0   0
exact-reference r7            1039  13  20  25  18  11   0  12   1  11  25  27  13  27  28  17  35  20  13  11  19   9  28  19  15  12   2   0   3   3   7  20  30  31  21  22  19   5  18   0  23
exact-address r7              1188   5   5  19  27  26  29  16  24   9   5  14   4  44  20  54  22  40  22   4   2   9   0   4  13  22  38   8   2   1   0   1   0   1   1   0   0   1   0   2   1
phrase r7                     3329   8   2  10  14  39  18   3   1   1   0   1   0   2   0   0   1   0   1   2   0   1   0   1   0   0   0   1   1  15  23  19  15  27   2   0   0   0   0   0   3
scoped-filter r7              1982  23  17  28  21  16  20  15  13  13  16   0   1   1   2   3   0   0   1   1  12  46  31   0   1   0   0   1   0   2   0   0   0   1   1   1   3   0   0   0   2
negative-body r7              2831  20   0  15  23  15   2  19  16   1   1   0   0   0   1   0   0   1   0   0   0   1   0   0   0   0   1  13  33  24   2   0   0   1   0   2   0   1   0   0   1
exact-reference r8             580  18  21  19  20   9  18  26  14  15  17   7  37  29  19  23  25  26  18  11   2  12  21   9   8  17   8   2  18   4   9  17  20  19  20  10  10  16  28   6  21
exact-address r8              3288  21   1  13  19  22  17  20   7   9  26  21   9  19   8   2  10  18  12  23  18   5   3   4   3   2   3   2   3   5  38  28  42  29  35  16   6  46  17  22   8
phrase r8                     3584  31   4  25  22   5  19  17  16   7   2   1   3   2   1   3   2   2   3   3   2   3   2   2   2   2   2   1   4   1   2   1   2   4  10  30  41  22   3   1   1
scoped-filter r8              4607  26   5   7  10   1  19  27  11  11   5  17   2   0  20  17  29  34  15  15  14   2   0   1   1   1   1   1   0   1   0   1   0   0   0   0   0   1   2   0   1
negative-body r8              4548   9  16  28  24  13   3  30   0   0   0   0   0   0   0   1   0   2   1   1   1   0   1   0   0   0   1   0   0   3   0   1   3   0   2   0   1   1   0   0   0
exact-reference r9            1758  16   8  14  28   7  13   3   3   3   3   6   3   3   3  20  28  12  15  19  17   4   7  20  22  23  15  27  46  25   8  21  13  10  33  19  24  26  23  23  20
exact-address r9               990  21  11  29   3   1   3   3   2   3  13  18  26  30  32  25  25  16  15  37  29  35  40  17  20   9   3   3   6   5   9  25  27  10  19  17  15  24  16  13  23
phrase r9                     2737  32  17  12  22   6  24  22  15  20  22  11  12   2   3   2   1   2   1   1   2   2   1   4   2   3  34  31   5   2   2   1   1   2   0   0   2   3   1   2   2
scoped-filter r9              3807  30  12  15   6  24  19  39  28  38  43  14   0   5   1   4   3   3   4   3   3   2   2   2   1   1   3   3   2   2   2   3   5   2  20  22  29  25  25   3   4
negative-body r9              4538  25  19  15   8   2   0   1   3   3   7  20  28   1   1   0   1   4   1   4   3   1   3   4   3   5   1   1   1   0   2   1   0   0   1   0   1   0   1   2   0
mean                                19  15  17  17  13  13  14  11  11  10  10   9   9   9  10  10   9   8   9  11  10  11   8   8   8   8   9  10   9  11  11  11   9   9   8   9   8   6   6   7
```

### Main-thread busy ms per 100 ms bucket, whole search phase

One line per second of trace time, ten buckets per line; `*` marks a second holding a timed keystroke.

```
    53.6s    20  23  24  11   6   9  11  23  24  24
    54.6s     5   2   1   2   2   0   0   1   1   1
    55.6s     1   4   2  10   9   7  10   0   0   0
    56.6s     0   0   0   1   8   6  12   6   3   7
    57.6s    29  11  14  17   4  11  14   0   0   0
    58.6s *   2   4   2   4  13   5   5   6   6  14
    59.6s     9  21  13   5  17  13  11   5  16   5
    60.6s     4  16  12  15  26  20  13  21  14   7
    61.6s     0   3   8   7   3  21  11  14  17  13
    62.6s    25   7  12  23  15  14   9   8  21   5
    63.6s     4   6   9   6   9   3   4   6  13   5
    64.6s     3   8  18  21  15   6   7  10  18   7
    65.6s     7   9   6   8  10  16  14   5  17  18
    66.6s    29   9   6  12  10   7   9   8   8  13
    67.6s     8  14  10  25   9  19  16  13  13   8
    68.6s     4  12   6  11  11  17  22  26  16   8
    69.6s *  10   6  12  11  15  18  28  19  48  28
    70.6s    10  15   4  10  13  11   5   7  19   8
    71.6s     8  29  16  12   8   6   5   6  15   9
    72.6s    14   2   5   8  19   9  11  10   7   5
    73.6s    18  12  10  18  16  22  40  20  31  13
    74.6s     7  27   7  22   9  20   5  23  18  13
    75.6s    15  24  18  12  11  22   9  10   3   9
    76.6s    18  20  21   9  19  52  26  17   8  18
    77.6s    23  32  35  22  10   7   1  14   5   4
    78.6s *   6   7  11  14   7  17  34  29   9  10
    79.6s     5   0   0   0   1   0   2   0   0   0
    80.6s     2   6   2  19  11  23  10  16  11  11
    81.6s     4   6  19   2   0   2   0   5  15  33
    82.6s    50  18   7   9  14   9   8  18   7  10
    83.6s    11  14  18   9  16   8  22  37  25  31
    84.6s *  33  14  18   5   2  16  28  35  12   5
    85.6s    31  10   8  16  17  46  16   5  10  31
    86.6s    25  23  28  31  16  23   1   9  18   9
    87.6s     6   0   1   0   0   0   0   0   1   2
    88.6s     2   4   2   1   2   3   3   2   0   2
    89.6s     2   1   2   1   3   2   1   0   2   1
    90.6s     2   1   2   2   2   2   1   2   2   2
    91.6s     0   0   3   2   1   2   1   1   2   1
    92.6s     2   0   2   2   4   0   2   3   2   0
    93.6s     5   1   0   0   2   0   1   2   0   1
    94.6s     0   3   2   1   1   3   0   2   0   2
    95.6s     1   1   2   2   0   1   1   0   2   1
    96.6s     1   2   2   0   0   1   7  14  13  17
    97.6s     1   7   9  36   0  11  32  15  10   8
    98.6s    15   9   9   6   4  17  23  24  28  24
    99.6s    25  35  40  28  17   8  11   6   6   0
   100.6s     1   1   3   2   0   7  12   6   5   2
   101.6s     1   2   2   2   0   2   1   0   1   2
   102.6s     2   0   0   1   3   0   1   0   0   2
   103.6s     0   2   0   0   0   2   0   2   1   0
   104.6s     1   2   0   0   0   2   0   3   0   0
   105.6s     0   0   1   0   0   1   1   0   0   1
   106.6s     0   0   0   0   1   0   1   9  12  18
   107.6s *   5  40  30  27   9  30   4  17   0   1
   108.6s     1   0   1   1   4   1   1   0   1   0
   109.6s     1   0   2  44  30  20   2   1   0   2
   110.6s     0   1   0   0   0   1   0   1   1   0
   111.6s     1   1   3   9  20  28  44  29  26  13
   112.6s    13   8  11   7  11  16  20  12  24  54
   113.6s    20   7  15   0   7   6  12  10  13   7
   114.6s     9  25  23  16   1   3  13   7   8  13
   115.6s *  19   1   1  13  21  15  28  11   8  17
   116.6s    17  29  20  12  22   6   9  38  15  20
   117.6s *  16  19   2  21  28  21  16   0   1   8
   118.6s    12   3  17   9  14  22  15  38  36  11
   119.6s    27   0   1   1  17  51  49  31   1   4
   120.6s     8  13   5  14  11  33  27  19  25   8
   121.6s *   3   7   8   5   6   9   5   5  18  27
   122.6s    12  18   1   8   1   1   0   0   0   1
   123.6s     2   4  13  12  30  22  22   0   0   1
   124.6s     0   0   1   0   1   0   0   0   1   0
   125.6s     2   1   1   1   0   1   2  12  21  21
   126.6s    36  21  38  19  15  25  49   9   1   2
   127.6s *   1   0  11  18  22  18  11   8  18  23
   128.6s    20  11  20  17  15  21  20  13   3   3
   129.6s     2  33  29   2   2   2   3   1   2   3
   130.6s     2   3   2   2   2   2   2   2   2   3
   131.6s     2   3   3   2   2   3   2   2   2  24
   132.6s    53  18   6  34  40  22  28  14   2   9
   133.6s     5  12  21  40  17   3  19  21   4  21
   134.6s    38  14   3   2   2   2   3   2   2   1
   135.6s     3   4   7   3   2   3   2   2   2   2
   136.6s     2   2   4   3   2   2   1   2   2   3
   137.6s *   2   7  39  51  21   2   3  15  11  33
   138.6s     7   2   3   3   3   2   3   1  19  20
   139.6s     3   2   1   3   4   2   2   2   4   2
   140.6s     2   3   1   3  20  27  18  31   7  24
   141.6s     2   1   2   2   2   3   0   3   0   3
   142.6s     1   1   8   1   2   2   1   5   9  14
   143.6s    27  61   7  13   8  13  17  13  10  12
   144.6s     1  12  10  45  20  31  28  12  18  14
   145.6s    22   4   3   2   1   5  16   7   5   0
   146.6s     0   0   1   0   0   1   1   3   2  22
   147.6s *  23   1   1   0  16   9  17  28   7   3
   148.6s     5   2   1   4  24  21  11  20   7   6
   149.6s    10  15  17   6  34  22  15  15  21  14
   150.6s *  25  26  18  33  13  18   2  10  14  19
   151.6s    16  24  16  26  18  22  25  22   0   8
   152.6s    44   6   9  19  12  36  41   5   3   9
   153.6s    13  33  15  13  26  24  17   4   2   1
   154.6s *   8  15  42  38   2  15   7  14   5  14
   155.6s    15  20  16   6   1   1   1   1   0   0
   156.6s     0   1   1   0   0   0  15  16  29  26
   157.6s    10   0   9   4   0   1   0   0   1   0
   158.6s     1   0   0   1   0   0   0   1   1   1
   159.6s     0   0   0   2   0   0   1   1   0   1
   160.6s     3   0   0   1   6  31  32  35  32   7
   161.6s    18  24  19  27  19  31  28   3   1   5
   162.6s *  20  14  21   3  18   5   6   5  18  17
   163.6s    33  26  13  14  12   2   0   0   1   0
   164.6s     0   0   0   0   1   0   0  24  32  16
   165.6s    11  10   6   0   0   0   0   0   0   1
   166.6s     0   1   1   0   1   0   1   0   0   1
   167.6s     1   0   1   1   1   0   0   0   1   0
   168.6s     0   0   1   0   0   1   0   0   0   1
   169.6s     0   0   0   1   4   1   1   1   0   0
   170.6s     0   0   0   0   0   2   0   1   0   1
   171.6s     1   1   2   0   0   0   0   1   0   2
   172.6s     1   1   0   0   0   0   1   0   1   0
   173.6s     0   0   1   0   0   0   0   0   1   0
   174.6s     0   0   0   0   0   1   1   0   0   1
   175.6s     1   0   1   0   0   0   0   0   1   0
   176.6s     0   6  18  28  13  28  17  10   8  24
   177.6s    28  19  19  44   6  18  18  11   1   1
   178.6s     1   1   1   0   0   0   0   0   1   0
   179.6s     0   6   0   2   0   1   0   0   0   0
   180.6s *   2  14   5  44  36  18  20  20   2   0
   181.6s    18   1   1   0   0   1   1   1   0   1
   182.6s     1   0   0   2   7   0   0   1   2   0
   183.6s     1   0   2  11  30  20   9  10  12   0
   184.6s     1   1   0   1   0   0   0   0   0   0
   185.6s     0   0   1   0   0   0   0   0   0   0
   186.6s     1   0   1   0   1   0   0   0   3  19
   187.6s    24  29  12  27  10  21   4  16   7  13
   188.6s    10  21  15  18  18  14  40  25   6   2
   189.6s    33  27  14  17  26  17   2   1   2   1
   190.6s     0   1   0   1   2   1   2   8  15   6
   191.6s *  18  18   7   0   0  20  13  20  27  11
   192.6s     1   0  19  16   5   6  14  10  18  20
   193.6s    22  19   8   8  15  30  32   6   9   3
   194.6s     9   6   7  15  21  13  22  30   9  12
   195.6s *  20  12  13  12  32   5   0  12  33  16
   196.6s    17  10  12  22  19  13   0   1   0  11
   197.6s    33  42  36  34  19   9  17  15   8  19
   198.6s *  42  18  27  32  11   1   4  17  20  23
   199.6s     7  14  28   1   1   2   1   0   2   2
   200.6s    11  36   7  12   8   2   0   0   1   0
   201.6s     0   1   0   1   1   0   1   0   1   0
   202.6s     0   1   1   0   1   1   1   0   1   1
   203.6s     0   0   0   5   0  19  39  48  22   4
   204.6s    27   8  12  14  10   3   6  19   7   5
   205.6s     4   7   6  19  12  17   2   7  10  11
   206.6s     2  21   7  13   1  14  11  19   1   1
   207.6s *  17   6  11  12  14   1   9   3  27  12
   208.6s    16  44  18  18  13  10  17   8  25  33
   209.6s    40   7   2   0   2   0   0   2   1   3
   210.6s     2   2   3   3   2   6   2   2   3   2
   211.6s     2   2   3   2   3   3   3   1   2   2
   212.6s     2   2   2   2   1   5   3   6   1   3
   213.6s     2   1   2   2   2   2   2   1   2   1
   214.6s     2   3  12  23  21  21  28   9   9  22
   215.6s    15  16  16  16  14   3   7   5  11  11
   216.6s    11  17  19  13  45  31  20  29  16  11
   217.6s     4   4   4   7   1  16   9  13   4  21
   218.6s    14  17   3   3   5   8   7   1   3   3
   219.6s     2   4   2   8  13   2   2   2   3   3
   220.6s     3   1   3   2   1   2   2   2   1   3
   221.6s     6  14   6   3   0   2   4   4   1   2
   222.6s     3   3   2   4   4   2   4   3   4   7
   223.6s *   7  28  29  23  22  22   4   4   2  14
   224.6s     4  10  34  19  21  16   3   4   4   2
   225.6s     2   4   3   4   3   3   5   3   4   4
   226.6s     4   4   5   6   7   4   8  20  34  14
   227.6s    11   3   2   2   3   2   4   2   3   2
   228.6s     2   2   1   2   4   3   4   3   2   2
   229.6s     2   3   3   2   2   5   4   6   2   2
   230.6s     2   2   5   2   2   2   2   2   2   2
   231.6s     2   2   2   0   3   3   2   1   6  20
   232.6s    20  28  29  15   4   8   7  21  14  17
   233.6s    14  34  35  23  14  25  15   3  29  17
   234.6s     6   8   4  12  14  10   3   4   3   3
   235.6s     4   2   0   2   1   2   2   1   2   2
   236.6s     2   2   2   3   2   1   2   2   1   3
   237.6s     3   2   2   3   2   1   2   0   2   2
   238.6s     2   3   3   4   2   2   2   3   3   1
   239.6s     1   1   2   1   3   3   2   0   3  17
   240.6s *  16   2   2   5  20  13  34  25  17  20
   241.6s    30  24  48  26  28  15  15   6   6  12
   242.6s *   7  14  17  24  25  20  15  21  16  25
   243.6s    26  13   8   8  20  19  11   9  13  21
   244.6s    10  19  15  10   6  19  15  14   9  21
   245.6s    41  26  25  36  28  21  32  48   8  13
   246.6s *  12   8  27  13  23  32   8   1   2   0
   247.6s     2   0   3   0   1   1   1   1   2   0
   248.6s     0   1   1   0   1   1   3   1   2   7
   249.6s    29  28   9  12   4  14   0   1   1   1
   250.6s     1   3   1   1   0   0   0   0   0   4
   251.6s     1   1   0   0   0   0   0   1   0   0
   252.6s     0   0   0   2   0  12  13  28  28  37
   253.6s    20  15  23  25  30  31   8  25   6   1
   254.6s *   5   7  12  20  31  14  27  36  33   6
   255.6s     0   0   2   1   1   0   2   0   1   4
   256.6s    27  41   1   0   0   0   0   0   0   0
   257.6s     0   0   1   1   1   0   0   4   0   1
   258.6s     1   1   0   0   2   0   0   0   0   0
   259.6s     0   1   1   0   1   1   1   0   0   0
   260.6s     0   0   0   0   1   3   0   2   0   0
   261.6s     0   0   1   0   0   0  21  37  21  17
   262.6s    36   8  22  21  42   7   6  26  42  21
   263.6s    17  18  18   4   4   5   0   0   1   1
   264.6s     1   0   1   0   1   2   0   0   1   0
   265.6s     0   1   0   1   3   2   1   1   0   2
   266.6s     1   2   6  15  41  19  34   5   0   1
   267.6s *  18  16  21  19   4   6  14   8   0   1
   268.6s     1   3   0   0   1   0   0   1   0   1
   269.6s    16  11  45  22   2   1   1   0   0   0
   270.6s     0   0   1   1   2   1   0   1   0   0
   271.6s     0   1   0   1   0   0   3   2   1   0
   272.6s     0   1   0   5  10   0   1   1   0   0
   273.6s     0   0   0   0   2   0   0   0   0   0
   274.6s     1   2   1   1   1   4   4   1   2   4
   275.6s     2   1   1   2   1   2   1   2   2   9
   276.6s    14  17  27  14  14   6  31  14  23  17
   277.6s    15  13   2  16   1  10  11  13   9  11
   278.6s    41   8  19  10   9  15  10   9  18  29
   279.6s    15   2  10   8  12   2   2   3   2   2
   280.6s     2   2   2   3   3   2   1   2   2   2
   281.6s     2   5   4   2   3   4   2   2   2   2
   282.6s     4   3   2   4   4  21   7  17  19  16
   283.6s *   7   2   2  12  16  15  14  24  21  19
   284.6s    23   9   3   3   3   2   2   2   2   3
   285.6s     2   5  12  13  25  19  17  18  10  17
   286.6s    15  20  39  30  21  11  23  33  15  32
   287.6s *   3   2   2  17  21  31  25  21  25   5
   288.6s    32  32   1   3   1   2   1   5   5   8
   289.6s    44  49  14  29  53  32  29  40  25  13
   290.6s *   2  12  12  14  27  14  11   3   3   1
   291.6s     2   1   3   1   1   2   2   1   3   2
   292.6s     3   3   2   2   1   0   3   1   5   2
   293.6s    13  34  42  35  20   1   2   1   1   2
   294.6s     2   1   2   1   0   1   0   1   0   2
   295.6s     0   0   0   2   5  23  41  23  17  10
   296.6s     9  22  25  35  26  24  18  16   2   6
   297.6s *   3  11   7   8  19  11  21  16  25  23
   298.6s    20  16  37  22  20   3   1   1   2   0
   299.6s     0   0   1   1   1   0   1   1   0   0
   300.6s     8  25  38   1   2   3   3   1   1   0
   301.6s     1   1   1   0   1   1   0   2   1   0
   302.6s     1   0   2   1   9   1   1   1   5   0
   303.6s     1   1   2   2   2   0   2   0   2   1
   304.6s     0   1   0   1   0   0   1   0   0   1
   305.6s     0   2   0   0   1   0   0   0   0   1
   306.6s     8  20  22  23  42  17  31  17  48  22
   307.6s    18   6   0   2   5   9  15  25  24  15
   308.6s    23  10   4  18  16  28  15  11  18   4
   309.6s     1   0   2   2   0   0   0   0   1   0
   310.6s     0   1   0   0   0   1   0   0   0   0
   311.6s     0   1   0   2   3   0   1   0   1   0
   312.6s     1   0   0   0   1   1   0   0   0   0
   313.6s *   4  16  15  33  22  12   0   0  16  21
   314.6s    10  21   8  15  21  13   1   0   0   0
   315.6s     0   0   0   1   0   0   0   1   2   0
   316.6s     0   2   1   0   0   1   0   0   2  10
   317.6s    20  31  17  17   8   0   0   1   1   2
   318.6s     0   2   1   0   1   0   0   1   0   0
   319.6s     3   0   3   0   0   3   1   2   1   1
   320.6s     2   2   3   3   2   1   2   1   2   3
   321.6s     2   2   2   2   2   1   3   1   3   2
   322.6s    24  25  18  19  32  33  13  28  14  39
   323.6s    11  18  17  22  28   8  22   7   2   4
   324.6s     2   2   1   3   2   2   2   2   2   3
   325.6s     2   2   2   2   1   2   3   2   6  22
   326.6s *   7   3   2   2  13  15  19   6  16  13
   327.6s    12  18   2   6   4   2   2   1   2   2
   328.6s     3   3   2   2   2   1   8  19  18  14
   329.6s    17  23  10  30  30  26  21  36  16  20
   330.6s *  28  23   4   2   4  17  26  29  17  35
   331.6s    13  23   3   1  27  58   8  42  22  15
   332.6s *  42  10   2   4  21  23  23   4   6   1
   333.6s     3   4   1   4   2   1   2   2   0   6
   334.6s     3   4   1   2   2   2   2   1   2   1
   335.6s     2  27  41  29   8   0   0   2   3   2
   336.6s     3   2   2   2   2   2   2   1   0   2
   337.6s     0   1   2   1   2   1   0   0   0   0
   338.6s     1   1   2   0   0   1   0   1   1   3
   339.6s     3   1  14  20  20  41  20  24  32   6
   340.6s    13  55  19  15  27  20  17  14   1   1
   341.6s *   0  12  21  24  35  16  21   4  31  15
   342.6s    20  25  20   3   1   2   1   1   1   0
   343.6s     2   0   0   0   0   1   2   0   0   1
   344.6s     0   2   8   6   0   0   1   2   6  24
   345.6s    36  31   6   1   1   1   1   1   1   0
   346.6s     1   2   2   2   0   0   1   0   3   0
   347.6s     1   1   0   0   1   0   0   1   1   2
   348.6s     1   0   1   0   1   1   0   0   1   1
   349.6s     1   0   1   0   0   0   0   1   0   0
   350.6s     0   1   0   0   0   2   0   1   0   2
   351.6s     0   2   2   0   1   2   0   0   0   1
   352.6s     0   0   0   2   0   1   1   1   1   0
   353.6s     1   3   1   2   1   0   0   0   1   1
   354.6s     4   0   0   0   2   1   0   1   0   1
   355.6s     1   0   1   0   1   1   0   0   0   0
   356.6s     0   0   1   0   2   0   0   0   9  14
   357.6s     3   6   4  35  33  23  39  21  40   8
   358.6s    28   3  14  13  17  19   9  28  11   2
   359.6s     3   1   1   2   2   0   0   1   0   0
   360.6s     0   0   1   1   0   0   0   2   0   1
   361.6s     0   0   0   0   0   0   2   3   1   1
   362.6s *   1   0   2   1  13  17  46   2   9  11
   363.6s    24  22   2   0   0   1   1   0   0   0
   364.6s     1   1   1   0   0   1   3   1   1   1
   365.6s     3   1   1   1   0   0   1  14  23  26
   366.6s    21  24   1   1   0   1   0   0   0   0
   367.6s     1   2   1   0   0   0   1   1   0   0
   368.6s     0   0   1   0   0   1   1   0   0   0
   369.6s     0   2   0   0   1   0   1   0   0   0
   370.6s     1   0   0   1   1   0   1   5  12   9
   371.6s    21  49  31  25  18  36  29  38  25  22
   372.6s     8  11  18  10  15   9  12   9   9   1
   373.6s     0   0   0   2   1   0   1   0   1  19
   374.6s *  14   0   8  14  20  20  21   5   0  13
   375.6s     0  15  26  25  19  28  20  20  41  10
   376.6s    16  12  16  12  33  15  12   9   2   0
   377.6s *   4   4   7  25  29  30  20  22  16   4
   378.6s    18   0  30  26  40  30  50  24   9   3
   379.6s *   1   8   1  11  12  39  20   3   1   1
   380.6s     0   1   0   2   0   0   1   0   1   1
   381.6s     1   1   0   0   1   0   0   1   1  14
   382.6s    21  22  11  27   6   0   0   0   0   0
   383.6s     3   1   0   0   0   0   0   0   0   0
   384.6s     1   0   2   3   0   1   0   0   2   1
   385.6s     0   1   0   0  11  31  44  22  36  17
   386.6s     9  12  30  37  16   9  31  15  10   0
   387.6s *   0  23  10  28  26  13  17  19  13  16
   388.6s    16   1   1   0   3   2   1   0   0   1
   389.6s     8  36  42   4   1   0   0   1   0   2
   390.6s     0   0   0   1   1   1   3   0   0   0
   391.6s     2   0   0   0   0   1   1   0   0   1
   392.6s     1   1   0   0   7   2   0   0   0   2
   393.6s     0   0   1   1   0   0   0   2   1   1
   394.6s     0   1   0   1   0   0   0   0   0   2
   395.6s     0   0   2   1   1   0   0   0   0   1
   396.6s     3  14  18  18  39  33  29  30  20  35
   397.6s    22  13  23  12   0   1   1   3  17  28
   398.6s    22   6  11  12  28  14   0   2   0   1
   399.6s     0   1   0   0   0   0   0   1   0   1
   400.6s     1   1   0   3   1   0   1   0   1   0
   401.6s     0   1   0   0   2   0   1   0   0   0
   402.6s     1   0   0   1   1   0   0   0   0   1
   403.6s     1   0   0   0   0   0   1   0   0   1
   404.6s     0   0   1   2   1   0   0   1   0   0
   405.6s     1   0   4  17  34  27  13   4  16   0
   406.6s *   0   0  19   1   8  25  18   4  12  23
   407.6s     1   1   0   0   0   0   1   0   1   0
   408.6s     0   0   1   0   0   0   0   1  10  32
   409.6s    28   1   1   0   1   0   2   0   1   0
   410.6s     0   1   0   2   0   1   0   0   3   1
   411.6s     2   1   0   1   0   0   0   0   1   0
   412.6s     3   4   1   0   3   2   1   1   2   3
   413.6s     1   1   3   1   2   3   3   2   1   1
   414.6s     6   5   4  16  17  19  32  23  16  34
   415.6s    11  24  24  47   6  34   3   4  14  22
   416.6s    29   1   6   4   2   2   1   2   1   2
   417.6s     1   1   1   2   2   1   2   0   2   3
   418.6s     2   3   3   2  11   8   9  26   4   2
   419.6s *   2  17  17  20  18  12  14  28  17  15
   420.6s    13  13  30  31  22  20  28  22  24   9
   421.6s *   4  11  15  14   8  17  10   2  18   4
   422.6s     2  23  15  23  19  13   9  12  28  11
   423.6s    18  14   4   6  11  18  12  26  10   6
   424.6s     3   4   2   2   3   3   2  17  41  25
   425.6s    45  24  33  11  15  36  27  14   4   3
   426.6s *  20  12  14  27  14   7  21  16   8   8
   427.6s     2   3   2   1   2   3   2   3   2   3
   428.6s     3   2   2   2   2   1   2   3   2   1
   429.6s     2   2   3   4  23  31  36  13   1   1
   430.6s     3   1   1   3   2   2   1   1   2   3
   431.6s     2   0   1   2   2   1   2   0   3   1
   432.6s     1   2   1   1   0   0   0   0   0   0
   433.6s     0   0   1   1   2   2   1   0   1   0
   434.6s     0   5  14  19  44  21   0  28  19  22
   435.6s *  30  32  21   7   3   3  20  13   4  11
   436.6s     4  11  22  19  13   4  15   8   0  17
   437.6s    12  24  30  26   6  29   0   2   1   0
   438.6s     2   1   1   0   1   0   1   0   0   0
   439.6s     0   0   1   0   2   1   1   0   2   8
   440.6s    34  23   7   1   1   0   0   1   2   3
   441.6s     2   1   0   2   1   1   1   1   0   0
   442.6s     3   0   0   1   1   0   1   0   0   0
   443.6s     0   0   1   0   2   0   0   0   1   0
   444.6s     0   2   0   2   0   1   0   0   0   2
   445.6s     2   2   3   1   0   1   1   0   0   2
   446.6s     1   0   2   0   0   0   2   1   0   3
   447.6s     0   1   1   1   0   0   0   1   0   1
   448.6s     1   0   0   0   1   1   1   0   0   0
   449.6s     0   1   1   0   1   0   0   0   0   1
   450.6s     1   0   0   0   1   1   0   1   0   0
   451.6s     2   0   0   2   0   1   1   1   0   1
   452.6s     0   0   0   1  11   1   0   2   1   4
   453.6s     2   0   0   1   0   0   0   0   0   0
   454.6s     0   1   1   3   0   0   0   1   1   1
   455.6s     0   3   2   2   0   0   0   0   0   1
   456.6s     3  13   9  26  23  31  22  19  26  16
   457.6s    12  21  20  22  28  28  24  20   1   3
   458.6s     3   3   1   3  11  21  17  24  26  27
   459.6s     1   3  12   9   2   0   0   2   0   1
   460.6s     1   1   1   4   2   1   1   1   1   0
   461.6s     1   0   0   2   0   0   1   1   1   0
   462.6s     0   0   0   0  11  16  16  40  23  10
   463.6s *   0   4   6  16  31  28   8   5  26   0
   464.6s     0   0   0   0   0   0   1   0   3   1
   465.6s     0   1   0   1   0   0   0   1   0   1
   466.6s     2   0   3   1   0   2   0   1   1   0
   467.6s     0   1   4  32  31  13  31  11   1   0
   468.6s     0   0   1   1   0   0   0   0   0   0
   469.6s     1   0   2   0   0   2   2   0   2   1
   470.6s     0   1   1   1   1   1   0   2   4   1
   471.6s     3   3   2   2   3   0   3   0   0   3
   472.6s     4   1   0   1   1   3   0   1   1   2
   473.6s     1   4   2   2   1   2   1   1   3   1
   474.6s     3   2   4   2   2   4   1   1   0  10
   475.6s    22  17  31  32  20  19  24  17  27  19
   476.6s    44  18  10  15  15  26   4  18   3   7
   477.6s     2   3   2   4   2   2   4   1   3   1
   478.6s     3   2   3   1   4   2   1  10  21   3
   479.6s *   7   2   2  10  12   5  17  25   8  11
   480.6s     4   2   3   3   6   3   4   3  29  19
   481.6s    16  14  21  11   4  13  17  22  24  11
   482.6s    31  49  19   7  22  16  10  33  17  27
   483.6s *  29  16  27  15  21  19   3   1   3   3
   484.6s     2   3  19  21  19  36  35  25  22  10
   485.6s    18  35  34  29  42  20  18   5   2   6
   486.6s *   5   4   9  32  25  10  18  12  20  25
   487.6s    17  14  21  19   7   7   1   3   2   1
   488.6s     2   0   2   2   2   3   2   3  19  35
   489.6s    17   1   2   1   1   2   1   0   1   3
   490.6s     2   0   4   2   2   1   2   1   2   2
   491.6s     1   3   2   7   3   3   3   3   2   1
   492.6s     0   1   1   2   1   2   0   2   1   0
   493.6s     2   0   0   1   0   2   3   1   1   2
   494.6s     2   3   0   1   0   3   1   4   4   1
   495.6s     1   2   1   1   2   1   2   1   1   2
   496.6s     4  24  19  32  43  28  23  26  24  22
   497.6s *  20  30  23  13  47  29   7   3   8  20
   498.6s    14  24   7  11  29  19  37  28  46  33
   499.6s     3   2   4   1   4   3   4   3   3   3
   500.6s     3   1   1   2   3   1   3   3   1   3
   501.6s     4   3   6  23  39   4  35  17   2   5
   502.6s     1   4   2   2   2   3   2   2   3   2
   503.6s     5   3   3   2   2   3   2   2   3   2
   504.6s     2   2   2   3   3   4   3   2   3   2
   505.6s     4   2   3   5   2   3   2   2   3   3
   506.6s     2   2   3   2   3   4   3   2   3   3
   507.6s     2   2   3   2   3   2   1   1   1   1
   508.6s     2   4   2   2   2   3   3   1   2   3
   509.6s     3   6   2   0   1   3   1   0   0   1
   510.6s     1   0   3   1   2   5   2   0   1   3
   511.6s     2   1   1   1   4   2   3   2   2   0
   512.6s     1   1   2   1   4   1   2   1   1   0
   513.6s     0   1   1   0   1   3   0   1   2   2
   514.6s     2   2   3   1   0   0   0   0   1   1
   515.6s     1   1   8  18  25  21  26  16  24  41
   516.6s    16  24  27  23  15   5   7  16  25  22
   517.6s    12  10  30  11  27  10  10   1   4   2
   518.6s     0   5   1   1   0   1   1   2   3   1
   519.6s     1   1   2   1   0   1   2   0   1   1
   520.6s     1   2   3   1   2   2   3   5   2   2
   521.6s     2   3   3   2   2   2   2   3   1   2
   522.6s     2   1   2   3   3   2   3   2   2   2
   523.6s     1   2   1   2   3   2   1   2   2   9
   524.6s *   9  28  30  23   3   1   3  24  18  17
   525.6s     8   2   0   1   3   3   5  21  29   1
   526.6s     1   0   1   3   2   4   3   1   3   4
   527.6s     3   5   1   1   1   0   2   1   0   0
   528.6s     1   0   1   0   1   2   0   1   5   9
   529.6s    28  32   2   0   0   1   0   1   1   0
   530.6s     0   0   0   1   2   0   1   1   1   0
   531.6s     2   0   1   0   1   0   0   0   1   0
   532.6s     3   0   3   1   1   1   1  22  29  15
   533.6s    36  10  36  27  31
```

### Post-query bursts

Window per sample: keystroke to max(rendered, opened) + 50 ms. Main thread busy 16700 ms over 50 windows.

#### Named areas in the bursts

| area | ms | % |
|---|---:|---:|
| AppKitPrototypeList | 2542 | 15.2 |
| MessageRowCellView | 1066 | 6.4 |
| Row presentation / formatters | 27 | 0.2 |
| ListWindowModel | 1536 | 9.2 |
| FrameBatcher | 211 | 1.3 |
| MessageListViewController | 2613 | 15.6 |
| SearchFieldController | 1524 | 9.1 |
| SearchFieldModel | 348 | 2.1 |
| Benchmark harness (runner, tick sampler) | 1835 | 11.0 |
| -[NSTableView reloadData] | 981 | 5.9 |
| NSTableView (any) | 3312 | 19.8 |
| NSView layout (layoutSubtreeIfNeeded) | 3038 | 18.2 |
| NSView display (displayIfNeeded / drawRect) | 481 | 2.9 |
| NSTextView insertText | 73 | 0.4 |
| NSTextCheckingController | 204 | 1.2 |
| Core Animation commit (CA::Transaction::commit) | 7264 | 43.5 |
| NSDisplayCycleFlush | 4653 | 27.9 |
| Swift concurrency job (completeTaskWithClosure) | 5020 | 30.1 |
| Rust core (sb_*) | 0 | 0.0 |

#### Heaviest main-thread call paths in the bursts

Distinct paths of the top-down call tree, each ending where no callee keeps 1% of burst time; the weight is the path's inclusive time. Leaf first, at most 30 frames (the run-loop frames nearest the root are cut when longer).

##### 1. 769 ms (4.6% of burst busy)

```
specialized AppKitPrototypeList.tableView(_:viewFor:row:) [atos 0x100a9fdcf]
@objc AppKitPrototypeList.tableView(_:viewFor:row:) [atos 0x100a9cc17]
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

##### 2. 663 ms (4.0% of burst busy)

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
main [atos 0x100a9575b]
start
```

##### 3. 341 ms (2.0% of burst busy)

```
-[NSViewBackingLayer display]
CA::Layer::display_if_needed(CA::Transaction*)
CA::Context::commit_transaction(CA::Transaction*, double, double*)
CA::Transaction::commit()
CA::Transaction::flush_as_runloop_observer(bool)
stepTransactionFlush
UC::DriverCore::continueProcessing()
__CFMachPortPerform
__CFRUNLOOP_IS_CALLING_OUT_TO_A_SOURCE1_PERFORM_FUNCTION__
__CFRunLoopDoSource1
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
main [atos 0x100a9575b]
start
```

##### 4. 301 ms (1.8% of burst busy)

```
-[NSView _setWindow:]
-[NSView removeFromSuperview]
MessageListViewController.detach()
MessageListViewController.install(_:ticket:)
closure #1 in MessageListViewController.show(_:) [atos 0x100b2e801]
<deduplicated_symbol> [atos 0x100a9b389]
<deduplicated_symbol> [atos 0x100b44fb9]
<deduplicated_symbol> [atos 0x100a9b389]
completeTaskWithClosure(swift::AsyncContext*, swift::SwiftError*)
```

##### 5. 296 ms (1.8% of burst busy)

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
main [atos 0x100a9575b]
start
```

##### 6. 279 ms (1.7% of burst busy)

```
0x19461e1bc
0x19461e13c
specialized static RunContext.displayID(_:) [atos 0x100b11b93]
RunContext.checkDisplay() [atos 0x100af6b27]
RunContext.tickDidFire(_:) [atos 0x100af6e1b]
closure #8 in ListBenchmarkRunner.runStore(controller:)
partial apply for closure #2 in ListBenchmarkRunner.runOnce(implementation:pass:generation:inheritedFailures:)
BenchmarkTickSampler.tickFired(_:)
@objc BenchmarkTickSampler.tickFired(_:) [atos 0x100acaff7]
-[_NSDisplayLinkForwarder displayLinkDidFire:]
CA::Display::DisplayLinkItem::dispatch_(CA::SignPost::Interval<(CA::SignPost::CAEventCode)835322056>&)
CA::Display::DisplayLink::dispatch_items(unsigned long long, unsigned long long, unsigned long long)
CA::Display::DisplayLink::dispatch_deferred_display_links(unsigned int)
stepTransactionFlush
UC::DriverCore::continueProcessing()
__CFMachPortPerform
__CFRUNLOOP_IS_CALLING_OUT_TO_A_SOURCE1_PERFORM_FUNCTION__
__CFRunLoopDoSource1
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
main [atos 0x100a9575b]
start
```

##### 7. 257 ms (1.5% of burst busy)

```
-[NSView _updateConstraintsForSubtreeIfNeededCollectingViewsWithInvalidBaselines:]
-[NSView _updateConstraintsForSubtreeIfNeededCollectingViewsWithInvalidBaselines:]
-[NSView _updateConstraintsForSubtreeIfNeededCollectingViewsWithInvalidBaselines:]
-[NSView _updateConstraintsForSubtreeIfNeededCollectingViewsWithInvalidBaselines:]
-[NSView _updateConstraintsForSubtreeIfNeededCollectingViewsWithInvalidBaselines:]
-[NSView _updateConstraintsForSubtreeIfNeededCollectingViewsWithInvalidBaselines:]
__82-[NSView _updateConstraintsForSubtreeIfNeededCollectingViewsWithInvalidBaselines:]_block_invoke
-[NSISEngine withBehaviors:performModifications:]
-[NSView _updateConstraintsForSubtreeIfNeededCollectingViewsWithInvalidBaselines:]
__45-[NSView updateConstraintsForSubtreeIfNeeded]_block_invoke_2
-[NSISEngine withBehaviors:performModifications:]
-[NSView(NSConstraintBasedLayoutInternal) _withAutomaticEngineOptimizationDisabled:]
__45-[NSView updateConstraintsForSubtreeIfNeeded]_block_invoke
NSPerformVisuallyAtomicChange
-[NSView updateConstraintsForSubtreeIfNeeded]
-[NSISEngine withBehaviors:performModifications:]
-[NSView(NSConstraintBasedLayoutInternal) _withAutomaticEngineOptimizationDisabled:]
-[NSWindow(NSConstraintBasedLayoutInternal) updateConstraintsIfNeeded]
__NSWindowGetDisplayCycleObserverForUpdateConstraints_block_invoke
NSDisplayCycleObserverInvoke
NSDisplayCycleFlush
CA::Transaction::run_commit_handlers(CATransactionPhase)
CA::Transaction::commit()
CA::Transaction::flush_as_runloop_observer(bool)
stepTransactionFlush
UC::DriverCore::continueProcessing()
__CFRUNLOOP_IS_CALLING_OUT_TO_A_SOURCE0_PERFORM_FUNCTION__
__CFRunLoopDoSource0
__CFRunLoopDoSources0
__CFRunLoopRun
... 11 more frames to the root
```

##### 8. 256 ms (1.5% of burst busy)

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

##### 9. 250 ms (1.5% of burst busy)

```
closure #1 in BenchmarkTickSampler.start(on:)
thunk for @escaping @callee_guaranteed (@guaranteed CFRunLoopObserverRef?, @unowned CFRunLoopActivity) -> () [atos 0x100acacd3]
__CFRUNLOOP_IS_CALLING_OUT_TO_AN_OBSERVER_CALLBACK_FUNCTION__
__CFRunLoopDoObservers
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
main [atos 0x100a9575b]
start
```

##### 10. 246 ms (1.5% of burst busy)

```
ListWindowModel.showProvisional(_:)
ListWindowModel.open() [atos 0x101eb0237]
MessageListViewController.install(_:ticket:)
closure #1 in MessageListViewController.show(_:) [atos 0x100b2e801]
<deduplicated_symbol> [atos 0x100a9b389]
<deduplicated_symbol> [atos 0x100b44fb9]
<deduplicated_symbol> [atos 0x100a9b389]
completeTaskWithClosure(swift::AsyncContext*, swift::SwiftError*)
```

##### 11. 236 ms (1.4% of burst busy)

```
UpdateKeyboardAwareShortcutsForCurrentInputSource
__35-[_NSMenuShortcutUpdater schedule:]_block_invoke
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
main [atos 0x100a9575b]
start
```

##### 12. 229 ms (1.4% of burst busy)

```
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
__36-[NSView _layoutSubtreeWithOldSize:]_block_invoke
NSPerformVisuallyAtomicChange
-[NSView _layoutSubtreeWithOldSize:]
__36-[NSView _layoutSubtreeWithOldSize:]_block_invoke
NSPerformVisuallyAtomicChange
... 49 more frames to the root
```

### CPU time by thread over the search phase (top 12)

| ms | thread |
|---:|---|
| 37299 | Main Thread (0x418a48a) (Sweetbriar, pid: 89282) |
| 8072 | tokio-rt-worker (0x418b1e3) (Sweetbriar, pid: 89282) |
| 3451 | tokio-rt-worker (0x418b904) (Sweetbriar, pid: 89282) |
| 3047 | tokio-rt-worker (0x418c946) (Sweetbriar, pid: 89282) |
| 2950 | tokio-rt-worker (0x418b8f0) (Sweetbriar, pid: 89282) |
| 2800 | tokio-rt-worker (0x418b8ef) (Sweetbriar, pid: 89282) |
| 2597 | tokio-rt-worker (0x418b90a) (Sweetbriar, pid: 89282) |
| 2489 | tokio-rt-worker (0x418af9a) (Sweetbriar, pid: 89282) |
| 2353 | tokio-rt-worker (0x418b909) (Sweetbriar, pid: 89282) |
| 2129 | tokio-rt-worker (0x418b1d3) (Sweetbriar, pid: 89282) |
| 2043 | tokio-rt-worker (0x418c957) (Sweetbriar, pid: 89282) |
| 1580 | tokio-rt-worker (0x418d353) (Sweetbriar, pid: 89282) |

### Top 40 Sweetbriar frames on the tokio workers (inclusive, search phase)

For context: the core's work off the main thread (46791 ms on all workers).

| ms | % of worker time | frame |
|---:|---:|---|
| 46743 | 99.9 | `<std::sys::thread::unix::Thread>::new::thread_start [atos 0x102592547]` |
| 46741 | 99.9 | `<std::thread::lifecycle::spawn_unchecked<<tokio::runtime::blocking::pool::Spawner>::spawn_thread::{closure#0}, ()>::{closure#1} as core::ops::function::FnOnc...` |
| 46693 | 99.8 | `std::sys::backtrace::__rust_begin_short_backtrace::<<tokio::runtime::blocking::pool::Spawner>::spawn_thread::{closure#0}, ()> [atos 0x10254ea47]` |
| 44412 | 94.9 | `sqlite3_step` |
| 30143 | 64.4 | `getPageNormal` |
| 29905 | 63.9 | `getAndInitPage [atos 0x1024097e7]` |
| 27641 | 59.1 | `<tokio::runtime::task::harness::Harness<tokio::runtime::blocking::task::BlockingTask<<sb_engine::engine::Engine>::read_through<sb_store::readers::Scoped<(sb_...` |
| 27607 | 59.0 | `<tokio::runtime::blocking::task::BlockingTask<<sb_engine::engine::Engine>::read_through<sb_store::readers::Scoped<(sb_store::query::counts::coverage::StoredS...` |
| 27607 | 59.0 | `<tokio::runtime::task::core::Core<tokio::runtime::blocking::task::BlockingTask<<sb_engine::engine::Engine>::read_through<sb_store::readers::Scoped<(sb_store:...` |
| 27528 | 58.8 | `<sb_engine::engine::Engine>::search_snapshot::<sb_store::lists::PageRead<sb_store::query::counts::paging::SearchPage>, <sb_engine::engine::Engine>::search_pa...` |
| 27528 | 58.8 | `<sb_store::readers::Readers>::read::<(sb_store::query::counts::coverage::StoredSearchCoverage, sb_store::lists::PageRead<sb_store::query::counts::paging::Sea...` |
| 26818 | 57.3 | `sb_store::query::counts::paging::read_search_relevance_page` |
| 25014 | 53.5 | `pcache1Fetch` |
| 23688 | 50.6 | `moveToChild` |
| 23228 | 49.6 | `<rusqlite::row::Rows as fallible_streaming_iterator::FallibleStreamingIterator>::advance [atos 0x1022fe92f]` |
| 21652 | 46.3 | `sqlite3BtreeTableMoveto` |
| 15787 | 33.7 | `sqlite3VdbeExec [atos 0x1023f40cb]` |
| 15155 | 32.4 | `<tokio::runtime::task::harness::Harness<tokio::runtime::blocking::task::BlockingTask<<sb_engine::engine::Engine>::read_through<sb_store::readers::Scoped<(sb_...` |
| 15133 | 32.3 | `<tokio::runtime::task::core::Core<tokio::runtime::blocking::task::BlockingTask<<sb_engine::engine::Engine>::read_through<sb_store::readers::Scoped<(sb_store:...` |
| 15130 | 32.3 | `<sb_store::readers::Readers>::read_scoped::<(sb_store::query::counts::coverage::StoredSearchCoverage, sb_store::lists::PageRead<(u64, alloc::vec::Vec<i64>)>)...` |
| 15130 | 32.3 | `<tokio::runtime::blocking::task::BlockingTask<<sb_engine::engine::Engine>::read_through<sb_store::readers::Scoped<(sb_store::query::counts::coverage::StoredS...` |
| 15099 | 32.3 | `<sb_engine::engine::Engine>::search_snapshot::<sb_store::lists::PageRead<(u64, alloc::vec::Vec<i64>)>, <sb_engine::engine::Engine>::search_window_ids::{closu...` |
| 14270 | 30.5 | `<sb_store::query::counts::Compiled>::read::<alloc::vec::Vec<i64>, sb_store::query::counts::paging::search_ids::{closure#1}>` |
| 14270 | 30.5 | `sb_store::query::counts::paging::search_ids [atos 0x1022dac4f]` |
| 14221 | 30.4 | `<core::iter::adapters::GenericShunt<rusqlite::row::MappedRows<sb_store::query::counts::paging::search_ids::{closure#1}::{closure#0}>, core::result::Result<co...` |
| 13816 | 29.5 | `sb_store::query::counts::paging::fetch` |
| 13514 | 28.9 | `<alloc::vec::Vec<i64> as alloc::vec::spec_from_iter::SpecFromIter<i64, core::iter::adapters::GenericShunt<rusqlite::row::MappedRows<sb_store::query::counts::...` |
| 13386 | 28.6 | `<rusqlite::row::Rows>::get_expected_row [atos 0x10228ce43]` |
| 9334 | 19.9 | `sqlite3BtreeIndexMoveto` |
| 9052 | 19.3 | `sqlite3VdbeExec [atos 0x1023f2cbf]` |
| 7887 | 16.9 | `<sb_store::query::counts::Compiled>::read::<sb_store::query::counts::paging::Streamed, sb_store::query::counts::paging::stream::{closure#1}>` |
| 7554 | 16.1 | `sb_store::query::counts::paging::stream::{closure#1} [atos 0x102279c53]` |
| 5861 | 12.5 | `sqlite3VdbeExec [atos 0x1023fb1df]` |
| 5856 | 12.5 | `sqlite3VdbeFinishMoveto [atos 0x1023fde9f]` |
| 5670 | 12.1 | `sb_store::query::counts::paging::read_page [atos 0x1022dd7ff]` |
| 5606 | 12.0 | `<sb_store::query::counts::Compiled>::read::<i64, sb_store::query::counts::paging::read_page::{closure#7}> [atos 0x102275a3f]` |
| 5403 | 11.5 | `<rusqlite::statement::Statement>::query_row::<i64, rusqlite::params::ParamsFromIter<&[rusqlite::types::value::Value]>, sb_store::query::counts::paging::read_...` |
| 5153 | 11.0 | `sb_store::query::counts::compile_driven [atos 0x10227d29f]` |
| 5153 | 11.0 | `sb_store::query::counts::drive::restrict` |
| 5053 | 10.8 | `sb_store::query::counts::paging::read_page [atos 0x1022dda6b]` |
