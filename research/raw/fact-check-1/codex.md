<!-- provenance: helper=codex model=gpt-6.1-sol started=2026-10-11T20:41:27+00:00 exit=0 brief=research/briefs/ -->

# Sweetbriar: fact check of recent claims

Research cutoff and check date: **2026-10-11 (UTC)**.

Apple identifies **macOS 27 Golden Gate** as the current major release and lists Apple silicon models as compatible. Its compatibility page is dated **2026-09-14**. [Apple compatibility](https://support.apple.com/en-us/127255).

All 18 claims were checked against vendor documentation, upstream repositories, release artifacts, registries, or archived vendor pages. **Confirmed** means the primary source supports the stated fact; it does not mean the software was independently tested. Publication dates, announced rollout dates, and observed behavior are distinguished below. Recommendations are engineering judgments.

The snapshot in `./research/` was searched with `rg`. Document locations identify proposed corrections; those files were not edited. Downloaded evidence and fetch metadata are retained in `./checks/`.

## 1. TL;DR

- **UniFFI is the important false claim:** current release **0.32.2**, published September 23; July's merged async Swift concurrency fix is absent from its templates. [Registry](https://crates.io/api/v1/crates/uniffi), [released template](https://raw.githubusercontent.com/mozilla/uniffi-rs/v0.32.2/uniffi_bindgen/src/bindings/swift/templates/CallbackInterfaceImpl.swift).
- **BoltFFI and Crux's migration are real.** Crux's June 8 core release adds effect routing and replaces shell binding tooling; BoltFFI's speed comparisons are publisher benchmarks. [Crux 0.19](https://github.com/redbadger/crux/releases/tag/crux_core-v0.19.0), [benchmark methodology](https://raw.githubusercontent.com/boltffi/boltffi/main/benchmarks/README.md).
- **The client release claims check out:** Mailspring 1.26 changes folder membership; eM Client 11 and Thunderbird 154 announce Graph support. Feature scope still matters. [Mailspring](https://github.com/Foundry376/Mailspring/releases/tag/1.26.0), [eM Client](https://www.emclient.com/blogpost.aspx?id=842), [Thunderbird](https://www.thunderbird.net/en-US/thunderbird/154.0/releasenotes/).
- **Notion announced its inbox's September 22 shutdown months earlier.** Gmail history survives; client-only drafts and settings required separate preservation. [Archived June 25 notice](https://web.archive.org/web/20260625175000id_/https://www.notion.com/help/notion-mail-inbox-is-going-away-what-to-do-next).
- **Proton's October 9 revision is real, but its automation clause predates it.** The observed change concerns service discontinuation, not a new Bridge-specific ban. [Current terms](https://proton.me/legal/terms), [October 3 archive](https://web.archive.org/web/20261003105539id_/https://proton.me/legal/terms).
- **Exchange policy and Apple's roadmap are distinct:** October 10 EWS allowlist enforcement initially concerns Microsoft's Worldwide cloud; Apple promises Graph in a future macOS 27 update. [Microsoft rollout](https://techcommunity.microsoft.com/blog/exchange/ews-deprecation-is-here-%E2%80%93-what-this-means-to-you/4561431), [Apple deployment guide](https://support.apple.com/guide/deployment/integrate-with-microsoft-exchange-dep158966b23/web).
- **Google confirms the Gmail changes and limits.** Consumer third-party “Send as” ends January 2027; new-project API quotas are lower, billing remains announced, and Workspace IMAP has bandwidth limits. [Send-as notice](https://support.google.com/mail/answer/17101213?hl=en), [API quotas](https://developers.google.com/workspace/gmail/api/reference/quota), [IMAP limits](https://knowledge.workspace.google.com/admin/gmail/gmail-bandwidth-limits?hl=en).
- **Apple's new model APIs are documented; 4,096 tokens is the on-device budget.** macOS 27 also denies cross-developer container access by default. [Foundation Models updates](https://developer.apple.com/documentation/updates/foundationmodels), [TN3193](https://developer.apple.com/documentation/technotes/tn3193-managing-the-on-device-foundation-model-s-context-window), [OS release notes](https://developer.apple.com/documentation/macos-release-notes/macos-27-release-notes).
- **Swift-callable Fluent exists, with maturity questions; Akonadi's SQLite default arrived earlier than August**, in 26.04.0. [Swift wrapper](https://raw.githubusercontent.com/jcollie/zig-fluent/main/examples/swift/Sources/FluentKit/Fluent.swift), [Akonadi 26.04.0](https://invent.kde.org/pim/akonadi/-/raw/v26.04.0/CMakeLists.txt).

## 2. Findings

### 1. UniFFI release and Swift 6 async callbacks

**Verdict: false. Checked: 2026-10-11.**

The crates.io registry reports **0.32.2** as the current non-yanked release, published **2026-09-23 UTC**. The release-tag changelog dates it **2026-09-22**. The default-branch changelog still ends at 0.32.1, so it cannot establish that version as current. [Registry](https://crates.io/api/v1/crates/uniffi), [0.32.2 changelog](https://github.com/mozilla/uniffi-rs/blob/v0.32.2/CHANGELOG.md), [default-branch changelog](https://raw.githubusercontent.com/mozilla/uniffi-rs/main/CHANGELOG.md).

The specific async foreign-trait bug is issue **#2929**. PR **#2943**, merged **2026-07-15**, adds `@Sendable` closures and explicit handling of `RustBuffer` captures. However, comparing the tagged **0.32.2** `CallbackInterfaceImpl.swift` and `Async.swift` with the current branch shows that this fix is absent from the released templates. A merged fix is not a shipped fix. The separate 0.31.2 callback-table warning fix must not be conflated with this async error. [Issue](https://github.com/mozilla/uniffi-rs/issues/2929), [PR](https://github.com/mozilla/uniffi-rs/pull/2943), [released callback template](https://raw.githubusercontent.com/mozilla/uniffi-rs/v0.32.2/uniffi_bindgen/src/bindings/swift/templates/CallbackInterfaceImpl.swift), [released async template](https://raw.githubusercontent.com/mozilla/uniffi-rs/v0.32.2/uniffi_bindgen/src/bindings/swift/templates/Async.swift).

**Correct statement:** “UniFFI 0.32.2 is current; the Swift 6 async foreign-trait fix merged upstream in July 2026 but is not present in that release’s templates.” Compile Sweetbriar’s actual bindings before selecting a release, audited commit, or local patch.

**Curated documents that should change:** [research/01-architecture.md:185](codex-artifacts/research/01-architecture.md:185) should distinguish the current release from a selected pin and explicitly identify the fix as unreleased. [research/notes/working-notes.md:768](codex-artifacts/research/notes/working-notes.md:768)–771 should resolve its “verify in the pinned version” question to “absent.” These documents correctly say the fix **merged**; the stronger claim that it **shipped** is false. Review the deliberate 0.32.1 pin in `research/debate/02-claude-response.md:100`; a tested pin can remain a valid choice.

### 2. BoltFFI and published UniFFI benchmarks

**Verdict: confirmed. Checked: 2026-10-11.**

BoltFFI is a real Rust binding generator documenting Swift and Kotlin support, including Swift async/await and Kotlin coroutines. Its repository was created **2025-12-23**, according to GitHub's metadata. [Upstream repository](https://github.com/boltffi/boltffi), [repository metadata](https://api.github.com/repos/boltffi/boltffi).

It publishes benchmark code and comparisons with UniFFI. The benchmark README describes a shared Rust demo crate, platform harnesses, and release runs published to a dashboard. These are **publisher benchmarks**; their existence is confirmed, their applicability to Sweetbriar is unverified. [Benchmark methodology](https://raw.githubusercontent.com/boltffi/boltffi/main/benchmarks/README.md).

**Correct statement:** “BoltFFI supports Rust-to-Swift/Kotlin bindings and publishes UniFFI comparisons; measure the chosen versions on Sweetbriar's workload before claiming a speed advantage.” The current README's headline table differs from the curated **1.4×–13.7×** range; this report could not establish the exact historical run behind that range. [Current publisher table](https://github.com/boltffi/boltffi).

**Curated locations:** `research/01-architecture.md:186`, `research/notes/working-notes.md:186`. Preserve existence; attach a dated run and hardware/toolchain to any performance number, or remove the unsupported range.

### 3. Crux 0.19 and its core/shell boundary

**Verdict: confirmed. Checked: 2026-10-11.**

The relevant package is **`crux_core` 0.19.0**, released **2026-06-08**. Its release introduces `EffectRouter`, replacing effect middleware with routing by effect type. The three lanes are `Serialized` for the existing serialization bridge, `Parked` for requests represented by opaque handles, and `Buffer` for handling in process or tests. Follow-up effects use the same routing policy. [Crux release notes](https://github.com/redbadger/crux/releases/tag/crux_core-v0.19.0).

The same release migrates examples and shell templates from UniFFI to BoltFFI, removes `crux_cli` and rustdoc-based generation, and retains separate `facet_typegen` generation for application types. [Crux release notes](https://github.com/redbadger/crux/releases/tag/crux_core-v0.19.0).

**Correct statement:** “Crux's June 2026 core release adds effect routing and changes its supported shell tooling to BoltFFI plus facet type generation.”

**Curated locations:** `research/01-architecture.md:186`, `research/01-architecture.md:189`, `research/notes/working-notes.md:187`. Their basic migration claim is supported; specify `crux_core` to avoid confusing independently versioned Crux crates.

### 4. Mailspring 1.26.0 and multiple folders per message

**Verdict: confirmed. Checked: 2026-10-11.**

The release notes explicitly describe replacing the one-folder model with tracking every folder containing a message. They name iCloud, messages addressed to oneself appearing in Inbox and Sent, Exchange's duplicate Sent behavior, and Proton Mail labels. They also document CI against Dovecot, Cyrus, and fake servers reproducing problematic behavior. [Mailspring 1.26.0](https://github.com/Foundry376/Mailspring/releases/tag/1.26.0).

There are two dates on the primary artifact: the notes heading says **2026-10-01**, while GitHub records publication on **2026-10-02**. The curated October 2 date is therefore defensible as the publication date. [Mailspring release](https://github.com/Foundry376/Mailspring/releases/tag/1.26.0).

**Correct statement:** “Mailspring 1.26.0, published October 2, changes mailsync to track multiple folder memberships per message.” This is evidence for preserving memberships, not evidence that every matching `Message-ID` can safely be collapsed into one record; that latter conclusion would need its own proof.

**Curated locations:** `research/09-linux.md:53`, `research/notes/working-notes.md:20`. No factual reversal is required; identify which release date is being used.

### 5. eM Client 11's release and Microsoft Graph

**Verdict: confirmed. Checked: 2026-10-11.**

eM Client's official release announcement is dated **2026-10-07** and says version 11 is available. It identifies Microsoft Graph support for Microsoft 365 and migration of existing accounts from EWS during the update. [Vendor release announcement](https://www.emclient.com/blogpost.aspx?id=842).

**Correct statement:** “eM Client announced the release of version 11 on October 7, 2026, including Microsoft Graph support for Microsoft 365.” This verifies the vendor's release and feature announcement; it is not an independent test of every Graph operation, tenant policy, or platform build.

**Curated locations:** `research/06-account-backends.md:22`, `research/10-landscape.md:23`, `research/10-landscape.md:52`. Keep the claim and cite the vendor rather than a review or search summary.

### 6. Notion Mail's September 22 shutdown

**Verdict: confirmed as an announced shutdown. Checked: 2026-10-11.**

Notion's official shutdown notice schedules the inbox's closure for **2026-09-22**, with September 21 as the last day to save its own data. The notice is preserved in a **2026-06-25** archive, proving that September 22 was not the announcement date. Its live URL now redirects to the help index. [Archived primary notice](https://web.archive.org/web/20260625175000id_/https://www.notion.com/help/notion-mail-inbox-is-going-away-what-to-do-next).

The notice distinguishes Gmail's email history from Notion-only drafts, scheduled messages, snippets, and organization. It also says Gmail AI connectors and agent email tools continue. [Archived notice](https://web.archive.org/web/20260625175000id_/https://www.notion.com/help/notion-mail-inbox-is-going-away-what-to-do-next).

**Correct statement:** “Notion announced closure of the Notion Mail inbox for September 22, 2026; separate Notion email connectors continue.” Runtime shutdown was not independently tested.

**Curated locations:** `research/10-landscape.md:27`, `research/10-landscape.md:60`, `research/10-landscape.md:117`. Preserve the discontinued-client finding, while separating inbox retirement from continued email integrations.

### 7. Thunderbird 154 and Exchange Online Graph support

**Verdict: confirmed. Checked: 2026-10-11.**

Thunderbird's **154.0** release notes give **2026-08-18** as the release date and explicitly enable Microsoft Graph for Microsoft 365. [154.0 release notes](https://www.thunderbird.net/en-US/thunderbird/154.0/releasenotes/).

The project's **2026-09-02** announcement adds necessary scope: Graph first shipped in the Release channel's version 154; ESR 153 did not include it. Support covered **email**, while calendars and address books were not yet supported at the announcement. On-premises Exchange remained an EWS use case. [Thunderbird Graph announcement](https://blog.thunderbird.net/2026/09/thunderbird-desktop-new-protocol-support-microsoft-graph-api/).

**Correct statement:** “Thunderbird 154.0 shipped Graph email support for Microsoft 365 on August 18, 2026, in the Release channel.” Do not infer ESR parity or a complete Exchange PIM replacement from that statement.

**Curated locations:** `research/06-account-backends.md:22`, `research/09-linux.md:42`, `research/10-landscape.md:21`, `research/notes/working-notes.md:907`. Dates and basic support are supported; attach the channel and feature-scope qualifications.

### 8. Proton's October 9 terms revision

**Verdict: partly true. Checked: 2026-10-11.**

Proton's current terms display **last modified 2026-10-09**. Comparing them with the **2026-10-03** archive, whose displayed revision is June 23, identifies a replacement in section 4: the new paragraph reserves the ability to modify, suspend, or discontinue services or features, with qualified advance-notice language. That is a general service-continuity provision, not a Bridge-specific restriction. [Current terms](https://proton.me/legal/terms), [archived primary terms](https://web.archive.org/web/20261003105539id_/https://proton.me/legal/terms).

The automation restriction and its exception for traffic matching normal human-client behavior are **unchanged in that comparison**. A claim that October 9 introduced those restrictions would be false. Proton still documents Bridge as an IMAP/SMTP route for third-party desktop clients on qualifying paid plans. [Archived terms](https://web.archive.org/web/20261003105539id_/https://proton.me/legal/terms), [Bridge](https://proton.me/mail/bridge).

**Correct statement:** “Proton revised its terms on October 9, adding general discontinuation language; the automation provisions relevant to unofficial clients already existed. No new Bridge-specific prohibition was verified.”

**Curated location:** `research/06-account-backends.md:149`. Its wording about the currently dated terms is supportable. Add the historical qualification; do not portray the automation clause as newly introduced on October 9.

### 9. DavMail 7.0.0 and Graph

**Verdict: confirmed. Checked: 2026-10-11.**

DavMail's maintainer release announcement is dated **2026-09-19**. Version **7.0.0** focuses on Graph stability and performance, including folder delta synchronization with caching. This directly verifies a 2026 release with Graph support. [Maintainer announcement](https://sourceforge.net/p/davmail/news/2026/09/davmail-700-released/).

“Adds Graph” should not be interpreted as the first implementation. Upstream release notes identify **6.8.0, 2026-06-12**, as the first version with active Graph support available in the GUI, and document experimental work before that. [Upstream release history](https://raw.githubusercontent.com/mguessan/davmail/master/RELEASE-NOTES.md).

**Correct statement:** “DavMail 7.0.0, released September 19, 2026, strengthens its existing Graph backend.” Its gateway architecture is a separate product choice from implementing a native Graph account backend in Sweetbriar.

**Curated location to tighten:** `research/06-account-backends.md:56`. Replace “7.0.0 adds Graph” with the more precise release description above.

### 10. Foundation Models APIs and TN3193

**Verdict: confirmed. Checked: 2026-10-11.**

Apple documents `LanguageModel: Sendable` and `PrivateCloudComputeLanguageModel` with macOS **27.0** availability. The June 2026 framework updates describe adopting the protocol for local or server models. This establishes the API claim directly. [LanguageModel](https://developer.apple.com/documentation/foundationmodels/languagemodel), [PrivateCloudComputeLanguageModel](https://developer.apple.com/documentation/foundationmodels/privatecloudcomputelanguagemodel), [Framework updates](https://developer.apple.com/documentation/updates/foundationmodels).

TN3193 explicitly records a **2026-03-31** revision for APIs introduced in 26.4 and a **4,096-token context per on-device session**. Instructions, prompts, tool schemas and results, generated-type schemas, and model responses all count toward that total. It does not establish the same limit for PCC or arbitrary protocol implementations. [TN3193](https://developer.apple.com/documentation/technotes/tn3193-managing-the-on-device-foundation-model-s-context-window).

**Suggested wording:** “macOS 27 generalizes Foundation Models beyond on-device execution. Select the execution provider explicitly; TN3193 documents a 4,096-token on-device session budget, and each selected model’s limits must be checked.” PCC requires a managed entitlement and developer eligibility; API existence alone does not establish availability for Sweetbriar’s chosen distribution. [PCC access requirements](https://developer.apple.com/private-cloud-compute/).

**Curated locations:** `research/10-landscape.md:130`, `research/08-macos-ui.md:161`. These already qualify the budget as on-device; preserve that wording wherever it is summarized.

### 11. Apple’s Exchange-to-Graph commitment

**Verdict: confirmed. Checked: 2026-10-11.**

Apple’s deployment guide, displaying **2026-04-14** as its publication date, states that Apple is working with Microsoft to migrate Mail, Calendar, Contacts, Notes, and Reminders to Graph in a **future macOS 27 update**, including declarative device-management configuration. It provides no delivery version or date. The page’s displayed publication date does not independently establish when that paragraph was added. [Apple deployment guide](https://support.apple.com/guide/deployment/integrate-with-microsoft-exchange-dep158966b23/web).

**Correct statement:** “Apple publicly plans Graph support in a future macOS 27 update.” Do not upgrade this to a claim that Graph already shipped in 27.0 or that a particular point release is guaranteed.

**Curated locations:** `research/06-account-backends.md:19`, `research/10-landscape.md:20`, `research/10-landscape.md:182`. The first two correctly describe a promise; the last should remain conditional.

### 12. Exchange Online’s mandatory EWS application allowlist

**Verdict: partly true. Checked: 2026-10-11.**

The Exchange Team’s **2026-10-01** post, updated **2026-10-02**, says enforcement starts **2026-10-10**: keeping `EWSEnabled=True` requires an `EWSAllowedAppIDs` list. The initial scope is the **Worldwide multi-tenant cloud**; other clouds receive separate timelines. The claim is correct for that scope but too broad if read as every Exchange Online environment. [Microsoft Exchange Team](https://techcommunity.microsoft.com/blog/exchange/ews-deprecation-is-here-%E2%80%93-what-this-means-to-you/4561431).

Microsoft records eligible tenants on October 2 and populates their lists on October 8–9 using 60 days of observed activity. Tenants enabling EWS after that snapshot must populate the list themselves. “Starting October 10” is rollout language, not independently observed enforcement in every tenant. [Same primary announcement](https://techcommunity.microsoft.com/blog/exchange/ews-deprecation-is-here-%E2%80%93-what-this-means-to-you/4561431).

**Correct statement:** “From the October 10 rollout, Worldwide Exchange Online tenants retaining or re-enabling EWS need their applications in `EWSAllowedAppIDs`; other clouds have separate schedules.” Public Microsoft documentation directly verifies the policy without relying on Message Center mirrors. [EWS access-control documentation](https://learn.microsoft.com/en-us/exchange/client-developer/exchange-web-services/how-to-control-access-to-ews-in-exchange).

**Curated locations to qualify:** `research/debate/01-review-devin.md:210` and `research/debate/01-review-devin.md:311`. Add cloud scope and distinguish phased retirement from a universal October 1 shutdown. The latter passage's conclusion that no Exchange Online EWS fallback remains is too strong: appropriately allowlisted applications can still use the temporary window. The original MC1227454/MC1485116 notices were not accessed in an authenticated tenant; the public primary sources verify the policy itself.

### 13. Consumer Gmail’s third-party “Send as” retirement

**Verdict: confirmed. Checked: 2026-10-11.**

Google’s official help explicitly schedules removal in **January 2027** from Gmail on the web and Gmail mobile apps, while exempting Gmail-to-Gmail identities and Google Workspace “Send as.” Its timeline places the announcement in **Q3 2026** and allows restrictions on new configurations during Q3–Q4. No exact January day is supplied. [Google’s transition notice](https://support.google.com/mail/answer/17101213?hl=en), [Send-as help](https://support.google.com/mail/answer/22370?hl=en).

**Correct statement:** “Consumer Gmail will remove third-party-address ‘Send as’ in January 2027; Workspace ‘Send as’ and Google-hosted aliases remain.” This does not retire external clients’ access to Gmail through IMAP, POP, or Gmail API, and does not eliminate adding third-party accounts separately in the Gmail mobile app. [Google’s transition notice](https://support.google.com/mail/answer/17101213?hl=en).

**Curated location:** `research/notes/working-notes.md:910`. Replace the secondary Android Authority citation with Google’s notice, preserving the scope qualification.

### 14. Gmail API quotas and announced billing

**Verdict: confirmed. Checked: 2026-10-11.**

Google’s quota page, updated **2026-09-10**, applies the new policy to projects created on or after **2026-05-01**. Projects using the API during **November 2025–April 2026** retain previously assigned quotas. It specifies **6,000 units/user/project/minute**, **1,200,000 units/project/minute**, **20 units for `messages.get`**, and an announced **80,000,000 units/project/day** billing threshold. [Gmail API quota policy](https://developers.google.com/workspace/gmail/api/reference/quota).

**Billing qualification:** Google still describes details as forthcoming with at least **90 days’ notice**. These sources establish an announced threshold, not a verified current price or charging start date. [Standardized API model, updated 2026-09-03](https://developers.google.com/workspace/tools-safety).

**Derived implication:** at 20 units per fetch and 6,000 per minute, one user can perform at most **300 `messages.get` requests/minute**. One million such fetches require about **55.6 hours of quota time**, excluding listing, retries, and other calls. This conditional arithmetic does not establish the fastest import strategy or a wall-clock guarantee. [Inputs](https://developers.google.com/workspace/gmail/api/reference/quota).

**Curated locations:** `research/06-account-backends.md:114`, `research/notes/working-notes.md:622`, `research/debate/01-review-codex.md:132`. Keep the numbers and conditional calculation; preserve the pending-billing qualification.

### 15. Workspace IMAP bandwidth

**Verdict: confirmed. Checked: 2026-10-11.**

Google’s Workspace help, updated **2026-10-09**, lists **2,500 MB/day IMAP download** and **500 MB/day IMAP upload** for all Workspace editions, and says limits can change without notice. These are bandwidth limits, separate from API quota units and SMTP sending limits. [Workspace bandwidth policy](https://knowledge.workspace.google.com/admin/gmail/gmail-bandwidth-limits?hl=en).

**Correct statement:** “Workspace currently publishes per-day IMAP allowances of 2,500 MB down and 500 MB up.” Other clients' usage must be included in migration planning. [Workspace bandwidth policy](https://knowledge.workspace.google.com/admin/gmail/gmail-bandwidth-limits?hl=en).

**Curated locations:** `research/debate/01-review-codex.md:134`, `research/debate/02-claude-response.md:186`. Those numbers are supported; IMAP should not be assumed universally faster than API import.

### 16. Project Fluent support callable from Swift

**Verdict: confirmed for a Swift-callable runtime; production suitability unverified. Checked: 2026-10-11.**

Project Fluent lists official JavaScript, Python, and Rust implementations; its official/community list has no Swift entry. The Rust implementation remains active, with a commit dated **2026-09-27**; crates.io reports **`fluent-bundle` 0.16.0**, published **2025-05-22**. A Rust implementation can be wrapped for Sweetbriar's Swift shell, but that adapter is engineering work, not an existing official Swift runtime. [Implementation list](https://github.com/projectfluent/fluent), [Rust activity](https://api.github.com/repos/projectfluent/fluent-rs/commits?per_page=3), [crate registry](https://crates.io/api/v1/crates/fluent-bundle).

A concrete community candidate also exists: **`jcollie/zig-fluent`** publishes a C library, a SwiftUI example wrapping its C API, and `FluentKitTests`; the README describes CI builds on macOS. Its public GitHub repository was created **2026-09-10** and has commits through **2026-10-07**. This is positive evidence of a maintained Swift-callable implementation, although its short history warrants caution. [Project README](https://tangled.org/jcollie.dev/zig-fluent/raw/main/README.md), [repository metadata](https://api.github.com/repos/jcollie/zig-fluent), [recent commits](https://api.github.com/repos/jcollie/zig-fluent/commits?per_page=5).

The actual Swift code exposes `FluentBundle`, calls `fluent_bundle_format`, and frees handles in `deinit`; tests cover loading, fallback, plurals, attributes, and argument ownership. These files were inspected, not built on macOS. [Swift wrapper](https://raw.githubusercontent.com/jcollie/zig-fluent/main/examples/swift/Sources/FluentKit/Fluent.swift), [Swift tests](https://raw.githubusercontent.com/jcollie/zig-fluent/main/examples/swift/Tests/FluentKitTests/CatalogTests.swift).

**Correct statement:** “Swift can call a published community Fluent runtime through its C API and example wrappers. Sweetbriar can alternatively wrap maintained `fluent-rs` in its Rust core. An official Swift-native implementation was not verified.”

**Curated locations:** `research/09-linux.md:100`, `research/notes/working-notes.md:28`. Replace blanket uncertainty about existence with these concrete candidates, while retaining an integration/production-readiness gate. General lossless `.ftl` to `.xcstrings` conversion is **unverified**; Fluent's locale-specific selectors and terms need deliberate preservation. [Fluent language design](https://projectfluent.org/).

### 17. macOS 27 access to another app’s data

**Verdict: partly true. Checked: 2026-10-11.**

The macOS 27 release notes explicitly describe cross-developer app and app-group container access as denied by default, with permission managed through Privacy & Security settings rather than an automatic access prompt (issue **161835690**). They also describe additional XProtect restrictions on targeted app data (**178668601**). [macOS 27 release notes](https://developer.apple.com/documentation/macos-release-notes/macos-27-release-notes).

The claim needs a historical and filesystem qualification. Apple DTS identifies app-container protection from **macOS 14**, app-group protection from **15**, and Full Disk Access from **10.14**. It explicitly says selecting `~/Library` does not authorize reading `~/Library/Mail`, which is protected by mandatory access control (MAC). That path is protected Mail data, not simply a generic app sandbox container. [Apple DTS: filesystem permissions, revised 2026-09-18](https://developer.apple.com/forums/thread/678819).

**Correct statement:** “macOS 27 tightens existing cross-app data protection. Direct Apple Mail import requires an authorized access path and must handle denial; selecting a parent directory or leaving Sweetbriar unsandboxed does not grant access automatically.” Apple documents export to `.mbox` as a supported Mail operation. Prefer that user-mediated import route initially. [Apple Mail export](https://support.apple.com/guide/mail/import-or-export-mailboxes-mlhlp1030/mac).

**Curated locations to clarify:** `research/04-data-safety.md:121` and `research/04-data-safety.md:315`. Replace the unverified note with the confirmed default-denial behavior, and avoid suggesting that every file-picker selection bypasses MAC. Apple’s **2026-10-02** announcement separately promises further Full Disk Access controls; it does not specify their shipping date. [Full Disk Access announcement](https://developer.apple.com/news/?id=p6zjojqw).

### 18. Akonadi's default SQLite backend

**Verdict: partly true. Checked: 2026-10-11.**

Upstream **MR !311**, “Switch default database backend to SQLITE,” merged **2026-01-27**. It changes the CMake default when `DATABASE_BACKEND` is not explicitly supplied. [Upstream MR metadata](https://invent.kde.org/api/v4/projects/pim%2Fakonadi/merge_requests/311).

The change was already present in **Akonadi 26.04.0**, released with KDE Gear on **2026-04-16**, and remains present in 26.08.0. Thus “available in 26.08.x” is true, while “introduced in 26.08.x” is false. KDE's March 2 update specifically describes SQLite as the default for **new installations**, not automatic conversion of existing databases. [26.04.0 source](https://invent.kde.org/pim/akonadi/-/raw/v26.04.0/CMakeLists.txt), [26.08.0 source](https://invent.kde.org/pim/akonadi/-/raw/v26.08.0/CMakeLists.txt), [KDE Gear release](https://kde.org/announcements/gear/26.04.0/), [KDE March update](https://blogs.kde.org/2026/03/02/this-month-in-kde-apps/).

**Correct statement:** “Akonadi switched its default for new installations to SQLite in 26.04.0; distributions can choose their backend, and existing installations are a separate migration.”

**Curated locations:** `research/notes/working-notes.md:959` mixes the upstream change with Gentoo's 26.04.1 packaging date and should distinguish them. `research/raw/database/codex.md:107` correctly observes the 26.08.2 default; it does not claim that version introduced the change. `research/raw/database/devin.md:13` correctly places it in 26.04. No curated statement claiming an August introduction was located.

## 3. Options compared

The status column records evidence checked **2026-10-11**. Trade-offs and selection criteria are my recommendations, rather than vendor performance claims.

| Rust/native boundary option | Verified position | Sweetbriar trade-off |
|---|---|---|
| UniFFI 0.32.2 with a small Swift adapter | Released async foreign-trait templates lack the July fix. [Templates](https://raw.githubusercontent.com/mozilla/uniffi-rs/v0.32.2/uniffi_bindgen/src/bindings/swift/templates/CallbackInterfaceImpl.swift) | Reasonable starting candidate if the actual API compiles; otherwise audit a patch or avoid the affected callback shape. |
| BoltFFI | Swift/Kotlin generation and comparative harnesses exist. [Upstream](https://github.com/boltffi/boltffi), [harnesses](https://raw.githubusercontent.com/boltffi/boltffi/main/benchmarks/README.md) | Worth a bounded comparison. Select on correctness, ownership, cancellation, and measured UI workload, rather than the largest advertised speedup. |
| Crux's core/shell framework | June release changes routing and binding tooling. [Release](https://github.com/redbadger/crux/releases/tag/crux_core-v0.19.0) | Consider if its effect model fits the application; adopting a whole framework solely to obtain FFI adds unnecessary scope. |
| A small, manually maintained C ABI | Swift-to-C wrappers are demonstrated by the Fluent example. [Wrapper](https://raw.githubusercontent.com/jcollie/zig-fluent/main/examples/swift/Sources/FluentKit/Fluent.swift) | Offers direct control, but Sweetbriar owns memory, error, async, and compatibility conventions. Keep it as a focused alternative. |

| Account/import choice | Evidence boundary | Recommended response |
|---|---|---|
| Microsoft 365 through Graph | Shipping client announcements exist; Apple's implementation remains a promise. [Thunderbird](https://www.thunderbird.net/en-US/thunderbird/154.0/releasenotes/), [Apple](https://support.apple.com/guide/deployment/integrate-with-microsoft-exchange-dep158966b23/web) | Build and test a direct Graph backend rather than scheduling around Apple's unspecified update. |
| Exchange Online through EWS | Worldwide tenants need permitted application IDs; this differs from User-Agent policies. [Microsoft access controls](https://learn.microsoft.com/en-us/exchange/client-developer/exchange-web-services/how-to-control-access-to-ews-in-exchange) | Treat as a transitional path with explicit policy diagnostics. Keep on-premises requirements separate. |
| Proton through Bridge | Paid-plan local IMAP/SMTP access is documented. [Bridge](https://proton.me/mail/bridge) | Support the documented gateway first; assess unofficial API integration separately. |
| Gmail API versus IMAP | API request budgets and Workspace IMAP bandwidth are different constraints. [API quotas](https://developers.google.com/workspace/gmail/api/reference/quota), [IMAP bandwidth](https://knowledge.workspace.google.com/admin/gmail/gmail-bandwidth-limits?hl=en) | Measure initial archival sync and normal operation separately. Neither transport wins solely from one limit. |
| Apple Mail export versus direct database access | Mail exports `.mbox`; protected Mail data needs authorization. [Export](https://support.apple.com/guide/mail/import-or-export-mailboxes-mlhlp1030/mac), [Apple DTS](https://developer.apple.com/forums/thread/678819) | Ship verified export import first; prototype direct access under the actual signed app and helper configuration. |

## 4. Recommendations for Sweetbriar, trade-offs, and strongest counterarguments

**1. Keep the Rust/native boundary small, and make the Swift build the deciding evidence.** Correct the UniFFI version/fix statements before architecture approval. Test generated bindings for Sweetbriar's actual async foreign traits under strict Swift 6, then cancellation, reentrancy, and subscription delivery. A released version number alone does not resolve the reported generator error. [Issue #2929](https://github.com/mozilla/uniffi-rs/issues/2929).

My preference remains a narrow adapter over a demonstrated binding solution, with an audited pin. Compare BoltFFI if UniFFI needs a substantial workaround. **Strongest counterargument:** Crux has already adopted BoltFFI, so postponing evaluation could leave Sweetbriar maintaining avoidable glue. That supports an early comparison, not an unmeasured wholesale migration. [Crux migration](https://github.com/redbadger/crux/releases/tag/crux_core-v0.19.0).

**2. Prioritize Graph for Microsoft 365, while preserving deployment-specific alternatives.** Thunderbird and eM Client provide concrete evidence that Graph is shipping in competing clients. EWS access now depends on additional tenant policy. [eM Client release](https://www.emclient.com/blogpost.aspx?id=842), [Microsoft access controls](https://learn.microsoft.com/en-us/exchange/client-developer/exchange-web-services/how-to-control-access-to-ews-in-exchange).

Design migration to retain local drafts, queued sends, account settings, and archives before switching protocols. **Strongest counterargument:** a target customer may still need on-premises EWS or an authorized gateway. Graph priority should follow the target deployment, not erase those requirements. DavMail's existing Graph gateway is also a concrete interoperability option. [DavMail release](https://sourceforge.net/p/davmail/news/2026/09/davmail-700-released/).

**3. Make preservation cover client-owned state as well as server mail.** Notion's shutdown notice specifically separates preserved Gmail history from drafts, scheduled mail, snippets, and state kept only in Notion. Sweetbriar should make those objects independently exportable and recoverable. [Notion notice](https://web.archive.org/web/20260625175000id_/https://www.notion.com/help/notion-mail-inbox-is-going-away-what-to-do-next).

Model remote items and memberships separately from a local message/content record; keep provider identity and origin evidence when deduplicating. Mailspring's correction is a useful regression fixture for iCloud, Exchange Sent behavior, and Proton labels. **Strongest counterargument:** this model complicates move, delete, and reconciliation semantics. The remedy is explicit operations and fixtures, rather than premature merging based only on header similarity. [Mailspring release](https://github.com/Foundry376/Mailspring/releases/tag/1.26.0).

**4. Treat provider budgets as scheduler inputs.** Persist import progress, reserve interactive capacity, and account for shared bandwidth. Recheck assigned Gmail quotas and pending billing. **Strongest counterargument:** adaptive scheduling adds complexity. Start with bounded concurrency and durable checkpoints; expand after measurement. [Google billing policy](https://developers.google.com/workspace/tools-safety), [Workspace bandwidth](https://knowledge.workspace.google.com/admin/gmail/gmail-bandwidth-limits?hl=en).

**5. Make model execution explicit and optional.** Keep search, drafts, and account operation usable without Foundation Models. Select local or cloud execution deliberately and query the selected model's context/availability rather than applying 4,096 tokens universally. **Strongest counterargument:** PCC may enable useful operations beyond the on-device budget. That is a reason to design a provider boundary and validate eligibility, not assume every distribution can use it. [PCC API](https://developer.apple.com/documentation/foundationmodels/privatecloudcomputelanguagemodel), [PCC eligibility](https://developer.apple.com/private-cloud-compute/).

**6. Prototype Fluent through the shared Rust core before choosing a localization pipeline.** `fluent-bundle` gives a concrete engine to evaluate; the community Swift example shows that foreign-runtime integration is possible. Check locale fallback, plural selectors, attributes, bidirectional isolation, formatting, and ownership across the boundary. [Rust implementation](https://github.com/projectfluent/fluent-rs), [Swift tests](https://raw.githubusercontent.com/jcollie/zig-fluent/main/examples/swift/Tests/FluentKitTests/CatalogTests.swift).

**Strongest counterargument:** platform catalogs may fit native UI tooling more simply, while returning every localized string through Rust adds an integration to maintain. Choose a shared runtime only if its translation advantages justify that cost; do not assume unrestricted `.ftl` can be flattened without loss. [Fluent design](https://projectfluent.org/).

**7. Validate import authorization and storage safety independently.** Start Apple Mail import from a preserved `.mbox` export, producing an inventory and verification result. Test direct import separately under macOS 27. Akonadi's SQLite default is useful precedent, but it cannot prove Sweetbriar's crash recovery, backup, or migration correctness. [Mail export](https://support.apple.com/guide/mail/import-or-export-mailboxes-mlhlp1030/mac), [Akonadi default](https://invent.kde.org/pim/akonadi/-/raw/v26.04.0/CMakeLists.txt).

**Strongest counterargument:** an export-first workflow adds steps, and richer source metadata may motivate direct import. Validate metadata coverage and permissions with real fixtures before committing to that path; the user must retain the original archive throughout.

## 5. Risks and open questions

- **UniFFI:** the fix's presence in current source does not establish which future release will contain it. Compatibility of a patched pin and Sweetbriar's generated surface remains **unverified** until a macOS build. [Merged fix](https://github.com/mozilla/uniffi-rs/pull/2943).
- **Benchmarks:** BoltFFI's historical 1.4×–13.7× range was not tied to a specific consulted run. Independent reproduction and relevance to Sweetbriar remain **unverified**. Preserve versions, build flags, hardware, payloads, and raw results when making a comparison. [Published methodology](https://raw.githubusercontent.com/boltffi/boltffi/main/benchmarks/README.md).
- **Graph delivery and parity:** Apple's promised point release has no verified delivery date. Thunderbird's September announcement establishes its then-current email-only scope, not every later release's PIM capabilities. [Apple roadmap](https://support.apple.com/guide/deployment/integrate-with-microsoft-exchange-dep158966b23/web), [Thunderbird announcement](https://blog.thunderbird.net/2026/09/thunderbird-desktop-new-protocol-support-microsoft-graph-api/).
- **Graph workflow coverage:** Microsoft's EWS migration roadmap, updated **2026-09-04**, still lists target dates and excluded capabilities, including generic public-folder CRUD. A client announcing “Graph support” does not establish support for every Sweetbriar archival or enterprise workflow. [Microsoft's parity roadmap](https://learn.microsoft.com/en-us/exchange/clients-and-mobile-in-exchange-online/deprecation-of-ews-exchange-online).
- **Tenant enforcement:** no live tenant was inspected and the authenticated Message Center originals were unavailable. The October 10 finding verifies Microsoft's published Worldwide policy, not completed enforcement in each tenant. [Rollout](https://techcommunity.microsoft.com/blog/exchange/ews-deprecation-is-here-%E2%80%93-what-this-means-to-you/4561431).
- **Google:** API billing prices and the effective charging date remain **unverified**. Retained legacy quotas are not a basis for a new Sweetbriar project. Consumer send-as retirement also requires an account/identity-specific migration plan. [Billing announcement](https://developers.google.com/workspace/tools-safety), [Send-as notice](https://support.google.com/mail/answer/17101213?hl=en).
- **Proton:** the compared service terms do not resolve every supplemental policy or the acceptability of a particular unofficial client's traffic. Bridge support should be tested with real account and synchronization fixtures. [Terms](https://proton.me/legal/terms), [Bridge](https://proton.me/mail/bridge).
- **Fluent:** inspected Swift source/tests establish an integration candidate, not its production reliability. The community repository's maintenance history is short; package choice, native UI tooling, accessibility text, and translation workflow need a bounded prototype. [Repository history](https://api.github.com/repos/jcollie/zig-fluent/commits?per_page=5).
- **macOS import:** permission behavior must be checked for the distributed app and any helper. Apple lists a macOS 27 known issue for processes without bundle identifiers obtaining container access through Files & Folders settings. Avoid designing authorization around a successful developer-shell experiment. [OS release notes, issue 184660124](https://developer.apple.com/documentation/macos-release-notes/macos-27-release-notes).
- **Evidence scope:** release notes prove the vendor's stated release behavior; no competing client or macOS SDK was installed or exercised for this report. Notion's announcement is archived primary evidence; its shutdown was not tested through a live account. The other claims in the curated research remain outside this 18-claim audit.

## 6. Sources consulted

Every source below was consulted **2026-10-11 UTC**. Dates in the findings are the source's release, revision, publication, or archive dates, explicitly identified. HTTP fallback, direct raw/Markdown fetches, and Wayback copies were used where normal HTTPS rendering failed. Search-index copies helped locate removed notices; the final Notion finding relies on the archived vendor notice.

**Bindings, Crux, and localization**

- [UniFFI crates.io registry](https://crates.io/api/v1/crates/uniffi).
- [UniFFI 0.32.2 changelog](https://github.com/mozilla/uniffi-rs/blob/v0.32.2/CHANGELOG.md), [raw tagged changelog](https://raw.githubusercontent.com/mozilla/uniffi-rs/v0.32.2/CHANGELOG.md), [default-branch changelog](https://raw.githubusercontent.com/mozilla/uniffi-rs/main/CHANGELOG.md).
- [UniFFI issue #2929](https://github.com/mozilla/uniffi-rs/issues/2929), [issue API](https://api.github.com/repos/mozilla/uniffi-rs/issues/2929), [PR #2943](https://github.com/mozilla/uniffi-rs/pull/2943), [PR API](https://api.github.com/repos/mozilla/uniffi-rs/pulls/2943).
- [Released callback template](https://raw.githubusercontent.com/mozilla/uniffi-rs/v0.32.2/uniffi_bindgen/src/bindings/swift/templates/CallbackInterfaceImpl.swift), [released async template](https://raw.githubusercontent.com/mozilla/uniffi-rs/v0.32.2/uniffi_bindgen/src/bindings/swift/templates/Async.swift), [current callback template](https://raw.githubusercontent.com/mozilla/uniffi-rs/main/uniffi_bindgen/src/bindings/swift/templates/CallbackInterfaceImpl.swift).
- [BoltFFI repository](https://github.com/boltffi/boltffi), [metadata](https://api.github.com/repos/boltffi/boltffi), [raw README](https://raw.githubusercontent.com/boltffi/boltffi/main/README.md), [benchmark methodology](https://raw.githubusercontent.com/boltffi/boltffi/main/benchmarks/README.md).
- [Crux core 0.19 release](https://github.com/redbadger/crux/releases/tag/crux_core-v0.19.0), [release metadata](https://api.github.com/repos/redbadger/crux/releases?per_page=10).
- [Project Fluent](https://projectfluent.org/), [implementation list](https://github.com/projectfluent/fluent), [fluent-rs](https://github.com/projectfluent/fluent-rs), [Rust commit activity](https://api.github.com/repos/projectfluent/fluent-rs/commits?per_page=3), [fluent-bundle registry](https://crates.io/api/v1/crates/fluent-bundle).
- [zig-fluent README](https://tangled.org/jcollie.dev/zig-fluent/raw/main/README.md), [repository metadata](https://api.github.com/repos/jcollie/zig-fluent), [recent commits](https://api.github.com/repos/jcollie/zig-fluent/commits?per_page=5), [Swift wrapper](https://raw.githubusercontent.com/jcollie/zig-fluent/main/examples/swift/Sources/FluentKit/Fluent.swift), [Swift tests](https://raw.githubusercontent.com/jcollie/zig-fluent/main/examples/swift/Tests/FluentKitTests/CatalogTests.swift).

**Clients, Proton, and KDE**

- [Mailspring 1.26.0](https://github.com/Foundry376/Mailspring/releases/tag/1.26.0).
- [eM Client 11 announcement](https://www.emclient.com/blogpost.aspx?id=842).
- [Notion shutdown notice, June 25 archive](https://web.archive.org/web/20260625175000id_/https://www.notion.com/help/notion-mail-inbox-is-going-away-what-to-do-next), [live notice URL, now redirected](https://www.notion.com/help/notion-mail-inbox-is-going-away-what-to-do-next).
- [Thunderbird 154 release notes](https://www.thunderbird.net/en-US/thunderbird/154.0/releasenotes/), [September 2 Graph announcement](https://blog.thunderbird.net/2026/09/thunderbird-desktop-new-protocol-support-microsoft-graph-api/).
- [Proton current terms](https://proton.me/legal/terms), [October 3 archived terms](https://web.archive.org/web/20261003105539id_/https://proton.me/legal/terms), [Proton Bridge](https://proton.me/mail/bridge).
- [DavMail 7.0.0 announcement](https://sourceforge.net/p/davmail/news/2026/09/davmail-700-released/), [upstream release history](https://raw.githubusercontent.com/mguessan/davmail/master/RELEASE-NOTES.md), [project site](https://davmail.sourceforge.net/).
- [Akonadi MR !311](https://invent.kde.org/pim/akonadi/-/merge_requests/311), [MR API](https://invent.kde.org/api/v4/projects/pim%2Fakonadi/merge_requests/311), [26.04.0 CMake](https://invent.kde.org/pim/akonadi/-/raw/v26.04.0/CMakeLists.txt), [26.08.0 CMake](https://invent.kde.org/pim/akonadi/-/raw/v26.08.0/CMakeLists.txt).
- [KDE March 2 update](https://blogs.kde.org/2026/03/02/this-month-in-kde-apps/), [KDE Gear 26.04 release, April 16](https://kde.org/announcements/gear/26.04.0/), [26.04.0 tag metadata](https://api.github.com/repos/KDE/akonadi/git/tags/37566a0b7e41210a27003d8afbb4bf985fc61c58).

**Microsoft and Google policy**

- [Exchange Team October rollout announcement](https://techcommunity.microsoft.com/blog/exchange/ews-deprecation-is-here-%E2%80%93-what-this-means-to-you/4561431), [EWS access-control documentation](https://learn.microsoft.com/en-us/exchange/client-developer/exchange-web-services/how-to-control-access-to-ews-in-exchange).
- [EWS deprecation and Graph parity roadmap](https://learn.microsoft.com/en-us/exchange/clients-and-mobile-in-exchange-online/deprecation-of-ews-exchange-online).
- [Consumer Gmail send-as transition](https://support.google.com/mail/answer/17101213?hl=en), [send-as help](https://support.google.com/mail/answer/22370?hl=en).
- [Gmail API quotas](https://developers.google.com/workspace/gmail/api/reference/quota), [Workspace API model and billing policy](https://developers.google.com/workspace/tools-safety).
- [Workspace IMAP bandwidth policy](https://knowledge.workspace.google.com/admin/gmail/gmail-bandwidth-limits?hl=en), [original Help URL](https://support.google.com/a/answer/1071518?hl=en).

**Apple**

- [macOS 27 Golden Gate compatibility](https://support.apple.com/en-us/127255), [macOS overview](https://www.apple.com/os/macos/).
- [Exchange deployment guide](https://support.apple.com/guide/deployment/integrate-with-microsoft-exchange-dep158966b23/web).
- [LanguageModel reference, Markdown](https://developer.apple.com/documentation/foundationmodels/languagemodel.md).
- [PrivateCloudComputeLanguageModel reference, Markdown](https://developer.apple.com/documentation/foundationmodels/privatecloudcomputelanguagemodel.md).
- [Foundation Models updates, Markdown](https://developer.apple.com/documentation/updates/foundationmodels.md).
- [TN3193, Markdown](https://developer.apple.com/documentation/technotes/tn3193-managing-the-on-device-foundation-model-s-context-window.md).
- [PCC access requirements](https://developer.apple.com/private-cloud-compute/).
- [macOS 27 release notes, Markdown](https://developer.apple.com/documentation/macos-release-notes/macos-27-release-notes.md).
- [Apple DTS filesystem-permission explanation](https://developer.apple.com/forums/thread/678819).
- [Privacy & Security settings](https://support.apple.com/guide/mac-help/change-privacy-security-settings-on-mac-mchl211c911f/mac).
- [Apple Mail import/export](https://support.apple.com/guide/mail/import-or-export-mailboxes-mlhlp1030/mac).
- [Full Disk Access announcement, October 2](https://developer.apple.com/news/?id=p6zjojqw).
