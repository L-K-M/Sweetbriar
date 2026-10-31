Request: bench-requests/20261031T033607Z-r4c4-closed-id-reads.md @ 9f21847ee3566910323e3df372d9bb226df27952

# Search benchmark: 9f21847, r4c4-closed-id-reads

- **Built commit:** `9f21847ee3566910323e3df372d9bb226df27952`, "Stop
  closed-window ID scans inside SQLite"
  (origin/m1.6r4c4/closed-id-reads), Release, built with
  `scripts/build.sh app`. It builds on `2f961b3` (origin/main, request
  `20261031T033607Z-r4c4-baseline`). Between the two: `04ee4d3`, a merge
  of main, and this commit.
- **Request:** LABEL r4c4-closed-id-reads, RUNS 2, PROFILE yes,
  SEARCH_TIMINGS no.
- **Date:** 2026-10-31. Run 1 03:48 to 03:50 UTC; run 2 03:50 to 03:51
  UTC; profiled run about 03:51 to 04:02 UTC.
- **Mac:** Mac mini M1 (Macmini9,1, 8 cores, 8 GiB), macOS 26.5.2
  (25F84), Xcode build 17F113, on AC power.
- **Display:** one external display, 1920 x 1080 at 60 Hz (the reports
  give `refreshRateHertz` 60).
- **Archive:** `~/sweetbriar-search-archive`, 100,000 synthetic
  messages, seeded 2026-10-29 and unchanged since.

## Timed runs

| run | ok | samples | p95 ms | p50 ms | per-query p95 ms |
|---|---|---|---:|---:|---|
| 1 | true | 50 of 50 | 552.9 | 174.3 | scoped-filter 256, phrase 291, exact-reference 334, exact-address 348, negative-body 1,007 |
| 2 | true | 50 of 50 | 617.2 | 171.2 | exact-address 329, exact-reference 403, scoped-filter 483, phrase 944, negative-body 1,691 |

Both runs miss spec/08's budget (`met: false`, p95 under 100 ms).
`storeMessages` is 100000 in both. No retries were needed.

Stages (p50 / p95 ms, keystroke to stage):

| run | presented | firstPage | landed | opened |
|---|---:|---:|---:|---:|
| 1 | 42.5 / 64.4 | 167.0 / 505.2 | 167.3 / 535.6 | 341.7 / 2,473.9 |
| 2 | 42.2 / 97.3 | 165.8 / 612.5 | 166.0 / 613.1 | 362.9 / 2,573.0 |

### Against the 2f961b3 baseline

| build | run 1 p95 / p50 | run 2 p95 / p50 | opened p95 |
|---|---:|---:|---:|
| 2f961b3 (r4c4-baseline) | 732.8 / 207.1 | 759.6 / 224.2 | 2,809.7, 2,352.3 |
| 9f21847 (this request) | 552.9 / 174.3 | 617.2 / 171.2 | 2,473.9, 2,573.0 |

Both builds were run minutes apart under the same memory state.

- **Overall:** p95 drops by 19 to 25% and p50 by 16 to 24%.
- **By query:** only exact-reference is faster at p95 in both runs (334
  and 403 against 592 and 590 ms). Scoped-filter, phrase and
  exact-address are each faster in one run and slower in the other.
- **Negative-body:** no faster (1,007 and 1,691 against 742 and 1,542
  ms).
- **The `opened` stage's p95:** about 2.5 s in both builds, unchanged.
- **How firm:** two runs per build, so treat the gain as likely rather
  than measured.

## Profiled run (numbers are not results)

`profiled.json`: `ok` true, 50 of 50, p95 4,548 ms, p50 2,414 ms. The
analysis is in `profile.md`. Against the `8086cb5` profile, which this
build carries:

- The workers used 46.7 s of CPU in the search phase, against 54.3 s.
- `lists::list_ids` (19.6% of worker time there) no longer shows up.
- The search's id reads run through `Readers::read_scoped` and
  `Engine::search_snapshot`.
- The main thread is on CPU for 7.8% of the search phase.

## Machine state

| | before (03:48:27Z) | after (04:02:20Z, after the profiled run) |
|---|---:|---:|
| `hw.memsize` | 8589934592 | 8589934592 |
| memory free (`memory_pressure`) | 7% | 7% |
| pages wired down (16 KiB) | 399,723 (6.1 GiB) | 399,157 (6.1 GiB) |
| file-backed pages | 13,459 (210 MiB) | 14,425 (225 MiB) |
| anonymous pages | 19,111 | 21,056 |
| pages occupied by compressor | 54,646 | 53,529 |
| swap used (`vm.swapusage`) | 4,151.62 MiB of 5,120 | 3,873.94 MiB of 5,120 |

The same memory state as the baseline request: about 6.1 GiB of 8 GiB
wired and a file cache of about 210 to 225 MiB.

Full outputs:

```text
== before runs: 2026-10-31T03:48:27Z
hw.memsize: 8589934592
System-wide memory free percentage: 7%
Mach Virtual Memory Statistics: (page size of 16384 bytes)
Pages free:                                     4007.
Pages active:                                  16706.
Pages inactive:                                15039.
Pages speculative:                               825.
Pages throttled:                                   0.
Pages wired down:                             399723.
Pages purgeable:                                   0.
"Translation faults":                    19054178806.
Pages copy-on-write:                       380669746.
Pages zero filled:                        2786911238.
Pages reactivated:                        6334669456.
Pages purged:                               85861903.
File-backed pages:                             13459.
Anonymous pages:                               19111.
Pages stored in compressor:                   609175.
Pages occupied by compressor:                  54646.
Decompressions:                           9433393635.
Compressions:                             9642159029.
Pageins:                                   442648717.
Pageouts:                                    3220736.
Swapins:                                   436183911.
Swapouts:                                  442951935.
vm.swapusage: total = 5120.00M  used = 4151.62M  free = 968.38M  (encrypted)
```

```text
== after runs: 2026-10-31T04:02:20Z
hw.memsize: 8589934592
System-wide memory free percentage: 7%
Mach Virtual Memory Statistics: (page size of 16384 bytes)
Pages free:                                     2622.
Pages active:                                  18126.
Pages inactive:                                16654.
Pages speculative:                               700.
Pages throttled:                                   0.
Pages wired down:                             399157.
Pages purgeable:                                   0.
"Translation faults":                    19133696018.
Pages copy-on-write:                       380763948.
Pages zero filled:                        2788215064.
Pages reactivated:                        6364057325.
Pages purged:                               85984911.
File-backed pages:                             14425.
Anonymous pages:                               21056.
Pages stored in compressor:                   582940.
Pages occupied by compressor:                  53529.
Decompressions:                           9484481340.
Compressions:                             9693778837.
Pageins:                                   449536457.
Pageouts:                                    3296234.
Swapins:                                   446498989.
Swapouts:                                  453283099.
vm.swapusage: total = 5120.00M  used = 3873.94M  free = 1246.06M  (encrypted)
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
- **Build:** 107 s (incremental). `scripts/build.sh` left its one-line
  `apps/macos/Sweetbriar/Info.plist` edit in the build checkout; the
  next request restores it.
- **Kept on this Mac:** the traces, XML exports, logs and machine-state
  files are in `~/sb-bench-archive/20261031T033607Z-r4c4-closed-id-reads/`.
