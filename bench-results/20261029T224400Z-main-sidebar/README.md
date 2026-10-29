Request: bench-requests/20261029T224400Z-main-sidebar.md @ 5f28c50ac2772426f0aaccad3a271919ad47acae

# Search benchmark: 5f28c50, main-sidebar

- **Built commit:** `5f28c50ac2772426f0aaccad3a271919ad47acae`, "Merge
  pull request #163 from L-K-M/sidebar-search-generation" (on origin/main
  and origin/m1.6r4c2/reader-cache), Release, built with
  `scripts/build.sh app`.
- **Request:** LABEL main-sidebar, RUNS 2, PROFILE no, SEARCH_TIMINGS no.
- **Date:** 2026-10-29. Invalid attempt finished 22:55 UTC; run 1
  22:55 to 22:57 UTC; run 2 22:57 to 22:59 UTC.
- **Mac:** Mac mini M1 (Macmini9,1, 8 cores, 8 GiB), macOS 26.5.2
  (25F84), Xcode build 17F113, on AC power.
- **Display:** one external display, 1920 x 1080 at 60 Hz (the reports
  give `refreshRateHertz` 60).
- **Archive:** `~/sweetbriar-search-archive`, 100,000 synthetic
  messages, seeded 2026-10-29 and unchanged since.

## Timed runs

| run | ok | samples | p95 ms | p50 ms | per-query p95 ms |
|---|---|---|---:|---:|---|
| 1 | true | 50 of 50 | 1,199.8 | 774.1 | exact-address 223, scoped-filter 243, exact-reference 1,028, negative-body 1,224, phrase 1,244 |
| 2 | true | 50 of 50 | 1,271.0 | 644.2 | exact-address 239, scoped-filter 241, phrase 1,224, exact-reference 1,321, negative-body 1,810 |

Both runs miss spec/08's budget (`met: false`, p95 under 100 ms).
`storeMessages` is 100000 in both.

Stages (p50 / p95 ms, keystroke to stage):

| run | presented | firstPage | landed | opened |
|---|---:|---:|---:|---:|
| 1 | 36.4 / 56.3 | 763.4 / 1,192.3 | 764.3 / 1,198.7 | 1,412.1 / 2,232.7 |
| 2 | 35.6 / 52.0 | 635.5 / 1,229.2 | 636.0 / 1,229.8 | 1,166.6 / 2,149.9 |

The core's opening page still accounts for almost all of the latency:
rows land within about 1 ms (p50) to 6 ms (p95) of `firstPage`. For comparison, the valid
30888e0 run on this Mac
(`bench-results/20261029T212655Z-30888e0-main-profile-run2.json`)
had p95 1,557.8 ms and p50 1,154.6 ms. Here scoped-filter's p95 is
less than half of that run's (241 to 243 ms against 571), and
exact-address's is lower too (223 to 239 against 294). Two runs each, under the
memory pressure below, are too few to call that a measured improvement.

## Machine state

| | before (22:52:34Z) | after (22:59:39Z) |
|---|---:|---:|
| `hw.memsize` | 8589934592 | 8589934592 |
| memory free (`memory_pressure`) | 8% | 8% |
| pages wired down (16 KiB) | 386,550 (5.9 GiB) | 382,895 (5.8 GiB) |
| file-backed pages | 15,682 (245 MiB) | 16,356 (256 MiB) |
| anonymous pages | 22,892 | 24,099 |
| pages occupied by compressor | 61,755 | 63,873 |

`vm.swapusage` after the runs: 3,824.56 MiB used of 5,120 MiB.

About 5.9 GiB of the 8 GiB is wired, and the OS file cache holds only
about 250 MiB, so the archive's 791 MiB store can't stay cached.
SQLite's page reads (55% of core worker time in `pread` in the
30888e0 profile) mostly go to the SSD. This likely explains much of the
gap to the M3 Pro results in `plan/qualification/M1.6q2.md`. What holds
the wired memory wasn't investigated.

Full outputs:

```text
== before runs: 2026-10-29T22:52:34Z
hw.memsize: 8589934592
System-wide memory free percentage: 8%
Mach Virtual Memory Statistics: (page size of 16384 bytes)
Pages free:                                     3969.
Pages active:                                  19863.
Pages inactive:                                17365.
Pages speculative:                              1346.
Pages throttled:                                   0.
Pages wired down:                             386550.
Pages purgeable:                                   0.
"Translation faults":                    15554588791.
Pages copy-on-write:                       365112286.
Pages zero filled:                        2596533606.
Pages reactivated:                        5164335919.
Pages purged:                               75219803.
File-backed pages:                             15682.
Anonymous pages:                               22892.
Pages stored in compressor:                   573123.
Pages occupied by compressor:                  61755.
Decompressions:                           7008614270.
Compressions:                             7184121782.
Pageins:                                   305916644.
Pageouts:                                    2340224.
Swapins:                                   179563571.
Swapouts:                                  183728246.
```

```text
== after runs: 2026-10-29T22:59:39Z
hw.memsize: 8589934592
System-wide memory free percentage: 8%
Mach Virtual Memory Statistics: (page size of 16384 bytes)
Pages free:                                     3720.
Pages active:                                  20600.
Pages inactive:                                18105.
Pages speculative:                              1750.
Pages throttled:                                   0.
Pages wired down:                             382895.
Pages purgeable:                                   4.
"Translation faults":                    15575904881.
Pages copy-on-write:                       365226985.
Pages zero filled:                        2597365933.
Pages reactivated:                        5175349962.
Pages purged:                               75260945.
File-backed pages:                             16356.
Anonymous pages:                               24099.
Pages stored in compressor:                   536078.
Pages occupied by compressor:                  63873.
Decompressions:                           7022165147.
Compressions:                             7197787684.
Pageins:                                   308040940.
Pageouts:                                    2343654.
Swapins:                                   180739586.
Swapouts:                                  184898498.
```

## Anything unusual

- **One retry:** the first attempt (`invalid1.json`) failed with "the
  run lost the foreground". At launch an app named "RunningMan" was
  frontmost. It was gone from the process list right after, and I
  don't know what it is. The retry brought Sweetbriar to the front
  within 0.1 s of its process appearing and passed, and so did run 2.
- **Do Not Disturb:** not verifiable. This job runs unattended, and the
  sandbox can't read the Focus state.
- **Build:** 68 s (incremental over the 30888e0 build).
  `scripts/build.sh` left its one-line
  `apps/macos/Sweetbriar/Info.plist` edit in the build checkout; the
  next request restores it.
- **Kept on this Mac:** build log and machine-state files in
  `~/sb-bench-archive/20261029T224400Z-main-sidebar/`.
