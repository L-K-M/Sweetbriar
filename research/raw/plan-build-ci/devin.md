<!-- provenance: helper=devin model=swe-2-max started=2026-10-12T07:11:31+00:00 exit=0 brief=research/briefs/ -->

# Sweetbriar: build, signing and CI pipeline

Research report for the implementation plan. Repo: `L-K-M/Sweetbriar`
(personal-account repository). Agents write code on Linux; the owner
reviews and merges PRs and runs the app on their own Mac (macOS 27
"Golden Gate", Apple silicon). All time-sensitive facts were checked on
**2026-10-12** unless noted otherwise.

## 1. TL;DR

- **CI home base is `macos-26`** (GA since 2026-03, arm64 M1 3-core /
  7 GB / 14 GB SSD): macOS 26.6.x, Xcode 26.6 default (Swift 6.3.3),
  Rust 1.98.1 preinstalled. A macOS 27 image exists only as the
  **`xcode-27` preview label** (macOS 27.0.1, Xcode 27.0 GM + 27.1/27.2
  betas, arm64, same 3-core size) — usable, but marked preview.
- **No Intel runner for this repo.** `macos-26-intel`/`*-large`/
  `*-xlarge` are "larger runners": billed always, and per GitHub docs
  available only to Team/Enterprise **organizations** — a personal
  account can't use them. Cross-compile `x86_64-apple-darwin` on the
  arm64 runner if Intel is ever needed (build-only). Given macOS 27 is
  arm64-only and the floor is macOS 26, arm64-first is correct.
- **Cost shape:** standard macOS minutes bill at **$0.062** (~10× the
  $0.006 Linux minute); free on public repos, drawn from a 2,000
  (Free) / 3,000 (Pro) monthly minute quota on private repos. Keep the
  macOS PR job to ~10–15 min; push UI tests and release work to
  nightly/tag workflows.
- **Rust→Swift:** one `staticlib` "megazord" crate (`sb-ffi`) built for
  `aarch64-apple-darwin`, bindings generated with UniFFI 0.32.x
  (`uniffi-bindgen-swift`, new in 0.32), assembled into an XCFramework,
  consumed through a local SPM package — the Mozilla
  application-services / Firefox-iOS pattern. **`cargo-swift` is a dead
  end right now**: it tops out at UniFFI 0.31.1 while UniFFI is at
  0.32.2 — write a small `cargo xtask` instead.
- **Signing can be deferred safely.** Until a Developer ID certificate
  exists, CI produces ad-hoc signed `.app`/`.dmg` artifacts; the owner
  runs them with `xattr -dr com.apple.quarantine`. The full
  secrets→keychain→`notarytool`→`stapler`→Sparkle EdDSA chain is a
  well-documented recipe; budget it as a milestone before the first
  external build, not day one.
- **Testing:** Swift Testing for unit tests (Xcode 26/27 run it
  alongside XCTest in one target); XCUITest still required for UI tests.
  UI tests **do work** on the hosted mac runners — images enable
  automation mode without authentication — but the virtual display is
  1024×768, so avoid geometry-dependent assertions.
- **Agent-friendly layout:** XcodeGen `project.yml` → generated
  `.xcodeproj` (never hand-edit pbxproj), and when the toolchain moves
  to Xcode ≥27.2 consider Apple's new JSON `project.xcproj` format,
  which exists explicitly for Git/AI-tool diffs. Everything an agent
  can break must be checkable on Linux; the macOS job exists to catch
  what can't be.
- **Bootstrap order:** Linux CI first (fmt/clippy/test/cargo-deny +
  the architecture check), then the macOS "build + unit test" job, then
  unsigned artifact builds, then nightly UI tests, then the signed
  release pipeline.

---

## 2. Findings

### 2.1 GitHub-hosted macOS runners (as of 2026-10)

**Available images** ([runner-images README](https://github.com/actions/runner-images)):

| Label | Arch | OS | Size | Status |
|---|---|---|---|---|
| `macos-latest`, `macos-26` | arm64 | macOS 26.6.2 | 3-core M1, 7 GB, 14 GB | GA, standard |
| `macos-26-xlarge` | arm64 (M2 Pro) | macOS 26 | 5-core, 14 GB | larger runner (paid) |
| `macos-26-intel`, `macos-26-large`, `macos-latest-large` | x64 | macOS 26 | 12-core Intel, 30 GB | larger runner (paid) |
| `macos-15` | arm64 | macOS 15 | 3-core M1 | GA, standard |
| `macos-15-intel`, `macos-15-large` | x64 | macOS 15 | 12-core Intel | larger runner (paid) |
| `macos-14` (x64+arm64) | both | macOS 14 | — | **deprecated** (began 2026-07-06, unsupported 2026-11-02) |
| `xcode-27`, `xcode-27-xlarge` | arm64 | **macOS 27.0.1** | 3-core M1 / 5-core M2 Pro | **public preview** (since ~2026-07; rebased to macOS 27 on 2026-09-10) |

There is no `macos-27` label yet; macOS 27 is reachable only via
`xcode-27`. ([changelog](https://github.blog/changelog/2026-09-10-xcode-27-runner-image-now-runs-on-macos-27/),
[image issue #14404](https://github.com/actions/runner-images/issues/14404))

**Software on `macos-26` arm64** (image 20260907.0351.1,
[readme](https://github.com/actions/runner-images/blob/main/images/macos/macos-26-arm64-Readme.md)):

- Xcode **26.6 (default)** plus 26.0.1–26.5 selectable; CLT 26.6.
  Switch with `sudo xcode-select -s /Applications/Xcode_26.x.app/Contents/Developer`.
  Xcode 26.6 ships **Swift 6.3.3** ([Swift forums](https://forums.swift.org/t/announcing-swift-6-3-3/87888),
  [macosdb](https://macosdb.com/xcode/release/26.6-17F113/)).
- **Rust 1.98.1 / Cargo 1.98.1 / rustup 1.29** preinstalled (clippy,
  rustfmt). On `xcode-27`: Rust 1.99.0.
- Preinstalled tools relevant here: Fastlane 2.239, `xcbeautify` 3.2.1,
  `Xcodes`, `Unxip`, SwiftFormat 0.63, Homebrew 6.x, `gh`, `jq`,
  `create-dmg` is **not** preinstalled (brew/npm install it).
- On `xcode-27` (image 20261006.0244.1): macOS 27.0.1, Xcode **27.0
  (default, build 27A266a — GM, released 2026-09-14 alongside macOS 27;
  [Apple releases](https://developer.apple.com/news/releases/))** +
  27.1 beta + 27.2 beta 2, CLT 27.0.

**Billing and quotas** ([runner pricing](https://docs.github.com/en/billing/reference/actions-runner-pricing),
[Actions billing](https://docs.github.com/en/billing/concepts/product-billing/github-actions)):

- Standard runner per-minute rates: Linux x64 $0.006 · Linux arm
  $0.005 · **macOS (M1 3-core / Intel 4-core) $0.062** · Windows $0.010.
  Larger runners: macOS 5-core M2 Pro $0.102 (`macos_xl`), macOS
  12-core Intel $0.077 (`macos_l`). Linux slim $0.002.
- Standard runners are **free on public repos**; private repos draw
  from the plan quota (Free 2,000 min/mo, Pro 3,000, Team 3,000,
  Enterprise 50,000). macOS usage historically consumes ~10× the quota
  rate ("minute multipliers" still exist in billing), so a 2,000-minute
  quota is roughly **~200 real macOS minutes/month** — about 13–16
  runs of a 12–15 min macOS job.
- Cache storage: **10 GB per repository** on every plan — relevant to
  the Rust/Swift caching strategy below.
- **Larger runners are orgs-only** (Team / Enterprise Cloud),
  [docs](https://docs.github.com/en/actions/reference/runners/github-hosted-runners).
  On a personal-account repo, `-intel`, `-large`, `-xlarge` labels
  effectively don't exist. Consequence: no paid speed-up option and no
  native Intel runner for `L-K-M/Sweetbriar`.
- **Concurrency** ([limits](https://docs.github.com/en/actions/reference/limits)):
  Free 20 jobs total / **5 macOS**; Pro 40/5; Team 60/5; Enterprise
  500/50. A solo project will not hit this.
- Jobs round up to the nearest whole minute; self-hosted runners joined
  the billed model on 2026-03-01 (n/a here, but noted for the "run a
  Mac mini as runner someday" option).

**Cold build times (estimates, not measured):** the 3-core M1 is a real
but modest machine. Published reports put uncached `xcodebuild`-based
project builds at ~10–20 min on these runners and multi-hour stalls on
cold builds of large Rust-heavy apps ([runner-images #9751](https://github.com/actions/runner-images/discussions/9751),
[kolbo-code commit](https://github.com/Zoharvan12/kolbo-code/commit/2cd23140407dcba455550358c7b4d49d9264fa33),
[runner-images #11509](https://github.com/actions/runner-images/issues/11509)).
For Sweetbriar's early footprint — a moderate Rust workspace plus a
small app target — expect roughly **8–15 min cold, 3–6 min warm** for
the combined macOS job (estimate; verify empirically in week one).
Watch for image-update regressions: macOS image updates have
periodically doubled build times (the issues above); pin
`runs-on: macos-26` rather than `macos-latest` and record image
versions in the job log.

### 2.2 The Rust → Swift pipeline

**The settled architecture** ([01-architecture](../../01-architecture.md))
is one `staticlib` crate (`sb-ffi`, the "megazord" in Mozilla terms)
re-exporting all UniFFI surfaces, packaged as an **XCFramework** and
consumed through a **local Swift package** — the pattern Mozilla uses
for application-services and Firefox iOS
([shipping Rust components as Swift packages](https://mozilla.github.io/application-services/book/design/swift-package-manager.html),
[megazords/ios-rust](https://github.com/mozilla/application-services/tree/main/megazords/ios-rust)).

**Targets.** `aarch64-apple-darwin` is the primary (and on arm64 CI,
native) target. `x86_64-apple-darwin` cross-compiles fine on the arm64
runner (`rustup target add x86_64-apple-darwin`) — rustc links with the
Xcode toolchain — but **tests cannot run** for it there. An XCFramework
with both arch slices is a two-line change (`xcodebuild
-create-xcframework -library <arm64>.a -library <x64>.a …`) once
needed; defer it to the explicit Intel support decision (macOS 26 is
the floor and the last Intel OS).

**UniFFI 0.32.x.** Latest is **0.32.2** (2026-09-23;
[crates.io](https://crates.io/crates/uniffi),
[changelog](https://github.com/mozilla/uniffi-rs/blob/main/CHANGELOG.md)).
Relevant to us:

- New **`uniffi-bindgen-swift` binary** ([guide](https://mozilla.github.io/uniffi-rs/next/swift/uniffi-bindgen-swift.html)):
  runs in "library mode" (proc-macro metadata, no `.udl` needed), emits
  headers, modulemaps and — with `--xcframework` — an
  XCFramework-compatible modulemap, plus a `link_frameworks` option for
  extra frameworks. Ship it as a tiny workspace bin crate pinned to the
  workspace's `uniffi` version so bindgen and runtime can never drift.
- Swift output gained `Sendable` conformances and an opt-in
  `experimental_sendable_value_types` config — relevant to the strict
  Swift 6 goal.
- 0.32.0 changed `[ByRef] bytes` UDL args to `&[u8]` and made generated
  source ordering deterministic — deterministic output makes the CI
  "binding drift" check reliable.
- Known gap from the earlier research: the Swift async template's
  cancellation handling still doesn't propagate Swift task cancellation
  into Rust — handle in the hand-written adapter, not the generator.

**`cargo-swift` vs `xtask`.** `cargo-swift` is a nice bootstrap tool but
lags UniFFI hard — its own docs map UniFFI 0.31.1 → cargo-swift 0.11.1
and there is no 0.32 mapping ([docs.rs](https://docs.rs/crate/cargo-swift/latest),
[repo](https://github.com/antoniusnaumann/cargo-swift)). Sweetbriar
also needs non-default things (modulemap customization, drift checks,
release packaging) that a plugin can't express. **Recommendation: a
`xtask` crate** in the workspace with subcommands:

- `xtask ffi` — `cargo build -p sb-ffi --target aarch64-apple-darwin
  [--release]` → run `uniffi-bindgen-swift` on the `.a` → write Swift
  sources + headers + `--xcframework` modulemap → `xcodebuild
  -create-xcframework` → stage into `apps/macos/SweetbriarCore/` (a
  local SPM package: `binaryTarget` with relative `path:` + a target
  for the generated `.swift` files, exactly like
  `MozillaRustComponents/Package.swift` in firefox-ios).
- `xtask check-ffi` — regenerate into a temp dir and diff against the
  committed artifacts (bindings + modulemap). Non-zero exit = "you
  changed Rust exports without regenerating" — the binding-drift check
  the architecture doc asks for. Committing generated files is a
  judgment call: committing them makes drift detectable and builds
  reproducible; regenerating in CI avoids stale code but hides diffs.
  Commit them.
- `xtask dist` — later: archive, sign, notarize, staple, DMG, appcast.

For debug iteration the app can also link the Rust `staticlib` directly
in Xcode (skip the XCFramework) — but keep **one** packaging path in CI
or they'll diverge.

**Caching.** Two viable tools; the ecosystem has moved back and forth:

- [`Swatinem/rust-cache`](https://github.com/Swatinem/rust-cache):
  snapshots `~/.cargo` + `target/`, keyed on toolchain + `Cargo.lock`.
  Near-instant warm builds; but a `Cargo.lock` change busts the whole
  archive, and archives can eat the 10 GB/repo cache quota.
- [`mozilla-actions/sccache-action`](https://github.com/mozilla/sccache)
  (`RUSTC_WRAPPER=sccache`, `SCCACHE_GHA_ENABLED=1`): content-addressed
  per-compilation-unit cache, survives `Cargo.lock` churn and also
  caches C compiles (useful once `cc`-built deps appear). Real
  migrations report both directions working; the coarse-target approach
  is simpler, sccache degrades more gracefully.

Recommendation: start with **sccache on all Rust jobs** (it also serves
the `cc`/build-script compiles), keep `Swatinem/rust-cache` as fallback
if hit-rates disappoint. Additionally cache: the `~/.cargo/bin` install
of any pinned tools (or install via `taiki-e/install-action`), Xcode
**derived data is not worth caching** across runs (it corrupts more
than it saves; rely on `xcodebuild` incremental flags instead), and
SPM's `SourcePackages`/`.build` in the app job, keyed on
`Package.resolved`.

**Reproducibility.** `rust-toolchain.toml` pins the Rust version
(`channel = "1.98.1"` or newer); `Cargo.lock` committed (it's an
application, not a library); UniFFI's bindgen version tied to the
library version through the workspace bin crate; `DEVELOPER_DIR`
pinned in workflows; `xcodegen` version pinned via brew install of a
tag or a mise/aqua lockfile. Log `xcodebuild -version`, `rustc -Vv`,
and image version at job start — macOS image bumps have broken builds
before.

### 2.3 The Swift app project

**Xcode project vs SwiftPM.** An AppKit app with entitlements,
Info.plist, app extensions (Share/Quick Look), asset catalogs,
`.sdef`, and Sparkle cannot be expressed as a pure SPM package —
**Xcode project it is**. The pbxproj problem is real for an
agent-written repo: it's the file most likely to conflict and the file
agents edit worst. Options:

| Option | What it gives | Cost |
|---|---|---|
| Hand-maintained `.xcodeproj` | Zero tooling | pbxproj merge conflicts; agents can't reliably edit it; no validation |
| **XcodeGen (`project.yml`)** | YAML spec → generated project; `.xcodeproj` gitignored; agents edit YAML | One more tool (brew-installable on the image); spec must list every build setting |
| Tuist (`Project.swift`) | Swift-typed spec, modularization, caching | Heavier; needs install; overkill pre-modularization |
| Xcode ≥27.2 `project.xcproj` | Apple's new JSON5 on-disk format, designed for readable diffs and tool/agent editing ([Tuist changelog](https://tuist.dev/changelog/2026.09.17-xcode-27-project-format), [MacObserver](https://www.macobserver.com/news/xcode-27-2-beta-json-project-format-git-ai-tools/), [apple/xcode-project-format](https://github.com/apple/xcode-project-format)) | Experimental; every machine incl. CI needs Xcode ≥27.2 — today that's the `xcode-27` preview image with a 27.2 **beta** |

Recommendation: **XcodeGen now** (`xcodegen generate` as the first CI
step; keep a committed generated project out of the tree), evaluate
`project.xcproj` when Xcode 27.x is the pinned toolchain and the format
is stable. Real projects already run exactly this shape on macos-*
runners (`brew install xcodegen` → `xcodegen` → `xcodebuild`).

**`xcodebuild` in CI**, for an unsigned Debug build + unit tests:

```
sudo xcodebuild -runFirstLaunch          # once per job (licenses, platform support)
xcodegen generate                      # if using XcodeGen
xcodebuild -project apps/macos/Sweetbriar.xcodeproj -scheme Sweetbriar \
  -destination 'platform=macOS,arch=arm64' \
  -derivedDataPath .build/DerivedData \
  CODE_SIGN_IDENTITY=- CODE_SIGN_STYLE=Manual DEVELOPMENT_TEAM= \
  test | xcbeautify
```

Notes, verified against real workflows
([Blip ci.yml](https://github.com/blaineam/Blip/blob/main/.github/workflows/ci.yml),
[IronTV ci.yml](https://github.com/joaquinbejar/IronTV/blob/67d51a06f23388ff3cf21fee5393f14a1aed33fb/.github/workflows/ci.yml),
[MarkdownQuickLook release.yml](https://github.com/rkaregaran/MarkdownQuickLook/blob/ae10d2d64e2eec179a6e3cd2c3f37b9a54552265/.github/workflows/release.yml)):

- arm64 binaries must be signed to launch at all — `CODE_SIGN_IDENTITY=-`
  gives the ad-hoc signature. `CODE_SIGNING_ALLOWED=NO` is only safe
  for things that never execute (simulator builds); a test bundle that
  launches needs the ad-hoc signature and **entitlements dropped**
  (`CODE_SIGN_ENTITLEMENTS=`) or AMFI kills the host on restricted
  entitlements. Keep testable code free of profile-requiring
  entitlements; put keychain-access-groups etc. behind the app target,
  not the test seam.
- `-destination 'platform=macOS,arch=arm64'` selects the runner itself —
  no simulator involved for macOS apps.
- Upload `.xcresult` on failure (`actions/upload-artifact`,
  `if: failure()`); the log alone is miserable for triage.
- `xcbeautify` (preinstalled) for readable logs; `-quiet` alone hides
  warnings you want.

**Unit tests: Swift Testing vs XCTest.** Swift Testing (`@Test`,
`#expect`, parameterized cases, async-native) ships in the Swift 6
toolchain, runs under Xcode and `swift test`, coexists with XCTest in
the same target, and is the community default for new code in 2026
([Julian Astrada](https://julianastrada.com/blog/swift-testing-vs-xctest),
[comparison](https://muhittincamdali.com/en/comparisons/swift-testing-vs-xctest/));
Xcode 27 turns on cross-framework issue reporting by default
([WWDC26 session 267](https://wwdc.ai/2026/267)). XCTest remains
**required** for UI automation (`XCUIApplication`) and `measure{}`
performance tests. Sweetbriar: new unit tests in Swift Testing, UI and
perf tests in XCTest, all inside normal test targets.

**UI tests on runners: yes, they work.** GitHub's macOS images enable
automation mode without authentication and `DevToolsSecurity --enable`
(since image updates in 2022,
[runner-images #5410/#5417](https://github.com/actions/runner-images/issues/5410);
re-verified on a 2026 macos-15-arm64 image in
[vitrine ci](https://github.com/johnny4young/vitrine/commit/dd77515f57a55b5a68b3f67373142f04386fb545)).
Caveats: the virtual display is **1024×768** (geometry-sensitive tests
flake), first-launch modal prompts (TCC, default-app dialogs) must be
pre-seeded or avoided, and UI tests are the flakiest, slowest leg —
nightly, not per-PR.

**Swift 6 strict concurrency.** Build the app and the UniFFI-generated
bindings under `SWIFT_STRICT_CONCURRENCY=complete` and treat warnings
as errors in a dedicated leg *before* flipping `-swift-version 6`:
`xcodebuild build-for-testing SWIFT_STRICT_CONCURRENCY=complete` and
grep the log (IronTV does exactly this gate). Compile the *actual*
generated bindings under strict checking early — the 01-architecture
doc flags UniFFI 0.32.2's template for the Swift 6 async-trait issue
(UniFFI PR #2943 fix not in 0.32.2), so this isn't hygiene, it's a
known-live check.

### 2.4 Signing and notarization in CI

**What the full pipeline needs** (Developer ID direct distribution —
the D12 decision):

| Secret | Content |
|---|---|
| `DEVELOPER_ID_P12` | base64 of exported "Developer ID Application" cert + private key |
| `DEVELOPER_ID_P12_PASSWORD` | the .p12 export password |
| `APPLE_API_KEY` | base64 of App Store Connect API `.p8` |
| `APPLE_API_KEY_ID` / `APPLE_API_ISSUER` | 10-char key ID + issuer UUID |
| `SPARKLE_EDDSA_KEY` | Sparkle EdDSA private key (from `generate_keys`/`generate_keys -x` export) |

The dance, verified against Apple's docs and multiple production
workflows ([tenki.cloud guide](https://tenki.cloud/blog/sign-macos-ios-builds-github-actions),
[TN3147](https://apple-docs.everest.mt/docs/technotes/tn3147-migrating-to-the-latest-notarization-tool/),
[notarytool(1)](https://keith.github.io/xcode-man-pages/notarytool.1.html),
[AnswerCue release-macos.yml](https://github.com/FarzamHejaziK/AnswerCue/blob/62f20d5915c2d84c0f79a1bf5fe9a4b852ea3fc3/.github/workflows/release-macos.yml),
[CaskHub release.yml](https://github.com/alielsokary/CaskHub/blob/981dfe456c9617ac6ac513d32a7056565ad22f64/.github/workflows/release.yml)):

```bash
KEYCHAIN="$RUNNER_TEMP/build.keychain-db"
security create-keychain -p "$(uuidgen)" "$KEYCHAIN"
security set-keychain-settings -lut 21600 "$KEYCHAIN"
security unlock-keychain -p "$KPW" "$KEYCHAIN"
security import cert.p12 -k "$KEYCHAIN" -P "$P12_PW" -T /usr/bin/codesign
security set-key-partition-list -S apple-tool:,apple: -s -k "$KPW" "$KEYCHAIN"
security list-keychains -d user -s "$KEYCHAIN" login.keychain-db
# codesign with: --options runtime --timestamp (hardened runtime)
# notarize:
xcrun notarytool submit dist/Sweetbriar.zip \
  --key AuthKey.p8 --key-id "$APPLE_API_KEY_ID" --issuer "$APPLE_API_ISSUER" --wait
xcrun stapler staple Sweetbriar.app && xcrun stapler staple Sweetbriar.dmg
```

- **Hardened runtime** is required for notarization; enable in the
  project (`ENABLE_HARDENED_RUNTIME=YES`) and keep the entitlements
  list minimal — it also restricts which third-party libraries/plug-ins
  may load (Team ID validation), per the macOS-UI research.
- `notarytool --wait` blocks until verdict; typical turnaround is
  minutes. Stapling the `.app` **and** the `.dmg` matters (offline
  Gatekeeper).
- The App Store Connect API key route is the non-interactive standard;
  do not use Apple ID + app-specific password in CI. Individual vs Team
  key: **do not pass `--issuer` for Individual keys** (401), required
  for Team keys (notarytool man page).
- **Sparkle 2.10.0** (2026-09-13, "Golden Gate Bump") is current:
  min deployment macOS 12, drops CocoaPods
  ([releases](https://github.com/sparkle-project/Sparkle/releases)).
  EdDSA signing in CI: `sign_update --ed-key-file <file> <archive>`
  (the `-s` raw-key flag is deprecated since 2.8 — pipe via
  `--ed-key-file /dev/stdin` or a temp file);
  `generate_appcast` signs items automatically when keys are on the
  keychain; public key in `Info.plist` as `SUPublicEDKey`
  ([Sparkle publishing](https://sparkle-project.org/documentation/publishing/)).
  Re-sign Sparkle's own components (Autoupdate, XPC services, the
  framework) with your Developer ID before packaging.
- Sign inside-out: nested frameworks/XPC/extensions first, then the
  app bundle, then the dmg.

**What can wait.** Everything above waits until the first distributable
build. Before that: **ad-hoc signed artifacts are fine.** On Apple
silicon every binary is at least ad-hoc signed by the linker
([Apple dev forums](https://developer.apple.com/forums/thread/703059));
a CI artifact downloaded as a zip gets a quarantine xattr, and the
owner runs it via `xattr -dr com.apple.quarantine Sweetbriar.app` or
right-click → Open → "Open Anyway"
([Gatekeeper notes](https://github.com/perez987/Xattr-Editor/blob/master/DOCS/App-damaged.md)).
On the owner's Mac, Xcode "Sign to Run Locally" or a free "Apple
Development (Personal Team)" identity signs local debug builds. A
useful intermediate state (the OpCode precedent): a tag-triggered job
that attaches an **unsigned/ad-hoc `.dmg`** to the GitHub Release —
zero secrets needed, real binary for the owner, before Developer ID
enrollment lands.

**Ordering constraint:** entitlements that need a provisioning profile
(keychain-access-groups for shared items, app groups for the agent,
App Intents in some forms) can't run under ad-hoc signing — they get
SIGKILLed by AMFI or silently disabled. The macOS-UI research already
requires "a signed distribution build exercising default-mail
registration, a Share extension, scripting and an optional helper" as
prototype #4 — that prototype **is** the deadline for the Developer ID
secrets, not general availability.

### 2.5 Linux CI

For the Rust core this is the agent-visible gate, so it should be
complete, fast, and identical to what agents run locally
(`make check` / `cargo xtask ci`):

- **Image:** `ubuntu-24.04` (current `ubuntu-latest`) or `ubuntu-26.04`
  (GA 2026; [issue #14747](https://github.com/actions/runner-images/issues/14747)).
  `ubuntu-22.04` is mid-deprecation (unsupported 2026-04). Pick
  `ubuntu-26.04` for the newest toolchain; it costs the same $0.006.
- **Jobs:** `cargo fmt --check`, `cargo clippy --workspace
  --all-targets -- -D warnings`, `cargo test --workspace`, `cargo doc`
  (broken-doc-link warnings), the **module-architecture check** from
  01-architecture (a small script/`cargo metadata` verifier over
  allowed edges, plus a deliberately-forbidden dependency proving it
  fails — lands with the first commit), `cargo deny check` via
  [EmbarkStudios/cargo-deny-action](https://github.com/EmbarkStudios/cargo-deny-action)
  — advisories + `bans licenses sources`, with the `[licenses]` allow
  list shaped to the D13 policy (permissive + audited MPL; LGPL only
  dynamically linked).
- **Later GTK leg** (research/09: GTK4 + libadwaita + gtk-rs, no FFI):
  `sudo apt-get install -y libgtk-4-dev libadwaita-1-dev libwebkitgtk-6.0-dev`.
  Ubuntu 24.04 ships GTK **4.14.5**, libadwaita **1.5.0**, WebKitGTK
  **6.0 (2.52.6, Aug 2026 security update)**
  ([ubuntuupdates libgtk-4-dev](https://www.ubuntuupdates.org/package/core/noble/main/updates/libgtk-4-dev),
  [libwebkitgtk-6.0-dev](https://www.ubuntuupdates.org/package/core/noble/universe/updates/libwebkitgtk-6.0-dev),
  [gtk4+webkit guide](https://github.com/syrtcevvi/gtk4_webkitgtk_getting_started)).
  Ubuntu 26.04 ships newer GTK (4.2x, matching the research's GTK 4.22
  baseline — verify exact version when the leg is added). Build check
  only; headless GTK tests need `xvfb-run` or a nested Wayland
  compositor — doable but defer.

### 2.6 Repository layout for agents + CI

Recommended tree (consistent with 01-architecture's "modules inside
fewer crates; crates only at real boundaries"):

```
Cargo.toml            # workspace; [workspace.package] version single-sourced
rust-toolchain.toml
deny.toml
crates/
  sb/                 # the core, mostly modules (engine, store, mime, search, backends)
  sb-ffi/             # UniFFI surface — thin, exports only
  sb-cli/             # debug/ops CLI (doubles as a Linux-testable app)
  uniffi-bindgen-swift/  # tiny bin crate pinned to uniffi =0.32
xtask/                # cargo xtask: ffi, check-ffi, ci, dist
apps/
  macos/
    project.yml       # XcodeGen spec (the .xcodeproj is generated, gitignored)
    Sweetbriar/       # app target sources
    SweetbriarBridge/ # Swift adapter over the generated UniFFI bindings
    SweetbriarCore/   # local SPM package: binaryTarget(XCFramework) + bindings
    Tests/
  linux/              # later; gtk4-rs app calling the core directly
fixtures/             # .eml samples, crash/restore corpora, test mailboxes
docs/
scripts/              # build.sh / release.sh per the L-K-M conventions
.github/workflows/
```

Notes:

- `AGENTS.md` documents exactly which commands are Linux-verifiable
  (`cargo xtask ci` = fmt + clippy + test + deny + arch-check) and
  which are macOS-CI-only; this is what makes agent PRs mergeable
  without the owner re-running everything by hand.
- Branch protection: **on a personal Free account, protected branches
  work only on public repos; private repos need Pro+**
  ([docs](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-protected-branches/about-protected-branches)).
  Required checks to set once eligible: `linux` (all), `macos-build-test`;
  not the nightly/UI jobs (they're advisory). Also set
  `concurrency`/required-check hygiene per the L-K-M ci.yml trio
  (`permissions: contents: read`, `concurrency` with PR cancellation,
  `timeout-minutes` on every job).
- Follow the house CI conventions (§3 of the conventions doc): least
  privilege permissions, cancel-in-progress on PRs only, pinned
  toolchains, timeouts.

### 2.7 Agent workflow constraints

Agents cannot compile Swift, cannot run `xcodebuild`, cannot link for
`aarch64-apple-darwin` — cross-linking Rust for Darwin on Linux needs
osxcross and an SDK that's not legally distributable (unverified for
current terms; treat as unavailable). Consequences:

- **Linux-verifiable (agents must be green before PR):** fmt, clippy
  `-D warnings`, `cargo test` (workspace; includes all sync/MIME/store/
  search logic — i.e., *the correctness-critical code*), doc tests,
  `cargo deny`, the architecture-check, `xtask` unit logic, any pure
  Rust in `sb-ffi` (the FFI crate itself compiles and tests on Linux —
  keep platform-specific bits behind `#[cfg]`).
- **macOS-CI-only:** `aarch64-apple-darwin` build+test, UniFFI Swift
  binding generation + compile under strict concurrency, the
  XCFramework assembly, `xcodebuild` build + Swift unit tests, UI
  tests, signing/notarization, `Info.plist`/entitlements changes.
- **Keeping the macOS loop short:** (a) one PR job
  (`macos-build-test`) that does Rust-test → `xtask ffi` → xcodegen →
  `xcodebuild test` in sequence, fail-fast; (b) UI tests + `xcode-27`
  canary leg + release-candidate packaging on a nightly schedule; (c)
  keep generated bindings committed so a Rust-side API change shows its
  Swift diff in the PR itself — the owner can see breakage risk without
  running Xcode; (d) `paths:` filters so docs-only PRs skip the macOS
  job entirely.
- Agents **can** edit `project.yml`, workflows, Package.swift — text
  formats, reviewable diffs. They cannot *verify* them locally; the
  macOS job must therefore be the cheapest thing that still catches a
  broken project file (xcodegen validation is cheap; a full
  archive/sign is not).

## 3. Options compared

**macOS runner choice**

| | `macos-26` | `xcode-27` | `macos-26-xlarge` | `macos-26-intel` |
|---|---|---|---|---|
| Cost | free public / $0.062 | free public / $0.062 | $0.102 always | $0.077 always |
| Available to personal repo | yes | yes (preview) | no (orgs only) | no (orgs only) |
| macOS / Xcode | 26.6 / 26.6 | 27.0.1 / 27.0 GM + betas | 26 / 26.x | 26 / 26.x |
| Use for | all PR jobs | nightly canary leg now, primary later | — | — |

**Project definition**

| | pbxproj in git | XcodeGen | Tuist | `.xcproj` (Xcode ≥27.2) |
|---|---|---|---|---|
| Agent-editable | poor | good (YAML) | good (Swift) | good (JSON5) |
| Merge conflicts | frequent | none (generated) | none | rare by design |
| Tooling cost | none | `brew install xcodegen` | heavy | Xcode 27.2 beta today |
| Verdict | no | **yes, now** | later if modularizing | revisit when stable |

**Rust caching**

| | Swatinem/rust-cache | sccache |
|---|---|---|
| Cache unit | whole `target/`+registry | per-compilation-unit |
| Lockfile bump | full bust | partial bust |
| Also caches C deps | no | yes |
| Complexity | one line | RUSTC_WRAPPER + action |
| Verdict | fallback | **start here** |

**Signing**

| | Ad-hoc (now) | Developer ID + notarized (release) |
|---|---|---|
| Secrets | none | 7 (table §2.4) |
| Owner's Mac run | `xattr -dr com.apple.quarantine` | double-click |
| Extensions/entitlements | limited | full |
| When | every PR optionally | tag builds, once Developer ID exists |

## 4. Recommendations for Sweetbriar

**CI matrix**

| Workflow | Trigger | Runner | Jobs |
|---|---|---|---|
| `ci.yml` | PR + push to main | `ubuntu-26.04` | fmt, clippy `-D warnings`, `cargo test`, doc, arch-check, cargo-deny (advisories continue-on-error per the cargo-deny recommended pipeline), xtask unit tests |
| `macos.yml` | PR + push to main (with `paths:` filter excluding docs-only) | `macos-26` | `cargo test --target aarch64-apple-darwin` → `xtask ffi` (bindings + XCFramework) → `xtask check-ffi` (drift) → `xcodegen` → `xcodebuild build` + `test` (unit), strict-concurrency leg, `.xcresult` on failure |
| `macos-nightly.yml` | schedule + dispatch | `macos-26` + `xcode-27` matrix | UI tests (XCUITest), perf baselines, unsigned `.dmg` artifact, Swift 6 strict-concurrency error-gate, image/Xcode-beta canary (advisory) |
| `release.yml` | tag `v*` + dispatch | `macos-26` | unsigned `.dmg` first; flip to Developer ID → notarize → staple → Sparkle appcast once secrets exist |
| `gtk.yml` (later) | PR when `apps/linux` exists | `ubuntu-26.04` | apt GTK stack, `cargo build -p sweetbriar-gtk`, clippy |

**Bootstrap order (first workflow files)**

1. `rust-toolchain.toml`, workspace skeleton, `xtask ci`, `ci.yml`
   (Linux), `deny.toml`, arch-check script, `AGENTS.md` with the
   Linux/macOS check split. *This is the agent loop; everything else is
   additive.*
2. `apps/macos` skeleton + `project.yml` + `xtask ffi` stub (bindings
   only; XCFramework assembly follows once `sb-ffi` exports exist) +
   `macos.yml` PR job. Proves the whole toolchain before real code
   depends on it.
3. Nightly `macos-nightly.yml` (UI-test placeholder + `xcode-27` leg) —
   learns flakiness before it matters.
4. `release.yml` in the **unsigned** shape (tag → ad-hoc `.dmg` →
   release asset). The owner gets installable builds from day one.
5. When Developer ID enrollment is real: secrets in place → signed +
   notarized + stapled release → Sparkle EdDSA + appcast (a separate
   `updates` repo or the same repo's `gh-pages`, whichever the
   distribution plan picks) → then the "prototype #4" signed-build
   checklist from 08-macos-ui.
6. GTK leg last, when `apps/linux` lands.

**Costs.** On a **public** repo: $0 for all of the above (standard
runners). On a **private** repo: Linux is noise; the macOS PR job at
~12 min ≈ $0.75/run over quota — realistically $5–20/month at agent
PR volume, plus needing Pro for branch protection anyway. If the
project will be open-source eventually (D18 pending), making the repo
public sooner is the single biggest CI-cost lever.

**Strongest counter-arguments.**

- *"Don't burn a macOS runner per PR — make the macOS job nightly or
  path-gated."* Fair while there's no Swift to compile; but the moment
  `sb-ffi` or `project.yml` can break, discovering it nightly multiplies
  the debugging loop by a day. Compromise: keep the PR job but let it
  run only when paths under `crates/`, `xtask/`, `apps/` change.
- *"Skip the XCFramework; link the staticlib straight into Xcode."*
  Simpler until you have extensions, an agent host, and a second app —
  each of which wants the same library packaged once, identically. The
  XCFramework is the format that survives those additions without
  rework; it's also what the proven exemplars (Firefox iOS, Element X)
  converged on.
- *"Pay for `macos-26-xlarge` instead of tuning for the 3-core."* You
  can't — larger runners are org-only on a personal account. If CI pain
  becomes real, moving the repo into a free org unlocks them (but they
  bill even on public repos).
- *"Adopt `xcode-27` as primary now to match the owner's Mac."* The
  deployment floor is macOS 26 — building on 26.6/Xcode 26.6 validates
  the floor; `xcode-27` is a preview image that itself warns of
  instability and queueing. Canary leg now, promote when it goes GA.

## 5. Risks and open questions

- **`xcode-27` is a preview**: image churn, capacity warnings, and an
  Xcode 27.2 *beta* for the `.xcproj` format. Don't pin product
  decisions to it before GA.
- **macOS image regressions**: repeated real incidents of 1.5–2×
  slowdowns after image bumps (runner-images #9751, #11509, #10098).
  Log image version; be ready to pin `DEVELOPER_DIR` and report
  upstream.
- **UniFFI 0.32.2 template gap** (Swift 6 async foreign-traits, PR
  #2943 fix not in 0.32.2 per 01-architecture): decide between patching
  the template, an audited commit, avoiding the shape, or waiting for
  0.33 — *first* strict-compile experiment decides.
- **Entitlement ceiling under ad-hoc signing**: keychain-access-groups,
  app groups, some App Intents/extension features need profiles → they
  set the real deadline for Developer ID, not the release date.
- **Private-repo quota math**: ~200 effective macOS minutes/month on
  Free — easy to exceed with a per-PR macOS job plus nightlies. Decide
  public-vs-private early (ties to D18 licensing).
- **Apple Developer Program** ($99/yr) + App Store Connect API key
  provisioning are owner tasks with human latency — schedule ahead of
  the signed-build milestone.
- **Open questions:** commit generated UniFFI bindings? (recommended
  yes, for drift + review). Universal binary ever? (only if the
  support policy targets Intel — costs a cross-compiled slice CI can't
  test). Where does the appcast live? (separate public repo vs
  gh-pages). Merge queue later if agent PR volume grows.

## 6. Sources

Runners, images, billing:

- <https://github.com/actions/runner-images> (Available Images table)
- <https://github.com/actions/runner-images/blob/main/images/macos/macos-26-arm64-Readme.md>
- <https://github.com/actions/runner-images/blob/main/images/macos/xcode-27-arm64-Readme.md>
- <https://github.blog/changelog/2026-09-10-xcode-27-runner-image-now-runs-on-macos-27/>
- <https://github.com/actions/runner-images/issues/14404> (Xcode 27 preview)
- <https://github.com/actions/runner-images/issues/14747> (Ubuntu 26.04 GA)
- <https://github.com/actions/runner-images/issues/13008> (macOS 26 availability notes)
- <https://docs.github.com/en/actions/reference/runners/github-hosted-runners>
- <https://docs.github.com/en/actions/reference/runners/larger-runners>
- <https://docs.github.com/en/actions/reference/limits>
- <https://docs.github.com/en/billing/reference/actions-runner-pricing>
- <https://docs.github.com/en/billing/concepts/product-billing/github-actions>
- <https://github.com/resources/insights/2026-pricing-changes-for-github-actions>
- <https://github.com/actions/runner-images/discussions/9751>, <https://github.com/actions/runner-images/issues/11509>, <https://github.com/actions/runner-images/issues/10098> (runner perf regressions)

Rust → Swift:

- <https://crates.io/crates/uniffi>, <https://github.com/mozilla/uniffi-rs/blob/main/CHANGELOG.md>
- <https://mozilla.github.io/uniffi-rs/next/swift/uniffi-bindgen-swift.html>
- <https://mozilla.github.io/uniffi-rs/latest/swift/module.html>, <https://mozilla.github.io/uniffi-rs/latest/swift/xcode.html>
- <https://docs.rs/crate/cargo-swift/latest>, <https://github.com/antoniusnaumann/cargo-swift>
- <https://mozilla.github.io/application-services/book/design/swift-package-manager.html>
- <https://github.com/mozilla/application-services/tree/main/megazords/ios-rust>
- <https://github.com/Swatinem/rust-cache>, <https://github.com/mozilla/sccache>
- <https://github.com/EmbarkStudios/cargo-deny-action>, <https://embarkstudios.github.io/cargo-deny/checks/cfg.html>

Xcode, Swift, testing, project formats:

- <https://forums.swift.org/t/announcing-swift-6-3-3/87888> (Xcode 26.6 = Swift 6.3.3)
- <https://macosdb.com/xcode/release/26.6-17F113/>
- <https://developer.apple.com/news/releases/?id=06252026a>
- <https://tuist.dev/changelog/2026.09.17-xcode-27-project-format>
- <https://www.macobserver.com/news/xcode-27-2-beta-json-project-format-git-ai-tools/>
- <https://tuist.dev/en/docs/guides/features/projects/adoption/migrate/xcodegen-project>, <https://tuist.dev/blog/2025/03/21/git-conflicts>
- <https://mrozek.dev/posts/ios-tooling-for-coding-agents>
- <https://wwdc.ai/2026/267> (Swift Testing/XCTest interop, Xcode 27)
- <https://julianastrada.com/blog/swift-testing-vs-xctest>, <https://muhittincamdali.com/en/comparisons/swift-testing-vs-xctest/>
- <https://www.simplified.guide/xcuitest/ci-run-github-actions>
- <https://github.com/actions/runner-images/issues/5410> (automation mode fix)
- <https://github.com/johnny4young/vitrine/commit/dd77515f57a55b5a68b3f67373142f04386fb545> (empirical UI-test verification)
- Real CI references: <https://github.com/blaineam/Blip/blob/main/.github/workflows/ci.yml>, <https://github.com/joaquinbejar/IronTV/blob/67d51a06f23388ff3cf21fee5393f14a1aed33fb/.github/workflows/ci.yml>, <https://github.com/rkaregaran/MarkdownQuickLook/blob/ae10d2d64e2eec179a6e3cd2c3f37b9a54552265/.github/workflows/release.yml>, <https://github.com/mdsaifurrahmann/OpCode/blob/main/.github/workflows/ci.yml>

Signing, notarization, Sparkle:

- <https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution.md>
- <https://developer.apple.com/documentation/security/customizing-the-notarization-workflow>
- <https://apple-docs.everest.mt/docs/technotes/tn3147-migrating-to-the-latest-notarization-tool/>
- <https://keith.github.io/xcode-man-pages/notarytool.1.html>
- <https://tenki.cloud/blog/sign-macos-ios-builds-github-actions>
- <https://github.com/FarzamHejaziK/AnswerCue/blob/62f20d5915c2d84c0f79a1bf5fe9a4b852ea3fc3/.github/workflows/release-macos.yml>
- <https://github.com/alielsokary/CaskHub/blob/981dfe456c9617ac6ac513d32a7056565ad22f64/.github/workflows/release.yml>
- <https://sparkle-project.org/documentation/publishing/>, <https://github.com/sparkle-project/Sparkle/releases> (2.10.0), <https://sparkle-project.github.io/documentation/upgrading/>
- <https://gist.github.com/rampatra/08c6e895e051f35c9cb05f3789f243e0> (Sparkle+notarize+appcast workflow)
- <https://developer.apple.com/forums/thread/703059>, <https://developer.apple.com/forums/thread/722039> (ad-hoc signing / Gatekeeper)
- <https://github.com/perez987/Xattr-Editor/blob/master/DOCS/App-damaged.md>

Linux + misc:

- <https://www.ubuntuupdates.org/package/core/noble/main/updates/libgtk-4-dev>, <https://www.ubuntuupdates.org/package/core/noble/universe/updates/libwebkitgtk-6.0-dev>
- <https://github.com/syrtcevvi/gtk4_webkitgtk_getting_started>
- <https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-protected-branches/about-protected-branches>
- <https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-rulesets/available-rules-for-rulesets>

Internal: `research/01-architecture.md`, `research/08-macos-ui.md`,
`research/09-linux.md`, `research/notes/decision-agenda.md` (D12),
L-K-M project conventions (CI hardening trio, release workflow shape).
