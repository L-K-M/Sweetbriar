Request: bench-requests/20261029T224400Z-r4c2-reader-cache.md @ 2639cb71054531011fcb8b8e7d38041f31b0ab51

# Search benchmark: 2639cb7, r4c2-reader-cache

- **Built commit:** `2639cb71054531011fcb8b8e7d38041f31b0ab51`, "Give
  each reader a 64 MiB page cache" (origin/m1.6r4c2/reader-cache),
  Release, built with `scripts/build.sh app`. It is `5f28c50`
  (origin/main, request `20261029T224400Z-main-sidebar`) plus this one
  commit, so that request is a direct baseline.
- **Request:** LABEL r4c2-reader-cache, RUNS 2, PROFILE no,
  SEARCH_TIMINGS no.
- **Date:** 2026-10-29. Run 1 23:02 to 23:04 UTC; run 2 23:04 to 23:06
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
| 1 | true | 50 of 50 | 817.3 | 211.4 | exact-address 196, scoped-filter 243, phrase 814, negative-body 832, exact-reference 856 |
| 2 | true | 50 of 50 | 986.6 | 242.4 | exact-address 219, scoped-filter 354, exact-reference 807, negative-body 987, phrase 1,206 |

Both runs miss spec/08's budget (`met: false`, p95 under 100 ms).
`storeMessages` is 100000 in both. No retries were needed.

Stages (p50 / p95 ms, keystroke to stage):

| run | presented | firstPage | landed | opened |
|---|---:|---:|---:|---:|
| 1 | 36.8 / 60.0 | 200.5 / 808.4 | 200.7 / 808.9 | 740.9 / 1,532.8 |
| 2 | 37.0 / 67.6 | 232.3 / 980.1 | 232.9 / 980.4 | 744.3 / 1,750.2 |

### Against the 5f28c50 baseline

| build | run 1 p95 / p50 | run 2 p95 / p50 |
|---|---:|---:|
| 5f28c50 (main-sidebar) | 1,199.8 / 774.1 | 1,271.0 / 644.2 |
| 2639cb7 (this request) | 817.3 / 211.4 | 986.6 / 242.4 |

Both runs were taken minutes apart on the same Mac, under the same
memory state. The reader cache cuts p50 to about a third (211 to 242 ms
against 644 to 774) and p95 by roughly a quarter to a third. The
`firstPage` stage moves the same way (p50 200 to 232 ms against 636 to
763), so the gain is in the core's opening page. The exact-reference,
phrase and negative-body queries still reach 800 to 1,200 ms at p95.
That's two runs per build, so treat it as a strong signal rather than
a settled figure.

## Machine state

| | before (23:02:24Z) | after (23:05:50Z) |
|---|---:|---:|
| `hw.memsize` | 8589934592 | 8589934592 |
| memory free (`memory_pressure`) | 8% | 9% |
| pages wired down (16 KiB) | 384,553 (5.9 GiB) | 383,963 (5.9 GiB) |
| file-backed pages | 16,374 (256 MiB) | 16,278 (254 MiB) |
| anonymous pages | 23,148 | 27,010 |
| pages occupied by compressor | 63,081 | 59,727 |
| swap used (`vm.swapusage`) | 3,892.25 MiB of 5,120 | 3,950.38 MiB of 5,120 |

As in the main-sidebar request, about 5.9 GiB of the 8 GiB is wired and
the OS file cache holds only about 250 MiB, so the 791 MiB store can't
stay cached. A per-reader page cache keeps pages in the process
instead, which fits the gain above. What holds the wired memory wasn't
investigated.

Full outputs:

```text
== before runs: 2026-10-29T23:02:24Z
hw.memsize: 8589934592
System-wide memory free percentage: 8%
Mach Virtual Memory Statistics: (page size of 16384 bytes)
Pages free:                                     3731.
Pages active:                                  20224.
Pages inactive:                                17546.
Pages speculative:                              1752.
Pages throttled:                                   0.
Pages wired down:                             384553.
Pages purgeable:                                   4.
"Translation faults":                    15584634459.
Pages copy-on-write:                       365293114.
Pages zero filled:                        2598388195.
Pages reactivated:                        5179087413.
Pages purged:                               75269323.
File-backed pages:                             16374.
Anonymous pages:                               23148.
Pages stored in compressor:                   547014.
Pages occupied by compressor:                  63081.
Decompressions:                           7027243997.
Compressions:                             7203052710.
Pageins:                                   308778037.
Pageouts:                                    2349800.
Swapins:                                   181088642.
Swapouts:                                  185255370.
vm.swapusage: total = 5120.00M  used = 3892.25M  free = 1227.75M  (encrypted)
```

```text
== after runs: 2026-10-29T23:05:50Z
hw.memsize: 8589934592
System-wide memory free percentage: 9%
Mach Virtual Memory Statistics: (page size of 16384 bytes)
Pages free:                                     3985.
Pages active:                                  22126.
Pages inactive:                                19139.
Pages speculative:                              2023.
Pages throttled:                                   0.
Pages wired down:                             383963.
Pages purgeable:                                   2.
"Translation faults":                    15597708683.
Pages copy-on-write:                       365336741.
Pages zero filled:                        2598804109.
Pages reactivated:                        5184667516.
Pages purged:                               75287210.
File-backed pages:                             16278.
Anonymous pages:                               27010.
Pages stored in compressor:                   543009.
Pages occupied by compressor:                  59727.
Decompressions:                           7036049308.
Compressions:                             7211902383.
Pageins:                                   309558628.
Pageouts:                                    2352263.
Swapins:                                   181811446.
Swapouts:                                  185982602.
vm.swapusage: total = 5120.00M  used = 3950.38M  free = 1169.62M  (encrypted)
```

## Anything unusual

- **Foreground:** Sweetbriar was brought to the front within 0.1 s of
  its process appearing in both runs, and neither lost the foreground.
- **Do Not Disturb:** not verifiable. This job runs unattended, and the
  sandbox can't read the Focus state.
- **Build:** 75 s (incremental). `scripts/build.sh` left its one-line
  `apps/macos/Sweetbriar/Info.plist` edit in the build checkout; the
  next request restores it.
- **Kept on this Mac:** build log and machine-state files in
  `~/sb-bench-archive/20261029T224400Z-r4c2-reader-cache/`.
