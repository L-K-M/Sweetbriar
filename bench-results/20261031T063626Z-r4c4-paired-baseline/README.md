Request: bench-requests/20261031T063626Z-r4c4-paired-baseline.md @ 2f961b3b2a5b3bc6bdebc383634d363d36ef0c90

# Search benchmark: 2f961b3, r4c4-paired-baseline

**Read with care: the Mac was swapping about 6,600 pages a second in
and out before and after these runs.** Compare them with the paired
head request (`20261031T063626Z-r4c4-paired-head`), run right after
under the same conditions. Don't compare them with the quieter
2026-10-31 03:42 runs of this commit.

- **Built commit:** `2f961b3b2a5b3bc6bdebc383634d363d36ef0c90`, "Merge
  pull request #168 from L-K-M/dependabot/cargo/tokio-1.54.1"
  (origin/main), Release, built with `scripts/build.sh app`. This is
  the same commit as request `20261031T033607Z-r4c4-baseline`.
- **Request:** LABEL r4c4-paired-baseline, RUNS 3, PROFILE no,
  SEARCH_TIMINGS no.
- **Date:** 2026-10-31.
  - Run 1: 06:43 to 06:45 UTC.
  - Run 2: 06:45 to 06:48 UTC.
  - Run 3: 06:48 to 06:51 UTC.
- **Mac:** Mac mini M1 (Macmini9,1, 8 cores, 8 GiB), macOS 26.5.2
  (25F84), Xcode build 17F113, on AC power.
- **Display:** one external display, 1920 x 1080 at 60 Hz.
- **Archive:** `~/sweetbriar-search-archive`, 100,000 synthetic
  messages, seeded 2026-10-29 and unchanged since.

## Timed runs

| run | ok | samples | p95 ms | p50 ms | per-query p95 ms |
|---|---|---|---:|---:|---|
| 1 | true | 50 of 50 | 936.7 | 356.1 | exact-address 557, scoped-filter 616, phrase 641, exact-reference 749, negative-body 1,349 |
| 2 | true | 50 of 50 | 1,761.7 | 500.6 | scoped-filter 599, exact-address 741, exact-reference 773, phrase 1,762, negative-body 2,418 |
| 3 | true | 50 of 50 | 1,501.9 | 357.1 | exact-address 399, exact-reference 923, scoped-filter 1,089, phrase 1,628, negative-body 3,905 |

All three runs pass the benchmark's own checks and miss spec/08's
budget (`met: false`). `storeMessages` is 100000 in all three. No
retries were needed.

Stages (p50 / p95 ms, keystroke to stage):

| run | presented | firstPage | landed | opened |
|---|---:|---:|---:|---:|
| 1 | 48.6 / 138.2 | 333.1 / 922.5 | 350.8 / 923.8 | 1,074.3 / 3,593.0 |
| 2 | 67.2 / 145.3 | 457.1 / 1,750.5 | 459.9 / 1,753.2 | 1,671.6 / 3,897.8 |
| 3 | 65.4 / 119.2 | 345.7 / 1,486.0 | 346.3 / 1,499.0 | 1,416.3 / 3,961.5 |

Each run took 135 to 169 s, against about 95 s for this commit at
03:42 UTC. The earlier runs' p95 was 733 and 760 ms and p50 207 and
224 ms. The presented stage, which is about 40 ms when the Mac isn't
loaded, is 49 to 67 ms here.

## Machine state

| | before (06:43:02Z) | after (06:50:58Z) |
|---|---:|---:|
| load averages (1, 5, 15 min) | 4.34, 3.19, 2.66 | 4.95, 4.61, 3.60 |
| swap rate over 5 s (in / out per s) | 6,684 / 6,286 | 6,574 / 6,366 |
| `hw.memsize` | 8589934592 | 8589934592 |
| memory free (`memory_pressure`) | 7% | 6% |
| pages wired down (16 KiB) | 404,323 (6.2 GiB) | 405,474 (6.2 GiB) |
| file-backed pages | 12,311 (192 MiB) | 12,507 (195 MiB) |
| anonymous pages | 20,115 | 18,163 |
| pages occupied by compressor | 50,228 | 50,604 |
| swap used (`vm.swapusage`) | 4,377.06 MiB of 5,120 | 4,505.06 MiB of 5,120 |

Wired memory has crept up again, to about 6.2 GiB of 8 GiB, and the
file cache is under 200 MiB. At poll time, eight minutes before the
runs, the swap-in rate was about 1,600 per second.

Full outputs:

```text
== before runs: 2026-10-31T06:43:02Z
 7:43  up 87 days, 16:09, 1 user, load averages: 4.34 3.19 2.66
hw.memsize: 8589934592
System-wide memory free percentage: 7%
Mach Virtual Memory Statistics: (page size of 16384 bytes)
Pages free:                                     3871.
Pages active:                                  16583.
Pages inactive:                                15475.
Pages speculative:                               368.
Pages throttled:                                   0.
Pages wired down:                             404323.
Pages purgeable:                                   0.
"Translation faults":                    19824573834.
Pages copy-on-write:                       381930064.
Pages zero filled:                        2803455697.
Pages reactivated:                        6542419710.
Pages purged:                               88632340.
File-backed pages:                             12311.
Anonymous pages:                               20115.
Pages stored in compressor:                   589366.
Pages occupied by compressor:                  50228.
Decompressions:                           9934286440.
Compressions:                            10148467417.
Pageins:                                   482678096.
Pageouts:                                    3480498.
Swapins:                                   489868195.
Swapouts:                                  496869206.
vm.swapusage: total = 5120.00M  used = 4377.06M  free = 742.94M  (encrypted)
swap rate over 5 s: 6684 swap-ins/s, 6286 swap-outs/s
```

```text
== after runs: 2026-10-31T06:50:58Z
 7:50  up 87 days, 16:17, 1 user, load averages: 4.95 4.61 3.60
hw.memsize: 8589934592
System-wide memory free percentage: 6%
Mach Virtual Memory Statistics: (page size of 16384 bytes)
Pages free:                                     3940.
Pages active:                                  15674.
Pages inactive:                                13851.
Pages speculative:                              1146.
Pages throttled:                                   0.
Pages wired down:                             405474.
Pages purgeable:                                   0.
"Translation faults":                    19885305867.
Pages copy-on-write:                       381986946.
Pages zero filled:                        2804284918.
Pages reactivated:                        6562880538.
Pages purged:                               88808187.
File-backed pages:                             12507.
Anonymous pages:                               18163.
Pages stored in compressor:                   595838.
Pages occupied by compressor:                  50604.
Decompressions:                           9974237778.
Compressions:                            10188714427.
Pageins:                                   486061711.
Pageouts:                                    3492149.
Swapins:                                   493362051.
Swapouts:                                  500372138.
vm.swapusage: total = 5120.00M  used = 4505.06M  free = 614.94M  (encrypted)
swap rate over 5 s: 6574 swap-ins/s, 6366 swap-outs/s
```

## Anything unusual

- **Swapping:** see above. Nothing from this job was running besides
  the benchmark.
- **Foreground:** Sweetbriar was brought to the front within 0.1 s of
  its process appearing in all three runs, and none lost the
  foreground.
- **Do Not Disturb:** not verifiable. This job runs unattended, and the
  sandbox can't read the Focus state.
- **Build:** 102 s. `scripts/build.sh` left its one-line
  `apps/macos/Sweetbriar/Info.plist` edit in the build checkout; the
  next request restores it.
- **Kept on this Mac:** build log and machine-state files in
  `~/sb-bench-archive/20261031T063626Z-r4c4-paired-baseline/`.
