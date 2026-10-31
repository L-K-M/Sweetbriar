# Time Profiler run: 963515c, r4c4-closed-id-final (failed)

Built commit `963515c060e7d15fc0c0d634153c5686704ede5a` ("Keep stopped
Search reads ahead of late failures"), Release, Mac mini M1
(Macmini9,1, 8 GiB), macOS 26.5.2, 60 Hz, 100,000-message synthetic
archive. Recorded 2026-10-31 with `xcrun xctrace record --template
'Time Profiler' --launch` over `-runSearchBenchmark`.

**Neither profiled attempt passed.**

- **Attempt 1** (`profiled-invalid1.json`, about 04:18 to 04:27 UTC)
  failed with "the window wasn't key and the app active" and
  "exact-reference round 0: the typed prefix never settled", with 0 of
  50 samples. The Mac was swapping heavily: load average about 21 on 8
  cores, and about 11,700 swap-ins and 11,600 swap-outs per second.
  System processes were stuck waiting: Spotlight, `mobileassetd`,
  `siriknowledged`, `duetexpertd`. A Keychain Circle Notification was
  also up, which may have taken the key window.
- **Attempt 2** (`profiled.json`, 04:46 to 04:55 UTC) is the one
  allowed retry. It started after the load had fallen to about 2 and
  swap-ins to about 170 per second. It failed with "exact-reference
  round 3: the typed prefix never settled" after 15 of 50 samples. The
  load average rose to 10.5 during the run.

The analysis below is of attempt 2. Its timings are profiler-distorted
and come from a failed run: they aren't results.

## How this was produced

The method is the same as for
`bench-results/20261029T212655Z-30888e0-main-profile-profile.md`, and
that file has the analysis script and demangler source.

- 15 timed keystrokes were found for the report's 16 samples. The 16th
  sample stalled on its typed prefix and never reached a timed
  keystroke, so pairing the 15 keystrokes in order with the first 15
  samples is correct.
- `xctrace` reported "The log archive is corrupt or incomplete" and
  exited 2 on both attempts, so there are no signposts.
- The stall window section comes from a second script,
  `stall_window.py`, included at the end. It takes the time from the
  last completed sample's end (its keystroke plus max(rendered,
  opened)) to the runner's last main-thread sample.

## Findings

1. **During the stall the app was mostly off CPU.** The window is
   44.6 s, after negative-body round 2.
   - Workers: 160 to 180 ms of CPU per 5 s, about 3.5% of one core.
   - Main thread: about 45 ms per 5 s.
   - Every worker sample is in
     `sb_store::query::counts::paging::read_search_relevance_page` →
     `sqlite3_step`, through `Engine::search_snapshot`.

   The read kept making progress, so this isn't a deadlock or a busy
   loop. It ran very slowly while mostly blocked off CPU. Time Profiler
   records running threads only, so it can't show what the read waited
   on. Under the swap pressure on this Mac, page faults are the likely
   cause, but this trace can't confirm it.
2. **Paired runs could separate a regression from the Mac's load.**
   Both attempts stalled on a typed prefix. That never happened in the
   `9f21847` profile, which passed on a less loaded Mac. Paired runs of
   `9f21847` and `963515c` on an idle Mac would show which it is.
3. **Before the stall, the 15 completed samples look like `9f21847`'s
   profile.** Worker time is in `read_search_relevance_page`,
   `paging::fetch`, `paging::stream` and `paging::read_page`, with
   pages from `pcache1Fetch`. The main thread's split is the same:
   Core Animation commit, layout, `MessageListViewController` and
   `AppKitPrototypeList`.

---

## Stall window (attempt 2)

Stall window: 297.125 s to 341.764 s (44.6 s), after sample negative-body round 2.

CPU by thread kind in the window (ms): main 1662, other 480, worker 1964

CPU per 5 s of the window (ms on main / workers / other):

```
   0-5   s  main    583  workers    421  other     47
   5-10  s  main    215  workers    304  other     81
  10-15  s  main    101  workers    221  other     67
  15-20  s  main     63  workers    173  other     64
  20-25  s  main     42  workers    158  other     59
  25-30  s  main     44  workers    172  other     63
  30-35  s  main     45  workers    180  other     68
  35-40  s  main    232  workers    156  other     16
  40-45  s  main    337  workers    179  other     15
```

| ms | thread |
|---:|---|
| 1662 | Main Thread (0x419e93c) (Sweetbriar, pid: 96215) |
| 639 | tokio-rt-worker (0x419fe63) (Sweetbriar, pid: 96215) |
| 607 | tokio-rt-worker (0x419fe3e) (Sweetbriar, pid: 96215) |
| 541 | tokio-rt-worker (0x41a080e) (Sweetbriar, pid: 96215) |
| 187 | Sweetbriar (0x419f25c) (Sweetbriar, pid: 96215) |
| 134 | Sweetbriar (0x419f08d) (Sweetbriar, pid: 96215) |
| 122 | tokio-rt-worker (0x41a08bb) (Sweetbriar, pid: 96215) |
| 95 | Sweetbriar (0x41a09e4) (Sweetbriar, pid: 96215) |
| 60 | Sweetbriar (0x41a08ba) (Sweetbriar, pid: 96215) |
| 50 | tokio-rt-worker (0x41a0e3c) (Sweetbriar, pid: 96215) |

##### Top 25 frames on worker threads in the window (inclusive)

| ms | % of that kind's CPU | frame |
|---:|---:|---|
| 1946 | 99.1 | `thread_start` |
| 1946 | 99.1 | `_pthread_start` |
| 1945 | 99.0 | `<std::thread::lifecycle::spawn_unchecked<<tokio::runtime::blocking::pool::Spawner>::spawn_thread::{closure#0}, ()>::{closure#1} as core::ops::function::FnOnc...` |
| 1945 | 99.0 | `<std::sys::thread::unix::Thread>::new::thread_start [atos 0x1060e6447]` |
| 1944 | 99.0 | `std::sys::backtrace::__rust_begin_short_backtrace::<<tokio::runtime::blocking::pool::Spawner>::spawn_thread::{closure#0}, ()> [atos 0x1060a2947]` |
| 1883 | 95.9 | `sqlite3_step` |
| 1751 | 89.2 | `<tokio::runtime::task::harness::Harness<tokio::runtime::blocking::task::BlockingTask<<sb_engine::engine::Engine>::read_through<sb_store::readers::Scoped<(sb_...` |
| 1751 | 89.2 | `<tokio::runtime::task::core::Core<tokio::runtime::blocking::task::BlockingTask<<sb_engine::engine::Engine>::read_through<sb_store::readers::Scoped<(sb_store:...` |
| 1751 | 89.2 | `<tokio::runtime::blocking::task::BlockingTask<<sb_engine::engine::Engine>::read_through<sb_store::readers::Scoped<(sb_store::query::counts::coverage::StoredS...` |
| 1750 | 89.1 | `<sb_engine::engine::Engine>::search_snapshot::<sb_store::lists::PageRead<sb_store::query::counts::paging::SearchPage>, <sb_engine::engine::Engine>::search_pa...` |
| 1750 | 89.1 | `<sb_store::readers::Readers>::read::<(sb_store::query::counts::coverage::StoredSearchCoverage, sb_store::lists::PageRead<sb_store::query::counts::paging::Sea...` |
| 1740 | 88.6 | `sb_store::query::counts::paging::read_search_relevance_page` |
| 1300 | 66.2 | `getPageNormal` |
| 1286 | 65.5 | `getAndInitPage [atos 0x105f5d6e7]` |
| 1258 | 64.1 | `pcache1Fetch` |
| 979 | 49.8 | `<sb_store::query::counts::Compiled>::read::<sb_store::query::counts::paging::Streamed, sb_store::query::counts::paging::stream::{closure#1}>` |
| 979 | 49.8 | `sb_store::query::counts::paging::fetch [atos 0x105e2f6ab]` |
| 959 | 48.8 | `sb_store::query::counts::paging::stream::{closure#1} [atos 0x105dcdb53]` |
| 907 | 46.2 | `sqlite3BtreeTableMoveto` |
| 895 | 45.6 | `sb_store::query::counts::paging::read_page [atos 0x105e317cb]` |
| 889 | 45.3 | `moveToChild` |
| 756 | 38.5 | `<rusqlite::row::Rows>::get_expected_row [atos 0x105de0d43]` |
| 573 | 29.2 | `sb_store::query::counts::paging::read_page [atos 0x105e316ff]` |
| 573 | 29.2 | `<sb_store::query::counts::Compiled>::read::<i64, sb_store::query::counts::paging::read_page::{closure#7}> [atos 0x105dc993f]` |
| 573 | 29.2 | `<rusqlite::statement::Statement>::query_row::<i64, rusqlite::params::ParamsFromIter<&[rusqlite::types::value::Value]>, sb_store::query::counts::paging::read_...` |

##### Top 25 frames on main threads in the window (inclusive, Sweetbriar only)

| ms | % of that kind's CPU | frame |
|---:|---:|---|
| 1220 | 73.4 | `main [atos 0x10437975b]` |
| 396 | 23.8 | `<deduplicated_symbol> [atos 0x10437f389]` |
| 396 | 23.8 | `<deduplicated_symbol> [atos 0x104428fb9]` |
| 250 | 15.0 | `<deduplicated_symbol> [atos 0x10437ee55]` |
| 235 | 14.1 | `ListBenchmarkRunner.runSearch(controller:)` |
| 235 | 14.1 | `closure #1 in AppDelegate.startBenchmark(mode:reportPath:)` |
| 159 | 9.6 | `BenchmarkTickSampler.tickFired(_:)` |
| 159 | 9.6 | `@objc BenchmarkTickSampler.tickFired(_:) [atos 0x1043aeff7]` |
| 143 | 8.6 | `closure #1 in MessageListViewController.show(_:) [atos 0x104412801]` |
| 135 | 8.1 | `closure #8 in ListBenchmarkRunner.runStore(controller:)` |
| 135 | 8.1 | `partial apply for closure #2 in ListBenchmarkRunner.runOnce(implementation:pass:generation:inheritedFailures:)` |
| 133 | 8.0 | `RunContext.tickDidFire(_:) [atos 0x1043dae1b]` |
| 132 | 7.9 | `specialized reset #1 (scope:) in ListBenchmarkRunner.runSearch(controller:) [atos 0x1043f7953]` |
| 130 | 7.8 | `SearchFieldModel.updateText(_:composition:)` |
| 129 | 7.8 | `closure #4 in MessageListViewController.init(bridge:sidebar:) [atos 0x10441110f]` |
| 126 | 7.6 | `RunContext.checkDisplay() [atos 0x1043dab27]` |
| 117 | 7.0 | `@objc AppKitPrototypeList.tableView(_:viewFor:row:) [atos 0x104380c17]` |
| 115 | 6.9 | `MessageListViewController.show(_:) [atos 0x10440fca7]` |
| 98 | 5.9 | `specialized static RunContext.displayID(_:) [atos 0x1043f5b93]` |
| 97 | 5.8 | `MessageListViewController.install(_:ticket:)` |
| 92 | 5.5 | `specialized AppKitPrototypeList.tableView(_:viewFor:row:) [atos 0x104383dcf]` |
| 76 | 4.6 | `specialized reset #1 (scope:) in ListBenchmarkRunner.runSearch(controller:) [atos 0x1043f8b6b]` |
| 48 | 2.9 | `MessageListViewController.detach()` |
| 34 | 2.0 | `AppKitPrototypeList.init(model:formatter:accountMarker:selectionLoss:onIndexChange:)` |
| 30 | 1.8 | `closure #3 in MainWindowController.install(_:) [atos 0x1044016bf]` |

## Generated analysis: main thread, search phase (attempt 2, 15 samples)

Samples: 60255 total, 31932 on the main thread (1 ms sampling, running threads only).
Timed keystrokes found: 15; report samples: 16.

**The keystroke count doesn't match the report: the per-sample alignment below pairs them in order and may be off.**

Search phase: 53.329 s to 341.764 s of the trace (288.4 s wall); main thread busy 23000 ms (8.0% of wall).

### Top 40 frames by self time (main thread, search phase)

| # | ms | % of main busy | frame |
|---:|---:|---:|---|
| 1 | 1375 | 6.0 | `objc_msgSend` |
| 2 | 492 | 2.1 | `_xzm_xzone_malloc_tiny` |
| 3 | 460 | 2.0 | `getMethodNoSuper_nolock(objc_class*, objc_selector*)` |
| 4 | 457 | 2.0 | `<deduplicated_symbol>` |
| 5 | 334 | 1.5 | `__CFStringHash` |
| 6 | 333 | 1.4 | `_platform_strcmp$VARIANT$Base` |
| 7 | 266 | 1.2 | `objc_retain` |
| 8 | 236 | 1.0 | `CFEqual` |
| 9 | 220 | 1.0 | `protocol_conformsToProtocol_nolock(protocol_t*, protocol_t*)` |
| 10 | 199 | 0.9 | `mach_msg2_trap` |
| 11 | 191 | 0.8 | `objc_class::demangledName(bool)` |
| 12 | 173 | 0.8 | `<no backtrace>` |
| 13 | 167 | 0.7 | `objc_release` |
| 14 | 164 | 0.7 | `_class_conformsToProtocol_unrealized_nolock(objc_class*, Protocol*)` |
| 15 | 161 | 0.7 | `_platform_memcmp` |
| 16 | 161 | 0.7 | `__CF_IS_OBJC` |
| 17 | 149 | 0.6 | `weak_entry_for_referent` |
| 18 | 147 | 0.6 | `__kdebug_trace64` |
| 19 | 144 | 0.6 | `list_array_tt<unsigned long, protocol_list_t, RawPtr>::iteratorImpl<false>::skipEmptyLists()` |
| 20 | 111 | 0.5 | `__CFSearchStringROM` |
| 21 | 96 | 0.4 | `___CFBasicHashFindBucket_Linear` |
| 22 | 94 | 0.4 | `_xzm_xzone_malloc_from_freelist_chunk` |
| 23 | 93 | 0.4 | `swift_retain` |
| 24 | 87 | 0.4 | `_platform_strlen` |
| 25 | 84 | 0.4 | `_xzm_free` |
| 26 | 72 | 0.3 | `_platform_memmove` |
| 27 | 71 | 0.3 | `objc::StringHashTable::tryGetIndex(char const*) const` |
| 28 | 67 | 0.3 | `objc_loadWeakRetained` |
| 29 | 67 | 0.3 | `protocol_getMethod_nolock(protocol_t*, objc_selector*, bool, bool, bool)` |
| 30 | 64 | 0.3 | `search_method_list(method_list_t const*, objc_selector*)` |
| 31 | 62 | 0.3 | `class_copyProtocolList` |
| 32 | 59 | 0.3 | `swift_allocObject` |
| 33 | 58 | 0.3 | `CFRetain` |
| 34 | 58 | 0.3 | `objc_autorelease` |
| 35 | 57 | 0.2 | `ItemVariationStore::ValueForDeltaSet(unsigned short, unsigned short, unsigned long, short const*, double const*) const` |
| 36 | 57 | 0.2 | `_xzm_xzone_malloc_freelist_outlined` |
| 37 | 55 | 0.2 | `_Block_copy` |
| 38 | 52 | 0.2 | `bool objc::DenseMapBase<objc::DenseMap<void const*, objc::ObjcAssociation, objc::DenseMapValueInfo<objc::ObjcAssociation>, objc::DenseMapInfo<void const*>, o...` |
| 39 | 51 | 0.2 | `std::__1::pair<swift::HashMapElementWrapper<swift::GenericCacheEntry>*, unsigned int> swift::ConcurrentReadableHashMap<swift::HashMapElementWrapper<swift::Ge...` |
| 40 | 50 | 0.2 | `os_unfair_lock_lock` |

### Top 40 frames by inclusive time (main thread, search phase)

| # | ms | % of main busy | frame |
|---:|---:|---:|---|
| 1 | 17167 | 74.6 | `main [atos 0x10437975b]` |
| 2 | 17167 | 74.6 | `-[NSApplication run]` |
| 3 | 17167 | 74.6 | `start` |
| 4 | 17155 | 74.6 | `-[NSApplication(NSEventRouting) _nextEventMatchingEventMask:untilDate:inMode:dequeue:]` |
| 5 | 17155 | 74.6 | `-[NSApplication(NSEventRouting) nextEventMatchingMask:untilDate:inMode:dequeue:]` |
| 6 | 17116 | 74.4 | `_DPSNextEvent` |
| 7 | 17114 | 74.4 | `_DPSBlockUntilNextEventMatchingListInMode` |
| 8 | 17113 | 74.4 | `_BlockUntilNextEventMatchingListInMode` |
| 9 | 17112 | 74.4 | `ReceiveNextEventCommon` |
| 10 | 17067 | 74.2 | `RunCurrentEventLoopInMode` |
| 11 | 16992 | 73.9 | `_CFRunLoopRunSpecificWithOptions` |
| 12 | 16713 | 72.7 | `__CFRunLoopRun` |
| 13 | 12732 | 55.4 | `UC::DriverCore::continueProcessing()` |
| 14 | 12472 | 54.2 | `stepTransactionFlush` |
| 15 | 11650 | 50.7 | `CA::Transaction::commit()` |
| 16 | 11584 | 50.4 | `CA::Transaction::flush_as_runloop_observer(bool)` |
| 17 | 11090 | 48.2 | `__CFRunLoopDoSources0` |
| 18 | 11064 | 48.1 | `__CFRUNLOOP_IS_CALLING_OUT_TO_A_SOURCE0_PERFORM_FUNCTION__` |
| 19 | 11064 | 48.1 | `__CFRunLoopDoSource0` |
| 20 | 7855 | 34.2 | `CA::Transaction::run_commit_handlers(CATransactionPhase)` |
| 21 | 7783 | 33.8 | `NSDisplayCycleFlush` |
| 22 | 7752 | 33.7 | `NSDisplayCycleObserverInvoke` |
| 23 | 7497 | 32.6 | `0x19469f4a4` |
| 24 | 7489 | 32.6 | `0x19469f47c` |
| 25 | 5832 | 25.4 | `NSPerformVisuallyAtomicChange` |
| 26 | 5659 | 24.6 | `completeTaskWithClosure(swift::AsyncContext*, swift::SwiftError*)` |
| 27 | 5133 | 22.3 | `<deduplicated_symbol> [atos 0x10437f389]` |
| 28 | 5133 | 22.3 | `<deduplicated_symbol> [atos 0x104428fb9]` |
| 29 | 5014 | 21.8 | `-[NSWindow(NSConstraintBasedLayoutInternal) layoutIfNeeded]` |
| 30 | 5007 | 21.8 | `-[NSView layoutSubtreeIfNeeded]` |
| 31 | 5004 | 21.8 | `-[NSView _layoutSubtreeIfNeededAndAllowTemporaryEngine:]` |
| 32 | 5003 | 21.8 | `__56-[NSView _layoutSubtreeIfNeededAndAllowTemporaryEngine:]_block_invoke` |
| 33 | 4966 | 21.6 | `-[NSView _layoutSubtreeWithOldSize:]` |
| 34 | 4966 | 21.6 | `__36-[NSView _layoutSubtreeWithOldSize:]_block_invoke` |
| 35 | 4897 | 21.3 | `_NSViewLayout` |
| 36 | 4885 | 21.2 | `-[NSWindow(NSConstraintBasedLayoutInternal) _layoutViewTree]` |
| 37 | 4883 | 21.2 | `___NSViewLayout_block_invoke` |
| 38 | 4842 | 21.1 | `0x194664178` |
| 39 | 4842 | 21.1 | `0x194664d5c` |
| 40 | 4798 | 20.9 | `__NSWindowGetDisplayCycleObserverForLayout_block_invoke` |

### Top 40 Sweetbriar frames by inclusive time (main thread, search phase)

Frames from the Sweetbriar and SweetbriarBridge binaries (the Rust core is linked into SweetbriarBridge).

| # | ms | % of main busy | frame |
|---:|---:|---:|---|
| 1 | 17167 | 74.6 | `main [atos 0x10437975b]` |
| 2 | 5133 | 22.3 | `<deduplicated_symbol> [atos 0x10437f389]` |
| 3 | 5133 | 22.3 | `<deduplicated_symbol> [atos 0x104428fb9]` |
| 4 | 2985 | 13.0 | `<deduplicated_symbol> [atos 0x10437ee55]` |
| 5 | 2031 | 8.8 | `closure #1 in MessageListViewController.show(_:) [atos 0x104412801]` |
| 6 | 1705 | 7.4 | `closure #1 in AppDelegate.startBenchmark(mode:reportPath:)` |
| 7 | 1673 | 7.3 | `@objc AppKitPrototypeList.tableView(_:viewFor:row:) [atos 0x104380c17]` |
| 8 | 1608 | 7.0 | `MessageListViewController.install(_:ticket:)` |
| 9 | 1348 | 5.9 | `ListBenchmarkRunner.runSearch(controller:)` |
| 10 | 1292 | 5.6 | `SearchFieldController.applyModel()` |
| 11 | 1287 | 5.6 | `specialized AppKitPrototypeList.tableView(_:viewFor:row:) [atos 0x104383dcf]` |
| 12 | 1268 | 5.5 | `closure #1 in closure #2 in implicit closure #1 in SearchFieldController.observeModel()` |
| 13 | 701 | 3.0 | `@objc BenchmarkTickSampler.tickFired(_:) [atos 0x1043aeff7]` |
| 14 | 691 | 3.0 | `BenchmarkTickSampler.tickFired(_:)` |
| 15 | 679 | 3.0 | `closure #4 in MessageListViewController.init(bridge:sidebar:) [atos 0x10441110f]` |
| 16 | 668 | 2.9 | `SearchFieldModel.updateText(_:composition:)` |
| 17 | 648 | 2.8 | `specialized reset #1 (scope:) in ListBenchmarkRunner.runSearch(controller:) [atos 0x1043f7953]` |
| 18 | 630 | 2.7 | `@nonobjc NSButton.__allocating_init(title:target:action:)` |
| 19 | 527 | 2.3 | `MessageListViewController.show(_:) [atos 0x10440fca7]` |
| 20 | 526 | 2.3 | `partial apply for closure #2 in ListBenchmarkRunner.runOnce(implementation:pass:generation:inheritedFailures:)` |
| 21 | 525 | 2.3 | `closure #8 in ListBenchmarkRunner.runStore(controller:)` |
| 22 | 510 | 2.2 | `RunContext.tickDidFire(_:) [atos 0x1043dae1b]` |
| 23 | 510 | 2.2 | `closure #2 in AppKitPrototypeList.init(model:formatter:accountMarker:selectionLoss:onIndexChange:) [atos 0x10437faab]` |
| 24 | 501 | 2.2 | `ListWindowModel.open()` |
| 25 | 494 | 2.1 | `MessageListViewController.detach()` |
| 26 | 484 | 2.1 | `RunContext.checkDisplay() [atos 0x1043dab27]` |
| 27 | 423 | 1.8 | `specialized reset #1 (scope:) in ListBenchmarkRunner.runSearch(controller:) [atos 0x1043f8b6b]` |
| 28 | 394 | 1.7 | `specialized static RunContext.displayID(_:) [atos 0x1043f5b93]` |
| 29 | 336 | 1.5 | `specialized closure #1 in FrameBatcher.post(_:) [atos 0x105a10107]` |
| 30 | 333 | 1.4 | `closure #1 in ListWindowModel.queue(_:)` |
| 31 | 333 | 1.4 | `specialized FrameBatcher.enqueue(_:) [atos 0x105a01da7]` |
| 32 | 321 | 1.4 | `closure #6 in ListBenchmarkRunner.runSearch(controller:)` |
| 33 | 309 | 1.3 | `SearchFieldController.refreshScopeMenu()` |
| 34 | 308 | 1.3 | `closure #3 in MainWindowController.install(_:) [atos 0x1044016bf]` |
| 35 | 307 | 1.3 | `ListWindowModel.deliver(_:) [atos 0x105a0ef9b]` |
| 36 | 307 | 1.3 | `closure #1 in AppKitPrototypeList.init(model:formatter:accountMarker:selectionLoss:onIndexChange:)` |
| 37 | 304 | 1.3 | `AppKitPrototypeList.pagesDidLand(_:)` |
| 38 | 291 | 1.3 | `AppKitPrototypeList.init(model:formatter:accountMarker:selectionLoss:onIndexChange:)` |
| 39 | 263 | 1.1 | `PlainTextReaderView.text.setter` |
| 40 | 242 | 1.1 | `MessageRowCellView.init(showsAccountMarker:) [atos 0x1043821c7]` |

### Top 40 Sweetbriar frames by attributed self time (main thread, search phase)

Each sample is charged to its leaf-most Sweetbriar frame, so system work a Sweetbriar call causes counts against that call.

| # | ms | % of main busy | frame |
|---:|---:|---:|---|
| 1 | 14575 | 63.4 | `main [atos 0x10437975b]` |
| 2 | 588 | 2.6 | `@nonobjc NSButton.__allocating_init(title:target:action:)` |
| 3 | 527 | 2.3 | `MessageListViewController.show(_:) [atos 0x10440fca7]` |
| 4 | 507 | 2.2 | `closure #2 in AppKitPrototypeList.init(model:formatter:accountMarker:selectionLoss:onIndexChange:) [atos 0x10437faab]` |
| 5 | 483 | 2.1 | `MessageListViewController.detach()` |
| 6 | 423 | 1.8 | `specialized reset #1 (scope:) in ListBenchmarkRunner.runSearch(controller:) [atos 0x1043f8b6b]` |
| 7 | 394 | 1.7 | `specialized static RunContext.displayID(_:) [atos 0x1043f5b93]` |
| 8 | 299 | 1.3 | `SearchFieldController.refreshScopeMenu()` |
| 9 | 294 | 1.3 | `closure #6 in ListBenchmarkRunner.runSearch(controller:)` |
| 10 | 242 | 1.1 | `MessageRowCellView.init(showsAccountMarker:) [atos 0x1043821c7]` |
| 11 | 236 | 1.0 | `PlainTextReaderView.text.setter` |
| 12 | 204 | 0.9 | `ListBenchmarkRunner.runSearch(controller:)` |
| 13 | 201 | 0.9 | `BadgeView.init(frame:)` |
| 14 | 165 | 0.7 | `MessageRowCellView.init(showsAccountMarker:) [atos 0x1043821e7]` |
| 15 | 162 | 0.7 | `AppKitPrototypeList.buildTable()` |
| 16 | 127 | 0.6 | `MessageRowCellView.init(showsAccountMarker:) [atos 0x104381a3f]` |
| 17 | 112 | 0.5 | `@nonobjc NSTableColumn.init(identifier:)` |
| 18 | 112 | 0.5 | `MessageRowFormatter.dateString(_:now:) [atos 0x1044165a7]` |
| 19 | 100 | 0.4 | `MessageRowCellView.configure(content:)` |
| 20 | 90 | 0.4 | `closure #1 in SearchFieldController.rebuildChips()` |
| 21 | 87 | 0.4 | `MessageRowCellView.init(showsAccountMarker:) [atos 0x104381b23]` |
| 22 | 86 | 0.4 | `MessageListViewController.install(_:ticket:)` |
| 23 | 84 | 0.4 | `MessageListViewController.updateStatus()` |
| 24 | 84 | 0.4 | `closure #1 in closure #1 in static SystemProbes.physicalFootprint()` |
| 25 | 77 | 0.3 | `@nonobjc NSScrollView.init()` |
| 26 | 69 | 0.3 | `AppKitPrototypeList.pagesDidLand(_:)` |
| 27 | 67 | 0.3 | `specialized SearchCoverageNote.init(coverage:excludedUnknown:locale:timeZone:) [atos 0x105a247a7]` |
| 28 | 66 | 0.3 | `SearchFieldController.rebuildChips()` |
| 29 | 65 | 0.3 | `MessageRowFormatter.dateString(_:now:) [atos 0x104416657]` |
| 30 | 65 | 0.3 | `SearchFieldController.refreshStatus()` |
| 31 | 61 | 0.3 | `closure #1 in MessageRowCellView.applyLayerColors()` |
| 32 | 61 | 0.3 | `SearchFieldController.refreshErrorMarking()` |
| 33 | 53 | 0.2 | `@objc MessageRowCellView.isFlipped.getter [atos 0x10438350b]` |
| 34 | 51 | 0.2 | `MessageRowCellView.init(showsAccountMarker:) [atos 0x1043822cf]` |
| 35 | 46 | 0.2 | `closure #1 in BenchmarkTickSampler.start(on:)` |
| 36 | 44 | 0.2 | `@nonobjc NSTableView.init()` |
| 37 | 44 | 0.2 | `specialized static RunContext.displayID(_:) [atos 0x1043f5be3]` |
| 38 | 42 | 0.2 | `@nonobjc NSMenuItem.init(title:action:keyEquivalent:)` |
| 39 | 42 | 0.2 | `@objc ClauseChipButton.init(frame:) [atos 0x104425aff]` |
| 40 | 41 | 0.2 | `SearchFieldController.applyModel()` |

### Named areas, main thread, search phase

Inclusive: a sample counts for every area with a matching frame on its stack.

| area | ms | % |
|---|---:|---:|
| AppKitPrototypeList | 2744 | 11.9 |
| MessageRowCellView | 1545 | 6.7 |
| Row presentation / formatters | 66 | 0.3 |
| ListWindowModel | 1070 | 4.7 |
| FrameBatcher | 338 | 1.5 |
| MessageListViewController | 2837 | 12.3 |
| SearchFieldController | 1435 | 6.2 |
| SearchFieldModel | 814 | 3.5 |
| Benchmark harness (runner, tick sampler) | 2602 | 11.3 |
| -[NSTableView reloadData] | 457 | 2.0 |
| NSTableView (any) | 3849 | 16.7 |
| NSView layout (layoutSubtreeIfNeeded) | 5007 | 21.8 |
| NSView display (displayIfNeeded / drawRect) | 788 | 3.4 |
| NSTextView insertText | 72 | 0.3 |
| NSTextCheckingController | 231 | 1.0 |
| Core Animation commit (CA::Transaction::commit) | 11650 | 50.7 |
| NSDisplayCycleFlush | 7783 | 33.8 |
| Swift concurrency job (completeTaskWithClosure) | 5659 | 24.6 |
| Rust core (sb_*) | 3 | 0.0 |

### The timed keystroke itself

Main-thread samples inside the timed keystroke's `post` closure (`-[NSTextView insertText:]` of the last character, synchronous). `span` runs from its first to its last sample; on-CPU time below `span` means the main thread was off CPU inside the call.

| sample | rendered ms | span ms | on-CPU ms |
|---|---:|---:|---:|
| exact-reference r0 | 1312 | 119 | 31 |
| exact-address r0 | 1585 | 38 | 7 |
| phrase r0 | 3453 | 80 | 18 |
| scoped-filter r0 | 2158 | 180 | 29 |
| negative-body r0 | 10172 | 65 | 16 |
| exact-reference r1 | 1113 | 84 | 22 |
| exact-address r1 | 1662 | 99 | 6 |
| phrase r1 | 1015 | 47 | 8 |
| scoped-filter r1 | 7376 | 207 | 38 |
| negative-body r1 | 5071 | 54 | 15 |
| exact-reference r2 | 3266 | 477 | 53 |
| exact-address r2 | 9655 | 215 | 29 |
| phrase r2 | 6647 | 100 | 27 |
| scoped-filter r2 | 7392 | 101 | 17 |
| negative-body r2 | 8833 | 77 | 7 |

### Main-thread busy ms per 100 ms bucket after each timed keystroke

Column k is [k*100, (k+1)*100) ms after the keystroke; `rendered` is the report's keystroke-to-rows time for that sample.

```
sample                    rendered   0   1   2   3   4   5   6   7   8   9  10  11  12  13  14  15  16  17  18  19  20  21  22  23  24  25  26  27  28  29  30  31  32  33  34  35  36  37  38  39
exact-reference r0            1312  25  32  17  21  21   5   6  17  30  21  22  13  20  45  16  10  11   7  10  33   8   8   3  19  10  22  10  25  13  26   4  16   9   8  14   4   7   5  10   2
exact-address r0              1585  28  34  30   3  16  13  20  26  29  15  15  26  19  33  24  26  23   8  12   8  21  15   7  27  22  14  39  26  14   7  16  26   6  21  24  10  25  20  17  44
phrase r0                     3453  24  19  19  11   5   5  11  10   4  25  12  21  27  17  17   1   2   0   1   3   1   0   0   0   0   1  19  15  18  33  25  13  29  19  19   2   3   0   2   0
scoped-filter r0              2158  18  14  23  10  25  24  17  20  25  19  22  13  25  21   9   9  27   6  15   8  10  16  30  31  12  32  12  19  31  24   3   0   4   0   1   0   1   0   1   0
negative-body r0             10172  23   0   2   1   2   2   2   5   2   4   2   2   2   5   3   2   4   3   5   1   2   2   2   5   3   1   2   1   0   1   1   4   3   2   1   4   2   4   1   3
exact-reference r1            1113  23  12   6  11  16  24  19  15   6  21   8  24  27  41  22  25  37  22  21  22  30  21  12   2  22  32  13  32  20  30   6  21  20  26   4   5   0   2   1   0
exact-address r1              1662   7  44   7  30  24  30  10  19  15  33   5   8   0   1   2   0  15  38  36  57  21  20  29  17  20  20  43  14   0   2   1  14  17  13   5   1   9  16   6   1
phrase r1                     1015  17  17  10   5   2   8  18   4   1  19  50  23  33   4   2   1   1   0   1   0   0   0   1   0   0   1   0   0   0   1   3   0   1   2   1   1   4  17  32  23
scoped-filter r1              7376  14  21  22  10  12  13  18  28  12  16  15  18  12  18   5  23  18  25  23  24  30  18  27  29  26  14  20   4  21  18   2   2   0  14   4   1   1   0   2   1
negative-body r1              5071  20   3  17  12  11  18  11   2   1   1   1   2   2   2   1   4   2   3   1   2   2   3   1   2   3   2   3   2   1   2   3   2   2   2   2   2   3   4   5   2
exact-reference r2            3266   5   6   6  25  14  15  14  12   9  18  16  19  26  20   7   8  13   9  11  14   2   6  11  13  34  22  23  24  26  16  11  19  15   3   2  15   7   2   5   2
exact-address r2              9655  16   8  12  24  44   9  16  18   2   5  14   8  11  16  19  17  15  13  18  21  13   9   8   8   8  14  19  28  11  20   4   5   6   3  10   7  17   2  10  11
phrase r2                     6647  26  18  11   6  12   6  11  11  11  12   2  14   8   1   7   4   5   4   7   6   6   5   7   6  17   7   6  23   9   8   4   9   7  10  14  15  10   9   2   4
scoped-filter r2              7392  16   5  14  13   5   7  11  18  11   8  22  18   6   9   3   4  15   8   6   4   4  10   0   1   2   5   5   5   9  19  10  12  11  10   8  16  12   1  10   3
negative-body r2              8833  14   5  16  16   7  12   2   2  10  13  19   6   1   1   1   0   0   0   2   1   0   1   0   1   1   1   1   0   1   2   1   1   1   1   1   1   2   0   2   0
mean                                18  16  14  13  14  13  12  14  11  15  15  14  15  16   9   9  13  10  11  14  10   9   9  11  12  13  14  15  12  14   6  10   9   9   7   6   7   5   7   6
```

### Main-thread busy ms per 100 ms bucket, whole search phase

One line per second of trace time, ten buckets per line; `*` marks a second holding a timed keystroke.

```
    53.3s     5   1   1   0   1   1   0   0   0   2
    54.3s     3   1   1   1   5   1   2   1   1   2
    55.3s     0   0   0   1   3   2   1   1   1   0
    56.3s     0   1   0   1   2   4   0   1   3   1
    57.3s     2   2  12  13  12   5  18  39   3   1
    58.3s *  25  32  17  21  21   5   6  17  30  21
    59.3s    22  13  20  45  16  10  11   7  10  33
    60.3s     8   8   3  19  10  22  10  25  13  26
    61.3s     4  16   9   8  14   4   7   5  10   2
    62.3s     5  20  12  21  36  14  17  19   9  13
    63.3s    26  22  20  18  15  16  11  29  17  23
    64.3s    31  25  18  28  13  25  10  15  32   4
    65.3s *  23  28  16  17  28   9  35  24  29  31
    66.3s    30   3  17  13  19  27  28  15  15  31
    67.3s    14  36  24  25  21   8  13   8  20  16
    68.3s     9  25  26  10  43  21  14  10  13  26
    69.3s     6  21  24  12  24  19  20  46   8  12
    70.3s     7  15  26  18  45  30  28  12  12  19
    71.3s *  18  19  12  13  19  23  15  20  18  17
    72.3s    15   8   6  11   9   5  24  11  23  27
    73.3s    13  18   3   2   1   1   3   1   0   0
    74.3s     0   0   1  14  19  16  32  28  10  29
    75.3s    23  19   1   3   1   2   0   0   0   1
    76.3s     0   0   0   0   0   1   2  23  49  23
    77.3s    23  32  27  50  29  26  53  22  11  12
    78.3s    13  39  27  27  15  28  17   2   6   9
    79.3s *   5   7  16  11  29  21  13  14  25  11
    80.3s    22  24  17  25  21  21  19  13  27  21
    81.3s     7  10  28   5  16   6  11  17  28  32
    82.3s    15  30  11  19  32  22   3   0   4   0
    83.3s     1   0   1   0   1   0   1   0   1   2
    84.3s     0   1   0   2   1   1   1   1   2   4
    85.3s     3   2   2   2   2   2   3   3   2   2
    86.3s     2   4   2   2   2   2   2   2   2   1
    87.3s     2   3   2   2   2   4   3   5   5   3
    88.3s     4   3   2   4   2   3   2   3   3   7
    89.3s     2   3   2   3   2   2   2   2   3   2
    90.3s     1   4   3   3   1   6   2   4   3   4
    91.3s    11   2   2   3   2   2   2   3   2   4
    92.3s     2  12  23  26  53  29  19  24  14   6
    93.3s    14   9  13  16  15  19  26  45  15  29
    94.3s    20  26  14  20  15  14  16  17  55   9
    95.3s     9  11   7   9  19   2   3   2   2   3
    96.3s     2   4   5   2   3   2   2   3   4   4
    97.3s     3   4   1   3   3   4   4   2   2   3
    98.3s     2   2   2   2   3   3   1   3   2  17
    99.3s *  36  21   6   7  14   0   2  11  12   0
   100.3s     2   2   2   2   5   1   3   4   2   2
   101.3s     3   4   2   4   4   3   3   2   2   2
   102.3s     3   4   2   2   1   1   0   2   2   4
   103.3s     3   0   2   4   3   3   1   5   4   3
   104.3s     9   5   2   1   2   3   2   2   3   2
   105.3s     2  10  13  25  20  24   9  15  21  23
   106.3s    17   6  21  15  32   8   2   2   0   1
   107.3s     1   1   1   2   2   2   4   2   0   3
   108.3s     0   2   1   2   4   3   1   1   3   4
   109.3s    12  20   6  14  10  41  24  22  14   1
   110.3s     1   0   0   1   1   1   4   4   1   0
   111.3s     5   2   2   4   4   1   4   2   0   2
   112.3s     0   1   2   1   1   2   0   1   0   2
   113.3s     0   1   0   0   0   1   1   2   0   2
   114.3s     1   0   0   0   0   0   1   1   3   7
   115.3s    18  34  37  23  22  28   9  17  23  11
   116.3s    16  25  45  24  19  18  22  35  17  31
   117.3s    25   6  17  18  22  28   4   4  13   5
   118.3s    12  11   3   2   5   2   2   2   2   2
   119.3s     3   1   1   1   1   1   0   0   0   1
   120.3s     1   1   2   5   3   2   1   1   5   2
   121.3s     1   2   0   1   0   1   3   0   0   0
   122.3s     2   1   1   1   1   0   1   7   1   0
   123.3s     1   0   0   0   0   3   5  21  16  16
   124.3s *   3   2   4  19   6  16   3  19  20  20
   125.3s    24   0  18  15  18  29  39  22  28  24
   126.3s *  26  31  16  28  24  21   1   5  46   7
   127.3s    29  25  30  10  19  13  34   6   8   0
   128.3s     1   2   0  15  36  36  59  21  19  30
   129.3s *  17  20  19  44  14   0   2   1  14  16
   130.3s    14   4   2   9  16   6   1  18  41  32
   131.3s    34   3   3   1   1   0   1   0   0   0
   132.3s     1   0   0   1   0   0   0   0   3   1
   133.3s     1   2   1   1   3  15  32  21  49  38
   134.3s    26  38  22  21  38  16  16  14   4   9
   135.3s    10   2   5  18  18   6  16  10  11   4
   136.3s *   6  10   2   4   7  11  24  22   9  10
   137.3s    11  21  25  15  14  16  20  11  13  14
   138.3s    17  20  20  23  23  31  23  28  26  24
   139.3s    19  22   5  13  23   5   2   0  11   7
   140.3s     1   0   1   1   2   2   2   1   1   2
   141.3s     0   3   1   1   0   0   0   1   0   0
   142.3s     1   3   1   0   0   0   0   0   0   0
   143.3s     1   4   2   1   6  21  20  16  26   0
   144.3s     1   1   1   0   0   1   1   0   0   1
   145.3s     1   2   2   3   1   1   0   1   1   0
   146.3s     0   0   0   0   0   0   1   0   0   1
   147.3s     0   0   0   0   0   0   0   2   2   8
   148.3s    26  30  42  35  32  30  20  23  10   8
   149.3s    17  29  23  24  24  13   5   8  10  15
   150.3s     7  25  17  11   0   0   1   1   3   2
   151.3s     0   0   2   1   3   8   4  35  37  29
   152.3s    17  13  10  22  10  18  37  28   4  12
   153.3s    21  13  13  19  16   8   2  13  16  16
   154.3s    29  10   9  16  19   6  17   2   2   6
   155.3s    11   7   1   1   3   1   1   0   3   2
   156.3s     0   1   0   3   2   2   0   1   1   4
   157.3s     3   1   1   2   1   5   3   2   4   1
   158.3s     3   3   3   3   2   0   1   5   2   3
   159.3s     9   9  28  25  10  49   8  10  11  12
   160.3s *  24  31  11   3  20   5  10  12  15  10
   161.3s    21   3   0   2   1   1   3   1   1   4
   162.3s     3   2   1   2   2   2   2   2   3   2
   163.3s     2   3   1   2   3   2   1   3   2   2
   164.3s     3   3   6   2   8  26  26  25  22  13
   165.3s    13  12  24  11   6   2   1   1   2   2
   166.3s     2   3   3   5   2   2   2   2   3   2
   167.3s     1   2   2   4   3   5   2   3   3   2
   168.3s     4   2   2   3   3   5   4   2   5   3
   169.3s     4   4   3   2   3   3   7   3   4   3
   170.3s     2   3   1   4   2   1   3   3   4   2
   171.3s     3   3   4   3   1   1   2   4   2   2
   172.3s     2   2   2   1   3   2   2   2   2   2
   173.3s     3   1   2   1   2   0   3   1   3   7
   174.3s     3   7   5   2   3   2   2   5   7   5
   175.3s    10  19  10   5  14   7  17   9  33   7
   176.3s    28  19   6  10   8   6  15  10  16  17
   177.3s    22  20  14   9   6   6  14  11  15  15
   178.3s    10  14  15   8  15   6  13   8  20   7
   179.3s    12  10   5   8  12   9   0   7   5  10
   180.3s    28  20   8   8  11   7  14  12   4  15
   181.3s    16   5   9  15   5  13  13  19   6   4
   182.3s     7   5   9   6  10   1   7   6   6   0
   183.3s    14  15  12  16  15   9   5  12   4  13
   184.3s     4   5  12   9  17   6  10  11   2   0
   185.3s     6   9   3   3   3   9   4  14  13   8
   186.3s     4   8   5   7   3   6   7   6   8   6
   187.3s     7  11   8   9  15  12  14   5  12  15
   188.3s    10   2   9  14   3  10  10   5   4   1
   189.3s    15   9   3   5   5   4   0   7  17  13
   190.3s *   8  11  15   3   9   8   2   2   2   2
   191.3s     5   9  23  11  14  19  12   6  20  17
   192.3s    12  32  11  17  10  12   9  10  11   6
   193.3s     4  14   6  28  26  31  23  21  20  11
   194.3s    13  18   9   2   4  14   6   5   1   4
   195.3s     7  18  15  18   6   6   6  14  10   7
   196.3s     6  11  12   4   7  11   9  18   7   9
   197.3s     2  10   1   8  10  11  17  14   6   6
   198.3s    11  10   7  12  12  10  15  12   3   4
   199.3s     6   6   2   3   2   9   6  10   3   5
   200.3s     9  24  13  30   4   9  11  10  20   7
   201.3s     8   8  11   6   9   5   5   7  21  21
   202.3s    10  11   8  12   7  11  24   9  14  16
   203.3s    23  25  19   9   0   3   5   9  12  12
   204.3s     3   6   4  12  15   7   8  21  10  11
   205.3s *  11   7  28  15  20  21   9  17  15  14
   206.3s    10  10  29  39  11  17  15   2   6  14
   207.3s     7  11  17  23  13  14  15  18  21  11
   208.3s     9  10   6   9  13  20  27  14  18   4
   209.3s     5   7   1  10   8  16   2  14   9   8
   210.3s    10   8   9   3   3   4  12  22  11  12
   211.3s     6  26  13   9  16  13  14   9   8   1
   212.3s     9   5   9  14  24  14   8  10   5   4
   213.3s     9  19  12  11  12   9   6   5  14  19
   214.3s    18  18  12  11  26   9   3   7  25  18
   215.3s    23   8  21  17  25   7   8  11   4  12
   216.3s    10   9  21  13  17  20  17   5  13  13
   217.3s     7   6   8  14   4   6  17   5   2   6
   218.3s     6  10  14  10   5  10  16   1   5   8
   219.3s     9  11  42  12   9  12  18   9   6   5
   220.3s    10   7   9  15  23   6   6   5  12  10
   221.3s    18  16  16  24   6  12  15  18  32  12
   222.3s    14  17  15  18  10  11  11   9  10  19
   223.3s *  10  18  21   8   9  12   8   0   0  17
   224.3s    21  10  13   6   8   7  14   8  15   5
   225.3s     8  11   6   4   6   3   6   3   8   3
   226.3s     8   7   5  20   3   7   9  23   9   4
   227.3s     6   8  12  15  11   9  13   5   2   3
   228.3s     7  11   5  16  13  13  34  16  25  26
   229.3s     4   0   0   2   9   5   9   2   4   5
   230.3s    16  20  17  11  18  13   4   1   3   2
   231.3s     2   2   2   4   1   0   2   1   0   0
   232.3s     0   1   1   0   0   2   0   0   1   1
   233.3s     1   0   1   6   7   1   0   2   1   1
   234.3s     0   8   7  10  10   9   4   7   6   7
   235.3s    13  11   6   8  11  28  35   8  19  33
   236.3s    24  17   6   9  11  13  18  14  11   3
   237.3s     4   3  14   8  15  22  15  13   7   9
   238.3s     7   7   9  18  10  14   2   4   9  12
   239.3s     9   6   0   0   0   2   2  11   1   2
   240.3s     7   1   5   4  10   1   3   6   5   2
   241.3s    10  23  17   9  11   7  10   6  16  10
   242.3s    12  18   4   4   1   6   5   4   7   7
   243.3s     2   1   7   4   7  18   1   3   4   6
   244.3s     4   4   5   8   7  11  10   4   5   4
   245.3s     3   5   6   8   7   7   4   2   4  12
   246.3s     4  11   6   9   3   4   7   5   5   9
   247.3s     1   5   7  17   7  13  15   8   8   2
   248.3s     9  13  16  20   8  18  12  17   8   8
   249.3s    14  11  11  10  13  11   8  12  11  13
   250.3s *  12  14  15   6  14  11   5   7  12  20
   251.3s     8   8  25  15   9   6   3   7  13   7
   252.3s     6   5   3  10   0   1   3   4   5   6
   253.3s     9  21   7  15   8  10   9  16  11   4
   254.3s     7   3  10   5   9   4  13  18  13  11
   255.3s     8   5   4   4   8   8   3   5   3   3
   256.3s     7   5   4   9  25   8  10   9  13   9
   257.3s     9   7   7   2  19   4   2   1   2   2
   258.3s     2   2   3   2   3   1   4   1   1   1
   259.3s     2   1   2   3   1   3   1   2   2   1
   260.3s     2   1   1   1   1   1   2   2   3   1
   261.3s     0   1   2   3   2   2   1   2   0   0
   262.3s     0   0   0   0   0   2   0   0   2   0
   263.3s     1   1   2   2   1   1   2   2   2   0
   264.3s     1   1   3   7   6   1   5  10   5  11
   265.3s    11   7  28   5   8   8  11   3  10   6
   266.3s     7   8   2   6   8   5   9   2   9  10
   267.3s     5   3  11   5  11  13   5   4   3   4
   268.3s     7   5   1   9  11  13   9   8   6  13
   269.3s    13   6   7   7  11   8  13   5   8   8
   270.3s     8   3  10   5   6   9  10  11   2   9
   271.3s    12   7   0   4  11  22  17  14  20   8
   272.3s    12   4  22   5  18   4  13   5   4   8
   273.3s     4   6  14  15  16  14  12  10   9   8
   274.3s    16   7  14  14   4   6   6   2   4   2
   275.3s     3   4   7  14   8   8   5   1   8   5
   276.3s     8   3   5   3   5   2   6   1   9  20
   277.3s    18   9   2   1   1   2   1   0   0   5
   278.3s     2  14   1   1   0   0   0   0   0   0
   279.3s     2   1   1   1   0   0   0   1   0   0
   280.3s     1   0   0   3   0   0   0   0   2   1
   281.3s     0   0   0   2   0   1   0   1   1   0
   282.3s     0   0   0   1   2   8   2   6   9   7
   283.3s *  14  23  16  21  11  22  14  20   8  13
   284.3s    11  17  11  11   2   1  10  15  12  13
   285.3s     1   0   2   0   0   0   1   1   1   1
   286.3s     0   1   1   1   1   0   1   2   1   1
   287.3s     1   1   1   1   1   1   1   1   5   1
   288.3s     1   2   0   0   2   3   2   2   3   3
   289.3s     3   0   0   2   0   2   2   2   2   3
   290.3s     2   1   1   1   2   3   3   3   2   3
   291.3s     3   2   3   2   4   2   3   3   2   4
   292.3s     9  15  24  26  18  15  23   2   2   2
   293.3s     2   3   2   2   2   2   3   3   2   2
   294.3s     3   2   3   2   3   2   2   3   2   4
   295.3s     2   2   2   2   2   1   2   3   3   2
   296.3s     2   3   3   3   7   0   9  13  10  14
   297.3s    29  13  25  13  17   7  15  12  22  11
   298.3s     5   4   7  13   5  26  10  10  17  12
   299.3s     8   7   5   6   3   5  15  11  30  17
   300.3s    17  14   4   8  15  14  22   8   6   4
   301.3s     6   4  10   7   7   9  14   9   5   5
   302.3s     9  11  11  12  12   9   9   5   0  12
   303.3s     2   3   2   4   4   2   2   3   3   6
   304.3s    15   2   3   4   2   2   2   4   3   2
   305.3s     4   2   4   2   3   2   1   3   0   4
   306.3s     2   2   2   2   3   1   4   4   1   2
   307.3s     2   4   4   3   2   2   2   2   1   1
   308.3s     2   2   2   3   3   1   2   2   1   0
   309.3s     2   4   2   3   2   2   0   1   1   0
   310.3s     2   2   2   3   2   2   3   4   3   2
   311.3s     2   2   3   2   1   0   2   3   2   1
   312.3s     2   0   0   1   2   1   0   1   1   3
   313.3s     1   1   3   3   1   2   1   1   3   3
   314.3s     1   2   0   1   1   1   1   1   0   2
   315.3s     1   2   1   0   4   0   0   1   1   2
   316.3s     1   1   2   1   1   1   0   1   0   2
   317.3s     1   0   0   0   1   3   1   0   1   1
   318.3s     1   1   2   1   1   0   2   2   1   0
   319.3s     0   0   2   0   2   0   0   0   1   0
   320.3s     0   2   2   1   1   0   1   1   1   1
   321.3s     1   1   1   0   0   3   0   0   1   1
   322.3s     2   1   2   3   1   1   0   0   1   0
   323.3s     1   0   1   3   0   1   0   2   1   0
   324.3s     1   0   2   1   2   1   0   1   0   0
   325.3s     1   1   1   0   1   0   1   0   0   0
   326.3s     0   2   1   0   0   1   3   2   0   1
   327.3s     0   0   1   1   0   0   1   1   2   0
   328.3s     2   0   0   1   1   0   0   1   0   1
   329.3s     0   0   1   2   0   0   0   1   0   0
   330.3s     1   0   2   1   1   0   1   1   0   0
   331.3s     2   0   6   4   4   1   2   2   2   1
   332.3s     1   1   1   0   0   1   2   0   4   7
   333.3s     4   2   6  10   6   5   7   8   5   8
   334.3s     2   5   6   7   1   4   7   6   4   9
   335.3s     8   8   6   7   2   7   4   9   1   6
   336.3s     8   7   1   6   5   9   3   3   4  10
   337.3s     7   7   5   6   3   7   8   9   6   8
   338.3s     8   8   5   4  15   7   2   7   7  11
   339.3s    13  13  13  12   7  11   4  10   8  10
   340.3s     5   2   1   7   9  10   8   6   2   2
   341.3s     5   9   7   5   4
```

### Post-query bursts

Window per sample: keystroke to max(rendered, opened) + 50 ms. Main thread busy 8653 ms over 15 windows.

#### Named areas in the bursts

| area | ms | % |
|---|---:|---:|
| AppKitPrototypeList | 1186 | 13.7 |
| MessageRowCellView | 533 | 6.2 |
| Row presentation / formatters | 24 | 0.3 |
| ListWindowModel | 621 | 7.2 |
| FrameBatcher | 138 | 1.6 |
| MessageListViewController | 1072 | 12.4 |
| SearchFieldController | 747 | 8.6 |
| SearchFieldModel | 139 | 1.6 |
| Benchmark harness (runner, tick sampler) | 843 | 9.7 |
| -[NSTableView reloadData] | 347 | 4.0 |
| NSTableView (any) | 1504 | 17.4 |
| NSView layout (layoutSubtreeIfNeeded) | 1776 | 20.5 |
| NSView display (displayIfNeeded / drawRect) | 269 | 3.1 |
| NSTextView insertText | 37 | 0.4 |
| NSTextCheckingController | 137 | 1.6 |
| Core Animation commit (CA::Transaction::commit) | 4011 | 46.4 |
| NSDisplayCycleFlush | 2817 | 32.6 |
| Swift concurrency job (completeTaskWithClosure) | 2357 | 27.2 |
| Rust core (sb_*) | 1 | 0.0 |

#### Heaviest main-thread call paths in the bursts

Distinct paths of the top-down call tree, each ending where no callee keeps 1% of burst time; the weight is the path's inclusive time. Leaf first, at most 30 frames (the run-loop frames nearest the root are cut when longer).

##### 1. 399 ms (4.6% of burst busy)

```
specialized AppKitPrototypeList.tableView(_:viewFor:row:) [atos 0x104383dcf]
@objc AppKitPrototypeList.tableView(_:viewFor:row:) [atos 0x104380c17]
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

##### 2. 263 ms (3.0% of burst busy)

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
main [atos 0x10437975b]
start
```

##### 3. 170 ms (2.0% of burst busy)

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

##### 4. 155 ms (1.8% of burst busy)

```
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
closure #2 in AppKitPrototypeList.init(model:formatter:accountMarker:selectionLoss:onIndexChange:) [atos 0x10437faab]
ListWindowModel.open()
MessageListViewController.install(_:ticket:)
closure #1 in MessageListViewController.show(_:) [atos 0x104412801]
<deduplicated_symbol> [atos 0x10437f389]
<deduplicated_symbol> [atos 0x104428fb9]
<deduplicated_symbol> [atos 0x10437f389]
completeTaskWithClosure(swift::AsyncContext*, swift::SwiftError*)
```

##### 5. 133 ms (1.5% of burst busy)

```
0x19461e1bc
0x19461e13c
specialized static RunContext.displayID(_:) [atos 0x1043f5b93]
RunContext.checkDisplay() [atos 0x1043dab27]
RunContext.tickDidFire(_:) [atos 0x1043dae1b]
closure #8 in ListBenchmarkRunner.runStore(controller:)
partial apply for closure #2 in ListBenchmarkRunner.runOnce(implementation:pass:generation:inheritedFailures:)
BenchmarkTickSampler.tickFired(_:)
@objc BenchmarkTickSampler.tickFired(_:) [atos 0x1043aeff7]
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
main [atos 0x10437975b]
start
```

##### 6. 129 ms (1.5% of burst busy)

```
-[NSTextView(NSSharing) didChangeText]
-[NSTextView _insertText:replacementRange:]
closure #6 in ListBenchmarkRunner.runSearch(controller:)
closure #1 in AppDelegate.startBenchmark(mode:reportPath:)
<deduplicated_symbol> [atos 0x10437ee55]
<deduplicated_symbol> [atos 0x104428fb9]
<deduplicated_symbol> [atos 0x10437f389]
completeTaskWithClosure(swift::AsyncContext*, swift::SwiftError*)
```

##### 7. 121 ms (1.4% of burst busy)

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

##### 8. 120 ms (1.4% of burst busy)

```
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
closure #2 in AppKitPrototypeList.init(model:formatter:accountMarker:selectionLoss:onIndexChange:) [atos 0x10437faab]
ListWindowModel.open()
MessageListViewController.install(_:ticket:)
closure #1 in MessageListViewController.show(_:) [atos 0x104412801]
<deduplicated_symbol> [atos 0x10437f389]
<deduplicated_symbol> [atos 0x104428fb9]
<deduplicated_symbol> [atos 0x10437f389]
completeTaskWithClosure(swift::AsyncContext*, swift::SwiftError*)
```

##### 9. 116 ms (1.3% of burst busy)

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
main [atos 0x10437975b]
start
```

##### 10. 115 ms (1.3% of burst busy)

```
SearchFieldController.refreshScopeMenu()
SearchFieldController.applyModel()
closure #1 in closure #2 in implicit closure #1 in SearchFieldController.observeModel()
<deduplicated_symbol> [atos 0x10437ee55]
<deduplicated_symbol> [atos 0x104428fb9]
<deduplicated_symbol> [atos 0x10437f389]
completeTaskWithClosure(swift::AsyncContext*, swift::SwiftError*)
```

##### 11. 113 ms (1.3% of burst busy)

```
-[NSView _setWindow:]
-[NSView removeFromSuperview]
MessageListViewController.detach()
MessageListViewController.install(_:ticket:)
closure #1 in MessageListViewController.show(_:) [atos 0x104412801]
<deduplicated_symbol> [atos 0x10437f389]
<deduplicated_symbol> [atos 0x104428fb9]
<deduplicated_symbol> [atos 0x10437f389]
completeTaskWithClosure(swift::AsyncContext*, swift::SwiftError*)
```

##### 12. 112 ms (1.3% of burst busy)

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
main [atos 0x10437975b]
start
```

### CPU time by thread over the search phase (top 12)

| ms | thread |
|---:|---|
| 23000 | Main Thread (0x419e93c) (Sweetbriar, pid: 96215) |
| 5087 | tokio-rt-worker (0x419f0e3) (Sweetbriar, pid: 96215) |
| 2974 | tokio-rt-worker (0x419fe3e) (Sweetbriar, pid: 96215) |
| 2022 | tokio-rt-worker (0x419fe63) (Sweetbriar, pid: 96215) |
| 1435 | tokio-rt-worker (0x419f581) (Sweetbriar, pid: 96215) |
| 1294 | tokio-rt-worker (0x419f56e) (Sweetbriar, pid: 96215) |
| 1159 | tokio-rt-worker (0x419f79c) (Sweetbriar, pid: 96215) |
| 1148 | tokio-rt-worker (0x419f6a3) (Sweetbriar, pid: 96215) |
| 947 | tokio-rt-worker (0x419ef4e) (Sweetbriar, pid: 96215) |
| 933 | tokio-rt-worker (0x41a080e) (Sweetbriar, pid: 96215) |
| 825 | Sweetbriar (0x419f08d) (Sweetbriar, pid: 96215) |
| 753 | Sweetbriar (0x419f25c) (Sweetbriar, pid: 96215) |

### Top 40 Sweetbriar frames on the tokio workers (inclusive, search phase)

For context: the core's work off the main thread (18801 ms on all workers).

| ms | % of worker time | frame |
|---:|---:|---|
| 18757 | 99.8 | `<std::sys::thread::unix::Thread>::new::thread_start [atos 0x1060e6447]` |
| 18756 | 99.8 | `<std::thread::lifecycle::spawn_unchecked<<tokio::runtime::blocking::pool::Spawner>::spawn_thread::{closure#0}, ()>::{closure#1} as core::ops::function::FnOnc...` |
| 18730 | 99.6 | `std::sys::backtrace::__rust_begin_short_backtrace::<<tokio::runtime::blocking::pool::Spawner>::spawn_thread::{closure#0}, ()> [atos 0x1060a2947]` |
| 17609 | 93.7 | `sqlite3_step` |
| 12582 | 66.9 | `getPageNormal` |
| 12503 | 66.5 | `getAndInitPage [atos 0x105f5d6e7]` |
| 10876 | 57.8 | `<tokio::runtime::task::harness::Harness<tokio::runtime::blocking::task::BlockingTask<<sb_engine::engine::Engine>::read_through<sb_store::readers::Scoped<(sb_...` |
| 10863 | 57.8 | `<tokio::runtime::task::core::Core<tokio::runtime::blocking::task::BlockingTask<<sb_engine::engine::Engine>::read_through<sb_store::readers::Scoped<(sb_store:...` |
| 10863 | 57.8 | `<tokio::runtime::blocking::task::BlockingTask<<sb_engine::engine::Engine>::read_through<sb_store::readers::Scoped<(sb_store::query::counts::coverage::StoredS...` |
| 10838 | 57.6 | `<sb_store::readers::Readers>::read::<(sb_store::query::counts::coverage::StoredSearchCoverage, sb_store::lists::PageRead<sb_store::query::counts::paging::Sea...` |
| 10838 | 57.6 | `<sb_engine::engine::Engine>::search_snapshot::<sb_store::lists::PageRead<sb_store::query::counts::paging::SearchPage>, <sb_engine::engine::Engine>::search_pa...` |
| 10542 | 56.1 | `sb_store::query::counts::paging::read_search_relevance_page` |
| 10273 | 54.6 | `pcache1Fetch` |
| 10064 | 53.5 | `moveToChild` |
| 9205 | 49.0 | `<rusqlite::row::Rows as fallible_streaming_iterator::FallibleStreamingIterator>::advance [atos 0x105e5282f]` |
| 8273 | 44.0 | `sqlite3BtreeTableMoveto` |
| 6161 | 32.8 | `sqlite3VdbeExec [atos 0x105f47fcb]` |
| 5173 | 27.5 | `<tokio::runtime::task::harness::Harness<tokio::runtime::blocking::task::BlockingTask<<sb_engine::engine::Engine>::read_through<sb_store::readers::Scoped<(sb_...` |
| 5163 | 27.5 | `<tokio::runtime::blocking::task::BlockingTask<<sb_engine::engine::Engine>::read_through<sb_store::readers::Scoped<(sb_store::query::counts::coverage::StoredS...` |
| 5163 | 27.5 | `<tokio::runtime::task::core::Core<tokio::runtime::blocking::task::BlockingTask<<sb_engine::engine::Engine>::read_through<sb_store::readers::Scoped<(sb_store:...` |
| 5163 | 27.5 | `<sb_store::readers::Readers>::read_scoped::<(sb_store::query::counts::coverage::StoredSearchCoverage, sb_store::lists::PageRead<(u64, alloc::vec::Vec<i64>)>)...` |
| 5154 | 27.4 | `<sb_engine::engine::Engine>::search_snapshot::<sb_store::lists::PageRead<(u64, alloc::vec::Vec<i64>)>, <sb_engine::engine::Engine>::search_window_ids::{closu...` |
| 4860 | 25.8 | `<sb_store::query::counts::Compiled>::read::<alloc::vec::Vec<i64>, sb_store::query::counts::paging::search_ids::{closure#1}>` |
| 4860 | 25.8 | `sb_store::query::counts::paging::search_ids [atos 0x105e2eb4f]` |
| 4839 | 25.7 | `<core::iter::adapters::GenericShunt<rusqlite::row::MappedRows<sb_store::query::counts::paging::search_ids::{closure#1}::{closure#0}>, core::result::Result<co...` |
| 4831 | 25.7 | `<rusqlite::row::Rows>::get_expected_row [atos 0x105de0d43]` |
| 4661 | 24.8 | `<alloc::vec::Vec<i64> as alloc::vec::spec_from_iter::SpecFromIter<i64, core::iter::adapters::GenericShunt<rusqlite::row::MappedRows<sb_store::query::counts::...` |
| 3610 | 19.2 | `<sb_store::query::counts::Compiled>::read::<sb_store::query::counts::paging::Streamed, sb_store::query::counts::paging::stream::{closure#1}>` |
| 3610 | 19.2 | `sb_store::query::counts::paging::fetch [atos 0x105e2f6ab]` |
| 3485 | 18.5 | `sb_store::query::counts::paging::stream::{closure#1} [atos 0x105dcdb53]` |
| 3372 | 17.9 | `sqlite3BtreeIndexMoveto` |
| 3224 | 17.1 | `sqlite3VdbeExec [atos 0x105f46bbf]` |
| 2472 | 13.1 | `sqlite3VdbeExec [atos 0x105f47297]` |
| 2461 | 13.1 | `sb_store::query::counts::paging::read_page [atos 0x105e316ff]` |
| 2450 | 13.0 | `sb_store::query::counts::paging::read_page [atos 0x105e317cb]` |
| 2439 | 13.0 | `<sb_store::query::counts::Compiled>::read::<i64, sb_store::query::counts::paging::read_page::{closure#7}> [atos 0x105dc993f]` |
| 2436 | 13.0 | `sqlite3BtreeNext` |
| 2345 | 12.5 | `<tokio::runtime::task::harness::Harness<tokio::runtime::blocking::task::BlockingTask<<sb_engine::engine::Engine>::read_through<sb_store::readers::Scoped<sb_s...` |
| 2343 | 12.5 | `<tokio::runtime::task::core::Core<tokio::runtime::blocking::task::BlockingTask<<sb_engine::engine::Engine>::read_through<sb_store::readers::Scoped<sb_store::...` |
| 2343 | 12.5 | `<tokio::runtime::blocking::task::BlockingTask<<sb_engine::engine::Engine>::read_through<sb_store::readers::Scoped<sb_store::lists::PageRead<(u64, alloc::vec:...` |

## stall_window.py

```python
"""What the app did after the last completed sample of a failed search
benchmark run: per-thread CPU and the top frames per thread kind.

Usage: stall_window.py <time-profile.xml> <report.json> <demangler> <dsyms>
"""
import collections, importlib.util, json, os, sys

spec = importlib.util.spec_from_file_location(
    "ap", os.path.join(os.path.dirname(os.path.abspath(__file__)), "analyze_profile.py"))
ap = importlib.util.module_from_spec(spec); spec.loader.exec_module(ap)

xml, report, demangler, dsyms = sys.argv[1:5]
rows, frame_binary, binaries = ap.load(xml)
samples = json.load(open(report))["search"]["samples"]
main = sorted((r for r in rows if r.get("thread", "").startswith("Main Thread")), key=lambda r: r["t"])
finals = ap.clusters([r["t"] for r in main if ap.is_final(r["stack"])], ap.KEYSTROKE_GAP_MS * ap.NS_PER_MS)
done = [s for s in samples if s.get("milliseconds") is not None]
last_t0 = finals[len(done) - 1][0]
last = done[-1]
start = last_t0 + int(max(last["milliseconds"], last.get("openedMilliseconds") or 0) * ap.NS_PER_MS)
end = max(r["t"] for r in main if ap.RUN_SEARCH in r["stack"])
win = [r for r in rows if start <= r["t"] <= end]
print(f"Stall window: {ap.ms(start)/1000:.3f} s to {ap.ms(end)/1000:.3f} s "
      f"({ap.ms(end - start)/1000:.1f} s), after sample {last['query']} round {last['round']}.\n")

names = {f for r in win for f in r["stack"]}
raw = {n: n for n in names}; res = ap.atos(names, frame_binary, binaries, dsyms); raw.update(res)
pretty = ap.demangle(set(raw.values()), demangler)
nice = {n: pretty[raw[n]] + (f" [atos {n}]" if n in res else "") for n in names}
own = lambda f: frame_binary.get(f) in ap.OWN_BINARIES

thr = collections.Counter(); kinds = collections.defaultdict(collections.Counter); tot = collections.Counter()
buckets = collections.defaultdict(collections.Counter)
for r in win:
    t = r.get("thread", "?"); kind = "main" if t.startswith("Main Thread") else ("worker" if "tokio-rt-worker" in t else "other")
    thr[t] += r["w"]; tot[kind] += r["w"]
    buckets[(r["t"] - start) // (5000 * ap.NS_PER_MS)][kind] += r["w"]
    for f in {nice[f] for f in r["stack"] if own(f) or kind == "worker"}:
        kinds[kind][f] += r["w"]

print("CPU by thread kind in the window (ms): " + ", ".join(f"{k} {ap.ms(v):.0f}" for k, v in tot.items()) + "\n")
print("CPU per 5 s of the window (ms on main / workers / other):\n\n```")
for b in sorted(buckets):
    c = buckets[b]
    print(f"{b*5:4d}-{b*5+5:<4d}s  main {ap.ms(c['main']):6.0f}  workers {ap.ms(c['worker']):6.0f}  other {ap.ms(c['other']):6.0f}")
print("```\n")
print("| ms | thread |\n|---:|---|")
for t, w in thr.most_common(10):
    print(f"| {ap.ms(w):.0f} | {t} |")
for kind in ("worker", "main"):
    print(f"\n#### Top 25 frames on {kind} threads in the window (inclusive{', Sweetbriar only' if kind == 'main' else ''})\n")
    print("| ms | % of that kind's CPU | frame |\n|---:|---:|---|")
    for f, w in kinds[kind].most_common(25):
        print(f"| {ap.ms(w):.0f} | {100*w/max(tot[kind],1):.1f} | `{ap.cell(f)}` |")
```
