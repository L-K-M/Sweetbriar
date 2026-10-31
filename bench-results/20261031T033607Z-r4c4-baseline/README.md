Request: bench-requests/20261031T033607Z-r4c4-baseline.md @ 2f961b3b2a5b3bc6bdebc383634d363d36ef0c90

# Search benchmark: 2f961b3, r4c4-baseline

- **Built commit:** `2f961b3b2a5b3bc6bdebc383634d363d36ef0c90`, "Merge
  pull request #168 from L-K-M/dependabot/cargo/tokio-1.54.1"
  (origin/main), Release, built with `scripts/build.sh app`. It carries
  the certain-unknowns change (`8086cb5`, merged in #170). It's the
  baseline for `9f21847`, request
  `20261031T033607Z-r4c4-closed-id-reads`.
- **Request:** LABEL r4c4-baseline, RUNS 2, PROFILE no, SEARCH_TIMINGS no.
- **Date:** 2026-10-31. Run 1 03:42 to 03:44 UTC; run 2 03:44 to 03:46
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
| 1 | true | 50 of 50 | 732.8 | 207.1 | exact-address 290, exact-reference 592, phrase 674, scoped-filter 718, negative-body 742 |
| 2 | true | 50 of 50 | 759.6 | 224.2 | scoped-filter 424, exact-reference 590, exact-address 705, phrase 760, negative-body 1,542 |

Both runs miss spec/08's budget (`met: false`, p95 under 100 ms).
`storeMessages` is 100000 in both. No retries were needed.

Stages (p50 / p95 ms, keystroke to stage):

| run | presented | firstPage | landed | opened |
|---|---:|---:|---:|---:|
| 1 | 41.3 / 73.8 | 190.8 / 701.9 | 192.2 / 720.2 | 373.4 / 2,809.7 |
| 2 | 43.7 / 100.7 | 217.8 / 747.4 | 218.1 / 748.3 | 696.6 / 2,352.3 |

This is in the range of the `8086cb5` runs from 2026-10-30 (p95 525
and 758 ms, p50 201 and 168 ms), which this merge carries. The
`opened` stage's p95 is the slowest yet on this Mac: 2,352 to 2,810 ms,
against 1,921 to 2,097 for `8086cb5`.

## Machine state

| | before (03:42:33Z) | after (03:45:50Z) |
|---|---:|---:|
| `hw.memsize` | 8589934592 | 8589934592 |
| memory free (`memory_pressure`) | 7% | 8% |
| pages wired down (16 KiB) | 399,450 (6.1 GiB) | 400,417 (6.1 GiB) |
| file-backed pages | 13,340 (208 MiB) | 16,213 (253 MiB) |
| anonymous pages | 20,275 | 20,829 |
| pages occupied by compressor | 54,065 | 49,320 |
| swap used (`vm.swapusage`) | 4,034.75 MiB of 5,120 | 4,156.88 MiB of 5,120 |

Wired memory is about 0.2 GiB higher than on 2026-10-30 (6.1 against
5.9 GiB), and the file cache is still about 200 to 250 MiB.

Full outputs:

```text
== before runs: 2026-10-31T03:42:33Z
hw.memsize: 8589934592
System-wide memory free percentage: 7%
Mach Virtual Memory Statistics: (page size of 16384 bytes)
Pages free:                                     3759.
Pages active:                                  17268.
Pages inactive:                                15643.
Pages speculative:                               704.
Pages throttled:                                   0.
Pages wired down:                             399450.
Pages purgeable:                                   2.
"Translation faults":                    19016016649.
Pages copy-on-write:                       380603632.
Pages zero filled:                        2785589076.
Pages reactivated:                        6321872994.
Pages purged:                               85724306.
File-backed pages:                             13340.
Anonymous pages:                               20275.
Pages stored in compressor:                   605203.
Pages occupied by compressor:                  54065.
Decompressions:                           9409930046.
Compressions:                             9618295414.
Pageins:                                   439866386.
Pageouts:                                    3200577.
Swapins:                                   434572628.
Swapouts:                                  441332067.
vm.swapusage: total = 5120.00M  used = 4034.75M  free = 1085.25M  (encrypted)
```

```text
== after runs: 2026-10-31T03:45:50Z
hw.memsize: 8589934592
System-wide memory free percentage: 8%
Mach Virtual Memory Statistics: (page size of 16384 bytes)
Pages free:                                     4071.
Pages active:                                  18804.
Pages inactive:                                17093.
Pages speculative:                              1145.
Pages throttled:                                   0.
Pages wired down:                             400417.
Pages purgeable:                                   4.
"Translation faults":                    19037906694.
Pages copy-on-write:                       380628744.
Pages zero filled:                        2785974953.
Pages reactivated:                        6329149258.
Pages purged:                               85807352.
File-backed pages:                             16213.
Anonymous pages:                               20829.
Pages stored in compressor:                   605579.
Pages occupied by compressor:                  49320.
Decompressions:                           9423959148.
Compressions:                             9632438000.
Pageins:                                   441217907.
Pageouts:                                    3202622.
Swapins:                                   435523354.
Swapouts:                                  442291279.
vm.swapusage: total = 5120.00M  used = 4156.88M  free = 963.12M  (encrypted)
```

## Anything unusual

- **Foreground:** Sweetbriar was brought to the front within 0.1 s of
  its process appearing in both runs, and neither lost the foreground.
- **Do Not Disturb:** not verifiable. This job runs unattended, and the
  sandbox can't read the Focus state.
- **Build:** 92 s (incremental). `scripts/build.sh` left its one-line
  `apps/macos/Sweetbriar/Info.plist` edit in the build checkout; the
  next request restores it.
- **Kept on this Mac:** build log and machine-state files in
  `~/sb-bench-archive/20261031T033607Z-r4c4-baseline/`.
