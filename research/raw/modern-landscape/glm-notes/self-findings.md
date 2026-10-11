# Self-verified findings (fetched 2026-10-11)

## Apple / macOS
- macOS 27 "Golden Gate" is current: announced WWDC June 8 2026, released Sept 14 2026, Apple silicon ONLY (last Intel release = Tahoe 26). Source: apple.com/macos/ (fetched 2026-10-11); en.wikipedia.org/wiki/MacOS_Tahoe (fetched 2026-10-11, cites The Verge 2026-06-08 + Apple newsroom).
- macOS 26 "Tahoe": released 2025-09-15, latest 26.7.1 (2026-09-28). Mixed reception; criticized for optimization/stability. (Wikipedia, fetched 2026-10-11)
- macOS 27 Mail improvements: "More relevant search in Mail — a new ranking system"; "Suggestions" in Mail (contextual quick actions, English); Siri AI can "locate an email buried in your inbox"; Call Context surfaces Mail info on calls; enhanced Liquid Glass. Siri AI = beta, English first, daily usage limits on server-side AI features, "Expanded access ... available for a fee in the future". Source: apple.com/macos/ fetched 2026-10-11.
- Apple Intelligence devices: Macs M1+, iPhone 15 Pro+, iPad mini A17 Pro, etc. (apple.com/macos/ footnote, fetched 2026-10-11)
- Foundation Models framework (developer.apple.com/documentation/foundationmodels.md, fetched 2026-10-11): on-device LLM for apps; guided generation (@Generable), tool calling, dynamic profiles/instructions; NEW in 2026 cycle: Private Cloud Compute routing (PrivateCloudComputeLanguageModel), multimodal image attachments, transcripts API, session KV-caching optimization, custom model provider via "Core AI" (run open-source models on device through FM session API), prompt evaluation. Requires Apple Intelligence-capable device.
- AFM "3 Cloud" models mentioned on apple.com/macos/ footnote (Shortcuts) — implies Foundation Models now gen-3 cloud models. (uncertain naming; verify if used)

## Thunderbird (thunderbird.net + blog, fetched 2026-10-11)
- Current desktop version 157.0.1 (site download links). ESR 153 "Meadow" (2026-07); prev ESR 140 "Eclipse".
- Native Microsoft Graph API support announced Sept 2026, because Microsoft disables EWS for Microsoft 365 "later this year" (2026). Blog: blog.thunderbird.net/2026/09/thunderbird-desktop-new-protocol-support-microsoft-graph-api/
- Thunderbird for iOS: still IN DEVELOPMENT as of 2026-10-01 ("A First Look at the Native iPhone Email App in Development", built natively from scratch; not yet in App Store).
- Thundermail (tb.pro, fetched 2026-10-11): email hosting beta, waitlist, "Full launch expected in 2026". Early Bird $6/mo paid annually: 30GB mail, 60GB Send storage, 1 inbox 15 addresses, 3 custom domains. Built on Stalwart server; JMAP + IMAP/SMTP; data hosted in Germany; includes Appointment + Send. Thunderbird Pro = subscription bundle (Thundermail + Appointment + Send).
- Thunderbird positioning on homepage: donation-funded, no ads, "We don't ... secretly train AI with your private conversations", open source, cross-platform (Win/mac/Linux + Android).

## Mimestream (mimestream.com + /pricing, fetched 2026-10-11)
- Native macOS Gmail client, Gmail API only, macOS 12+. Features: labels, server-side filter management, categories, templates w/ variables, markdown + code blocks in compose, send-and-archive, undo send, tracker blocking (75+ services), unified inbox, profiles, calendar invite responses, snooze, menu bar extra, multiple windows.
- Privacy: "only stores your data and tokens on your device ... direct connections to the Gmail API, without going through any intermediary sync service."
- Pricing: $49.99/yr or $4.99/mo, subscription ONLY (no lifetime), direct-only (not on Mac App Store), 14-day trial, 5 devices, family sharing. Group licensing exists.
- Copyright 2026; FAQ mentions future iOS app possibility ("if/when we release an iOS app").

## MailMate (freron.com, /pricing, blog; fetched 2026-10-11)
- IMAP-only macOS client by Benny Kjær Nielsen (Freron Software, Copenhagen). Keyboard control, Markdown composition, advanced search conditions + drill-down search links, smart mailboxes, OpenPGP + S/MIME, tagging, widescreen layout, third-party integration. macOS 10.12+.
- MailMate 2.0: still public beta (first 2.0 beta Dec 22 2024; "Another MailMate 2.0 Beta released" 2025; BusyContacts integration 2025).
- New license model: $10 every 3 months subscription (≈$40/yr); Free Mode = all features, personal/non-profit use, adds "MailMate Free Mode" header; legacy licenses switched to Free Mode on 2025-07-01 (controversial; dev withdrew "2.0 free upgrade" promise partially).

## Postbox (postbox-inc.com, fetched 2026-10-11)
- Acquired by eM Client (announced 2024-10-22). "Postbox will no longer be sold or developed." Support ended 2024-12-22; Help Center until 2025-12-22. Migration discounts to eM Client. => Postbox is DEAD as an actively developed product.

## Search engines
- html.duckduckgo.com bot-blocked (challenge). Bing HTML works: bing.com/search?q=...
