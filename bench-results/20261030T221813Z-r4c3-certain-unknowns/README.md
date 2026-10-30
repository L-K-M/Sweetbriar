Request: bench-requests/20261030T221813Z-r4c3-certain-unknowns.md @ 8086cb5e916f345d38fc6c49575467409c0b8bd2

# Search benchmark: 8086cb5, r4c3-certain-unknowns

- **Built commit:** `8086cb5e916f345d38fc6c49575467409c0b8bd2`, "Count
  certain Search unknowns without joins" (origin/m1.6r4c3/certain-unknowns),
  Release, built with `scripts/build.sh app`. It's `f67fa83`
  (origin/main, request `20261030T221813Z-r4c3-baseline`) plus this one
  commit, so that request is a direct baseline.
- **Request:** LABEL r4c3-certain-unknowns, RUNS 2, PROFILE yes,
  SEARCH_TIMINGS no.
- **Date:** 2026-10-30. Run 1 22:28 to 22:30 UTC; run 2 22:30 to 22:32
  UTC; profiled run about 22:32 to 22:42 UTC.
- **Mac:** Mac mini M1 (Macmini9,1, 8 cores, 8 GiB), macOS 26.5.2
  (25F84), Xcode build 17F113, on AC power.
- **Display:** one external display, 1920 x 1080 at 60 Hz (the reports
  give `refreshRateHertz` 60).
- **Archive:** `~/sweetbriar-search-archive`, 100,000 synthetic
  messages, seeded 2026-10-29 and unchanged since.

## Timed runs

| run | ok | samples | p95 ms | p50 ms | per-query p95 ms |
|---|---|---|---:|---:|---|
| 1 | true | 50 of 50 | 525.2 | 201.4 | scoped-filter 322, exact-address 445, exact-reference 449, phrase 575, negative-body 765 |
| 2 | true | 50 of 50 | 758.4 | 167.6 | exact-address 285, exact-reference 566, negative-body 758, scoped-filter 1,242, phrase 1,434 |

Both runs miss spec/08's budget (`met: false`, p95 under 100 ms).
`storeMessages` is 100000 in both. No retries were needed.

Stages (p50 / p95 ms, keystroke to stage):

| run | presented | firstPage | landed | opened |
|---|---:|---:|---:|---:|
| 1 | 36.5 / 52.9 | 195.6 / 515.6 | 196.9 / 516.0 | 440.9 / 1,921.1 |
| 2 | 36.6 / 111.3 | 155.6 / 742.3 | 155.8 / 742.9 | 326.6 / 2,097.3 |

### Against the f67fa83 baseline

| build | run 1 p95 / p50 | run 2 p95 / p50 | opened p50 |
|---|---:|---:|---:|
| f67fa83 (r4c3-baseline) | 1,156.6 / 247.2 | 1,046.9 / 202.2 | 984.0, 926.1 |
| 8086cb5 (this request) | 525.2 / 201.4 | 758.4 / 167.6 | 440.9, 326.6 |

Both builds were run minutes apart under the same memory state.

- **Overall:** p95 drops by 28 to 55% and p50 by about 17%. The list's
  open returns much sooner:  p50 drops from about 950 ms to
  330 to 440 ms.
- **By query:** exact-reference and negative-body improve in both runs
  (p95 449 to 765 ms, against 995 to 1,518).
- **Run 2's tail:** phrase (1,434 ms) and scoped-filter (1,242 ms)
  were slower than at baseline (1,108 and 399 ms), so the tail isn't
  settled.
- **How firm:** two runs per build, so treat the p95 gain as likely
  rather than measured.

## Profiled run (numbers are not results)

`profiled.json`: `ok` true, 50 of 50, p95 7,156 ms, p50 2,138 ms.
The analysis is in `profile.md`. In short:

- The core now reads pages from its own cache: `pcache1Fetch` 51% of
  worker time and `pread` 15%, against 17% and 55% at `30888e0`.
- The sidebar recount no longer appears during the search phase.
- Worker time is in `read_search_relevance_page`, `search_ids`,
  `paging::fetch` and `lists::list_ids`.
- The main thread is on CPU for 7% of the search phase.

## Machine state

| | before (22:28:37Z) | after (22:42:07Z, after the profiled run) |
|---|---:|---:|
| `hw.memsize` | 8589934592 | 8589934592 |
| memory free (`memory_pressure`) | 8% | 8% |
| pages wired down (16 KiB) | 387,667 (5.9 GiB) | 384,251 (5.9 GiB) |
| file-backed pages | 15,044 (235 MiB) | 16,170 (253 MiB) |
| anonymous pages | 24,221 | 24,213 |
| pages occupied by compressor | 60,156 | 62,586 |
| swap used (`vm.swapusage`) | 4,436.62 MiB of 5,120 | 4,480.88 MiB of 6,144 |

The same memory pressure as the baseline request. macOS grew the swap
file from 5,120 to 6,144 MiB during the profiled run.

Full outputs:

```text
== before runs: 2026-10-30T22:28:37Z
hw.memsize: 8589934592
System-wide memory free percentage: 8%
Mach Virtual Memory Statistics: (page size of 16384 bytes)
Pages free:                                     3757.
Pages active:                                  20049.
Pages inactive:                                18639.
Pages speculative:                               577.
Pages throttled:                                   0.
Pages wired down:                             387667.
Pages purgeable:                                   0.
"Translation faults":                    17643907230.
Pages copy-on-write:                       378258083.
Pages zero filled:                        2739687333.
Pages reactivated:                        5916554294.
Pages purged:                               79837844.
File-backed pages:                             15044.
Anonymous pages:                               24221.
Pages stored in compressor:                   605726.
Pages occupied by compressor:                  60156.
Decompressions:                           8504132326.
Compressions:                             8693849873.
Pageins:                                   365301600.
Pageouts:                                    2606235.
Swapins:                                   276259393.
Swapouts:                                  281507099.
vm.swapusage: total = 5120.00M  used = 4436.62M  free = 683.38M  (encrypted)
```

```text
== after runs: 2026-10-30T22:42:07Z
hw.memsize: 8589934592
System-wide memory free percentage: 8%
Mach Virtual Memory Statistics: (page size of 16384 bytes)
Pages free:                                     3530.
Pages active:                                  20789.
Pages inactive:                                17842.
Pages speculative:                              1752.
Pages throttled:                                   0.
Pages wired down:                             384251.
Pages purgeable:                                   4.
"Translation faults":                    17709973139.
Pages copy-on-write:                       378371402.
Pages zero filled:                        2741129794.
Pages reactivated:                        5943005757.
Pages purged:                               80039030.
File-backed pages:                             16170.
Anonymous pages:                               24213.
Pages stored in compressor:                   569404.
Pages occupied by compressor:                  62586.
Decompressions:                           8544757363.
Compressions:                             8735004710.
Pageins:                                   372492601.
Pageouts:                                    2678679.
Swapins:                                   285369262.
Swapouts:                                  290623927.
vm.swapusage: total = 6144.00M  used = 4480.88M  free = 1663.12M  (encrypted)
```

## Anything unusual

- **Foreground:** in the timed runs, Sweetbriar was brought to the
  front within 0.1 s of its process appearing. In the profiled run it
  took about 5 s after `xctrace` started, and the run still passed.
  No run lost the foreground.
- **Profiler:** `xctrace` reported "The log archive is corrupt or
  incomplete" and exited 2, but saved the trace. Its stopped copy of
  the app was killed afterwards.
- **Do Not Disturb:** not verifiable. This job runs unattended, and the
  sandbox can't read the Focus state.
- **Build:** 83 s (incremental). `scripts/build.sh` left its one-line
  `apps/macos/Sweetbriar/Info.plist` edit in the build checkout; the
  next request restores it.
- **Kept on this Mac:** the traces, XML exports, logs and machine-state
  files are in `~/sb-bench-archive/20261030T221813Z-r4c3-certain-unknowns/`.
