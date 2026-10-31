Request: bench-requests/20261031T040323Z-r4c4-closed-id-final.md @ 963515c060e7d15fc0c0d634153c5686704ede5a

# Search benchmark: 963515c, r4c4-closed-id-final

**Read with care: the timed runs were taken while the Mac was swapping
heavily, and both profiled attempts failed.** These numbers can't be
compared with the `9f21847` request run 25 minutes earlier.

- **Built commit:** `963515c060e7d15fc0c0d634153c5686704ede5a`, "Keep
  stopped Search reads ahead of late failures"
  (origin/m1.6r4c4/closed-id-reads), Release, built with
  `scripts/build.sh app`. It's `9f21847` (request
  `20261031T033607Z-r4c4-closed-id-reads`) plus this one commit.
- **Request:** LABEL r4c4-closed-id-final, RUNS 2, PROFILE yes,
  SEARCH_TIMINGS no.
- **Date:** 2026-10-31.
  - Run 1: 04:12 to 04:15 UTC.
  - Run 2: 04:15 to 04:19 UTC.
  - Profiled attempt 1: about 04:19 to 04:28 UTC.
  - Profiled attempt 2 (retry): 04:46 to 04:55 UTC.
- **Mac:** Mac mini M1 (Macmini9,1, 8 cores, 8 GiB), macOS 26.5.2
  (25F84), Xcode build 17F113, on AC power.
- **Display:** one external display, 1920 x 1080 at 60 Hz.
- **Archive:** `~/sweetbriar-search-archive`, 100,000 synthetic
  messages, seeded 2026-10-29 and unchanged since.

## Timed runs

| run | ok | samples | p95 ms | p50 ms | per-query p95 ms |
|---|---|---|---:|---:|---|
| 1 | true | 50 of 50 | 1,348.7 | 402.5 | exact-reference 458, exact-address 1,046, negative-body 1,349, phrase 2,227, scoped-filter 2,700 |
| 2 | true | 50 of 50 | 1,971.1 | 350.2 | exact-reference 709, phrase 729, exact-address 1,535, negative-body 1,971, scoped-filter 2,242 |

Both runs pass the benchmark's own checks and miss spec/08's budget
(`met: false`). `storeMessages` is 100000 in both.

Stages (p50 / p95 ms, keystroke to stage):

| run | presented | firstPage | landed | opened |
|---|---:|---:|---:|---:|
| 1 | 69.3 / 231.7 | 358.9 / 1,333.9 | 381.2 / 1,339.9 | 1,277.3 / 5,212.2 |
| 2 | 63.3 / 195.4 | 342.5 / 1,888.0 | 343.5 / 1,937.5 | 1,311.1 / 6,338.0 |

**Why these runs can't be compared.** Each run took about 190 s,
against about 90 s for the `9f21847` runs, and the build took 424 s,
against about 90 s for the earlier incremental builds. Right after the
timed runs and the first profiled attempt (04:28 UTC):

- the load average was about 21 on 8 cores;
- the Mac did about 11,700 swap-ins and 11,600 swap-outs per second;
- system processes were stuck in uninterruptible waits: Spotlight
  (`mds`, `mdbulkimport`), `mobileassetd`, `siriknowledged`,
  `duetexpertd`.

Nothing from this job was running besides the benchmark. Even the
presented stage, which is about 40 ms on this Mac when it isn't loaded,
is 63 to 69 ms at p50 here.

For reference only, the `9f21847` runs gave p95 552.9 and 617.2 ms and
p50 174.3 and 171.2 ms. This request's timed runs are 2 to 3 times
slower. That can't be told apart from the load without a paired rerun.

## Profiled runs (both failed)

- **Attempt 1** (`profiled-invalid1.json`): "the window wasn't key and
  the app active", "exact-reference round 0: the typed prefix never
  settled", with 0 samples. It ran under the load described above. A
  Keychain Circle Notification was up at the time and may have taken
  the key window.
- **Attempt 2** (`profiled.json`, the one allowed retry): started once
  the load had fallen to about 2 and swap-ins to about 170 per second.
  It failed with "exact-reference round 3: the typed prefix never
  settled" after 15 of 50 samples. The load average rose to 10.5 during
  the run.

`profile.md` analyses attempt 2. During the 44.6 s stall the app was
mostly off CPU, at about 3.5% of one worker core and under 1% of the
main thread. Its only work was a `read_search_relevance_page` SQLite
read that kept progressing slowly. That fits a read blocked on page
faults under memory pressure more than a CPU-bound query or a deadlock.
The profiler can't show what it waited on.

Both profiled attempts stalled on a typed prefix, which the `9f21847`
profile never did. That could be a regression in this commit or the
Mac's state. Paired reruns of `9f21847` and `963515c` on an unloaded
Mac would settle it.

## Machine state

Timed runs:

| | before (04:12:11Z) | after (04:27:42Z, after attempt 1) |
|---|---:|---:|
| `hw.memsize` | 8589934592 | 8589934592 |
| memory free (`memory_pressure`) | 7% | 7% |
| pages wired down (16 KiB) | 402,796 (6.1 GiB) | 401,200 (6.1 GiB) |
| file-backed pages | 12,909 (202 MiB) | 13,761 (215 MiB) |
| anonymous pages | 19,273 | 18,880 |
| pages occupied by compressor | 51,730 | 52,999 |
| swap used (`vm.swapusage`) | 4,965.12 MiB of 6,144 | 4,383.38 MiB of 5,120 |

Profiled retry:

| | before (04:46:26Z) | after (04:55:29Z) |
|---|---:|---:|
| load averages (1, 5, 15 min) | 2.82, 2.79, 5.73 | 10.47, 6.97, 6.65 |
| memory free (`memory_pressure`) | 7% | 7% |
| pages wired down (16 KiB) | 400,062 (6.1 GiB) | 401,900 (6.1 GiB) |
| file-backed pages | 13,493 (211 MiB) | 12,057 (188 MiB) |
| anonymous pages | 19,977 | 21,459 |
| pages occupied by compressor | 53,471 | 52,058 |
| swap used (`vm.swapusage`) | 4,159.62 MiB of 5,120 | 3,865.19 MiB of 5,120 |

Full outputs:

```text
== before runs: 2026-10-31T04:12:11Z
hw.memsize: 8589934592
System-wide memory free percentage: 7%
Mach Virtual Memory Statistics: (page size of 16384 bytes)
Pages free:                                     3978.
Pages active:                                  16720.
Pages inactive:                                14193.
Pages speculative:                              1269.
Pages throttled:                                   0.
Pages wired down:                             402796.
Pages purgeable:                                   0.
"Translation faults":                    19202509853.
Pages copy-on-write:                       380906543.
Pages zero filled:                        2790922192.
Pages reactivated:                        6387926554.
Pages purged:                               86141785.
File-backed pages:                             12909.
Anonymous pages:                               19273.
Pages stored in compressor:                   631418.
Pages occupied by compressor:                  51730.
Decompressions:                           9526764240.
Compressions:                             9737480025.
Pageins:                                   454758978.
Pageouts:                                    3347440.
Swapins:                                   454465746.
Swapouts:                                  461366414.
vm.swapusage: total = 6144.00M  used = 4965.12M  free = 1178.88M  (encrypted)
```

```text
== after runs: 2026-10-31T04:27:42Z
hw.memsize: 8589934592
System-wide memory free percentage: 7%
Mach Virtual Memory Statistics: (page size of 16384 bytes)
Pages free:                                     3809.
Pages active:                                  16704.
Pages inactive:                                14692.
Pages speculative:                              1245.
Pages throttled:                                   0.
Pages wired down:                             401200.
Pages purgeable:                                   4.
"Translation faults":                    19299221301.
Pages copy-on-write:                       381025229.
Pages zero filled:                        2792628917.
Pages reactivated:                        6420486586.
Pages purged:                               86326712.
File-backed pages:                             13761.
Anonymous pages:                               18880.
Pages stored in compressor:                   630577.
Pages occupied by compressor:                  52999.
Decompressions:                           9589807472.
Compressions:                             9801337436.
Pageins:                                   462998031.
Pageouts:                                    3382857.
Swapins:                                   468496147.
Swapouts:                                  475458589.
vm.swapusage: total = 5120.00M  used = 4383.38M  free = 736.62M  (encrypted)
```

```text
== load snapshot 2026-10-31T04:28:48Z
 5:28  up 87 days, 13:55, 1 user, load averages: 13.60 17.05 12.64
Swapins:                                   469170902.
Swapouts:                                  476128005.
Swapins:                                   469229513.
Swapouts:                                  476186241.
  PID STAT  %CPU COMM
   96 Us     0.3 /usr/libexec/logd
  125 Us     3.8 /System/Library/Frameworks/CoreServices.framework/Versions/A/Frameworks/Metadata.framework/Versions/A/Support/mds
  148 Us    11.3 /usr/libexec/dasd
  162 Us     0.3 /usr/sbin/bluetoothd
  187 Us     0.7 /System/Library/PrivateFrameworks/CoreAnalytics.framework/Support/analyticsd
  212 Us     1.7 /usr/libexec/symptomsd
  324 Us     1.3 /System/Library/CoreServices/Software Update.app/Contents/Resources/softwareupdated
  330 Us     1.8 /System/Library/Frameworks/CoreServices.framework/Versions/A/Frameworks/Metadata.framework/Versions/A/Support/mds_stores
  369 U      0.0 /System/Library/Frameworks/Accounts.framework/Versions/A/Support/accountsd
  380 U      0.7 /System/Library/PrivateFrameworks/CloudKitDaemon.framework/Support/cloudd
  466 U      0.7 /System/Library/CoreServices/WiFiAgent.app/Contents/MacOS/WiFiAgent
  546 U      0.6 /System/Library/CoreServices/diagnostics_agent
  672 UN     0.6 /System/Library/Frameworks/CoreServices.framework/Versions/A/Frameworks/Metadata.framework/Versions/A/Support/mdbulkimport
 4156 Rs     1.6 /usr/libexec/PerfPowerServices
26831 R      0.8 /Applications/Obtainintosh.app/Contents/MacOS/obtainintosh
37103 R     23.6 /Applications/Coreography.app/Contents/MacOS/Coreography
64160 Rs     0.9 /usr/libexec/mobileassetd
64180 U      1.1 /usr/libexec/nsurlsessiond
91467 R      0.4 /usr/libexec/siriknowledged
92079 R      1.0 /System/Library/PrivateFrameworks/VoiceShortcuts.framework/Versions/A/Support/siriactionsd
92082 R      2.1 /usr/libexec/duetexpertd
92470 Us     0.9 /System/Library/PrivateFrameworks/IntelligencePlatformCompute.framework/Versions/A/XPCServices/IntelligencePlatformComputeService.xpc/Contents/MacOS/IntelligencePlatformComputeService
93133 U      0.9 /System/Library/PrivateFrameworks/IntelligenceFlowRuntime.framework/Versions/A/intelligenceflowd
93292 Rs     1.0 /System/Library/PrivateFrameworks/XprotectFramework.framework/Versions/A/XPCServices/XProtectBridgeService.xpc/Contents/MacOS/XProtectBridgeService
93298 U      0.9 /System/Library/PrivateFrameworks/TelephonyUtilities.framework/callservicesd
93312 R      1.0 /System/Library/PrivateFrameworks/DoNotDisturbServer.framework/Support/donotdisturbd
93366 R      0.4 /System/Library/PrivateFrameworks/HearingCore.framework/heard
93374 R      0.9 /usr/libexec/replayd
93407 R      1.3 /usr/libexec/searchpartyuseragent
93462 R      1.1 /System/Library/Frameworks/Contacts.framework/Support/contactsd
93488 U      0.4 /usr/libexec/pkd
93542 R      1.0 /usr/libexec/triald
93544 Rs     6.6 /System/Library/PrivateFrameworks/CloudKitDaemon.framework/Support/cloudd
93571 R      1.0 /System/Library/PrivateFrameworks/ContextKit.framework/Versions/A/XPCServices/ContextService.xpc/Contents/MacOS/ContextService
93598 Rs     0.9 /System/Library/PrivateFrameworks/AssetCacheServicesExtensions.framework/XPCServices/AssetCacheTetheratorService.xpc/Contents/MacOS/AssetCacheTetheratorService
93645 R      1.6 /usr/libexec/knowledge-agent
93662 R      1.0 /System/Library/Frameworks/CoreSpotlight.framework/spotlightknowledged
93663 U      1.1 /System/Library/PrivateFrameworks/AppStoreDaemon.framework/Support/appstoreagent
93732 R      1.5 /System/Library/PrivateFrameworks/AssistantServices.framework/Versions/A/Support/assistantd
93802 R      0.5 /System/Library/PrivateFrameworks/iTunesCloud.framework/Support/itunescloudd
93849 Us     0.1 /System/Library/PrivateFrameworks/CloudTelemetry.framework/Versions/A/XPCServices/CloudTelemetryService.xpc/Contents/MacOS/CloudTelemetryService
93871 R      0.8 /usr/libexec/tipsd
93890 R      0.9 /System/Library/PrivateFrameworks/SoftwareUpdate.framework/Resources/SoftwareUpdateNotificationManager.app/Contents/MacOS/SoftwareUpdateNotificationManager
93894 U      0.9 /System/Library/PrivateFrameworks/CoreParsec.framework/parsec-fbf
93895 R      0.6 /System/Library/PrivateFrameworks/AppleMediaServices.framework/Resources/amsaccountsd
93918 Rs    11.5 /System/Library/PrivateFrameworks/UIFoundation.framework/Versions/A/XPCServices/nsattributedstringagent.xpc/Contents/MacOS/nsattributedstringagent
93934 Us     1.8 /Applications/Coreography.app/Contents/PlugIns/CoreographyWidgets.appex/Contents/MacOS/CoreographyWidgets
93943 R      3.9 /bin/launchctl
93944 R      5.1 /bin/launchctl
93947 R      0.0 ps
93948 R      0.0 awk
```

```text
== before profiled retry: 2026-10-31T04:46:26Z
 5:46  up 87 days, 14:12, 1 user, load averages: 2.82 2.79 5.73
hw.memsize: 8589934592
System-wide memory free percentage: 7%
Mach Virtual Memory Statistics: (page size of 16384 bytes)
Pages free:                                     3790.
Pages active:                                  17179.
Pages inactive:                                15834.
Pages speculative:                               457.
Pages throttled:                                   0.
Pages wired down:                             400062.
Pages purgeable:                                   0.
"Translation faults":                    19370875412.
Pages copy-on-write:                       381177270.
Pages zero filled:                        2794168255.
Pages reactivated:                        6437717150.
Pages purged:                               86729616.
File-backed pages:                             13493.
Anonymous pages:                               19977.
Pages stored in compressor:                   601063.
Pages occupied by compressor:                  53471.
Decompressions:                           9636003482.
Compressions:                             9847935777.
Pageins:                                   466550711.
Pageouts:                                    3396232.
Swapins:                                   471289481.
Swapouts:                                  478240609.
vm.swapusage: total = 5120.00M  used = 4159.62M  free = 960.38M  (encrypted)
```

```text
== after profiled retry: 2026-10-31T04:55:29Z
 5:55  up 87 days, 14:21, 1 user, load averages: 10.47 6.97 6.65
hw.memsize: 8589934592
System-wide memory free percentage: 7%
Mach Virtual Memory Statistics: (page size of 16384 bytes)
Pages free:                                     3387.
Pages active:                                  17259.
Pages inactive:                                16060.
Pages speculative:                               196.
Pages throttled:                                   0.
Pages wired down:                             401900.
Pages purgeable:                                   0.
"Translation faults":                    19424013812.
Pages copy-on-write:                       381248623.
Pages zero filled:                        2794777381.
Pages reactivated:                        6454957437.
Pages purged:                               86771033.
File-backed pages:                             12057.
Anonymous pages:                               21459.
Pages stored in compressor:                   576206.
Pages occupied by compressor:                  52058.
Decompressions:                           9673477208.
Compressions:                             9885670832.
Pageins:                                   470958335.
Pageouts:                                    3440685.
Swapins:                                   480854333.
Swapouts:                                  487816234.
vm.swapusage: total = 5120.00M  used = 3865.19M  free = 1254.81M  (encrypted)
```

## Anything unusual

- **Retries:** the profiled run used its one retry, after waiting about
  17 minutes for the load to settle. Both attempts failed, so there's
  no passing profile for this commit.
- **Foreground:** both timed runs kept the foreground. Sweetbriar was
  brought to the front within 0.1 s (run 1) and about 1 s (run 2) of
  its process appearing.
- **Profiler:** `xctrace` reported "The log archive is corrupt or
  incomplete" and exited 2 on both attempts, but saved the traces. Its
  stopped copies of the app were killed afterwards.
- **Do Not Disturb:** not verifiable. This job runs unattended, and the
  sandbox can't read the Focus state.
- **Build:** 424 s, far longer than the usual incremental build of
  about 90 s, because of the same load. `scripts/build.sh` left its
  one-line `apps/macos/Sweetbriar/Info.plist` edit in the build
  checkout; the next request restores it.
- **Kept on this Mac:** both traces, the XML exports, the logs and the
  load snapshots are in
  `~/sb-bench-archive/20261031T040323Z-r4c4-closed-id-final/`, with
  attempt 1's trace under `profiled-attempt1/`.
