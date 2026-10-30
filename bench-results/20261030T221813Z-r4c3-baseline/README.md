Request: bench-requests/20261030T221813Z-r4c3-baseline.md @ f67fa83f33437f844cc3cfabe707aff20f65abc4

# Search benchmark: f67fa83, r4c3-baseline

- **Built commit:** `f67fa83f33437f844cc3cfabe707aff20f65abc4`, "Merge
  pull request #165 from L-K-M/m1.6r4c2/reader-cache" (origin/main),
  Release, built with `scripts/build.sh app`. It's the parent of
  `8086cb5`, request `20261030T221813Z-r4c3-certain-unknowns`, so
  this request is that one's baseline.
- **Request:** LABEL r4c3-baseline, RUNS 2, PROFILE no, SEARCH_TIMINGS no.
- **Date:** 2026-10-30. Run 1 22:22 to 22:24 UTC; run 2 22:24 to 22:26
  UTC.
- **Mac:** Mac mini M1 (Macmini9,1, 8 cores, 8 GiB), macOS 26.5.2
  (25F84), Xcode build 17F113, on AC power.
- **Display:** one external display, 1920 x 1080 at 60 Hz (the reports
  give `refreshRateHertz` 60).
- **Archive:** `~/sweetbriar-search-archive`, 100,000 synthetic
  messages, seeded 2026-10-29 and unchanged since.

## Timed runs

| run | ok | samples | p95 ms | p50 ms | per-query p95 ms |
|---|---|---|---:|---:|---|
| 1 | true | 50 of 50 | 1,156.6 | 247.2 | exact-address 198, scoped-filter 274, phrase 1,108, exact-reference 1,157, negative-body 1,338 |
| 2 | true | 50 of 50 | 1,046.9 | 202.2 | exact-address 212, scoped-filter 399, exact-reference 995, phrase 1,099, negative-body 1,518 |

Both runs miss spec/08's budget (`met: false`, p95 under 100 ms).
`storeMessages` is 100000 in both. No retries were needed.

Stages (p50 / p95 ms, keystroke to stage):

| run | presented | firstPage | landed | opened |
|---|---:|---:|---:|---:|
| 1 | 39.0 / 57.8 | 232.2 / 1,151.0 | 232.9 / 1,154.2 | 984.0 / 1,903.4 |
| 2 | 37.8 / 60.2 | 189.9 / 996.4 | 190.1 / 1,029.0 | 926.1 / 2,059.9 |

The core's opening page still accounts for almost all of the latency.
The p50 is close to the 2639cb7 (r4c2-reader-cache) runs from
2026-10-29, at 211 and 242 ms; this merge carries that reader cache.
The p95 is higher here (1,047 to 1,157 ms against 817 to 987),
driven by exact-reference, phrase and negative-body.

## Machine state

| | before (22:22:28Z) | after (22:26:16Z) |
|---|---:|---:|
| `hw.memsize` | 8589934592 | 8589934592 |
| memory free (`memory_pressure`) | 8% | 8% |
| pages wired down (16 KiB) | 386,577 (5.9 GiB) | 385,941 (5.9 GiB) |
| file-backed pages | 15,974 (250 MiB) | 14,064 (220 MiB) |
| anonymous pages | 22,509 | 26,533 |
| pages occupied by compressor | 62,190 | 60,319 |
| swap used (`vm.swapusage`) | 4,279.94 MiB of 5,120 | 4,271.94 MiB of 5,120 |

The same memory pressure as on 2026-10-29: about 5.9 GiB of 8 GiB
wired and a file cache of about 250 MiB, so the 791 MiB store can't
stay cached. Swap use is about 400 MiB higher than yesterday.

Full outputs:

```text
== before runs: 2026-10-30T22:22:28Z
hw.memsize: 8589934592
System-wide memory free percentage: 8%
Mach Virtual Memory Statistics: (page size of 16384 bytes)
Pages free:                                     3615.
Pages active:                                  19443.
Pages inactive:                                17484.
Pages speculative:                              1556.
Pages throttled:                                   0.
Pages wired down:                             386577.
Pages purgeable:                                   2.
"Translation faults":                    17613230880.
Pages copy-on-write:                       378181203.
Pages zero filled:                        2738440779.
Pages reactivated:                        5905882680.
Pages purged:                               79655987.
File-backed pages:                             15974.
Anonymous pages:                               22509.
Pages stored in compressor:                   591579.
Pages occupied by compressor:                  62190.
Decompressions:                           8485239537.
Compressions:                             8674621235.
Pageins:                                   362695338.
Pageouts:                                    2596378.
Swapins:                                   275024906.
Swapouts:                                  280261459.
vm.swapusage: total = 5120.00M  used = 4279.94M  free = 840.06M  (encrypted)
```

```text
== after runs: 2026-10-30T22:26:16Z
hw.memsize: 8589934592
System-wide memory free percentage: 8%
Mach Virtual Memory Statistics: (page size of 16384 bytes)
Pages free:                                     3965.
Pages active:                                  20678.
Pages inactive:                                19672.
Pages speculative:                               247.
Pages throttled:                                   0.
Pages wired down:                             385941.
Pages purgeable:                                   2.
"Translation faults":                    17632896667.
Pages copy-on-write:                       378212380.
Pages zero filled:                        2738908513.
Pages reactivated:                        5912622301.
Pages purged:                               79769957.
File-backed pages:                             14064.
Anonymous pages:                               26533.
Pages stored in compressor:                   590907.
Pages occupied by compressor:                  60319.
Decompressions:                           8497899740.
Compressions:                             8687380269.
Pageins:                                   364069447.
Pageouts:                                    2599137.
Swapins:                                   275891653.
Swapouts:                                  281128391.
vm.swapusage: total = 5120.00M  used = 4271.94M  free = 848.06M  (encrypted)
```

## Anything unusual

- **Foreground:** Sweetbriar was brought to the front within 0.1 s of
  its process appearing in both runs, and neither lost the foreground.
- **Do Not Disturb:** not verifiable. This job runs unattended, and the
  sandbox can't read the Focus state.
- **Build:** 86 s (incremental). `scripts/build.sh` left its one-line
  `apps/macos/Sweetbriar/Info.plist` edit in the build checkout; the
  next request restores it.
- **Kept on this Mac:** build log and machine-state files in
  `~/sb-bench-archive/20261030T221813Z-r4c3-baseline/`.
