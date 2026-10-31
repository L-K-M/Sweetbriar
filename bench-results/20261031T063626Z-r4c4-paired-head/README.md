Request: bench-requests/20261031T063626Z-r4c4-paired-head.md @ 14017c0be7018ba9c1514f96eae36ddfe96d339c

# Search benchmark: 14017c0, r4c4-paired-head

**Read with care: the Mac was swapping 3,600 to 5,600 pages a second in
and out around these runs, and the load average rose from 5 to 8.5
during them.** They pair with request
`20261031T063626Z-r4c4-paired-baseline`, run just before.

- **Built commit:** `14017c0be7018ba9c1514f96eae36ddfe96d339c`, "Keep
  reader stop-state inspection private"
  (origin/m1.6r4c4/closed-id-reads), Release, built with
  `scripts/build.sh app`. It's `963515c` (request
  `20261031T040323Z-r4c4-closed-id-final`) plus this one commit, on top
  of the baseline `2f961b3`.
- **Request:** LABEL r4c4-paired-head, RUNS 3, PROFILE no,
  SEARCH_TIMINGS no.
- **Date:** 2026-10-31.
  - Run 1: 06:54 to 06:56 UTC.
  - Run 2: 06:56 to 06:59 UTC.
  - Run 3: 06:59 to 07:02 UTC.
- **Mac:** Mac mini M1 (Macmini9,1, 8 cores, 8 GiB), macOS 26.5.2
  (25F84), Xcode build 17F113, on AC power.
- **Display:** one external display, 1920 x 1080 at 60 Hz.
- **Archive:** `~/sweetbriar-search-archive`, 100,000 synthetic
  messages, seeded 2026-10-29 and unchanged since.

## Timed runs

| run | ok | samples | p95 ms | p50 ms | per-query p95 ms |
|---|---|---|---:|---:|---|
| 1 | true | 50 of 50 | 1,182.4 | 338.4 | phrase 458, exact-address 689, exact-reference 696, scoped-filter 1,182, negative-body 2,435 |
| 2 | true | 50 of 50 | 1,741.4 | 414.6 | exact-address 583, exact-reference 616, phrase 1,259, scoped-filter 1,774, negative-body 1,927 |
| 3 | true | 50 of 50 | 2,090.1 | 406.8 | exact-reference 748, exact-address 916, phrase 1,255, scoped-filter 2,342, negative-body 3,039 |

All three runs pass the benchmark's own checks and miss spec/08's
budget (`met: false`). `storeMessages` is 100000 in all three. No
retries were needed.

Stages (p50 / p95 ms, keystroke to stage):

| run | presented | firstPage | opened |
|---|---:|---:|---:|
| 1 | 63.9 / 163.6 | 327.2 / 1,139.4 | 930.0 / 3,885.9 |
| 2 | 59.0 / 191.3 | 398.1 / 1,656.3 | 1,191.9 / 4,687.6 |
| 3 | 72.3 / 201.5 | 387.0 / 1,968.0 | 1,123.9 / 5,344.6 |

### Against the paired baseline (2f961b3)

| build | p95 ms (runs 1, 2, 3) | median p95 | p50 ms (runs 1, 2, 3) | median p50 |
|---|---:|---:|---:|---:|
| 2f961b3 (paired-baseline) | 936.7, 1,761.7, 1,501.9 | 1,501.9 | 356.1, 500.6, 357.1 | 357.1 |
| 14017c0 (this request) | 1,182.4, 1,741.4, 2,090.1 | 1,741.4 | 338.4, 414.6, 406.8 | 406.8 |

- **No measurable gain.** Under this load the head isn't faster than
  the baseline. The ranges overlap, and the head's runs got slower as
  the load average climbed from 5.1 to 8.5. The swap rate was 3,600
  per second before the head's runs and 5,600 after. For the baseline
  it was 6,600 per second throughout. These runs can't settle a
  difference of this size.
- **No stall.** All 150 samples completed. Neither the timed runs nor
  the paired baseline's runs hit "the typed prefix never settled",
  which stopped both profiled attempts of `963515c`. These were timed
  runs, not profiled ones, so this doesn't fully rule out that stall.
- **The `opened` stage's p95** is higher here (3.9 to 5.3 s) than in the
  paired baseline (3.6 to 4.0 s).

## Machine state

| | before (06:53:57Z) | after (07:02:14Z) |
|---|---:|---:|
| load averages (1, 5, 15 min) | 5.12, 5.33, 4.13 | 8.48, 8.27, 6.26 |
| swap rate over 5 s (in / out per s) | 3,620 / 3,531 | 5,645 / 5,765 |
| `hw.memsize` | 8589934592 | 8589934592 |
| memory free (`memory_pressure`) | 7% | 7% |
| pages wired down (16 KiB) | 402,058 (6.1 GiB) | 404,923 (6.2 GiB) |
| file-backed pages | 14,198 (222 MiB) | 13,375 (209 MiB) |
| anonymous pages | 19,289 | 19,183 |
| pages occupied by compressor | 51,711 | 49,907 |
| swap used (`vm.swapusage`) | 4,590.56 MiB of 5,120 | 4,637.75 MiB of 6,144 |

Full outputs:

```text
== before runs: 2026-10-31T06:53:57Z
 7:53  up 87 days, 16:20, 1 user, load averages: 5.12 5.33 4.13
hw.memsize: 8589934592
System-wide memory free percentage: 7%
Mach Virtual Memory Statistics: (page size of 16384 bytes)
Pages free:                                     3551.
Pages active:                                  17204.
Pages inactive:                                14797.
Pages speculative:                              1486.
Pages throttled:                                   0.
Pages wired down:                             402058.
Pages purgeable:                                   2.
"Translation faults":                    19907909435.
Pages copy-on-write:                       382029630.
Pages zero filled:                        2805018879.
Pages reactivated:                        6570586279.
Pages purged:                               88859721.
File-backed pages:                             14198.
Anonymous pages:                               19289.
Pages stored in compressor:                   602324.
Pages occupied by compressor:                  51711.
Decompressions:                           9988251262.
Compressions:                            10203012636.
Pageins:                                   487808281.
Pageouts:                                    3503770.
Swapins:                                   494125392.
Swapouts:                                  501141590.
vm.swapusage: total = 5120.00M  used = 4590.56M  free = 529.44M  (encrypted)
swap rate over 5 s: 3620 swap-ins/s, 3531 swap-outs/s
```

```text
== after runs: 2026-10-31T07:02:14Z
 8:02  up 87 days, 16:28, 1 user, load averages: 8.48 8.27 6.26
hw.memsize: 8589934592
System-wide memory free percentage: 7%
Mach Virtual Memory Statistics: (page size of 16384 bytes)
Pages free:                                     3412.
Pages active:                                  16688.
Pages inactive:                                15375.
Pages speculative:                               495.
Pages throttled:                                   0.
Pages wired down:                             404923.
Pages purgeable:                                   0.
"Translation faults":                    19973431060.
Pages copy-on-write:                       382090024.
Pages zero filled:                        2806022146.
Pages reactivated:                        6594979241.
Pages purged:                               89039373.
File-backed pages:                             13375.
Anonymous pages:                               19183.
Pages stored in compressor:                   582100.
Pages occupied by compressor:                  49907.
Decompressions:                          10031164135.
Compressions:                            10246277980.
Pageins:                                   491525001.
Pageouts:                                    3507563.
Swapins:                                   498729465.
Swapouts:                                  505751734.
vm.swapusage: total = 6144.00M  used = 4637.75M  free = 1506.25M  (encrypted)
swap rate over 5 s: 5645 swap-ins/s, 5765 swap-outs/s
```

## Anything unusual

- **Swapping and rising load:** see above. Nothing from this job was
  running besides the benchmark. macOS grew the swap file from 5,120 to
  6,144 MiB during the runs.
- **Foreground:** Sweetbriar was brought to the front within 0.1 s of
  its process appearing in all three runs, and none lost the
  foreground.
- **Do Not Disturb:** not verifiable. This job runs unattended, and the
  sandbox can't read the Focus state.
- **Build:** 119 s. `scripts/build.sh` left its one-line
  `apps/macos/Sweetbriar/Info.plist` edit in the build checkout; the
  next request restores it.
- **Kept on this Mac:** build log and machine-state files in
  `~/sb-bench-archive/20261031T063626Z-r4c4-paired-head/`.
