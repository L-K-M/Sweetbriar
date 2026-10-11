# Fact check: claims the Sweetbriar research relies on

Several curated documents rely on recent (2025–2026) facts that one
reviewer could not verify. Check each against primary sources (vendor
docs, release notes, repositories, official blogs). Today is 2026-10-11.

For each claim: verdict (**confirmed / partly true / false / cannot
verify**), the corrected statement if needed, the primary source URL and
the date you checked it. Do not trust secondary summaries when a primary
source exists. If a claim is false, say which curated doc should change
(the repo snapshot is in `./research/`; grep for the claim).

Claims:

1. UniFFI 0.32.1 is the current release; a 2026 release fixed Swift 6
   strict-concurrency issues with async callback traits.
2. BoltFFI exists as a Rust↔Swift/Kotlin binding generator with published
   benchmarks against UniFFI.
3. Crux (redbadger) released 0.19 in 2026 and changed its core/shell
   model in some notable way.
4. Mailspring 1.26.0 rewrote its model so one message can live in many
   folders (iCloud folders, Exchange duplicate Sent, Proton labels).
5. eM Client 11 was released on 2026-10-07 and added Microsoft Graph.
6. Notion Mail shut down (or announced shutdown) on 2026-09-22.
7. Thunderbird 154 (August 2026) shipped Microsoft Graph support for
   Exchange Online.
8. Proton changed its terms of service on 2026-10-09 in a way relevant to
   third-party clients or Bridge.
9. DavMail 7.0.0 was released in 2026 with Graph support.
10. macOS 27's Foundation Models framework has a generalized
    `LanguageModel` protocol and a `PrivateCloudComputeLanguageModel`;
    TN3193 was revised for 26.4 and documents a 4,096-token session
    budget.
11. Apple publicly committed to Microsoft Graph support for Exchange in a
    macOS 27 update (Apple deployment guide).
12. Microsoft's `EWSAllowedAppIDs` became mandatory on 2026-10-10 for
    Exchange Online tenants that re-enable EWS (MC1227454 / MC1485116).
13. Gmail "Send mail as" for third-party addresses ends for consumer
    Gmail web and mobile in January 2027; Workspace is unaffected.
14. Gmail API new-project quotas: 6,000 units/user/min,
    `messages.get` = 20 units, billing announced above 80M units/day.
15. Google Workspace IMAP limits: 2,500 MB/day download, 500 MB/day
    upload.
16. Swift has usable Project Fluent (localization) support, or a
    maintained Fluent runtime callable from Swift.
17. macOS 27 requires a user permission (TCC / app data protection) for
    one app to read another app's container data, such as
    `~/Library/Mail` (affects importing Apple Mail archives).
18. Akonadi switched its default backend to SQLite (merge request !311,
    released in 26.08.x).
