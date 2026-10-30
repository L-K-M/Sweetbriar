# Time Profiler run: 8086cb5, r4c3-certain-unknowns

Built commit `8086cb5e916f345d38fc6c49575467409c0b8bd2` ("Count certain
Search unknowns without joins"), Release, Mac mini M1 (Macmini9,1, 8
GiB), macOS 26.5.2, 60 Hz, 100,000-message synthetic archive. Recorded
2026-10-30 with `xcrun xctrace record --template 'Time Profiler'
--launch` over `-runSearchBenchmark`; the report is `profiled.json`
(`ok: true`, 50 of 50 samples).

The profiler distorts the timings: this run's p95 is 7,156 ms against
525 and 758 ms in the timed runs. Read the numbers below for where time
goes, not for how long a search takes.

## How this was produced

The method is the same as for
`bench-results/20261029T212655Z-30888e0-main-profile-profile.md`, and
that file has the script and the demangler source. The benchmark runner
(`apps/macos/Sweetbriar/ListBenchmark/`) is unchanged since `30888e0`,
so the script's keystroke markers still apply: 50 timed keystrokes
were found, matching the report's 50 samples.

- `xctrace` again reported "The log archive is corrupt or incomplete"
  and exited 2. It saved the trace, but without signposts.
- The trace was re-symbolicated with `xctrace symbolicate --dsym`
  against the build's dSYMs. The remaining Sweetbriar addresses were
  resolved with `atos` (tagged `[atos 0x…]`) and Rust symbols
  demangled.
- Time Profiler samples running threads only, at 1 ms.
- Sweetbriar was brought to the front about 5 s after `xctrace` launched
  it, and the run didn't lose the foreground. The stopped copy that
  `xctrace --launch` leaves behind was killed after the recording.

## Findings

Compared with the `30888e0` profile from 2026-10-29. The builds differ
by more than this request's commit: the reader cache (`2639cb7`, merged
in `f67fa83`) and the sidebar search-generation fixes (`5f28c50`) also
landed in between.

1. **The core now reads pages from its own cache.** On the tokio
   workers, `pcache1Fetch` holds 51.4% of samples and
   `unixRead` (`pread`) 14.6%, against 16.9% and 55.3% at `30888e0`.
   That's the 64 MiB per-reader page cache at work.
2. **The sidebar recount is gone from the search phase.**
   `sb_store::sidebar::mailboxes` took 23.5% of worker time at
   `30888e0`. Here it isn't among the top 40 worker frames, and
   `MainWindowController.reloadSidebar()` isn't among the top 40
   Sweetbriar frames on the main thread.
3. **What the workers do now:**
   - `paging::read_search_relevance_page` takes 48.4% of worker time.
   - `paging::search_ids` takes 28.2% and `paging::fetch` 25.9%.
   - `sb_store::lists::list_ids` (`Engine::window_ids`) takes 19.6%:
     it loads the id list for each opened list, including the Archive
     list the benchmark returns to between samples.
   The certain-unknowns counting this commit changed doesn't show up
   as a separate heavy frame.
4. **The main thread is on CPU for 7.1% of the search phase** (34.4 s
   of 484.5 s), against 11.3% at `30888e0`, and the Rust core never runs
   on it. Its time splits much as before:
   - Core Animation commit 49.0%, with layout inside it (`NSView`
     layout 21.9%, `NSTableView` 19.5%);
   - `MessageListViewController` 14.3%;
   - `AppKitPrototypeList` 12.4% (`MessageRowCellView` 7.5%);
   - `SearchFieldController` 7.9%;
   - `ListWindowModel` 4.9%; `FrameBatcher` 1.0%; row formatters 0.2%.
5. **The timed keystroke's `insertText`** is on CPU for 12.9 ms on
   average. Its first and last samples are at most 407 ms apart,
   against 835 ms at `30888e0`.

---

## Generated analysis: main thread, search phase

Samples: 104776 total, 39621 on the main thread (1 ms sampling, running threads only).
Timed keystrokes found: 50; report samples: 50.

Search phase: 30.398 s to 514.919 s of the trace (484.5 s wall); main thread busy 34383 ms (7.1% of wall).

### Top 40 frames by self time (main thread, search phase)

| # | ms | % of main busy | frame |
|---:|---:|---:|---|
| 1 | 1856 | 5.4 | `objc_msgSend` |
| 2 | 716 | 2.1 | `getMethodNoSuper_nolock(objc_class*, objc_selector*)` |
| 3 | 616 | 1.8 | `<deduplicated_symbol>` |
| 4 | 543 | 1.6 | `__CFStringHash` |
| 5 | 511 | 1.5 | `_xzm_xzone_malloc_tiny` |
| 6 | 497 | 1.4 | `_platform_strcmp$VARIANT$Base` |
| 7 | 451 | 1.3 | `mach_msg2_trap` |
| 8 | 359 | 1.0 | `protocol_conformsToProtocol_nolock(protocol_t*, protocol_t*)` |
| 9 | 333 | 1.0 | `objc_retain` |
| 10 | 290 | 0.8 | `__kdebug_trace64` |
| 11 | 273 | 0.8 | `list_array_tt<unsigned long, protocol_list_t, RawPtr>::iteratorImpl<false>::skipEmptyLists()` |
| 12 | 272 | 0.8 | `objc_class::demangledName(bool)` |
| 13 | 251 | 0.7 | `CFEqual` |
| 14 | 245 | 0.7 | `_class_conformsToProtocol_unrealized_nolock(objc_class*, Protocol*)` |
| 15 | 232 | 0.7 | `_platform_memcmp` |
| 16 | 211 | 0.6 | `objc_release` |
| 17 | 206 | 0.6 | `__CF_IS_OBJC` |
| 18 | 192 | 0.6 | `<no backtrace>` |
| 19 | 186 | 0.5 | `weak_entry_for_referent` |
| 20 | 182 | 0.5 | `__CFSearchStringROM` |
| 21 | 171 | 0.5 | `_xzm_free` |
| 22 | 156 | 0.5 | `___CFBasicHashFindBucket_Linear` |
| 23 | 155 | 0.5 | `_xzm_xzone_malloc_from_freelist_chunk` |
| 24 | 152 | 0.4 | `_platform_memmove` |
| 25 | 117 | 0.3 | `objc_autorelease` |
| 26 | 115 | 0.3 | `_platform_strlen` |
| 27 | 112 | 0.3 | `_xzm_xzone_malloc_freelist_outlined` |
| 28 | 110 | 0.3 | `objc::StringHashTable::tryGetIndex(char const*) const` |
| 29 | 109 | 0.3 | `objc::ObjectHashTable::forEachObject(char const*, void (unsigned long long, unsigned short, bool&) block_pointer) const` |
| 30 | 108 | 0.3 | `swift_retain` |
| 31 | 107 | 0.3 | `ItemVariationStore::ValueForDeltaSet(unsigned short, unsigned short, unsigned long, short const*, double const*) const` |
| 32 | 100 | 0.3 | `search_method_list(method_list_t const*, objc_selector*)` |
| 33 | 93 | 0.3 | `_Block_copy` |
| 34 | 84 | 0.2 | `AutoreleasePoolPage::add(objc_object*)` |
| 35 | 83 | 0.2 | `list_array_tt<unsigned long, protocol_list_t, RawPtr>::iteratorImpl<false>::iteratorImpl(list_array_tt<unsigned long, protocol_list_t, RawPtr>::ListIterator,...` |
| 36 | 83 | 0.2 | `_platform_memset` |
| 37 | 78 | 0.2 | `__CFStringEqual` |
| 38 | 75 | 0.2 | `probeGC` |
| 39 | 75 | 0.2 | `objc_loadWeakRetained` |
| 40 | 75 | 0.2 | `os_unfair_lock_lock` |

### Top 40 frames by inclusive time (main thread, search phase)

| # | ms | % of main busy | frame |
|---:|---:|---:|---|
| 1 | 25645 | 74.6 | `main [atos 0x1042f575b]` |
| 2 | 25645 | 74.6 | `start` |
| 3 | 25645 | 74.6 | `-[NSApplication run]` |
| 4 | 25635 | 74.6 | `-[NSApplication(NSEventRouting) nextEventMatchingMask:untilDate:inMode:dequeue:]` |
| 5 | 25635 | 74.6 | `-[NSApplication(NSEventRouting) _nextEventMatchingEventMask:untilDate:inMode:dequeue:]` |
| 6 | 25596 | 74.4 | `_DPSNextEvent` |
| 7 | 25594 | 74.4 | `_DPSBlockUntilNextEventMatchingListInMode` |
| 8 | 25594 | 74.4 | `_BlockUntilNextEventMatchingListInMode` |
| 9 | 25591 | 74.4 | `ReceiveNextEventCommon` |
| 10 | 25511 | 74.2 | `RunCurrentEventLoopInMode` |
| 11 | 25385 | 73.8 | `_CFRunLoopRunSpecificWithOptions` |
| 12 | 25070 | 72.9 | `__CFRunLoopRun` |
| 13 | 18361 | 53.4 | `UC::DriverCore::continueProcessing()` |
| 14 | 18015 | 52.4 | `stepTransactionFlush` |
| 15 | 16844 | 49.0 | `CA::Transaction::commit()` |
| 16 | 16779 | 48.8 | `CA::Transaction::flush_as_runloop_observer(bool)` |
| 17 | 13357 | 38.8 | `__CFRunLoopDoSources0` |
| 18 | 13310 | 38.7 | `__CFRunLoopDoSource0` |
| 19 | 13309 | 38.7 | `__CFRUNLOOP_IS_CALLING_OUT_TO_A_SOURCE0_PERFORM_FUNCTION__` |
| 20 | 11133 | 32.4 | `0x19469f4a4` |
| 21 | 11115 | 32.3 | `0x19469f47c` |
| 22 | 10823 | 31.5 | `CA::Transaction::run_commit_handlers(CATransactionPhase)` |
| 23 | 10737 | 31.2 | `NSDisplayCycleFlush` |
| 24 | 10697 | 31.1 | `NSDisplayCycleObserverInvoke` |
| 25 | 8544 | 24.8 | `completeTaskWithClosure(swift::AsyncContext*, swift::SwiftError*)` |
| 26 | 8342 | 24.3 | `NSPerformVisuallyAtomicChange` |
| 27 | 8009 | 23.3 | `<deduplicated_symbol> [atos 0x1042fb389]` |
| 28 | 8009 | 23.3 | `<deduplicated_symbol> [atos 0x1043a4fb9]` |
| 29 | 7537 | 21.9 | `-[NSView layoutSubtreeIfNeeded]` |
| 30 | 7531 | 21.9 | `-[NSView _layoutSubtreeIfNeededAndAllowTemporaryEngine:]` |
| 31 | 7531 | 21.9 | `__56-[NSView _layoutSubtreeIfNeededAndAllowTemporaryEngine:]_block_invoke` |
| 32 | 7490 | 21.8 | `-[NSWindow(NSConstraintBasedLayoutInternal) layoutIfNeeded]` |
| 33 | 7467 | 21.7 | `-[NSView _layoutSubtreeWithOldSize:]` |
| 34 | 7466 | 21.7 | `__36-[NSView _layoutSubtreeWithOldSize:]_block_invoke` |
| 35 | 7385 | 21.5 | `_NSViewLayout` |
| 36 | 7360 | 21.4 | `___NSViewLayout_block_invoke` |
| 37 | 7343 | 21.4 | `-[NSWindow(NSConstraintBasedLayoutInternal) _layoutViewTree]` |
| 38 | 7296 | 21.2 | `0x194664d5c` |
| 39 | 7293 | 21.2 | `0x194664178` |
| 40 | 7246 | 21.1 | `__NSWindowGetDisplayCycleObserverForLayout_block_invoke` |

### Top 40 Sweetbriar frames by inclusive time (main thread, search phase)

Frames from the Sweetbriar and SweetbriarBridge binaries (the Rust core is linked into SweetbriarBridge).

| # | ms | % of main busy | frame |
|---:|---:|---:|---|
| 1 | 25645 | 74.6 | `main [atos 0x1042f575b]` |
| 2 | 8009 | 23.3 | `<deduplicated_symbol> [atos 0x1042fb389]` |
| 3 | 8009 | 23.3 | `<deduplicated_symbol> [atos 0x1043a4fb9]` |
| 4 | 4144 | 12.1 | `<deduplicated_symbol> [atos 0x1042fae55]` |
| 5 | 3691 | 10.7 | `closure #1 in MessageListViewController.show(_:) [atos 0x10438e801]` |
| 6 | 2740 | 8.0 | `MessageListViewController.install(_:ticket:)` |
| 7 | 2614 | 7.6 | `@objc AppKitPrototypeList.tableView(_:viewFor:row:) [atos 0x1042fcc17]` |
| 8 | 2480 | 7.2 | `closure #1 in AppDelegate.startBenchmark(mode:reportPath:)` |
| 9 | 2111 | 6.1 | `specialized AppKitPrototypeList.tableView(_:viewFor:row:) [atos 0x1042ffdcf]` |
| 10 | 1798 | 5.2 | `SearchFieldController.applyModel()` |
| 11 | 1781 | 5.2 | `ListBenchmarkRunner.runSearch(controller:)` |
| 12 | 1656 | 4.8 | `closure #1 in closure #2 in implicit closure #1 in SearchFieldController.observeModel()` |
| 13 | 1030 | 3.0 | `SearchFieldModel.updateText(_:composition:)` |
| 14 | 1028 | 3.0 | `closure #4 in MessageListViewController.init(bridge:sidebar:) [atos 0x10438d10f]` |
| 15 | 1012 | 2.9 | `closure #2 in AppKitPrototypeList.init(model:formatter:accountMarker:selectionLoss:onIndexChange:) [atos 0x1042fbaab]` |
| 16 | 998 | 2.9 | `specialized reset #1 (scope:) in ListBenchmarkRunner.runSearch(controller:) [atos 0x104373953]` |
| 17 | 983 | 2.9 | `ListWindowModel.open()` |
| 18 | 981 | 2.9 | `@objc BenchmarkTickSampler.tickFired(_:) [atos 0x10432aff7]` |
| 19 | 972 | 2.8 | `SearchFieldController.navigated(to:)` |
| 20 | 969 | 2.8 | `BenchmarkTickSampler.tickFired(_:)` |
| 21 | 968 | 2.8 | `@nonobjc NSButton.__allocating_init(title:target:action:)` |
| 22 | 884 | 2.6 | `MessageListViewController.detach()` |
| 23 | 698 | 2.0 | `partial apply for closure #2 in ListBenchmarkRunner.runOnce(implementation:pass:generation:inheritedFailures:)` |
| 24 | 697 | 2.0 | `closure #8 in ListBenchmarkRunner.runStore(controller:)` |
| 25 | 668 | 1.9 | `RunContext.tickDidFire(_:) [atos 0x104356e1b]` |
| 26 | 646 | 1.9 | `closure #6 in ListBenchmarkRunner.runSearch(controller:)` |
| 27 | 620 | 1.8 | `RunContext.checkDisplay() [atos 0x104356b27]` |
| 28 | 521 | 1.5 | `specialized static RunContext.displayID(_:) [atos 0x104371b93]` |
| 29 | 493 | 1.4 | `closure #3 in MainWindowController.install(_:) [atos 0x10437d6bf]` |
| 30 | 430 | 1.3 | `specialized reset #1 (scope:) in ListBenchmarkRunner.runSearch(controller:) [atos 0x104374b6b]` |
| 31 | 415 | 1.2 | `SearchFieldController.refreshScopeMenu()` |
| 32 | 366 | 1.1 | `AppKitPrototypeList.init(model:formatter:accountMarker:selectionLoss:onIndexChange:)` |
| 33 | 350 | 1.0 | `MessageRowCellView.init(showsAccountMarker:) [atos 0x1042fe1c7]` |
| 34 | 327 | 1.0 | `ReaderViewController.resetSelection(_:) [atos 0x1043983bb]` |
| 35 | 323 | 0.9 | `closure #1 in ListWindowModel.queue(_:)` |
| 36 | 321 | 0.9 | `specialized closure #1 in FrameBatcher.post(_:) [atos 0x105920107]` |
| 37 | 319 | 0.9 | `specialized FrameBatcher.enqueue(_:) [atos 0x105911da7]` |
| 38 | 308 | 0.9 | `thunk for @escaping @callee_guaranteed (@guaranteed CFRunLoopObserverRef?, @unowned CFRunLoopActivity) -> ()` |
| 39 | 297 | 0.9 | `closure #1 in AppKitPrototypeList.init(model:formatter:accountMarker:selectionLoss:onIndexChange:)` |
| 40 | 297 | 0.9 | `ListWindowModel.deliver(_:) [atos 0x10591ef9b]` |

### Top 40 Sweetbriar frames by attributed self time (main thread, search phase)

Each sample is charged to its leaf-most Sweetbriar frame, so system work a Sweetbriar call causes counts against that call.

| # | ms | % of main busy | frame |
|---:|---:|---:|---|
| 1 | 21561 | 62.7 | `main [atos 0x1042f575b]` |
| 2 | 1007 | 2.9 | `closure #2 in AppKitPrototypeList.init(model:formatter:accountMarker:selectionLoss:onIndexChange:) [atos 0x1042fbaab]` |
| 3 | 905 | 2.6 | `@nonobjc NSButton.__allocating_init(title:target:action:)` |
| 4 | 862 | 2.5 | `MessageListViewController.detach()` |
| 5 | 806 | 2.3 | `SearchFieldController.navigated(to:)` |
| 6 | 594 | 1.7 | `closure #6 in ListBenchmarkRunner.runSearch(controller:)` |
| 7 | 520 | 1.5 | `specialized static RunContext.displayID(_:) [atos 0x104371b93]` |
| 8 | 430 | 1.3 | `specialized reset #1 (scope:) in ListBenchmarkRunner.runSearch(controller:) [atos 0x104374b6b]` |
| 9 | 405 | 1.2 | `SearchFieldController.refreshScopeMenu()` |
| 10 | 350 | 1.0 | `MessageRowCellView.init(showsAccountMarker:) [atos 0x1042fe1c7]` |
| 11 | 327 | 1.0 | `ReaderViewController.resetSelection(_:) [atos 0x1043983bb]` |
| 12 | 270 | 0.8 | `BadgeView.init(frame:)` |
| 13 | 256 | 0.7 | `MessageRowCellView.init(showsAccountMarker:) [atos 0x1042fe1e7]` |
| 14 | 243 | 0.7 | `ListBenchmarkRunner.runSearch(controller:)` |
| 15 | 233 | 0.7 | `MessageListViewController.install(_:ticket:) [atos 0x10438f343]` |
| 16 | 197 | 0.6 | `AppKitPrototypeList.buildTable()` |
| 17 | 183 | 0.5 | `@nonobjc NSScrollView.init()` |
| 18 | 183 | 0.5 | `MessageRowCellView.init(showsAccountMarker:) [atos 0x1042fda3f]` |
| 19 | 152 | 0.4 | `MessageRowFormatter.dateString(_:now:) [atos 0x1043925a7]` |
| 20 | 152 | 0.4 | `@objc MessageRowCellView.isFlipped.getter [atos 0x1042ff50b]` |
| 21 | 149 | 0.4 | `closure #1 in closure #1 in static SystemProbes.physicalFootprint()` |
| 22 | 146 | 0.4 | `MessageRowCellView.configure(content:)` |
| 23 | 142 | 0.4 | `@nonobjc NSTableColumn.init(identifier:)` |
| 24 | 119 | 0.3 | `MessageRowCellView.init(showsAccountMarker:) [atos 0x1042fdb23]` |
| 25 | 118 | 0.3 | `closure #1 in SearchFieldController.rebuildChips()` |
| 26 | 103 | 0.3 | `MessageRowCellView.init(showsAccountMarker:) [atos 0x1042fe2cf]` |
| 27 | 96 | 0.3 | `closure #1 in MessageRowCellView.applyLayerColors()` |
| 28 | 94 | 0.3 | `@nonobjc NSTableView.init()` |
| 29 | 90 | 0.3 | `MessageListViewController.updateStatus()` |
| 30 | 88 | 0.3 | `MessageRowCellView.init(showsAccountMarker:) [atos 0x1042fe2af]` |
| 31 | 80 | 0.2 | `SearchFieldController.rebuildChips()` |
| 32 | 78 | 0.2 | `MessageListViewController.install(_:ticket:)` |
| 33 | 73 | 0.2 | `specialized SearchCoverageNote.init(coverage:excludedUnknown:locale:timeZone:) [atos 0x1059347a7]` |
| 34 | 71 | 0.2 | `AppKitPrototypeList.pagesDidLand(_:)` |
| 35 | 64 | 0.2 | `SearchFieldController.refreshStatus()` |
| 36 | 64 | 0.2 | `closure #1 in BenchmarkTickSampler.start(on:)` |
| 37 | 63 | 0.2 | `@objc ClauseChipButton.init(frame:) [atos 0x1043a1aff]` |
| 38 | 60 | 0.2 | `closure #3 in ListBenchmarkRunner.runSearch(controller:)` |
| 39 | 59 | 0.2 | `@objc MessageRowCellView.isFlipped.getter [atos 0x1042ff4ff]` |
| 40 | 57 | 0.2 | `MessageRowFormatter.dateString(_:now:) [atos 0x104392657]` |

### Named areas, main thread, search phase

Inclusive: a sample counts for every area with a matching frame on its stack.

| area | ms | % |
|---|---:|---:|
| AppKitPrototypeList | 4280 | 12.4 |
| MessageRowCellView | 2588 | 7.5 |
| Row presentation / formatters | 66 | 0.2 |
| ListWindowModel | 1685 | 4.9 |
| FrameBatcher | 330 | 1.0 |
| MessageListViewController | 4905 | 14.3 |
| SearchFieldController | 2717 | 7.9 |
| SearchFieldModel | 1199 | 3.5 |
| Benchmark harness (runner, tick sampler) | 3755 | 10.9 |
| -[NSTableView reloadData] | 927 | 2.7 |
| NSTableView (any) | 6692 | 19.5 |
| NSView layout (layoutSubtreeIfNeeded) | 7537 | 21.9 |
| NSView display (displayIfNeeded / drawRect) | 1204 | 3.5 |
| NSTextView insertText | 54 | 0.2 |
| NSTextCheckingController | 383 | 1.1 |
| Core Animation commit (CA::Transaction::commit) | 16844 | 49.0 |
| NSDisplayCycleFlush | 10737 | 31.2 |
| Swift concurrency job (completeTaskWithClosure) | 8544 | 24.8 |
| Rust core (sb_*) | 4 | 0.0 |

### The timed keystroke itself

Main-thread samples inside the timed keystroke's `post` closure (`-[NSTextView insertText:]` of the last character, synchronous). `span` runs from its first to its last sample; on-CPU time below `span` means the main thread was off CPU inside the call.

| sample | rendered ms | span ms | on-CPU ms |
|---|---:|---:|---:|
| exact-reference r0 | 237 | 50 | 19 |
| exact-address r0 | 991 | 32 | 18 |
| phrase r0 | 1062 | 111 | 11 |
| scoped-filter r0 | 668 | 107 | 15 |
| negative-body r0 | 2161 | 41 | 6 |
| exact-reference r1 | 330 | 35 | 12 |
| exact-address r1 | 1885 | 15 | 6 |
| phrase r1 | 3583 | 17 | 10 |
| scoped-filter r1 | 2843 | 74 | 15 |
| negative-body r1 | 3367 | 148 | 18 |
| exact-reference r2 | 537 | 55 | 13 |
| exact-address r2 | 788 | 72 | 10 |
| phrase r2 | 708 | 39 | 10 |
| scoped-filter r2 | 1311 | 8 | 2 |
| negative-body r2 | 3130 | 113 | 22 |
| exact-reference r3 | 826 | 44 | 8 |
| exact-address r3 | 1229 | 53 | 9 |
| phrase r3 | 1152 | 86 | 15 |
| scoped-filter r3 | 1590 | 56 | 13 |
| negative-body r3 | 1414 | 21 | 11 |
| exact-reference r4 | 556 | 30 | 5 |
| exact-address r4 | 688 | 34 | 10 |
| phrase r4 | 2521 | 11 | 6 |
| scoped-filter r4 | 1966 | 48 | 14 |
| negative-body r4 | 3099 | 44 | 12 |
| exact-reference r5 | 542 | 76 | 19 |
| exact-address r5 | 1591 | 71 | 11 |
| phrase r5 | 2251 | 21 | 10 |
| scoped-filter r5 | 1933 | 42 | 3 |
| negative-body r5 | 2587 | 55 | 10 |
| exact-reference r6 | 1661 | 24 | 9 |
| exact-address r6 | 2138 | 120 | 22 |
| phrase r6 | 2559 | 120 | 6 |
| scoped-filter r6 | 1842 | 148 | 13 |
| negative-body r6 | 4015 | 66 | 12 |
| exact-reference r7 | 3378 | 187 | 40 |
| exact-address r7 | 7164 | 26 | 6 |
| phrase r7 | 9522 | 134 | 8 |
| scoped-filter r7 | 7156 | 63 | 9 |
| negative-body r7 | 5147 | 41 | 14 |
| exact-reference r8 | 3189 | 150 | 16 |
| exact-address r8 | 3372 | 55 | 17 |
| phrase r8 | 2736 | 74 | 15 |
| scoped-filter r8 | 4387 | 251 | 25 |
| negative-body r8 | 4655 | 54 | 13 |
| exact-reference r9 | 4748 | 407 | 33 |
| exact-address r9 | 3062 | 89 | 16 |
| phrase r9 | 3003 | 62 | 18 |
| scoped-filter r9 | 2008 | 24 | 6 |
| negative-body r9 | 2704 | 251 | 17 |

### Main-thread busy ms per 100 ms bucket after each timed keystroke

Column k is [k*100, (k+1)*100) ms after the keystroke; `rendered` is the report's keystroke-to-rows time for that sample.

```
sample                    rendered   0   1   2   3   4   5   6   7   8   9  10  11  12  13  14  15  16  17  18  19  20  21  22  23  24  25  26  27  28  29  30  31  32  33  34  35  36  37  38  39
exact-reference r0             237  31  19  41  10  20  17  21  36  34  18  22  16   9  10  16  12  10  11  11  15  16  29  17  15  10   7   5  15  11   7   9  40  16  14  18  30   5  18  23  37
exact-address r0               991  35  15  16  22  22  11  26  26  23  39   7   9  25  13   3  16  35  15  16   4   6   3  10  26  29  15   9  14   5   7  22  15  73  32  17  18  16   1   0   1
phrase r0                     1062   7  30  19  46   0   0   0   2   0   1  10   8  24  25   8  14  18  26  26  16  25  11   4   7   4   9   6   3   0   5   2  14   7  33  14  12  10  15   0  10
scoped-filter r0               668  13  36   6  26  36  23  53   0   1   0   0   0   1   1   0   0   2   0   2   0   0   1   0   1   1   1   1   1   0   1   0   0   1   1   1   1   0   1   1   0
negative-body r0              2161  17  20  11   4   2   3   1   0   0   3   1   1   1   2   1   3   2   1   2  11  10  20  33  17   3   2   1   3   2   0   4   2   3   3   3   2   1   3   2   4
exact-reference r1             330  21  29  13  34  41  40  37  13   4   3  20  19  21  34  15  24   8   3   3  17   9   3   2   3   4   3  22  44  54  13  42  39  46  36   5   2   9   3  25   2
exact-address r1              1885  27  16  40  18  14  23   3   3   3  22   3   3   2   4   3   3  45  53  30  11  58  49  34  18   3   3   8  13  15   6  20  10   3   4   2   3   4   2   8  11
phrase r1                     3583  25   2  16  17   4   3   3   3   4   2   1  16   8   2   2   2   2   3   2   3  11  33   4   3   2   2   1   3   2   2   3  17  23   8  12  11   4  11   0   2
scoped-filter r1              2843  17   8  11  15  15   7  14  19   4  15  19   2   6   4  10  11  14  27   9   9  18  20   5   8   9  21  19  35   4   0   1   1   1   0   0   1   2   1   3   0
negative-body r1              3367  15  10   8   4   7  14   5   7   6   0   1  18  17   4   8   6   3   0   1   1   0   0   2   3   1   3  18  26  10   1  13  19  10  19   0   2   1   0   0   0
exact-reference r2             537  18   4  19  21  17  33  21  18  11  27  17  22  12   8  14  32  19  15  10  10  10  13   5   6  18  26  27  40  15   1   0  15  41  15  20  40  25  32  11   1
exact-address r2               788  18   8  33  34  35   3   1   9  30  30  13  20  45  33  19   0  12   7  12   8  15  36  25  17  56   5  10   4  16  20  37   4   2   1  27  36   0   1   1   0
phrase r2                      708  16  27  30   2   1   2  50  12   1   0   1   0   0   1   0   0   0   0   0   0   0   0   0   0   1   2  42  51  36  30  13   4  19  30  10  10  10  36   8   6
scoped-filter r2              1311  29  20  33   1   1   1   6  11  13  24   5   8  13   4   1   0   0   0   2   0   1   1   0   0   1   0   1   0   1   0   1   0   1   3   0   0   0   1   0   1
negative-body r2              3130  14  26   9  19   8   2   0   1   0   0   0   1   0   0   0   0   0   0   0   0   2  13  27   4   0   0   0   0   2   2   7  23  42  22   2   4   1   2   3   1
exact-reference r3             826  17  35   7   1   1   0   2   0   9  19  10  28  26  26  33  27  27  22   7   3   1   5  13  12  16   7  22  33  32   1   0   1   0  10  46  55  31  28  42  28
exact-address r3              1229  16   6  18  10  18  31  37   5   0   1   0   1  41  40  45  33  25  48  18   3   8   5   6  29   1  21  18   5  23   3   1   1   1   3  19  30  10  29   8   1
phrase r3                     1152  18  15  12  20   9  11  17   2   2   0   3   4  21  32  20  20   0   1   0   0   0   2   0   0   0   1   0   0   0   1   2   1   0   0   0   2   0   1   1   2
scoped-filter r3              1590  21  15   9   8  20  31  20   0  14  26  28  44  11   0   0   8  39  12   1   0   0   0   0   0   1   0   0   1   0   1   0   1   1   1   0   1   0   0   0   0
negative-body r3              1414  20  26  16   4  15   2   4   2   2   2   2   2  21  59   6   3   3   2   9   5   3   1   3   7  14   2   2   3   2   2   3   2   2   2   2   2   2   2   2   2
exact-reference r4             556  23  12   9   3   3  26  19  16  53  28  32  21   3   1   4  11  13  35  39   2   3  25  27  28  45  44   6  10   2  14  12   7  15  16   2   2   3   3   3   3
exact-address r4               688  12  10  38  32  10   3  12  39  20  48  49   7  10   2  10  14   9  13  18   2   1   4   2   4   3   2   3   3   2   0   1   1   3   2   5  19   2   4   1  17
phrase r4                     2521  15  12   9  18  10   2   3   2   3   4   2   3   2   3   2   0   2   1   2   2  19   5   4   2   2  42  15   2   2   2   1   1   2   1   2   0   3   3   1  11
scoped-filter r4              1966  17  25  26  28   1   0   0   5   1   2   1   0   1   0   3   0   2   0   2   7  17  21   3  13   0   0   0   0   0   1   0   0   1   0   1   1   1   0   1   0
negative-body r4              3099  23  29  14  15   1   0   1   2   0   1   1   1   0   1   1   0  20   1   0   2   0   8   2   0   0   0   1   1   0   0   8   5   4   9   3   4  31  17   1   1
exact-reference r5             542  23  12  34   6   8  25  15  21  16  15  12  49  23  12  27  31  19  16   6   1   4  13  20   7  27  23  33  12   6   3  27   1   2  18  17   8  48  22  29   7
exact-address r5              1591  22  11  10  30  21  33  10   4   7  23   1   3  27  10  12  51  20  24   5   2  10  20  22  20  13  12  16  18  12  15   1   0  27  14   3   8  10   3   2   0
phrase r5                     2251  26  12   5  13   2   3   2   1   1   0   2   0   1   0   1   1   0   0   1   0   2   1  19  48   1   0   1   1   1   0   0   0   0   8  34  27  24  10  24  25
scoped-filter r5              1933   8  15   5  14  13  13  10   9   9  16  26  13  13  28   7  12  20  33  26   4   0   0   0   0   1   0   0   1   1   0   0   8   0   0   0   5  19   1   0   0
negative-body r5              2587  16  15  28   8   2   1   1   4   0   1   0   0   1   1   0   3  10   0   0   1   0   0   1   1   0   8  10  34  24   3   1   0   1   0   0   1   0   0   0   0
exact-reference r6            1661  19  18  26   3   1   1   5   1   1   3   2   2   3  25  12  22  20  35  41  19  17  18  21  18  11   9   4  22   6   9  18  11   1   1   2   8  22  28  43  20
exact-address r6              2138  14  21  33  38  22  17  23  18   4   3   3   2   2   7  24   2   2   2   3  50  54  36  25   3  54  12   5   6  13  25   3   3   9   8  11  28  10   2   1  11
phrase r6                     2559   5  13  31   6   1   7   6   5   7  18   3   3   2   3   2   2   2   3   2   3   3   4   2   3   2  15  51  22   2   2   3   3   2   0   2   1   3   2   2   3
scoped-filter r6              1842   5  11  21  26  30  25   1   3   1   1   2   1   2   4   3   3   0   4  29  42   3   4   2   1   2   1   3   1   2   2   2   3   4   3   4   5   7  13   2   2
negative-body r6              4015  20   6   6   3   9   1   7   9  10   4   6   3  11   4   3   3   1   3   3   1   2   4   9  20   2   0   3   2   1   2   2   2   3   3   1   2  17  32  11  21
exact-reference r7            3378  28  18  12   3  18  14  26   1   1   2   3   1   1   0   1   0   0   0   1   0   1   0   0   0   1   1   0   1   4  22  20   6  15  19  20  16  16  13   2  13
exact-address r7              7164  20  15  13  17  15  10  18  17   2   0   1   1   0   1   0   0   1   1   1   0   0   0   1   2   0   2   0   1   0   2   1   0   0   0   0   2   1   1   1   0
phrase r7                     9522   6   8  11  14  25  12   2  10  15  17   7  12   4   3   1   2   3   1   2   2   2   1   2   3   1   2   2   2   2   2   1   2   3   2   1   2   1   2   4   4
scoped-filter r7              7156  13  16  11   7  16  20  10  17  12   2   2  14  30  10   1   3   3   4   2   1   1   2   2   0   3   2   0   3   3   1   1   2   3   0   2   0   1   3   1   3
negative-body r7              5147  18  24  32   8   3   1   5   2   3   3   3   3   5  10   3   2   2   2   2   2   2   2   3   2   3   2   2   4   2   2   2   2   2   2   2   3   2   3   2   3
exact-reference r8            3189  14   8  15   3   9   9   5  12   8  15   5  11   8   7  12   4  15  10   6   1   0   1   1   5   8   0   0   1   1   0   0  14  18  14  25   8  13   7   6  11
exact-address r8              3372  28  20  16  12   9  19   8  22  20  32  17  17  14  12   8   8  16  15   5   4  13  18   7  15  18  18  13  13   6   6  18  23  28  43  27  13   5  24  27  20
phrase r8                     2736  24   8  12  13  15  13  10  10   9  15  10   4   7  12   3   9   4   0   0   0   0  15  39  14  11  11  30   1   0   0   0   1   0   0   0   0   0   5   0   2
scoped-filter r8              4387  15   6   9  19  14   5  11  10  17  12  10   3   4  13  15  16  13  22  28  21  15  17   4   3   1   1   0   0   1   1   1   2   0   1   1   2  10  24  35  11
negative-body r8              4655  22   4  24  18  24   7   0   0   0   1   0   0   1   1   1   2   2   0   0   0   1   2   2   0   0   0   1   0   0   1   0   1   2   1   9  11   7   8   4   6
exact-reference r9            4748  10   9   4   7  16  11   7  15  19  17  10  15   6   8   8  11  18  15   7   9  15   8   3   1   0   1   0   4   3   5  28   2   1   1   0   2   2   1   0   0
exact-address r9              3062  18   7   6   8  11  13  10   8  11  10  21   2  11   8  18  26  11  11   4  14  13  15  22  14   9   9  21  13  12  13  35  62   9  32  18  11   6   8  18  38
phrase r9                     3003  28  14  11  10   7  17  14   8  13   9  21   7  10   9  11  10   5   3   9  11   4  12  10  13  26   9  11  15   5  12   2   4   1   2   2   2   3   3   2   1
scoped-filter r9              2008  35  34  13  13   9   5   2   1  14  10  12  17  26   7  15   9   6  26  27  15  11   8   3   1   2   1   0   2   2   1   5   0   1   6   1   4   2   3   0   1
negative-body r9              2704   6   8   6   6   2   2   0   4   8   4  12  12   5   3  12  12  26   1   4   4   7  25   9   5   6  13  13   1   1   3   1   1   3   0   0   0   0   0   2   2
mean                                19  16  17  14  12  11  11   9   9  11   9   9  11  10   8  10  11  11   9   7   8  11   9   8   9   7   9  10   7   5   7   8   9   9   8   9   8   9   7   7
```

### Main-thread busy ms per 100 ms bucket, whole search phase

One line per second of trace time, ten buckets per line; `*` marks a second holding a timed keystroke.

```
    30.4s     0   1   0   1   1   1   0   0   0   1
    31.4s     1   1   1   1   1   5   2   1   0   1
    32.4s     1   0   0   4   4   0   0   1   1   2
    33.4s     0   1   0   0   7   8   1   0   3   3
    34.4s     0   1   1   8  19  26  12   0   0   2
    35.4s *  31  19  41  10  20  17  21  36  34  18
    36.4s    22  16   9  10  16  12  10  11  11  15
    37.4s    16  29  17  15  10   7   5  15  11   7
    38.4s *   9  40  16  14  18  30   5  18  23  37
    39.4s    11  37   8  24  10   9   7  37  21  12
    40.4s     9   7   5   0  15  30  30  12   3  15
    41.4s     6  16  11  48  46  33  16  15  13   0
    42.4s *   0   3  24  20  24  32   0   0   1   1
    43.4s     0   3  14  23  12  18  17   8  19  43
    44.4s     6  18  25   7   5   5  10   6   4   2
    45.4s     1   6   8   7  19  25  11  11  14  10
    46.4s     3   7   6  26  17  28  56  12  14   2
    47.4s *   7  20  26   9  29  35  25  43   1   0
    48.4s     0   0   0   1   1   0   1   1   1   1
    49.4s     0   1   0   0   1   1   2   0   1   0
    50.4s     1   0   0   1   1   1   1   1   0   1
    51.4s     0  12  21  20  35   9  35  12  44  11
    52.4s    15   6   6  10   5   6  10  15  11   6
    53.4s     4   2   1   1   0  10  15   3   1   3
    54.4s *  15  34  25  11   0   0  15  17   9   8
    55.4s     5   1   3   1   0   2   1   1   1   2
    56.4s     1   1   4   2   1   4  16   3  48  19
    57.4s     3   3   2   2   2   1   2   3   2   3
    58.4s     4   2   1   2   3   3   3   2   4   2
    59.4s     2   1   1   2   4   5   2   2   3   2
    60.4s     3   3   5  26  39  33  29  35  10  43
    61.4s    15  20  24   7   3   4   3   2   4   3
    62.4s     1   4   2   5   1   2   1   1   2   2
    63.4s     2   3   2   2   2   3   3   3   2   2
    64.4s     3   0   3   2   2   2   2   3   3   2
    65.4s *   3  25  20   2   2   2  18  25  19  29
    66.4s *  39  47  34  17   4   3  18  20  18  36
    67.4s    16  20  13   3   3  13  12   3   3   3
    68.4s     4   2  20  44  50  19  39  39  47  35
    69.4s *   9   2   9   3  22   5  12  21   4   3
    70.4s     2   4   4   2   1  16   8   2   2   2
    71.4s     2   2   3   3   8  28  12   3   2   2
    72.4s     1   3   2   2   3   5  34   7  10  12
    73.4s     6  12   0   2   0   3   0   2   2   2
    74.4s     2   1   2   2   1   0   1  23  38  22
    75.4s    23  47  19   5  16  15  10  14  22  12
    76.4s    30  13  18  18   6  12  24   9  17   6
    77.4s *  16   8  10  13  16   9  13  18   6  15
    78.4s    18   4   5   4   9  13  12  29   9   6
    79.4s    20  20   6   8   9  15  25  28  11   0
    80.4s     1   1   1   0   0   1   1   2   3   0
    81.4s     1   1   0   0   0   1   1   1   3   0
    82.4s     2   1   0   1   2   1   0   1   1   0
    83.4s     2   0   0   1   0  13   5   4  18  29
    84.4s    21  15   6  15  10  17  13   6   7  19
    85.4s    14   9  11   3  10  11  29  19   6  10
    86.4s    12  13  19   5   5   3   7  15  15  13
    87.4s    13   4  25   1  10  10   0   0   0   0
    88.4s     1  12  33  10   8  15  10  11  13   0
    89.4s *   0   1  15   8   9   4   7  13   7   7
    90.4s     6   0   1  18  17   4   7   7   3   0
    91.4s     1   1   0   0   2   3   1   3  16  27
    92.4s    11   1  13  18  10  20   0   2   1   0
    93.4s     0   0   0   0   0   0   3   0   0   1
    94.4s     0   0   3   0   1   0   1   0   1   0
    95.4s     1   1   0   6  25  30  29  37  17  11
    96.4s     6  37   6   7   5  19  23  16   5  23
    97.4s    12   3   3   4   6   0   5   9   1   0
    98.4s     0   1   0   4   2   0   0   0   1   1
    99.4s     1   0   1   0   0   1   0   1   0   0
   100.4s *   1   6  17   5   0   0   8  12   7  23
   101.4s    20  25  25  21  12  20  28  13  23   5
   102.4s    11  28  28  13   7  14  10  12  11   1
   103.4s *  15  10  34  33  36   3   1   9  28  32
   104.4s    13  20  44  33  19   1  10   8  13   7
   105.4s *  16  36  24  17  57   5  10   4  15  18
   106.4s    40   4   2   1  22  41   0   1   1   0
   107.4s     0   1   0   0   0   0   0   0   0   0
   108.4s     0   0   1   0  19  59  39  28  21  11
   109.4s     6  34  15  10   9  23  25   5  15   3
   110.4s *   0   1  25  17  40   1   1   1   5   8
   111.4s    14  25   7   7   6  12   0   1   0   0
   112.4s     1   1   0   2   0   0   1   0   1   0
   113.4s     1   0   1   0   1   3   0   0   0   1
   114.4s     0   1   1  33  32  26  10  19  30  18
   115.4s    44  25   7  11   5   7  18   5   6   6
   116.4s     4   2   1   0   0   0   0   1   0   0
   117.4s     0   0   0   1   0   1   0   2   0   1
   118.4s     0   0   1  27   0   0   0   0   0   1
   119.4s     0   2   1   2   2   2   8  43  31   0
   120.4s *   1   0   9  22  19  18   3   8   0   1
   121.4s     0   0   0   0   1   0   0   0   0   0
   122.4s     0   0   0  11  23  12   0   0   0   0
   123.4s     1   3   5  23  37  28   2   4   1   2
   124.4s     3   1   1   0   0   0   2   1   3   1
   125.4s     2   2   1   1   1   1   3   1   1   1
   126.4s     0   0   0   0   1   1   1   3   1   2
   127.4s     4  23  32  10  35  24   6  26  23  17
   128.4s     8  20  41  16  22   8  20  10  19   2
   129.4s     2   1   1   1   8   2   1   0   2   1
   130.4s     2   1   0   0   0   1   1   2   0   3
   131.4s     2   0   0   1   0   1   1   0   1   0
   132.4s *   2  18  18   1   1   7  24  22   6   1
   133.4s     1   0   2   7   8  16  18  30  13  50
   134.4s *  12  32  32  16   3   0   1  13   8  14
   135.4s    15  10  27  33  21   0   1   0   1  19
   136.4s *  52  46  30  25  56  15   9   5   9   5
   137.4s    24   6  16  24   2  20   8   2   1   1
   138.4s     3   8  31  17  22  18   1   0   0   0
   139.4s     0   2   0   0   0   1   0   0   1   1
   140.4s     1   1   0   0   0   2   0   2   0   2
   141.4s     0   1   0   0   0   5  28  26  23  22
   142.4s    35  17  18   9  39   6   8  30  18  13
   143.4s     7   4   8  13   5   9   4   7  12  12
   144.4s *  12  15  11  11   8   5   1   2  10  24
   145.4s    11  14  10  15  37  10   5  19  22  37
   146.4s    39   1   0   0  22  26  11   1   0   0
   147.4s     0   0   1   0   0   1   0   0   1   0
   148.4s     1   2   0   0   1   0   0   0   0   0
   149.4s     1   1   1   0   0   0   0   0   1   0
   150.4s     1   0   0   0   1   1   0   0   1   0
   151.4s     0   0   2   0   1   1   0   0   0   0
   152.4s     1   0   1   4   1   0   1   2   1   2
   153.4s     2   2   2   3   2   1   2   2   1   2
   154.4s     2   2   2   1   3   3   1   3   2   2
   155.4s     2   2   2   2   2   5  17  20  35  36
   156.4s    32  46  21   8  11   2   6  14  14   6
   157.4s    13   1   4   1   2   3   2   2   3   3
   158.4s     1   2   2   0   2   3   2   3   4   2
   159.4s     2   2   1   2   2   1   2   2   5   2
   160.4s     2   2   3   3   4   2   2   2   4   2
   161.4s *   3   2   7  36  30  21   2   2   2  17
   162.4s    28  18   3  16   2   3   3   2   2   2
   163.4s     2  16  52  17   4   3   2   8   5   4
   164.4s     1   3   3  15   5   2   3   2   2   3
   165.4s     2   2   2   2   2   2   2   2   2   2
   166.4s     2   2   2   3   2  27  45  29  30  17
   167.4s    46  17  26   6   3   4   3   3   3   2
   168.4s     3   2   4   2   2   2   2   3   2   2
   169.4s     3   3   3   2   2   2   3  13  21   3
   170.4s *   1  14   1   2   5  24  12   6   4   2
   171.4s *  29  22  14  51  28  30  21   3   1  10
   172.4s     5  20  33  35   1   4  37  16  34  41
   173.4s *  44   2  10   2  14  13   9  17  11   2
   174.4s     3   2   3   3   3   3   2   3   2   0
   175.4s     1   2   2   2  18   6   4   2   1  38
   176.4s    20   2   2   2   1   1   2   1   2   0
   177.4s     2   4   1   7  31  46  36  51  30  24
   178.4s *   4   7  13  19  16  34  14   1   0   3
   179.4s     3   2   1   0   0   1   2   1   1   1
   180.4s     1   2  10  22  12  14   2   0   0   0
   181.4s     0   1   0   0   1   0   1   0   2   0
   182.4s     1   0   0   2   8   0   1   0   1   1
   183.4s     0   0   1   0   9  15   4   1   0   1
   184.4s     0   1   0   2   1   1   0   0   1   0
   185.4s     1   0   1   1   1   1   1   7  26  43
   186.4s    22   3  33  15  25  41  23  17   4   4
   187.4s     2   2   2   1   1   1   0   0   1   0
   188.4s     1   1   0   1   0   0   2   1   1   0
   189.4s     0   1   0   0   0   0   0   2   0   1
   190.4s *  26  13  31  13   0   0   8  16  35   8
   191.4s    16   0   0   2   1   0   1   1   1   0
   192.4s     1   1   0  20   1   0   2   0   9   1
   193.4s     0   0   0   1   1   0   0   8   6   7
   194.4s     5   3   4  33  15   1   1   0   0   1
   195.4s     1   0   1   0   1   0   0   0   2   1
   196.4s     1   1   0   0   0   0   6   6  22  36
   197.4s    30  27  27  46  20   1  22   4   3   1
   198.4s     0   2   1   0   0   2  10   7   3  12
   199.4s *   0   0   8  19  12  31  10   4  26  14
   200.4s    27  11  14  18  44  22  16  24  37  15
   201.4s *  15   4   1   4  20  13   9  27  25  31
   202.4s    12   4   5  25   1   2  26  11  13  47
   203.4s    19  27   7   1  10  20  22  19  15   9
   204.4s *  19  17  13  15   1   0  27  11   6   7
   205.4s    11   2   2   1   1   1   2   0   1   0
   206.4s     1   1   0   0   0   1   0   3   8  44
   207.4s    16   0   1   1   1   0   0   0   0   0
   208.4s    30  31  27  10  17  17  25  41  14  17
   209.4s *  22   0   3   2  12  11   4  17   9  13
   210.4s    11  11   7  17  27  10  15  28   6  16
   211.4s    15  39  24   0   0   0   0   0   1   0
   212.4s     0   1   1   0   0   8   0   0   0   7
   213.4s    17   1   0   0   1   1   0   1   2   0
   214.4s     0   0   0   1   0   0   0   1   0   1
   215.4s     0   0   0   1   0   1   0   0   2   0
   216.4s     0   0   0   0   2   0   1   0   0   1
   217.4s     0   2   0   1   0   1   0   0   1   0
   218.4s     0   0   1   0   0   1   1   0   0   1
   219.4s     0   0   1   0   1   0   0   0   1   0
   220.4s     7  25  17  46  12  23  61  17  32  20
   221.4s    26   6  10  11   0   2   0   0   8   3
   222.4s     0   0   0   0   1   1   1   0   1   1
   223.4s     9   7   0   0   0   0   0   0   2   0
   224.4s     1   0   1   0   1   0   1  11  38  21
   225.4s *  29   0   0  16   6  35   9   3   0   2
   226.4s     2   2   1   0   0   0   2   0   3   6
   227.4s     4   0   1   0   0   0   2   0   4  12
   228.4s    33  24   6   1   0   1   0   0   1   0
   229.4s     0   0   0   0  23   4   1   0   0   0
   230.4s     0   2   0   2   0   0   0   1   0   1
   231.4s     0   1   0   0   1   1   0   0   0  16
   232.4s    43   6  27  14  36  31  43  19  29  30
   233.4s    10  12   5   8   1   3   0   0   2  12
   234.4s     0   0   2   0   0   1   1   0   1   0
   235.4s     1   0   0   1   1   0   0   1   3   2
   236.4s     1   1   3   2   1   2   2   0   2   0
   237.4s     3   1   2   2   1   1   1   1   3   3
   238.4s     0   0   2   2   4   1   9  19  21   4
   239.4s *   2   2  19  14  24   9   1   1   3   3
   240.4s     1   3   2   2   3  16  19  15  18  36
   241.4s    34  35  15  16  21  16  15  11   4  22
   242.4s *   4   6  18  16   2   1   2   5  20  29
   243.4s    34  30  18  19  30   5   2   3   3   2
   244.4s     1  19  13   2   1   3  19  63  36  41
   245.4s    11  11  53   2   8   7  23  13   2   7
   246.4s *   9   4  20  27   2   2  10   5   3  12
   247.4s    13   3   3   2   3   2   2   2   3   2
   248.4s     3   3   4   2   3   2  23  56   9   2
   249.4s     2   3   3   1   1   1   3   3   1   3
   250.4s     2   2   2   1   3   1   3   2   1   2
   251.4s     3   2   3   3   3   2   2   1   5  15
   252.4s    38  28  17  15  11  12  27  49  30  20
   253.4s *   6   3   0   3   6  10  15  26  19  40
   254.4s     6   3   1   2   1   2   1   3   3   3
   255.4s     2   2  15  39  21   3   4   0   1   2
   256.4s     3   2   1   3   1   3   3   4   4   2
   257.4s     5  18   2   2   2   3   2   1   2  13
   258.4s     3   3   3   5  11  14  21  38  25  37
   259.4s    35  56  14  18   4  11  10  19  11   3
   260.4s     3   6   4   5   1   5   3   4   1  16
   261.4s    13   3   3   2   3   2   2  36  42   8
   262.4s *   2   8  19   5   4   4   6   3   6   9
   263.4s     9   7   3   9   6   4   2   3   1   4
   264.4s     2   1   2   4  14  15   2   0   3   2
   265.4s     1   2   2   2   4   2   1   3  23  29
   266.4s    12  16   2   3   2   2   2   2   4   3
   267.4s     2   3   1   2   2   3   2   2   2   2
   268.4s     1   2   2   2   2   5  16   5   3  10
   269.4s     1   6  16  10  13  22  11  12   9  12
   270.4s    11  16   5   2   2  10   9   8   5  27
   271.4s    16  10  17  12  15  11  21  48  26   9
   272.4s    12  22  10   9  11  13  17  13  15   1
   273.4s     1   4   2   4   1   1   3  19  14  14
   274.4s    37  14   5   6  12   5   0   1   2   1
   275.4s     7  11   2   3   4   2   3   0   2   2
   276.4s     1   1   4   2   0   0   5   3   2   2
   277.4s     2   3   1   1   0   0   0   1   1   2
   278.4s     3   2   3   2   1   2   1   2   2   3
   279.4s     4   3   2   0   1   1   0   2   3   0
   280.4s     3   2   1   0   0   1   1   1   2   0
   281.4s     2   1   3   1   2   1   1   1   1   0
   282.4s     1   4   0   1   2   1   1   1   1   1
   283.4s     1   1   0   0   0   2   1   1   0   0
   284.4s     0   3   2   2   2   1   1   0   1   0
   285.4s     1   1   0   0   0   0   2   0   1   1
   286.4s     0   2   1   0   0   1   2   0   0   0
   287.4s     0   0   0   0   1   2   3   0   0   1
   288.4s     0   1   0   0   0   0   3  12   4   1
   289.4s     0   0   0   0   0   0   2   3   0   0
   290.4s     0   0   0   2   1   0   2   0   0   1
   291.4s     1   0   0   2   0   0   0   0   2   0
   292.4s     2   0   0   1   0   1   0   1   0   1
   293.4s     1   0   1   0   0   0   0   0   0   1
   294.4s     1   1   0   7  12  17   4   0   0   1
   295.4s     0   0   0   0   0   0   0   1   0   0
   296.4s     0   3   1   1   0   2   9   4  12  10
   297.4s *  16   8  15   0   0   0  24  14  18   2
   298.4s    20  14  20   8   0   3   3   1   1   0
   299.4s     1   0   0   0   1   0   1   0   0   0
   300.4s     1   1   0   1   2  18  21   9  13  19
   301.4s    19  13  20  16   2  12  11  17  11  10
   302.4s    28  17  18  11  12  10  15  13   4  22
   303.4s *  13  11  17  16   9  17  19  15  11  16
   304.4s    12  15  14   2   0   1   1   1   0   0
   305.4s     0   1   1   1   0   0   1   1   1   1
   306.4s     1   1   0   1   1   1   0   0   0   0
   307.4s     2   1   2   0   0   0   2   3   0   0
   308.4s     0   1   0   1   0   1   0   0   0   0
   309.4s     1   1   0   1   2   3   4   8   4  30
   310.4s    18  10  12  13   7  14   5   1   2   0
   311.4s     0   1   1   0   0   1   0   1   1   1
   312.4s     1   2   0   1   2   2   0   0   0   2
   313.4s     0   0   0   1   0   2   0   1   0   0
   314.4s     0   0   1   0   4   1   1   6  12  12
   315.4s    14  23  19  21  11  14  10  12  14  16
   316.4s    13  18  16  19  10   1   1   1   1   1
   317.4s     1   1   1   0   1   1   1   1   1   0
   318.4s     0  18  41  26  16  11  11  20  19   4
   319.4s *   0   1   6   8  10  13  27  12   1   9
   320.4s    16  14  11  10   6   3   1   2   3   1
   321.4s     2   2   2   1   2   2   2   2   2   2
   322.4s     2   2   1   2   3   2   1   2   1   2
   323.4s     4   4   2   1   1   3   3   2   2   2
   324.4s     1   2   2   2   3   3   3   1   2   2
   325.4s     2   2   3   2   2   2   1   2   2   4
   326.4s     2   3   3   2   2   2   2   3   2   2
   327.4s     2   1  15   4   2   2   2   3   2   2
   328.4s     2   1   2   3   9  38  16  13   6  14
   329.4s     2   3   2   3   3   2   2   1   2   1
   330.4s     3   2   2   2   2   2   3   2   2   2
   331.4s    17   2   3   3   2   2   2   1   2   2
   332.4s     2   2   4   2   2   3   5   1   2   3
   333.4s     2   2   0   3   4   1   3   1   1   3
   334.4s     2   2   3   2   1  15  47  23  23  22
   335.4s *  26  35  17  20  16  20   5   3   3  14
   336.4s    14  12   7  17  18  10  20   9   2   3
   337.4s    13  33   7   1   3   3   4   2   1   1
   338.4s     2   2   0   3   2   0   3   3   1   1
   339.4s     2   3   1   1   0   1   3   1   3   1
   340.4s     1   1   1   0   2   2   1   1   1   2
   341.4s     0   0   0   2   0   0   0   3   3  18
   342.4s    26   7   3   1  11   7   4  12   7  23
   343.4s     3   1   0   0   1   0   1   0   2   1
   344.4s     2   1   0   0   0   1   0   1   0   1
   345.4s     0   2   1   1   0   1   0   2   2   0
   346.4s     0   2   0   0   1   0   0   1   1   1
   347.4s     0   0   0   2   1   1   0   1   0   2
   348.4s     0   3   2   1   3   2   3   2   0   3
   349.4s     1   2   4   1   2   2   2   2   3   4
   350.4s     2   1   1   4   1   2   2   2   2   3
   351.4s     3   2   2   2   2   3   8   6   6   7
   352.4s    17  11  36  18  31  33  23  28  13  10
   353.4s    11   9  10   3   2   6  13   0   3   2
   354.4s     1   2   2   1   2   3   1   2   0   3
   355.4s     1   3   2   2   2   1   3   2   1   3
   356.4s     3   2   3   2   2   2   2   4   0   2
   357.4s     2   3   3   3   2   2   2   2   2   3
   358.4s     2   2   1   3  11  22  10   3   2   3
   359.4s     3   3   2   2   2   4   2   2   3   5
   360.4s     4   2   2   6  10   8   8  36  21   2
   361.4s *   2   1  18  24  29  11   3   1   4   3
   362.4s     3   2   4   3   2  13   3   2   2   2
   363.4s     2   2   2   2   3   2   3   2   2   3
   364.4s     3   2   2   2   2   2   2   3   2   3
   365.4s     2   3   3   5   2   6   7  18  22  12
   366.4s    24  10  14   4   2   2   3   3   0   4
   367.4s     1   1   2   1   2   2   3   3   3   2
   368.4s     2   5   3   3   2   2   2   2   3   2
   369.4s     2   1   0   1   3   2   1   3   3   1
   370.4s     3   2   1   4   1   5   1   1   3  18
   371.4s    22  17  10   2   3   1   1   2   2   1
   372.4s     2   1   1   2   1   2   1   0   2   1
   373.4s     2   2   1   1   2   2   0   1   0   2
   374.4s     0   0   3   0   2   3   1   1   0   1
   375.4s     0   0   2   1   1   1   9   9  10  16
   376.4s     6   8   7  11   8  24  14  21  24  17
   377.4s    11  11  20  20  14  14  16  17  15   8
   378.4s    22  15   9  23  15   9  10  11  25  29
   379.4s    14  19  16  11  16   7  21  10   1   9
   380.4s    18  10   5  10   9  18  17  10  11   1
   381.4s     2   7  23   2   0   0   0   0   0   0
   382.4s     0   0   0   0   0   0   0   0   0   0
   383.4s     0   0   0   0   0   0   0   0   0   0
   384.4s     0   0   0   0   0   0   0   0   0   0
   385.4s     0   0   0   0   0   0   0   0   0   0
   386.4s     0   0   0   0   0   0   0   0   0   1
   387.4s    11   9  15  12  22  18   8  19   3   0
   388.4s *   0   9   6  23   1   9   4   8  11   8
   389.4s    12  11   5   9  10   9   9  12  13   7
   390.4s     1   1   0   2   1   7   5   0   1   1
   391.4s     0   0   0  28   7  28  10   9  16   3
   392.4s    12   8   9  12  15  22  29  11  32  30
   393.4s     7  19  29  11  11   6  12  10  11  14
   394.4s    17  12   7   4   6  10  10   9  20  10
   395.4s     9  10   9  11   6  16   9  10   9  18
   396.4s    12   6  17  17  14  12  17  14   8  11
   397.4s *  19  24  19  17   7  12  17  13  22  28
   398.4s    28   9  22  10  10  10   6  24   6   5
   399.4s     4  21  12   8  20  17  15  11  12   5
   400.4s    12  14  33  22  44  25   7  15  25  23
   401.4s *  23  42  15  24  21   8  10   6   3  22
   402.4s    11   9  13  14  15  10   8  10  16   8
   403.4s     4  15   6   1  10   3   0   0   0   0
   404.4s    19  37  14   9  14  28   0   0   0   0
   405.4s     1   0   0   0   0   0   5   0   2   0
   406.4s     0   0   0   2   0   0   0   0   0   0
   407.4s     0   1   0   2   6   7   1   1   0   1
   408.4s     1  18  15  23  30  24  18  38  30   9
   409.4s    31  20   5  16   6   6   6   7  10   6
   410.4s    12   4   5   7   9  11   8   8  10  13
   411.4s     8   4  11   2   5   5  16  10   7   8
   412.4s *   7   9   4  19   8  12   7  12  20  10
   413.4s     9   6  13  19  11   6   4   3  15  15
   414.4s    20  13  22  23  21  15  16   4   2   2
   415.4s     0   0   0   1   1   1   2   0   2   1
   416.4s     3  10  30  34  11   7   5  14   9   0
   417.4s     0   0   0   1   1   0   0   2   0   1
   418.4s     0   1   0   0   1   0   0   0   0   0
   419.4s     0   0   0   0   3   0   0   0   1   0
   420.4s     1   0   2   2   0   0   2   0   1   1
   421.4s     0   0   0   5  12   4   0   0   0   0
   422.4s     0   1   0   0  11   0   0   1   0   0
   423.4s     1   0   0   1   1   0   0   1   1   0
   424.4s     0   1   0   0   0   0   0   1   0   0
   425.4s     1   0   0   0   2   0   0   7  23  26
   426.4s    36  14   8  21   9  26  24   3   6  11
   427.4s    10  17  26  20   4  10   1   7   6  10
   428.4s     8   9   4   7   3   2   6   8  10   2
   429.4s     1   2   3   5   3   2   0   0   0   2
   430.4s     0   0   0   1   1   1   0   1   1   1
   431.4s     1   0   0   1   0   1   0   0   0   2
   432.4s     0   0   0   1   0   0   0   0   1   0
   433.4s     0   1  10  23   0   0   1   0   4  30
   434.4s *   9  16  18  35  21   3  22  23  20   6
   435.4s     0   0   0   1   0   0   1   1   1   2
   436.4s     2   0   0   0   2   1   2   0   0   0
   437.4s     1   0   0   1   1   0   3   0  11  10
   438.4s     6   8   4   7  30  12  32  18  10   4
   439.4s     3   1   0   2   1   1   0   1   2   0
   440.4s     0   0   0   0   1   2   0   0   0   1
   441.4s     0   1   2   2   0   1   0   0   1   1
   442.4s     0   0   0   0   0   0   1   1   5  22
   443.4s    19  20  12  28  11  14  15  13  20  15
   444.4s    17  11  23  13  32  14   9  43  14   6
   445.4s     5   9   9  22  15  13  28  13  14   4
   446.4s    12   1   1   0   0   1   1   1   4   1
   447.4s     4   4   0   2   0   1   0   1   0   0
   448.4s     0   2   1   0   0   0   0   1   0   1
   449.4s     4  16  10   9   4   6   8   4  10   7
   450.4s *   9  18   2   0   0   3   9   8   6   9
   451.4s    12  10  12  10  21  18   9  13   9   7
   452.4s    10   8  20  14   5  10  15   9   1   1
   453.4s     0   1   0   5   2   8  26   1   1   1
   454.4s     0   2   2   1   0   1   2   3  12  13
   455.4s    23  17  26  16  22  27  14   8  13  15
   456.4s    16  32  23  16  18   9   8   6   6   6
   457.4s    17   6   7   9  10  10  12   6  21  25
   458.4s *   5  12  15  10   8  19  12   2   4  16
   459.4s     7   7   8  12  11  11  10   8  22   9
   460.4s     5   9   9  20  25   8  11  10  11  15
   461.4s    19  19   9   9  16  20  11   8  14  38
   462.4s    62   7  37  12  10   6   7  24  33  11
   463.4s     4  12  15  13  15  32  22  10   8  25
   464.4s *  16   7  21   3   2   3  27  16   7  11
   465.4s    10  12  19   6  11  12  11  18   7  11
   466.4s     9  12   4   4   8  12   5   8  11  15
   467.4s    25   9  11  10  11  11   3   4   1   2
   468.4s     2   2   3   3   2   1   2   2   1   1
   469.4s    12   7  11  12  27  11  10  13  11  10
   470.4s    16  25  11   5   3   8   4  22  11   6
   471.4s     7  20  10  20  22   4   9  14  17  13
   472.4s *  38  17  18  11   7   7   0   1  20  39
   473.4s    18  17   6   6   5   0   8  14  11  17
   474.4s    26  10   8  13  10  11  26  22  16  10
   475.4s     4   1   2   2   0   1   3   1   1   4
   476.4s     1   6   1   2   3   2   2   1   0   0
   477.4s     0   0   0   1   1   2   0   1   0   1
   478.4s     0   0   2   0   0   1   0   0   0   0
   479.4s     1   1   0   0   0   0   5   0   1   0
   480.4s     1   0   1   0   0   2   1   0   1   0
   481.4s     0   1   0   1   1   1   0   1   0   0
   482.4s     0   0   0   0   0   3   3   4   4   0
   483.4s     0   2   0   0   0   1   1   0   1   2
   484.4s     1   1   0   0   0   0   1   0   0   0
   485.4s     2   1   1   0   1   1   0   0   1   1
   486.4s     1   1   0   2   1   0   0   0   0   0
   487.4s     2   1   1   0   7  16  12   6   0   0
   488.4s     2   2   0   1   1   0   0   0   1   0
   489.4s     1   1   1   0   1   8  10  14  15  19
   490.4s    14  27  24   5   8  20   7   4   8   5
   491.4s     6  19  17   8   9   7   6   7   8  23
   492.4s    26  19  23  29  12  10  15  18   8  16
   493.4s     8   5   1  14   1   3   0   0   1   1
   494.4s     2   0   0   0   0   0   2   1   0   2
   495.4s     3   2   1   7  12   3   0   0   0   1
   496.4s     0   1   0   1   1   1   1   1   0   0
   497.4s     0   0   0   1   0   0   1   1   0   0
   498.4s     0   0   0   0   0   1   1   0   1   0
   499.4s     0   6   8   5   3   7   3   2  11   3
   500.4s    10   6   5  19  11   7   9   3  23   6
   501.4s *   6  11   0   6   7   7   6   2   2   0
   502.4s     4   8   4  12  12   5   3  12  11  27
   503.4s     1   4   4   7  25   9   5   6  13  13
   504.4s     1   1   3   1   1   3   0   0   0   0
   505.4s     0   2   2   0   0   1   1   0   1   0
   506.4s     2   1   0   1   1   0   0   3   0   0
   507.4s     5  11  20  21   8  35  19  11  13  17
   508.4s    13  22   4   9   7   2  17   5  10  11
   509.4s     8   7   6   5   5   4   9   8  15   5
   510.4s     9   9   7  10   6   4   5   7   2   7
   511.4s     4   4  11  13   9   9   4   7   6   4
   512.4s     7   4   6   3   2   6   9  14  15   8
   513.4s     9   7   8  11   8   7  11  11   6   7
   514.4s     8   5   1   5   9   2
```

### Post-query bursts

Window per sample: keystroke to max(rendered, opened) + 50 ms. Main thread busy 15272 ms over 50 windows.

#### Named areas in the bursts

| area | ms | % |
|---|---:|---:|
| AppKitPrototypeList | 2219 | 14.5 |
| MessageRowCellView | 989 | 6.5 |
| Row presentation / formatters | 23 | 0.2 |
| ListWindowModel | 1250 | 8.2 |
| FrameBatcher | 145 | 0.9 |
| MessageListViewController | 2488 | 16.3 |
| SearchFieldController | 1275 | 8.3 |
| SearchFieldModel | 286 | 1.9 |
| Benchmark harness (runner, tick sampler) | 1632 | 10.7 |
| -[NSTableView reloadData] | 861 | 5.6 |
| NSTableView (any) | 3147 | 20.6 |
| NSView layout (layoutSubtreeIfNeeded) | 2883 | 18.9 |
| NSView display (displayIfNeeded / drawRect) | 464 | 3.0 |
| NSTextView insertText | 35 | 0.2 |
| NSTextCheckingController | 210 | 1.4 |
| Core Animation commit (CA::Transaction::commit) | 6459 | 42.3 |
| NSDisplayCycleFlush | 4241 | 27.8 |
| Swift concurrency job (completeTaskWithClosure) | 4514 | 29.6 |
| Rust core (sb_*) | 0 | 0.0 |

#### Heaviest main-thread call paths in the bursts

Distinct paths of the top-down call tree, each ending where no callee keeps 1% of burst time; the weight is the path's inclusive time. Leaf first, at most 30 frames (the run-loop frames nearest the root are cut when longer).

##### 1. 911 ms (6.0% of burst busy)

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
main [atos 0x1042f575b]
start
```

##### 2. 630 ms (4.1% of burst busy)

```
specialized AppKitPrototypeList.tableView(_:viewFor:row:) [atos 0x1042ffdcf]
@objc AppKitPrototypeList.tableView(_:viewFor:row:) [atos 0x1042fcc17]
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

##### 3. 269 ms (1.8% of burst busy)

```
<deduplicated_symbol>
<deduplicated_symbol>
<deduplicated_symbol>
completeTaskWithClosure(swift::AsyncContext*, swift::SwiftError*)
```

##### 4. 254 ms (1.7% of burst busy)

```
0x19461e1bc
0x19461e13c
specialized static RunContext.displayID(_:) [atos 0x104371b93]
RunContext.checkDisplay() [atos 0x104356b27]
RunContext.tickDidFire(_:) [atos 0x104356e1b]
closure #8 in ListBenchmarkRunner.runStore(controller:)
partial apply for closure #2 in ListBenchmarkRunner.runOnce(implementation:pass:generation:inheritedFailures:)
BenchmarkTickSampler.tickFired(_:)
@objc BenchmarkTickSampler.tickFired(_:) [atos 0x10432aff7]
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
main [atos 0x1042f575b]
start
```

##### 5. 244 ms (1.6% of burst busy)

```
-[NSCoreTypesetter _stringDrawingCoreTextEngineWithOriginalString:rect:padding:graphicsContext:forceClipping:attributes:stringDrawingOptions:drawingContext:stringDrawingInterface:]
__NSStringDrawingEngine
0x1944f5fa8
0x1944f6434
0x19469f47c
0x19469f4a4
0x194664d98
0x194665970
_NSViewDrawRect
-[NSView _recursive:displayRectIgnoringOpacity:inContext:stopAtLayerBackedViews:]
-[NSView(NSLayerKitGlue) _drawViewBackingLayer:inContext:drawingHandler:]
-[NSViewBackingLayer drawInContext:]
0x1946c1f3c
0x1946c2a0c
0x19475dda0
0x19475e364
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
... 8 more frames to the root
```

##### 6. 234 ms (1.5% of burst busy)

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
main [atos 0x1042f575b]
start
```

##### 7. 221 ms (1.4% of burst busy)

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

##### 8. 211 ms (1.4% of burst busy)

```
closure #1 in BenchmarkTickSampler.start(on:)
thunk for @escaping @callee_guaranteed (@guaranteed CFRunLoopObserverRef?, @unowned CFRunLoopActivity) -> ()
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
main [atos 0x1042f575b]
start
```

##### 9. 202 ms (1.3% of burst busy)

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

##### 10. 198 ms (1.3% of burst busy)

```
LayoutProxy.size(in:)
_FlexFrameLayout.sizeThatFits(in:context:child:)
protocol witness for UnaryLayout.sizeThatFits(in:context:child:) in conformance _FlexFrameLayout
UnaryLayoutEngine.sizeThatFits(_:)
0x23c924830
LayoutEngineBox.sizeThatFits(_:)
LayoutProxy.dimensions(in:)
StackLayout.UnmanagedImplementation.resize(_:proposal:proxy:)
specialized StackLayout.UnmanagedImplementation.sizeChildrenIdeally(in:minorProposalForChild:)
specialized StackLayout.UnmanagedImplementation.placeChildren1(in:minorProposalForChild:)
StackLayout.UnmanagedImplementation.placeChildren(in:)
<deduplicated_symbol>
_AnyLayoutBox.sizeThatFits(proposal:subviews:cache:)
protocol witness for Layout.sizeThatFits(proposal:subviews:cache:) in conformance AnyLayout
closure #1 in ViewLayoutEngine.sizeThatFits(_:)
ViewLayoutEngine.sizeThatFits(_:)
LayoutEngineBox.sizeThatFits(_:)
LayoutProxy.size(in:)
_FlexFrameLayout.sizeThatFits(in:context:child:)
protocol witness for UnaryLayout.sizeThatFits(in:context:child:) in conformance _FlexFrameLayout
UnaryLayoutEngine.sizeThatFits(_:)
0x23c924830
LayoutEngineBox.sizeThatFits(_:)
LayoutProxy.size(in:)
LayoutSubview.sizeThatFits(_:)
0x24f85d4d8
0x24f85e92c
closure #1 in ViewLayoutEngine.sizeThatFits(_:)
ViewLayoutEngine.sizeThatFits(_:)
LayoutEngineBox.sizeThatFits(_:)
... 49 more frames to the root
```

##### 11. 194 ms (1.3% of burst busy)

```
_class_conformsToProtocol_unrealized_nolock(objc_class*, Protocol*)
class_conformsToProtocol
-[NSObject conformsToProtocol:]
-[NSView _inputContext]
-[NSView _setWindow:]
-[NSView removeFromSuperview]
-[NSView removeFromSuperviewWithoutNeedingDisplay]
-[NSTableRowData _removeViewAndAddToReuse:forRow:]
-[NSTableRowData _removeRowViewForRow:]
__36-[NSTableRowData _removeVisibleRows]_block_invoke
-[NSTableRowData _removeVisibleRows]
-[NSTableRowData removeAllKnownSubviews]
-[NSTableRowData purgeRowViewData]
-[NSTableView reloadData]
closure #2 in AppKitPrototypeList.init(model:formatter:accountMarker:selectionLoss:onIndexChange:) [atos 0x1042fbaab]
ListWindowModel.open()
MessageListViewController.install(_:ticket:)
closure #1 in MessageListViewController.show(_:) [atos 0x10438e801]
<deduplicated_symbol> [atos 0x1042fb389]
<deduplicated_symbol> [atos 0x1043a4fb9]
<deduplicated_symbol> [atos 0x1042fb389]
completeTaskWithClosure(swift::AsyncContext*, swift::SwiftError*)
```

##### 12. 193 ms (1.3% of burst busy)

```
__CFSetApplyFunction_block_invoke
CFBasicHashApply
CFSetApplyFunction
__reusableDependencyContextForKey_block_invoke
__CFSetApplyFunction_block_invoke
CFBasicHashApply
CFSetApplyFunction
__reusableDependencyContextForKey_block_invoke
__CFSetApplyFunction_block_invoke
CFBasicHashApply
CFSetApplyFunction
-[NSView _setSuperview:]
-[NSView removeFromSuperview]
-[NSView removeFromSuperviewWithoutNeedingDisplay]
-[NSTableRowData _removeViewAndAddToReuse:forRow:]
-[NSTableRowData _removeRowViewForRow:]
__36-[NSTableRowData _removeVisibleRows]_block_invoke
-[NSTableRowData _removeVisibleRows]
-[NSTableRowData removeAllKnownSubviews]
-[NSTableRowData purgeRowViewData]
-[NSTableView reloadData]
closure #2 in AppKitPrototypeList.init(model:formatter:accountMarker:selectionLoss:onIndexChange:) [atos 0x1042fbaab]
ListWindowModel.open()
MessageListViewController.install(_:ticket:)
closure #1 in MessageListViewController.show(_:) [atos 0x10438e801]
<deduplicated_symbol> [atos 0x1042fb389]
<deduplicated_symbol> [atos 0x1043a4fb9]
<deduplicated_symbol> [atos 0x1042fb389]
completeTaskWithClosure(swift::AsyncContext*, swift::SwiftError*)
```

### CPU time by thread over the search phase (top 12)

| ms | thread |
|---:|---|
| 34383 | Main Thread (0x4131877) (Sweetbriar, pid: 47841) |
| 9356 | tokio-rt-worker (0x4131d95) (Sweetbriar, pid: 47841) |
| 8270 | tokio-rt-worker (0x4131c6d) (Sweetbriar, pid: 47841) |
| 7542 | tokio-rt-worker (0x4132045) (Sweetbriar, pid: 47841) |
| 6257 | tokio-rt-worker (0x413214c) (Sweetbriar, pid: 47841) |
| 5039 | tokio-rt-worker (0x413214b) (Sweetbriar, pid: 47841) |
| 2697 | tokio-rt-worker (0x4132bdf) (Sweetbriar, pid: 47841) |
| 2556 | tokio-rt-worker (0x41343d9) (Sweetbriar, pid: 47841) |
| 2322 | tokio-rt-worker (0x41349b0) (Sweetbriar, pid: 47841) |
| 2058 | tokio-rt-worker (0x4131c5b) (Sweetbriar, pid: 47841) |
| 1561 | tokio-rt-worker (0x41350ea) (Sweetbriar, pid: 47841) |
| 1377 | tokio-rt-worker (0x41343da) (Sweetbriar, pid: 47841) |

### Top 40 Sweetbriar frames on the tokio workers (inclusive, search phase)

For context: the core's work off the main thread (54322 ms on all workers).

| ms | % of worker time | frame |
|---:|---:|---|
| 54264 | 99.9 | `<std::sys::thread::unix::Thread>::new::thread_start [atos 0x105fe9387]` |
| 54262 | 99.9 | `<std::thread::lifecycle::spawn_unchecked<<tokio::runtime::blocking::pool::Spawner>::spawn_thread::{closure#0}, ()>::{closure#1} as core::ops::function::FnOnc...` |
| 54219 | 99.8 | `std::sys::backtrace::__rust_begin_short_backtrace::<<tokio::runtime::blocking::pool::Spawner>::spawn_thread::{closure#0}, ()> [atos 0x105f9f7a7]` |
| 51480 | 94.8 | `sqlite3_step` |
| 36033 | 66.3 | `getPageNormal` |
| 35830 | 66.0 | `getAndInitPage [atos 0x105e655e7]` |
| 31749 | 58.4 | `<rusqlite::row::Rows as fallible_streaming_iterator::FallibleStreamingIterator>::advance [atos 0x105d3c617]` |
| 29845 | 54.9 | `moveToChild` |
| 27912 | 51.4 | `pcache1Fetch` |
| 26795 | 49.3 | `<tokio::runtime::task::harness::Harness<tokio::runtime::blocking::task::BlockingTask<<sb_engine::engine::Engine>::read_through<(sb_store::query::counts::cove...` |
| 26769 | 49.3 | `<tokio::runtime::blocking::task::BlockingTask<<sb_engine::engine::Engine>::read_through<(sb_store::query::counts::coverage::StoredSearchCoverage, sb_store::l...` |
| 26769 | 49.3 | `<tokio::runtime::task::core::Core<tokio::runtime::blocking::task::BlockingTask<<sb_engine::engine::Engine>::read_through<(sb_store::query::counts::coverage::...` |
| 26759 | 49.3 | `<sb_store::readers::Readers>::read::<(sb_store::query::counts::coverage::StoredSearchCoverage, sb_store::lists::PageRead<sb_store::query::counts::paging::Sea...` |
| 26286 | 48.4 | `sb_store::query::counts::paging::read_search_relevance_page` |
| 21835 | 40.2 | `sqlite3BtreeTableMoveto` |
| 16500 | 30.4 | `sqlite3VdbeExec [atos 0x105e4fecb]` |
| 16173 | 29.8 | `<tokio::runtime::task::harness::Harness<tokio::runtime::blocking::task::BlockingTask<<sb_engine::engine::Engine>::read_through<(sb_store::query::counts::cove...` |
| 16162 | 29.8 | `<tokio::runtime::task::core::Core<tokio::runtime::blocking::task::BlockingTask<<sb_engine::engine::Engine>::read_through<(sb_store::query::counts::coverage::...` |
| 16159 | 29.7 | `<tokio::runtime::blocking::task::BlockingTask<<sb_engine::engine::Engine>::read_through<(sb_store::query::counts::coverage::StoredSearchCoverage, sb_store::l...` |
| 16053 | 29.6 | `<sb_store::readers::Readers>::read::<(sb_store::query::counts::coverage::StoredSearchCoverage, sb_store::lists::PageRead<(u64, alloc::vec::Vec<i64>)>), <sb_e...` |
| 15302 | 28.2 | `sb_store::query::counts::paging::search_ids [atos 0x105d45d93]` |
| 15302 | 28.2 | `<sb_store::query::counts::Compiled>::read::<alloc::vec::Vec<i64>, sb_store::query::counts::paging::search_ids::{closure#1}>` |
| 15260 | 28.1 | `<core::iter::adapters::GenericShunt<rusqlite::row::MappedRows<sb_store::query::counts::paging::search_ids::{closure#1}::{closure#0}>, core::result::Result<co...` |
| 14404 | 26.5 | `<alloc::vec::Vec<i64> as alloc::vec::spec_from_iter::SpecFromIter<i64, core::iter::adapters::GenericShunt<rusqlite::row::MappedRows<sb_store::query::counts::...` |
| 14068 | 25.9 | `sb_store::query::counts::paging::fetch` |
| 12055 | 22.2 | `<rusqlite::row::Rows>::get_expected_row [atos 0x105cea553]` |
| 10693 | 19.7 | `sqlite3VdbeExec [atos 0x105e4f197]` |
| 10662 | 19.6 | `<tokio::runtime::task::harness::Harness<tokio::runtime::blocking::task::BlockingTask<<sb_engine::engine::Engine>::read_through<sb_store::lists::PageRead<(u64...` |
| 10656 | 19.6 | `<tokio::runtime::task::core::Core<tokio::runtime::blocking::task::BlockingTask<<sb_engine::engine::Engine>::read_through<sb_store::lists::PageRead<(u64, allo...` |
| 10654 | 19.6 | `<tokio::runtime::blocking::task::BlockingTask<<sb_engine::engine::Engine>::read_through<sb_store::lists::PageRead<(u64, alloc::vec::Vec<i64>)>, <sb_engine::e...` |
| 10654 | 19.6 | `<sb_store::readers::Readers>::read::<sb_store::lists::PageRead<(u64, alloc::vec::Vec<i64>)>, <sb_engine::engine::Engine>::window_ids::{closure#0}::{closure#2...` |
| 10637 | 19.6 | `sb_store::lists::list_ids` |
| 10566 | 19.5 | `<alloc::vec::Vec<i64> as alloc::vec::spec_from_iter::SpecFromIter<i64, core::iter::adapters::GenericShunt<rusqlite::row::MappedRows<sb_store::lists::list_ids...` |
| 10480 | 19.3 | `<core::iter::adapters::GenericShunt<rusqlite::row::MappedRows<sb_store::lists::list_ids::{closure#1}>, core::result::Result<core::convert::Infallible, rusqli...` |
| 9610 | 17.7 | `sqlite3BtreeNext` |
| 8989 | 16.5 | `sqlite3BtreeIndexMoveto` |
| 8612 | 15.9 | `sqlite3VdbeExec [atos 0x105e4eabf]` |
| 7996 | 14.7 | `readDbPage` |
| 7935 | 14.6 | `unixRead [atos 0x105e2e89f]` |
| 7884 | 14.5 | `<sb_store::query::counts::Compiled>::read::<sb_store::query::counts::paging::Streamed, sb_store::query::counts::paging::stream::{closure#1}>` |
