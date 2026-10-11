# Email-client landscape — complaints, power-user gaps, AI (notes, 2026-10-11)

## 1. Recurring complaints, 2025–2026

**Apple Mail (macOS).** Chronically unreliable search: dead after Sequoia 15.3 updates (https://forums.macrumors.com/threads/search-not-working-in-mail.2447553/, https://discussions.apple.com/thread/255968355); a documented tokenization bug on macOS 26.2 where short alphanumeric strings like booking codes return zero results (https://discussions.apple.com/thread/256217726). Exchange/IMAP sync stalls — messages silently missing, "Searching…" hangs (https://forums.macrumors.com/threads/major-mail-problems.2462766/), "constantly downloading" loops and vanishing messages on Tahoe (https://discussions.apple.com/thread/256163230). Rules silently stop firing after Tahoe upgrade (https://discussions.apple.com/thread/256137545, https://discussions.apple.com/thread/256216195). Junk filtering called "garbage"; read/deleted state doesn't sync back to Gmail/O365 (r/macapps via https://r.datuan.dev/r/macapps/comments/1nu3mxm/). ISP-account stalls (https://talk.tidbits.com/t/apple-mail-problems/34357). Long-term decline thesis: lapcatsoftware's "Apple Mail app bugs finally pushed me to MailMate" — incomplete IMAP downloads, broken rules/unread counts (http://lapcatsoftware.com/articles/2025/2/1.html).

**New Outlook for Mac.** Cloud-first rewrite is slower than legacy: 5–10s to open/reply to a message (https://learn.microsoft.com/en-us/answers/questions/5653803/; HN thread "new Outlook takes 10 seconds to do what Outlook Classic does instantly": https://news.ycombinator.com/item?id=48584207). "Updating Inbox…" hangs, incomplete search results, read-state not propagating (https://learn.microsoft.com/en-sg/answers/questions/5684770/). IMAP accounts: stuck Outbox, "Failed to download this message" (https://learn.microsoft.com/en-us/answers/questions/5854125/). No "mail to keep offline" slider — on-demand download only, top complaint for years (https://macuser.org.uk/2025/11/28/outlook-for-mac-sync-fixes/). No local-IMAP support → Proton Mail Bridge accounts can't be added (https://learn.microsoft.com/en-us/answers/questions/4677432/). HN commenters note it syncs all data through Microsoft's cloud (https://news.ycombinator.com/item?id=43856576).

**Spark.** Spark 3 abandoned the native Mac app for an Electron/web-wrapper feel — higher RAM, "massive disappointment" (https://discovermacapps.com/apps/spark-mail). Sync bugs across devices; crashes; can't select text; send failures (r/SparkMail via https://clean.email/blog/ai-for-work/spark-mail-ai-review). Paywalled basics (block/unsubscribe) and nagging AI/premium upsells (https://apps.apple.com/hr/app/spark-mail-ai-email-assistant/id997102246?see-all=reviews). Privacy: credentials/mail routed through Readdle's servers (https://discovermacapps.com/apps/spark-mail). Still no CalDAV after years (r/macapps thread above).

**Superhuman.** $30/mo seen as overpriced for "slightly faster Gmail" (https://news.ycombinator.com/item?id=44436122, https://news.ycombinator.com/item?id=23614167 — ex-user: archive search took ~10s vs instant Gmail). Keyboard-first workflow genuinely loved; critics say it induces "speedrun inbox-zero" shallow replies (https://news.ycombinator.com/item?id=44434997). Users fear Grammarly-style AI enshittification post-acquisition (https://news.ycombinator.com/item?id=44436851).

**HEY.** No IMAP — web/iOS only, "closed system," feeling trapped (https://baty.net/posts/2024/05/i-ve-already-stopped-using-hey-email/, https://news.ycombinator.com/item?id=23527773). Weak search vs Gmail archive (https://sethadam1.com/blog/read/why-i-left-hey). Export loses Imbox/Feed/Paper Trail structure (https://numericcitizen.me/migrating-from-hey-mail-to-fastmail-my-guide-for-a-smooth-transition/). $99/yr for a silo.

**Thunderbird.** Global search (Gloda) recurrently broken — finds nothing even with exact body quotes; reindexing hangs (https://connect.mozilla.org/t5/discussions/thunderbird-nebula-mail-searching-did-not-work-well/m-p/107431, https://discussion.fedoraproject.org/t/any-betterbird-users-here-thunderbird-search-is-defective-alternative-suggestions-welcome/127020, https://bugzilla.mozilla.org/show_bug.cgi?id=1871095). Post-Supernova (115+) UI jank: 5–10s folder operations (https://support.mozilla.org/en-US/questions/1435554, https://connect.mozilla.org/t5/discussions/thunderbird-is-extremely-slow-after-the-new-ui-update-115-4-2/m-p/48006); slow IMAP sync rendering (https://bugzilla.mozilla.org/show_bug.cgi?id=1891416); "5 second fast search" (https://connect.mozilla.org/t5/discussions/thinderbird-115-very-slow-fast-search/m-p/66064); unified-folders/UX regressions (https://connect.mozilla.org/t5/discussions/supernova/m-p/45461).

**Notion Mail.** Just a Gmail front-end — no other providers (https://www.zdnet.com/article/notion-mail-offers-ai-perks-but-skips-a-few-crucial-notion-features/). No Notion database integration at launch; web+Mac only, no Windows/Android initially (https://www.androidauthority.com/gmail-vs-notion-mail-3556029/). Reads and stores mail in Notion's DCs; contradictory SOC 2 claims flagged on HN (https://news.ycombinator.com/item?id=43694310). AI auto-label/views paywalled after trial (https://www.androidauthority.com/gmail-vs-notion-mail-3556029/).

**Mimestream.** Loved as fast native "Mac-assed" client using Gmail API search (https://discovermacapps.com/alternatives/mimestream-vs-spark-mail), but Gmail/Workspace-only, $50/yr subscription resented, and iOS app stuck in TestFlight limbo while subscribers wait (same source). HN users worry about Google killing API access (https://news.ycombinator.com/item?id=48584207).

**MailMate.** Best-in-class search, Markdown composer, flawless IMAP per converts (http://lapcatsoftware.com/articles/2025/2/1.html, https://mjtsai.com/blog/2025/02/18/mailmate-2-0-beta/). But: Markdown-only composing — users demand WYSIWYG (mjtsai comments); IMAP-only, no local "On My Mac" archive (lapcat); Exchange only if IMAP enabled / via DavMail (https://talk.macpowerusers.com/t/mailmate-markdown-and-exchange/40908); no calendar; one developer; subscription license controversial (https://mjtsai.com/blog/2025/12/29/mailmate-license-model-one-year-later/).

## 2. Power-user gaps nobody ships well in 2026

- **Fast + native + server-agnostic.** Repeated demand for "fully native MODERN OS-native mail client… actually fast with search that is actually useful" (https://news.ycombinator.com/item?id=43699158); indie Marco: "market has a weird gap… the good ones scan your data or cost $300+/yr" (https://news.ycombinator.com/item?id=47519302). Reality: Mimestream=Gmail-only, MailMate=IMAP-only, Thunderbird=IMAP(+paid Exchange add-on), New Outlook drops local IMAP entirely, Proton needs a flaky local Bridge (https://github.com/flathub/ch.protonmail.protonmail-bridge/issues/162). No mainstream client covers IMAP+Exchange+Gmail-API+Proton natively at native speed. New entrants (Penguin Mail, 187-comment HN thread https://news.ycombinator.com/item?id=49984716; Talanoa; Marco) sell exactly this.
- **Great search.** Only Gmail-API clients (Mimestream) get server-grade search; Apple Mail tokenization bugs, Thunderbird Gloda failures, Superhuman slow archive search (links above).
- **Server-side-like rules.** Client rules are local, don't sync, and break (Apple Mail Tahoe). Users beg for ManageSieve/sieve editors: "Crazy, that there are literally no apps or clients to manage sieve scripts" (https://forum.emclient.com/t/feature-needed-sieve-compatibility/36088, https://community.getmailspring.com/t/sieve-editor-for-server-side-filters/1122). eM Client exposes server rules only for Exchange/Gmail (https://forum.emclient.com/t/rules-across-multiple-devices/101124).
- **Plain-text/Markdown composing.** MailMate is the only real Markdown composer on Mac and it's mandatory (WYSIWYG demand unmet — mjtsai); Airmail does MD but is "way too buggy" (MPU Talk).
- **Scripting/automation + deep keyboard.** MailMate bundles and AppleScript-era Mail remain the ceiling; Ask HN requests show demand for vim-style shortcuts and durable power tools (https://news.ycombinator.com/item?id=43856576). Superhuman-level keyboard control costs $360/yr.
- **Local-first privacy / offline-first.** Spark, Notion Mail, Superhuman and New Outlook all route/store mail via vendor clouds; New Outlook can't even do full local sync (no offline slider). Local-only options (Apple Mail, MailMate, Thunderbird) carry the bugs above. "Offline-first, no AI, IMAP-native" is now a pitch in itself (Marco, Talanoa — https://news.ycombinator.com/item?id=47519302).

## 3. AI in email, 2026

**Genuinely liked:** automatic triage/labeling — Spark Smart Inbox "only app that effectively maintains Inbox Zero" (discovermacapps); Notion Mail auto-label "legitimately useful" (https://www.tomsguide.com/ai/i-ditched-gmail-for-notion-mail-for-3-weeks-heres-what-its-like-to-use-ai-powered-email). Long-thread summaries and NL search (Gemini "find specific contents") get mixed-positive mentions. Drafting/replies adopted where optional (Superhuman claims 5x AI-compose growth — company claim, treat as (unverified) marketing, https://news.ycombinator.com/item?id=44434997).

**Rejected:** forced bundling — Google rolled Gemini into Workspace with a price hike and most admins could not disable it without contacting support (https://9to5google.com/2025/01/31/google-workspace-gemini-disable-support/, https://slate.com/technology/2025/01/google-gemini-ai-workspace-default-opt-in.html, https://ia.acs.org.au/article/2025/-can-t-turn-it-off---google-faces-backlash-over-gmail-ai.html). Privacy backlash when "smart features" quietly let Gemini scan inboxes — Nov 2025 opt-out furor (https://decrypt.co/349491/google-backlash-gmail-setting-gemini-peek-inside-inboxes). Accuracy backlash: Apple suspended AI news-notification summaries after the BBC "Luigi Mangione shoots himself" error (https://www.bbc.com/news/articles/cq5ggew08eyo, https://www.cnbc.com/2025/01/16/apple-disables-ai-notifications-for-news-in-its-beta-iphone-software.html). "No AI" is now a feature: Marco and Talanoa lead with it (https://news.ycombinator.com/item?id=47519302, https://news.ycombinator.com/item?id=43814802); Thunderbird's Assist experiment is strictly opt-in after user sensitivity (https://blog.thunderbird.net/2025/04/thundermail-and-thunderbird-pro-services/). Pattern: AI wins when opt-in, on triage/search/summaries; loses when it drafts unasked, can't be disabled, or requires shipping mail to vendor clouds.

**On-device AI, macOS 26.** Foundation Models framework (WWDC25) exposes the Apple Intelligence on-device LLM via Swift API: ~3B-parameter model, 2-bit quantization (https://developer.apple.com/videos/play/wwdc2025/286/, https://machinelearning.apple.com/research/apple-foundation-models-2025-updates; arXiv paper describes ~3B with KV-cache sharing + 2-bit QAT, https://arxiv.org/html/2507.13575). Free for developers and users — no API keys, no per-request cost, fully offline, zero app-size impact (https://developer.apple.com/videos/play/meet-with-apple/205/). Requires Apple Intelligence hardware: Mac with M1+, iPhone 15 Pro/16+, iPad A17 Pro/M1+ (https://support.apple.com/en-us/121115). Capabilities: guided generation (@Generable, constrained decoding to Swift types), tool calling, built-in content-tagging adapter; custom rank-32 LoRA adapters via a Python toolkit producing ~160MB `.fmadapter` packages distributed through Background Assets — must be retrained per OS model version; toolkit 26.0.0 is the final release for the 26 OS cycle (https://developer.apple.com/apple-intelligence/foundation-models-adapter/, https://machinelearning.apple.com/research/apple-foundation-models-2025-updates). Framework now also routes to Private Cloud Compute (free for Small Business Program devs) and third-party server models via the LanguageModel protocol; `fm` CLI ships in macOS 27 (https://developer.apple.com/foundation-models-framework/). Writing Tools are automatic in NSTextView/NSTextField; custom views adopt via NSWritingToolsCoordinator/delegate or pasteboard services; behaviors configurable (allowedWritingToolsResultOptions, writingToolsBehavior) (https://developer.apple.com/documentation/appkit/writing-tools?language=objc, https://developer.apple.com/documentation/appkit/nstextview.md). For an email client this means free, private, offline summarization/triage/tagging is a table-stakes option — removing the "mail must go to the vendor's cloud" objection that Spark/Notion/Superhuman carry.

## SOURCES

- https://r.datuan.dev/r/macapps/comments/1nu3mxm/a_good_email_client_fot_mac_please/
- https://discovermacapps.com/alternatives/mimestream-vs-spark-mail
- https://discovermacapps.com/apps/spark-mail
- https://talk.tidbits.com/t/apple-mail-problems/34357
- https://forums.macrumors.com/threads/icloud-mail-large-file-issue.2491084/
- https://forums.macrumors.com/threads/search-not-working-in-mail.2447553/
- https://forums.macrumors.com/threads/major-mail-problems.2462766/
- https://discussions.apple.com/thread/255949026
- https://discussions.apple.com/thread/255968355
- https://discussions.apple.com/thread/256217726
- https://discussions.apple.com/thread/256163230
- https://discussions.apple.com/thread/256137545
- https://discussions.apple.com/thread/256156086
- https://discussions.apple.com/thread/256216195
- https://discussions.apple.com/thread/256168367
- http://lapcatsoftware.com/articles/2025/2/1.html
- https://macuser.org.uk/2025/11/28/outlook-for-mac-sync-fixes/
- https://learn.microsoft.com/en-sg/answers/questions/5684770/outlook-for-mac-stuck-on-updating-inbox-with-new-b
- https://learn.microsoft.com/en-us/answers/questions/5653803/new-outlook-on-mac-is-slow-to-open-and-respond-to
- https://learn.microsoft.com/en-us/answers/questions/5854125/outlook-for-mac-(new-outlook)-with-imap-account-is
- https://learn.microsoft.com/en-us/answers/questions/4677432/unable-to-link-protonmail-account-to-new-outlook
- https://techcommunity.microsoft.com/discussions/outlookgeneral/outlook-for-mac-new-look-sync-mail-issues/3276267
- https://news.ycombinator.com/item?id=44436122
- https://news.ycombinator.com/item?id=44434997
- https://news.ycombinator.com/item?id=44435196
- https://news.ycombinator.com/item?id=44436851
- https://news.ycombinator.com/item?id=23614167
- https://news.ycombinator.com/item?id=48584207
- https://news.ycombinator.com/item?id=48375016
- https://news.ycombinator.com/item?id=49956092
- https://news.ycombinator.com/item?id=49984716
- https://news.ycombinator.com/item?id=44970563
- https://news.ycombinator.com/item?id=43814802
- https://news.ycombinator.com/item?id=43694310
- https://news.ycombinator.com/item?id=43856576
- https://news.ycombinator.com/item?id=47519302
- https://news.ycombinator.com/item?id=43699158
- https://news.ycombinator.com/item?id=42915586
- https://news.ycombinator.com/item?id=39353434
- https://news.ycombinator.com/item?id=23527773
- https://connect.mozilla.org/t5/discussions/thunderbird-nebula-mail-searching-did-not-work-well/m-p/107431
- https://discussion.fedoraproject.org/t/any-betterbird-users-here-thunderbird-search-is-defective-alternative-suggestions-welcome/127020
- https://bugzilla.mozilla.org/show_bug.cgi?id=1871095
- https://bugzilla.mozilla.org/show_bug.cgi?id=1891416
- https://connect.mozilla.org/t5/discussions/thinderbird-115-very-slow-fast-search/m-p/66064
- https://support.mozilla.org/en-US/questions/1435554
- https://connect.mozilla.org/t5/discussions/thunderbird-is-extremely-slow-after-the-new-ui-update-115-4-2/m-p/48006
- https://connect.mozilla.org/t5/discussions/supernova/m-p/45461
- https://support.mozilla.org/gu-IN/questions/1431845
- https://www.zdnet.com/article/notion-mail-offers-ai-perks-but-skips-a-few-crucial-notion-features/
- https://www.androidauthority.com/gmail-vs-notion-mail-3556029/
- https://www.tomsguide.com/ai/i-ditched-gmail-for-notion-mail-for-3-weeks-heres-what-its-like-to-use-ai-powered-email
- https://www.androidpolice.com/testing-notion-mail-vs-gmail/
- https://apps.apple.com/hr/app/spark-mail-ai-email-assistant/id997102246?see-all=reviews
- https://clean.email/blog/ai-for-work/spark-mail-ai-review
- https://apptail.io/app/spark-mail-ai-email-assistant-qXg/kenya
- https://baty.net/posts/2024/05/i-ve-already-stopped-using-hey-email/
- https://numericcitizen.me/migrating-from-hey-mail-to-fastmail-my-guide-for-a-smooth-transition/
- https://sethadam1.com/blog/read/why-i-left-hey
- https://jonathanwthomas.net/hey-com-email-6-months-on-a-review/
- https://mjtsai.com/blog/2025/02/18/mailmate-2-0-beta/
- https://mjtsai.com/blog/2025/12/29/mailmate-license-model-one-year-later/
- https://freron.com/2025/another-mailmate-2-0-beta-released/
- https://talk.macpowerusers.com/t/mailmate-markdown-and-exchange/40908
- https://forum.emclient.com/t/rules-not-syncing/60961
- https://forum.emclient.com/t/rules-across-multiple-devices/101124
- https://forum.emclient.com/t/feature-needed-sieve-compatibility/36088
- https://community.getmailspring.com/t/sieve-editor-for-server-side-filters/1122
- https://github.com/flathub/ch.protonmail.protonmail-bridge/issues/162
- https://proton.me/download/bridge/early_releases.html
- https://discuss.kde.org/t/imap-connection-on-startup/50125
- https://ia.acs.org.au/article/2025/-can-t-turn-it-off---google-faces-backlash-over-gmail-ai.html
- https://decrypt.co/349491/google-backlash-gmail-setting-gemini-peek-inside-inboxes
- https://9to5google.com/2025/01/31/google-workspace-gemini-disable-support/
- https://slate.com/technology/2025/01/google-gemini-ai-workspace-default-opt-in.html
- https://tech.yahoo.com/ai/gemini/articles/google-ai-access-content-gmail-232700594.html
- https://www.bbc.com/news/articles/cq5ggew08eyo
- https://www.cnbc.com/2025/01/16/apple-disables-ai-notifications-for-news-in-its-beta-iphone-software.html
- https://www.theguardian.com/technology/2025/jan/17/apple-suspends-ai-generated-news-alert-service-after-bbc-complaint
- https://www.businessinsider.com/apple-intelligence-pauses-ai-news-summaries-beta-2025-1
- https://www.cnn.com/2025/01/16/media/apple-ai-news-fake-headlines
- https://developer.apple.com/foundation-models-framework/
- https://developer.apple.com/documentation/foundationmodels.md
- https://developer.apple.com/videos/play/wwdc2025/286/
- https://developer.apple.com/videos/play/meet-with-apple/205/
- https://developer.apple.com/apple-intelligence/foundation-models-adapter/
- https://machinelearning.apple.com/research/apple-foundation-models-2025-updates
- https://machinelearning.apple.com/research/introducing-apple-foundation-models
- https://arxiv.org/html/2507.13575
- https://support.apple.com/en-us/121115
- https://developer.apple.com/documentation/appkit/writing-tools?language=objc
- https://developer.apple.com/documentation/appkit/nstextview.md
- https://blog.thunderbird.net/2025/04/thundermail-and-thunderbird-pro-services/
- https://hn.algolia.com/api/v1/search?query=email%20client (Algolia HN API, multiple queries)
