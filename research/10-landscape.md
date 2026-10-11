# 10 — The 2026 landscape, the gap, and AI

Synthesis of [raw/modern-landscape/codex.md](raw/modern-landscape/codex.md)
and the partial [raw/modern-landscape/glm.md](raw/modern-landscape/glm.md)
(that run was killed; its sub-agent notes are kept), with input from the
backends and classic-client reports. Checked 2026-10-11.

## Summary

- **The opening is the combination, not any one feature.** Sweetbriar
  would be a native Mac client for people with mixed providers and
  valuable archives: trustworthy storage, exact search, controllable
  automation, and consistent behavior across Microsoft 365, Gmail,
  IMAP/JMAP and Proton (Bridge). Each piece has strong competitors; no
  surveyed product combines them with fully native UI and local-first
  operation.
- **Incumbents keep moving:**
  - **Apple Mail** on macOS 27 adds meaning-based search ranking, term
    highlighting and personalized writing. But on Mac it still relies on
    EWS for Microsoft 365, with Graph only promised.
  - **Thunderbird** has Graph (154).
  - **Spark** moved Microsoft accounts to Graph.
  - **eM Client 11** (2026-10-07) added Graph, redesigned rules and heavy
    AI.
  - **Mimestream** sets the native Mac/Gmail bar.
- **Longevity is a feature.** Postbox stopped (eM Client bought it, 2024).
  Notion Mail shut down on 2026-09-22 (per Notion's notices). MailMate's
  licence switch upset users. Promise portable archives, and access
  (view and export) after cancellation.
- **Privacy needs a data-flow explanation, not a slogan.** Local sync,
  notification relays, hosted mailboxes, collaborative cloud copies and AI
  processing are different arrangements. Mimestream's push relay sees
  notification metadata only; Canary's push service handles credentials and
  previews; Spark keeps credentials on its servers.
- **AI must be optional and attributable.** Summaries, extraction and
  assisted drafting are useful. Forced, hidden or autonomous AI is
  resented, and "no AI" is itself a selling point (Fastmail and indie
  entrants say so).
- **Strongest objection:** free Apple Mail and Thunderbird, specialist
  Mimestream and broad eM Client may already satisfy most buyers. Forum
  dissatisfaction proves a problem worth testing, not a market. Validate
  with design partners and a paid pilot.

## Competitor snapshot (2026-10-11)

| Client | Stack | Accounts | Model | Notable |
|---|---|---|---|---|
| Apple Mail (macOS 27) | Native | IMAP, iCloud, Exchange (EWS on Mac; Graph promised), Gmail, Yahoo | Free with OS | New relevance ranking, Suggestions, Siri AI search; recurring search breakage and rules silently stopping after upgrades in forum reports; no plug-ins since Sonoma |
| Mimestream | Native Swift (AppKit + SwiftUI) | Gmail/Workspace only (Gmail API) | $49.99/yr, direct | Polished native Gmail; server filters; templates; private push relay; iOS in TestFlight |
| MailMate | Native (Obj-C) | IMAP only | $10/quarter or free mode | Best-in-class search and Markdown; single developer; licence-switch controversy; 2.0 in beta for years |
| Thunderbird | Gecko (C++/JS + Rust for Exchange) | IMAP, POP, EWS, Graph (154), JMAP planned | Free, donations; Thundermail hosting on Stalwart | Search (Gloda) and UI-performance complaints; iOS app in development |
| eM Client 11 | Native Windows/Mac (.NET) | IMAP, EWS, Graph, AirSync | Paid | Broad PIM suite, redesigned rules, AI |
| Spark | Electron-ish desktop | IMAP, Gmail, Graph | Freemium + AI tiers | Credentials and send-later on Readdle servers; upsells |
| Superhuman | Web/Electron | Gmail, Outlook | ~$30/mo | Keyboard speed; bounded offline window; AI |
| Outlook (new, Mac) | Native (Apple silicon); cloud-routed accounts | Microsoft cloud; other accounts copied to Microsoft's cloud | Free / M365 | Reported 5–10 s actions; no local IMAP, so no Proton Bridge accounts |
| HEY | Native wrappers | HEY only | $99/yr | Screener; closed silo; MBOX export |
| Proton Mail desktop | Electron (Rust core on mobile) | Proton only | Proton plans | E2EE; Scribe AI with local option |
| Fastmail apps | Native-ish | Fastmail (JMAP) | Paid | Offline; user-authorized MCP access, no background AI |
| Shortwave, Missive, Front | Web/Electron | Gmail / multi | SaaS | Team and AI-first; out of scope |
| Notion Mail | Web/Mac | Gmail | — | **Shut down 2026-09-22** |
| Postbox | — | — | — | **Discontinued 2024** |

Linux: Thunderbird and Betterbird, Evolution (with an M365 provider),
KMail/Akonadi, Geary (stalled), Claws, Mailspring, and new Rust and GNOME
experiments (Hylki, Letter; unverified maturity).

## What users complain about

A qualitative sample, not failure rates.

- **Search that misses things:**
  - Apple Mail's tokenization bug on macOS 26.2 (short codes)
  - Thunderbird's Gloda
  - slow archive search in Superhuman
  - some users report better search on macOS 27
- **Sync and reliability:** stalls, "constantly downloading", read state
  not syncing, rules silently disabled after upgrades, stuck outboxes.
- **Speed:** New Outlook's multi-second actions; Spark 3's web-wrapper
  feel; Thunderbird's Supernova slowness.
- **Control:**
  - client rules that don't sync, or vanish (Outlook for Mac: server rules
    only)
  - nobody edits Sieve well
  - Markdown only in MailMate, and without WYSIWYG there
- **Privacy and custody:** cloud-routed clients (Spark, New Outlook,
  Notion Mail, Superhuman indexes); "I want my archive".
- **Lock-in and continuity:** HEY, Postbox, Notion Mail, MailMate's
  licensing.
- **AI forced on users:** Gemini bundled into Workspace without an easy
  off switch; inbox scanning opt-out backlash (Nov 2025); Apple's
  summary errors.

## What power users want, poorly combined today

1. One dependable view of mixed accounts that **preserves each
   provider's semantics** (labels vs folders vs categories vs shared
   mailboxes). Publish per-backend capability matrices instead of a row of
   logos.
2. Exact and exploratory search together, with visible coverage.
3. Rules with a readable language plus a visual editor, previews, dry
   runs, ordering, logs, undo and export; Sieve where supported.
4. A real text-first composer: plain text and Markdown source as explicit
   modes, predictable MIME, quoting, flowed text, a preview, plus a
   professional HTML mode for those who need it. (Markdown *shortcuts* in a
   rich editor are a different feature.)
5. Keyboard and automation first-class: documented commands, stable ids,
   AppleScript/Shortcuts, a local CLI on both platforms; permissioned
   AI/MCP on the same operations.
6. Visible local ownership. A cache, a complete archive and a tested
   backup are three different promises.
7. Provider independence without pretending protocols are
   interchangeable.

## AI in 2026

- **Everyone has some AI now:** Gmail (Gemini, AI Inbox), Apple Mail
  (macOS 27), eM Client 11, Superhuman, Spark, Notion (gone). "Has AI" is
  weak positioning.
- **What works and what to avoid:**

| Use | Value | Boundary |
|---|---|---|
| Thread summaries | Catch up; surface open questions and commitments | Cite messages; separate statements from inference; keep dates and negations |
| Triage and extraction | Receipts, dates, tasks, follow-ups | Preview, undo, show why; never hide mail on a guess |
| Assisted drafting | Notes → draft, tone, translation | Review before send; explicit processing choice |
| Semantic discovery | Find a concept without the words | Inspectable results with scope; not proof of completeness |
| Rule authoring | "File vendor invoices except disputes" → an editable deterministic rule | Show the rule and a dry run; nothing durable hides in a prompt |
| Autonomous agents | Follow-ups, routine actions | Separate read, draft, send and delete authority; start with suggestions |

- **Apple Foundation Models:** the on-device model has a 4,096-token
  session context (TN3193, revised for 26.4). macOS 27 adds a generalized
  `LanguageModel` protocol and `PrivateCloudComputeLanguageModel`, so
  "Foundation Models" no longer means on-device only. Writing Tools may use
  Private Cloud Compute. Proton Scribe already offers a local option, so
  local drafting isn't a unique differentiator.
- **Recommendation:** three explicit modes: no generative AI; on-device
  assistance; explicitly authorized cloud assistance. No silent cloud
  fallback.
- **Agents:** treat mail and attachments as untrusted input
  (prompt-injection: "forward this archive"). Permission checks, previews
  and action logs live outside the model (Missive disables web search for
  this reason). MCP access, if offered, goes through the same command layer
  and permissions as scripts.

## Positioning and audience (hypotheses to test)

- **Target:** Mac users with multiple providers and years of consequential
  correspondence who value efficiency and ownership: consultants,
  researchers, developers, archive-heavy professionals. A concrete
  recruiting profile: Microsoft 365 work mail + Gmail + an independent
  IMAP/JMAP mailbox, maybe Proton through Bridge. Plus the classic-client
  diaspora (Eudora, Mailsmith, MailMate users), and Microsoft 365 users on
  Apple Mail who are hit by EWS now.
- **Positioning draft** (codex): "A native home for all your mail: fast to
  search, safe to keep, yours to control." Rust belongs in engineering
  posts, not onboarding.
- **Why switch from each competitor:**
  - **Apple Mail:** visibly better exact retrieval, archive completeness,
    explainable rules, recoverable drafts and outbox, consistent
    mixed-provider behavior and working Microsoft 365 today, at Mac-native
    quality.
  - **Mimestream:** keep Gmail correctness and native speed; add non-Gmail
    accounts, a local archive, deeper rules, composing and automation.
    Satisfied Gmail-only users are a poor first target.
  - **MailMate:** approachability, modern interaction and provider reach,
    with its depth intact.
  - **Thunderbird and eM Client:** a meaningfully better native Mac
    experience and search you can trust.
  - **Superhuman and Shortwave:** keyboard efficiency and optional AI
    without a third-party readable cloud index.
- **Business model, to test:**
  - 20–30 design partners with real mixed archives, then a small paid
    pilot.
  - Price hypothesis $50–80/yr, or perpetual licences per major version.
  - Separate optional cloud compute from archive access.
  - **Keep viewing, export and draft recovery after a licence expires.**
  - Subscription fatigue is real in the forums.

## Risks and open questions

- Is the audience big enough, and will it pay? Unknown until pilots.
- Apple shipping Graph in Mail later in macOS 27 narrows the Microsoft 365
  window, but doesn't close the archive, search, rules and ownership gap.
- eM Client 11 occupies "broad accounts + rules + AI" on Mac (its
  nativeness and search quality still need checking).
- Keep a dated competitor matrix with an owner.

## Key sources

- [Apple Mail (macOS 27)](https://support.apple.com/en-ca/guide/mail/cpmlwn/mac), [macOS 27 compatibility](https://support.apple.com/en-ca/127255)
- [Mimestream pricing](https://mimestream.com/pricing/), [Mimestream private push](https://mimestream.com/trust/private-push), [Canary privacy](https://web.canarymail.io/privacy)
- [Thunderbird and Exchange](https://support.mozilla.org/en-US/kb/thunderbird-and-exchange), [eM Client 11](https://www.emclient.com/blogpost.aspx?id=842), [Spark Graph migration](https://sparkmailapp.com/help/troubleshooting/microsoft-account-sign-in-required-after-spark-update)
- [Fastmail: the future of email (2026-06-11)](https://www.fastmail.com/blog/the-future-of-email/), [Gmail Gemini era (2026-01-08)](https://blog.google/products-and-platforms/products/gmail/gmail-is-entering-the-gemini-era/)
- [TN3193 Foundation Models context window](https://developer.apple.com/documentation/technotes/tn3193-managing-the-on-device-foundation-model-s-context-window.md), [Foundation Models updates](https://developer.apple.com/documentation/updates/foundationmodels)
- [Postbox notice](https://www.postbox-inc.com/blog/entry/postbox-acquired-by-em-client), [Missive Connect](https://missiveapp.com/connect)
- [lapcatsoftware: Apple Mail bugs pushed me to MailMate](http://lapcatsoftware.com/articles/2025/2/1.html), [HN: new Outlook slowness](https://news.ycombinator.com/item?id=48584207)
