<!-- provenance: helper=devin model=swe-2-max started=2026-10-11T18:14:20+00:00 exit=0 brief=research/briefs/ -->

# Sweetbriar research: Supporting non-standard email sources

**Date of research: 2026-10-11.** All facts verified against the linked sources on that date. Anything that could not be verified is marked "(unverified)".

## 1. TL;DR

- **EWS is dying right now.** Microsoft started blocking Exchange Web Services in Exchange Online on **October 1, 2026** (ten days ago); per-tenant `EWSAllowedAppIDs` allow-lists become required **October 10, 2026** (yesterday); permanent shutdown **April 1, 2027**, no extensions planned. On-premises Exchange (incl. Exchange SE) keeps EWS. [Microsoft Learn](https://learn.microsoft.com/en-us/exchange/clients-and-mobile-in-exchange-online/deprecation-of-ews-exchange-online), [MC1485116](https://mc.merill.net/message/MC1485116)
- **This is a market opening.** Apple Mail on macOS talks to Exchange via EWS and may be breaking for Microsoft 365 users as we speak; Apple says it is "actively working with Microsoft" on a Graph move for a future macOS 27 update. Classic Outlook for Mac is also EWS-based. A Mac-native, Graph-capable client arriving now fills a real hole. [MacRumors, 2026-10-01](https://www.macrumors.com/2026/10/01/apple-mail-mac-stop-sync-microsoft-365/)
- **Microsoft Graph is the required Exchange Online backend**: folder-delta queries for sync, `message/$value` for MIME, `sendMail` or MIME-POST for sending, upload sessions for 3–150 MB attachments, KQL `$search`, categories, rules, shared mailboxes — but webhooks need a **public HTTPS endpoint**, so a desktop client must poll deltas. Throttling: ~10,000 req/10 min and 4 concurrent requests per app per mailbox. [Microsoft Graph docs](https://learn.microsoft.com/en-us/graph/throttling-limits)
- **Google: prefer IMAP over the Gmail API.** Both need a restricted OAuth scope (`https://mail.google.com/` for IMAP), but the Gmail API adds per-project quotas that Google started restructuring on **May 1, 2026** (80M units/day free threshold; billing announced for "later in 2026"), and Pub/Sub push can't reach a desktop client without vendor-run infrastructure. Gmail's `X-GM-EXT-1` IMAP extensions expose labels, thread IDs and Gmail search syntax anyway. CASA is **not** required for a purely local client — restricted-scope verification only triggers the security assessment when data transits a third-party server. [Google quota page](https://developers.google.com/workspace/gmail/api/reference/quota), [restricted scope verification](https://developers.google.com/identity/protocols/oauth2/production-readiness/restricted-scope-verification)
- **Proton: ship "Proton via Bridge" support now (it's just local IMAP/SMTP on 127.0.0.1:1143/1025, paid plans only), and track Proton's official Rust SDK.** Proton's Mail v7 apps already run a shared Rust core (~80% shared code); the SDK is open-sourced in the `ProtonMail/clients` monorepo but not yet externally buildable. Direct API access is technically feasible (unofficial `proton-core` crate is byte-compatible) but is ToS gray-zone — Proton officially sanctions Bridge only. [proton-bridge](https://github.com/ProtonMail/proton-bridge), [Proton blog](https://proton.me/blog/next-generation-proton-mail-mobile-apps)
- **Tuta: impossible** — no IMAP/API by design and unofficial bridges violate the spirit of their ToS. **HEY: impossible** — no IMAP/POP/SMTP on any plan; only MBOX export. Offer import, not sync, for both.
- **JMAP is real and growing**: Fastmail (OAuth + API tokens, JMAP for mail + contacts), Stalwart, Cyrus, Apache James. JMAP for Calendars hit WG Last Call / RFC-Editor queue October 2026. JMAP is the best *shape* for Sweetbriar's internal model regardless of wire protocol. [Fastmail dev docs](https://www.fastmail.com/dev/), [draft-ietf-jmap-calendars](https://datatracker.ietf.org/doc/draft-ietf-jmap-calendars/)
- **Everything else is IMAP + a credential variant**: iCloud (app-specific password; OAuth exists but only for Apple-approved partners like Outlook), Yahoo/AOL (OAuth by application approval, else app password), Zoho (full REST API + IMAP XOAUTH2), GMX/Web.de/Posteo/mailbox.org/QQ/163 (app passwords / authorization codes).
- **Recommended backend order**: (1) IMAP+SMTP with Gmail extensions — covers ~90% of providers incl. Proton-via-Bridge; (2) Microsoft Graph — the EWS clock is ticking; (3) EWS for on-prem/allow-listed tenants, ideally reusing Thunderbird's MPL Rust crates; (4) JMAP; (5) local mbox/Maildir/POP3/NNTP archives. Design the unified model on JMAP semantics (multi-mailbox membership handles both folders and labels).

---

## 2. Findings

### 2.1 Microsoft (Exchange Online / Microsoft 365 / Outlook.com)

#### 2.1.1 EWS retirement in Exchange Online — exact dates

EWS deprecation was announced 2018 (no new features); the disablement timeline was set in 2023 and refined through 2025–2026. Current state, verified 2026-10-11:

| Date | Event |
|---|---|
| July 2018 | EWS deprecation announced (feature freeze) |
| 2023 | Disablement date set to 10/2026 |
| Jan 2024 | Midnight Blizzard incident → scope widened to *all* apps incl. Microsoft's |
| **Oct 1, 2026** | Phased blocking of EWS in Exchange Online begins. Without admin action Microsoft sets `EWSEnabled=False`; admins can re-enable temporarily |
| Oct 8–9, 2026 | Microsoft auto-populates `EWSAllowedAppIDs` for qualifying tenants based on prior-60-day EWS activity |
| **Oct 10, 2026** | `EWSEnabled=True` alone no longer suffices — the app must be on the tenant's `EWSAllowedAppIDs` allow-list |
| early Oct 2026 → early Jul 2027 | Rollout window (Worldwide, GCC, GCC High, DoD) |
| **Apr 1, 2027** | Permanent removal; no re-enablement, "no extensions currently planned" |

Sources: [Microsoft Learn — Deprecation of EWS in Exchange Online](https://learn.microsoft.com/en-us/exchange/clients-and-mobile-in-exchange-online/deprecation-of-ews-exchange-online), [MC1485116 (EWSAllowedAppIDs enforcement)](https://mc.merill.net/message/MC1485116), [MC1227454](https://mc.merill.net/message/MC1227454), [MC1447678](https://mc.merill.net/message/MC1447678), [Retirement announcement (TechCommunity)](https://techcommunity.microsoft.com/blog/exchange/retirement-of-exchange-web-services-in-exchange-online/3924440), [MacRumors PSA, 2026-10-01](https://www.macrumors.com/2026/10/01/apple-mail-mac-stop-sync-microsoft-365/).

Nuances:

- **Exemptions**: none for third parties after Apr 2027; only the tenant-level `EWSAllowedAppIDs` bridge period (Oct 2026–Jul 2027 rollout, ending Apr 2027).
- **On-premises Exchange is unaffected**: "no changes are being made to EWS in Exchange Server" — EWS remains fully supported for on-prem and is the *only* rich API there, so Sweetbriar will need EWS long-term anyway for corporate on-prem customers. [Retirement announcement](https://techcommunity.microsoft.com/blog/exchange/retirement-of-exchange-web-services-in-exchange-online/3924440)
- **Public folders**: programmatic EWS access to public folders restricted to supported Outlook clients after Oct 2026; Graph will not gain generic public-folder CRUD. [Microsoft Learn](https://learn.microsoft.com/en-us/exchange/clients-and-mobile-in-exchange-online/deprecation-of-ews-exchange-online)
- **Graph parity gaps**: Microsoft's roadmap (Q3 CY2026) covers Sticky Notes and extra contact properties; permanently excluded: generic Public Folder CRUD and generic M365-Group mailbox CRUD (use the Groups conversation APIs instead). [EWS→Graph mappings](https://learn.microsoft.com/en-us/graph/migrate-exchange-web-services-api-mapping)

#### 2.1.2 Microsoft Graph mail API

Capabilities verified against Microsoft Learn (2026-10-11):

- **Sync / delta**: `GET /users/{id}/mailFolders/{id}/messages/delta` gives created/updated/deleted per folder via `skipToken`/`deltaToken` state tokens; `changeType` filter; `$select/$top/$expand` supported; `$filter`/`$orderby` limited. This is the sync engine primitive. [delta query for messages](https://learn.microsoft.com/en-us/graph/delta-query-messages), [message: delta](https://learn.microsoft.com/en-us/graph/api/message-delta?view=graph-rest-1.0)
- **Change notifications**: webhook subscriptions (`/subscriptions`) on `/users/{id}/messages` or per-folder; delegated scopes Mail.ReadBasic/Mail.Read. **The endpoint must be a publicly reachable HTTPS URL** that answers validation within ~10 s and notifications within 3 s; retries up to 4 h. A desktop app cannot receive pushes directly — alternatives: (a) poll `delta` (normal approach: IMAP-IDLE-like cadence or on foreground/fetch), (b) vendor-run relay that forwards notifications to clients (requires holding subscription state server-side; heavier). [Webhooks](https://learn.microsoft.com/en-us/graph/change-notifications-delivery-webhooks), [Outlook change notifications overview](https://learn.microsoft.com/en-us/graph/outlook-change-notifications-overview)
- **MIME**: `GET /users/{id}/messages/{id}/$value` returns RFC 822 MIME; `POST /users/{id}/messages` with `Content-Type: text/plain` creates a draft from raw MIME; `POST .../messages/{id}/send` or `sendMail` sends. [mail API overview](https://learn.microsoft.com/en-us/graph/api/resources/mail-api-overview?view=graph-rest-1.0)
- **Attachments**: single POST under 3 MB; 3–150 MB via `createUploadSession` (resumable byte-range PUTs, ≤4 MB chunks; pre-authenticated upload URL). Default Exchange Online message size limit 35 MB (tenant-configurable). Known issue with large attachments in shared/delegated mailboxes. [attachment: createUploadSession](https://learn.microsoft.com/en-us/graph/api/attachment-createuploadsession?view=graph-rest-1.0), [large attachments](https://learn.microsoft.com/en-us/graph/outlook-large-attachments)
- **Search**: `$search` (KQL) on messages plus `$filter` OData; message `categories` property + Outlook master categories; Focused Inbox = `inferenceClassification` overrides per user; inbox rules via `mailFolders/inbox/messageRules`; shared/delegated mailboxes via `/users/{sharedMailbox}/...` when the user has rights. [mail API overview](https://learn.microsoft.com/en-us/graph/api/resources/mail-api-overview?view=graph-rest-1.0)
- **Throttling** (per app-ID × mailbox): 10,000 requests / 10 min, 4 concurrent requests, 150 MB upload / 5 min; HTTP 429 + `Retry-After`; JSON batching executes ≤4 requests concurrently per mailbox. [Graph throttling limits](https://learn.microsoft.com/en-us/graph/throttling-limits)
- **Caveat**: eM Client reports implementing Graph for ~2 years but holding it back because "the protocol is very buggy on Microsoft side, changing all the time" — expect parity papercuts (distribution lists on outlook.com flagged by them). [eM Client forum](https://forum.emclient.com/t/microsoft-graph-support/109139)

#### 2.1.3 IMAP/POP/SMTP + OAuth on Microsoft 365

- Basic auth for IMAP/POP/EAS/EWS/Autodiscover was disabled across Exchange Online in 2022–2023; OAuth 2.0 (XOAUTH2) is the only path, via Entra ID tokens with the Exchange scopes `IMAP.AccessAsUser.All`, `POP.AccessAsUser.All`, `SMTP.Send`. Tenants can still disable IMAP/POP per-protocol or per-mailbox, and Conditional Access can block devices. Consumer Outlook.com accounts support OAuth2 IMAP as well (eM Client pairs it with "AirSync" for calendar/contacts). [Deprecation of basic auth in Exchange Online](https://learn.microsoft.com/en-us/exchange/clients-and-mobile-in-exchange-online/deprecation-of-basic-authentication-exchange-online), [eM Client compare](https://www.emclient.com/how-we-compare?lang=en)
- **SMTP AUTH basic-auth retirement — moved again (Jan 27, 2026 revision)**: previously March 1 → April 30, 2026 ramp; now: unchanged until **end of December 2026**, then disabled *by default* for existing tenants (admins may re-enable); new tenants from ~Dec 2026/Jan 2027 can't use it at all; final removal date to be announced in **2H 2027**. OAuth for SMTP AUTH remains required long-term. [Updated deprecation timeline (TechCommunity)](https://techcommunity.microsoft.com/blog/exchange/updated-exchange-online-smtp-auth-basic-authentication-deprecation-timeline/4489835), [MC786329](https://mc.merill.net/message/MC786329), [Office365ITPros analysis](https://office365itpros.com/2026/01/29/smtp-auth-basic-retirement/)

#### 2.1.4 Exchange ActiveSync

- EAS is a Microsoft-licensed protocol: third parties need an EAS license via Microsoft's Technology Licensing program, and formal support additionally requires a Premier Support agreement; unlicensed commercial use gets referred to Microsoft legal. Historical reporting put the standard fee at "$100,000 or first-year's royalties, whichever is higher, with a per-unit royalty thereafter" — current terms are negotiated privately (unverified). [Microsoft Premier Developer blog](https://devblogs.microsoft.com/premier-developer/client-side-exchange-activesync-eas-developers-support-options/), [2008 licensing expansion](https://news.microsoft.com/source/2008/12/18/microsoft-expands-exchange-active-sync-licensing-program/), [ZDNet on fees](https://www.zdnet.com/article/in-apples-footsteps-google-licenses-microsoft-activesync/)
- EAS still works against Exchange Online and Outlook.com — iOS/iPadOS Mail uses it, and eM Client uses "AirSync" for consumer Microsoft accounts — but it is legacy: designed for mobile sync, no longer gaining features, and a poor fit for a desktop-first client. **Verdict: skip EAS; Graph for Exchange Online, EWS for on-prem.** (Apple's own Mac Mail chose EWS, not EAS.) [Apple deployment docs](https://support.apple.com/guide/deployment/integrate-with-microsoft-exchange-dep158966b23/web)

#### 2.1.5 App registration, consent, publisher verification

- A multi-tenant app registration in Entra ID ("Accounts in any organizational directory" or "...and personal Microsoft accounts") lets any tenant's users sign in; each tenant materializes an Enterprise Application the admin controls. [Register an app](https://learn.microsoft.com/en-us/entra/identity-platform/quickstart-register-app), [Establish applications](https://learn.microsoft.com/en-us/entra/architecture/establish-applications)
- **Admin consent problems**: tenants can disable user consent entirely or restrict it to admin-approved/low-risk scopes; delegated mail scopes like `Mail.Read`/`Mail.ReadWrite` may require admin approval depending on tenant policy; external-tenant users can't self-consent. Sweetbriar must implement the admin-consent flow and degrade gracefully. [Consent experience](https://learn.microsoft.com/en-us/entra/identity-platform/application-consent-experience)
- **Publisher verification**: since Nov 2020, with risk-based step-up consent enabled, users can't consent to most newly registered multi-tenant apps that aren't publisher-verified. Verification requires a Microsoft AI Cloud Partner Program (CPP/PartnerID) account and yields the blue "verified" badge — effectively mandatory for enterprise reach. [Publisher verification](https://learn.microsoft.com/en-us/entra/identity-platform/publisher-verification-overview)

#### 2.1.6 How others do Microsoft 365

- **Thunderbird**: shipped native EWS support — written in Rust — in monthly release **145** (Nov 2025), email only; calendar/contacts via EWS in development; **Microsoft Graph integration on the roadmap**. Auth via pre-configured OAuth2 app credentials at `login.microsoftonline.com`; NTLM in progress; on-prem basic-auth supported. Architecture: account/folder plumbing stays C++, the protocol client is Rust (`ews_xpcom`, `graph_xpcom` crates; XML via a serde/`xml_struct`-based layer). [Thunderbird blog, Nov 2025](https://blog.thunderbird.net/2025/11/thunderbird-adds-native-microsoft-exchange-email-support/), [Adventures in Rust](https://blog.thunderbird.net/2024/04/adventures-in-rust-bringing-exchange-support-to-thunderbird/), [MozillaWiki: Thunderbird:Exchange](https://wiki.mozilla.org/Thunderbird:Exchange), [Exchange code architecture](https://source-docs.thunderbird.net/en/latest/exchange/architecture.html)
- **Apple**: iOS uses **EAS**; macOS Mail/Calendar/Contacts use **EWS** — currently breaking for M365 tenants where EWS is blocked; Apple is "actively working with Microsoft" on Graph for a future macOS 27 update. [Apple deployment guide](https://support.apple.com/guide/deployment/integrate-with-microsoft-exchange-dep158966b23/web), [MacRumors](https://www.macrumors.com/2026/10/01/apple-mail-mac-stop-sync-microsoft-365/)
- **Outlook for Mac**: *new* Outlook is unaffected (cloud-first sync, doesn't use EWS; it even bridges IMAP accounts through Microsoft's sync service). *Classic* Outlook for Mac is EWS — its AppID needs allow-listing. [MC1485116](https://mc.merill.net/message/MC1485116), [Microsoft Q&A](https://learn.microsoft.com/en-us/answers/questions/5785155/outlook-sync-issues)
- **eM Client**: IMAP/POP3/EWS + **AirSync** (consumer Microsoft accounts) + MS Graph in v11; EWS users switch via re-login. [eM Client blog](https://www.emclient.com/blog/microsoft-is-retiring-exchange-web-services-ews-831), [compare page](https://www.emclient.com/how-we-compare?lang=en)
- **Spark**: stores OAuth tokens (Gmail/Outlook) or login+password (Exchange/IMAP) on Readdle's AWS servers for push/send-later/team features; connects via IMAP/EWS; caches ~1 month on device. [Spark privacy docs](https://support.readdle.com/spark/privacy/privacy-explained), [email storage docs](https://support.readdle.com/spark/spark-onboarding/email-storage-and-backups)
- **DavMail**: Java gateway translating IMAP/SMTP/POP/CalDAV/CardDAV/LDAP to Exchange OWA/EWS paths. Still actively released (6.4.0 Aug 2025, 6.5.0 Oct 2025 with WebView2/FIDO2 interactive auth); a Graph backend is "in progress but far from complete" — its EWS-based modes are on the same clock. Relevant as a stopgap, not a model. [DavMail news](https://sourceforge.net/p/davmail/news/2025/10/davmail-650-released/), [repo](https://github.com/mguessan/davmail/)

### 2.2 Google

#### 2.2.1 Gmail API vs IMAP

Both are viable; the trade-offs changed in 2026.

**Gmail API** (REST, JSON): threads/labels as first-class objects, `history.list` delta sync (2 units/call; `historyId` typically valid ≥1 week, else 404 → full resync), `users.watch` push, `messages.send`, filters/sendAs/settings APIs, and — new — an official **Gmail MCP server**. But: per-project quotas (below), API key/OAuth-project plumbing, and everything counts against your Cloud project across *all* users. [Gmail API quota page](https://developers.google.com/workspace/gmail/api/reference/quota), [users.history.list](https://developers.google.com/workspace/gmail/api/reference/rest/v1/users.history/list)

**IMAP**: standard wire protocol, works with XOAUTH2 or app passwords. Gmail exposes `X-GM-EXT-1` extensions: `X-GM-MSGID` (64-bit message ID matching the API's IDs), `X-GM-THRID` (thread/conversation ID), `X-GM-LABELS` (full label set per message; labels double as folders with `[Gmail]/` system names via `XLIST`), `X-GM-RAW` (Gmail web search syntax in `SEARCH`). With these, an IMAP backend recovers almost everything that made the API attractive — labels, conversations, server search — without per-project quotas. [Gmail IMAP extensions](https://developers.google.com/gmail/imap/imap-extensions)

**Quotas (updated 2026-05-01)**: per-minute 1,200,000 units/project and 6,000 units/user/project; **new daily threshold of 80,000,000 units/project beyond which charges apply** — billing details coming "later in 2026" under the "Google Workspace standardized model for agent tools and APIs". Per-method costs: `messages.get` 20, `messages.list` 5, `threads.get` 40, `messages.send` 100, `history.list` 2, `watch` 100, `batchModify` 50; 500 recipients/message cap. Projects active Nov 2025–Apr 2026 keep prior quotas; new projects from 2026-05-01 get the new ones. A full-mailbox initial sync through `messages.get` is the expensive path — another reason to sync via IMAP and reserve the API for account setup, sendAs/settings, or labels metadata. [Usage limits](https://developers.google.com/workspace/gmail/api/reference/quota)

#### 2.2.2 OAuth restricted scopes + CASA

- Google classifies 8 Gmail scopes as **restricted**, incl. `gmail.readonly`, `gmail.modify`, `gmail.compose`, `gmail.metadata`, `gmail.insert`, `gmail.settings.basic`, `gmail.settings.sharing`, and **`https://mail.google.com/` — the scope required for IMAP XOAUTH2**. So **IMAP does not dodge restricted-scope verification**. `gmail.send` is sensitive; `gmail.labels` is non-sensitive. [Nylas scope guide](https://developer.nylas.com/docs/provider-guides/google/google-verification-security-assessment-guide/)
- **Verification vs. assessment**: any app requesting restricted scopes needs OAuth app verification (brand verification → scope review → permitted-application-type review). The **annual CASA security assessment is required only for apps that access restricted data "from or through a third-party server"** — i.e., your own backend touches the Gmail data. A purely local desktop client where mail never transits vendor servers needs the verification but not CASA. (This is how Thunderbird et al. operate.) Assessments are via App Defense Alliance empanelled assessors, AL1/AL2 tiers, renewed every 12 months. Cost is negotiated per assessor; one vendor-adjacent analysis cites ~$540–$1,800/yr for CASA Tier 2 via TAC Security (single-source, treat as indicative). [Restricted scope verification](https://developers.google.com/identity/protocols/oauth2/production-readiness/restricted-scope-verification), [GCP security assessment](https://support.google.com/cloud/answer/13465431?hl=en), [Nango review guide](https://nango.dev/docs/api-integrations/google-shared/google-security-review), [CASA cost analysis](https://bright-softwares.com/blog/en/google-workspace/the-50000-gmail-add-on-myth-what-google-s-casa-certification-really-costs)
- **Password auth is dead**: "Less secure apps" were fully disabled — Workspace accounts March 14 / May 1, 2025; consumer accounts earlier in the rollout — for IMAP/POP/SMTP/CalDAV/CardDAV. What remains: OAuth (XOAUTH2) or **app passwords** (still generated at myaccount.google.com for 2FA users; Workspace admins can disable them). The per-user IMAP on/off toggle in Gmail settings was removed for Workspace. [Transition from less secure apps](https://support.google.com/a/answer/14114704?hl=en), [Workspace Updates blog](https://workspaceupdates.googleblog.com/2023/09/winding-down-google-sync-and-less-secure-apps-support.html)

#### 2.2.3 Push vs polling for a desktop app

`users.watch` publishes mailbox-change notifications to a **Cloud Pub/Sub topic in the developer's own GCP project** (100 units per call, watch must be renewed ~weekly). Subscriptions can be pull or webhook-push — but either way the subscriber authenticates to *your* Cloud project, not the user's Gmail account. A shipped desktop client has no credentials to your GCP project, so receiving pushes requires running a relay that subscribes on users' behalf — which means the relay touches restricted-scope data → **triggers CASA** and adds infra cost. For a local-first client: **poll `history.list`** (or use IMAP IDLE). [Push notifications guide](https://developers.google.com/workspace/gmail/api/guides/push), [users.watch](https://developers.google.com/workspace/gmail/api/reference/rest/v1/users/watch)

#### 2.2.4 Labels vs folders

Labels are multi-membership: one message can carry N labels; the INBOX label is just a label. Over IMAP, labels masquerade as folders and copying a message to a folder adds a label — move semantics diverge (delete from one "folder" ≠ remove from All Mail). `X-GM-LABELS` + `X-GM-MSGID` let a client present true label semantics and dedupe the same message across folders. [Gmail IMAP extensions](https://developers.google.com/gmail/imap/imap-extensions)

### 2.3 Proton

- **Bridge is the only officially supported third-party path.** It runs local IMAP4rev1 (default `127.0.0.1:1143`) + SMTP (`1025`), STARTTLS by default (switchable to implicit TLS), authenticates with a Bridge-generated password (not the account password), serves a self-signed cert (exportable for pinning). **Paid Proton plans only** — no IMAP on Free. No POP3. SMTP-only alternative: per-address **SMTP tokens** (`smtp.protonmail.ch:587`) for paid users with custom domains. GPL-3.0 app in Go on `ProtonMail/proton-bridge`. [Bridge repo](https://github.com/ProtonMail/proton-bridge), [IMAP/SMTP setup](https://proton.me/support/imap-smtp-and-pop3-setup), [port docs](https://proton.me/support/port-already-occupied-error), [Bridge overview](https://proton.me/mail/bridge), [why Bridge](https://proton.me/support/why-you-need-bridge)
- **Key libraries**: `gluon` — Proton's MIT-licensed Go IMAP4rev1 server library powering Bridge 3.x ("1000% faster"; Connector interface mirrors server state); `go-proton-api` — MIT-licensed Go client + dev-server implementing a subset of the Proton REST API (also forked by rclone for Proton Drive). [gluon](https://github.com/protonmail/gluon), [Gluon blog](https://proton.me/blog/gluon-imap-library), [go-proton-api](https://github.com/ProtonMail/go-proton-api)
- **Proton's Rust core is real but not yet usable.** Proton Mail v7 (Sept 2025 mobile relaunch) is SwiftUI/Jetpack Compose UI on a **shared Rust core doing all business logic (~80% shared code)** — exactly Sweetbriar's architecture. The "Proton Mail SDK" lives in the `ProtonMail/clients` monorepo (open-source mirror of their GitLab), but the iOS repo states it "cannot currently be built directly from GitHub… depends on the Proton Mail SDK… distributed internally"; Proton says they're working toward a buildable open stack. Public Rust crates exist for crypto only: `proton-crypto`, `proton-srp` (rustpgp-based). [Proton Mail v7 blog](https://proton.me/blog/new-mail-apps), [Engineering Transformation deep-dive](https://proton.me/blog/next-generation-proton-mail-mobile-apps), [ios-mail repo](https://github.com/ProtonMail/ios-mail/), [clients monorepo](https://github.com/ProtonMail/clients), [archived rust-mail](https://github.com/ProtonMail/rust-mail)
- **Direct API access is gray-zone.** Proton publishes no third-party mail API program; the ToS prohibit "accessing the Services through automated means… in a manner that is distinguishable from the standard client behavior" and reselling access. An unofficial pure-Rust client (`proton-core`, MIT, July 2026) demonstrates full feasibility — SRP login incl. 2FA/two-password/CAPTCHA, E2E send/read, drafts, filters, contacts, SQLite cache + local FTS — built on Proton's own `proton-crypto`/`proton-srp`, but it's tolerated at best, not sanctioned. [Proton ToS](https://proton.me/legal/terms), [protonmail-rs](https://github.com/filippofinke/protonmail-rs), [proton-core crate](https://crates.io/crates/proton-core)
- **Recommendation shape**: a "Proton" account type that auto-discovers a running Bridge (probe `127.0.0.1:1143`, guide the user through Bridge install/login), treats it as IMAP with known quirks (self-signed cert → trust-on-first-use or import Bridge's cert; labels exposed as folders), plus SMTP-token sending for custom domains. Keep a `proton-api` backend behind a feature flag for the day Proton opens its SDK.

### 2.4 Tuta

- **No third-party access, by design**: no IMAP/POP, no public API; Tuta says IMAP "would undermine our built-in encryption" and stores plaintext on-device only inside their own clients. The Thunderbird "add-ons" are just web-app embeds. Data export exists in their clients. [Tuta on no-IMAP](https://tuta.com/blog/desktop-clients-tutanota), [offline-mode post](https://tuta.com/blog/offline-support), [Thunderbird add-on](https://tuta.com/blog/tuta-add-on-in-thunderbird)
- Unofficial bridges exist (e.g. `tutabridge`, Rust+Tauri: local IMAP/SMTP over Tuta's private API + WebSocket event bus) and themselves warn they "go against Tuta's end-to-end philosophy… Tuta does not endorse". Legally it's ToS gray-zone; technically it's a maintenance treadmill against a private API. **Verdict: offer one-time import (via their export), not an account backend.** [tutabridge](https://github.com/spartanz51/tutabridge)

### 2.5 JMAP

- **Protocol family**: RFC 8620 (core), 8621 (mail), 8887 (WebSocket push), 9404 (blobs), 9425 (quotas), 9610 (contacts), 9661 (Sieve). **JMAP for Calendars** `draft-ietf-jmap-calendars-32` published 2026-10-05, in WG Last Call / RFC-Editor queue; **JSCalendar 2.0** submitted to IESG (2026-09-02), obsoleting RFC 8984. Tasks still in draft. [JMAP calendars draft](https://datatracker.ietf.org/doc/draft-ietf-jmap-calendars/), [JSCalendar bis](https://datatracker.ietf.org/doc/draft-ietf-calext-jscalendarbis/19/)
- **Fastmail**: full JMAP for mail **and contacts** (`urn:ietf:params:jmap:{core,mail,submission,contacts}` scopes), calendars via CalDAV pending spec finalization ("we will be opening up JMAP access as well, as soon as the specification is finalized"). Auth: **OAuth 2.0 authorization-code for distributed apps**, API tokens for personal/self-use (not on Basic plan; token types JMAP **or MCP** — Fastmail now ships an MCP endpoint too), app passwords for IMAP/POP/SMTP/CalDAV/CardDAV/WebDAV. JMAP session: `https://api.fastmail.com/jmap/session`. Explicitly friendly **API Developer Policy** — "we welcome Client Application developers". [Fastmail dev docs](https://www.fastmail.com/dev/), [API tokens](https://www.fastmail.help/hc/en-us/articles/5254602856719-API-tokens), [app passwords](https://www.fastmail.help/hc/en-us/articles/360058752854-App-passwords), [API Developer Policy](https://www.fastmail.com/policies/api-developer-policy/)
- **Servers**: Stalwart (Rust, AGPL-3.0/SELv2 dual license; JMAP mail+Sieve+WebSocket+blob+quotas, plus IMAP4rev2/POP3/SMTP/CalDAV/CardDAV/WebDAV); Cyrus IMAP (RFC 8620/8621, blobs, quotas, Sieve, WebSocket; calendars/contacts/sharing in development; no real JMAP auth — HTTP basic only); Apache James (JMAP `rfc-8621` + WebSocket). [Stalwart](https://stalw.art/), [stalwart repo](https://github.com/stalwartlabs/mail-server/), [Cyrus JMAP docs](https://www.cyrusimap.org/imap/download/installation/http/jmap.html), [Apache James](https://james.apache.org/)
- **Clients**: still thin — Ltt.rs (Android), Cyrus's sample web client, Fastmail's own UI; Thunderbird lists JMAP on the roadmap post-Exchange work. Green field for Sweetbriar. [Cyrus JMAP docs](https://www.cyrusimap.org/imap/download/installation/http/jmap.html), [Thunderbird:Exchange](https://wiki.mozilla.org/Thunderbird:Exchange)

### 2.6 Other providers + local formats

- **iCloud Mail**: IMAP `imap.mail.me.com:993` + SMTP `smtp.mail.me.com:587`; no POP. Auth: **app-specific password** (requires 2FA) — or, new, **OAuth authorization via Apple Account for "supported third-party apps"**: Microsoft rolled it into new Outlook (Mac/Windows/mobile) in 2025. Apple does not publish the partner list or a public enrollment path — treat OAuth as "request access from Apple; fall back to app-specific password". [Apple server settings](https://support.apple.com/en-us/102525), [app-specific passwords](https://support.apple.com/en-us/102654), [third-party authorization](https://support.apple.com/en-us/121539), [How-To Geek on Outlook+iCloud OAuth](https://www.howtogeek.com/outlook-finally-has-better-icloud-support/)
- **Yahoo/AOL**: IMAP/POP/SMTP with **app passwords** always works; **OAuth2 for IMAP (OAUTHBEARER) + SMTP (XOAUTH2) requires applying** via Yahoo's developer-access form (scopes are not self-serve; separate credentials per Yahoo and AOL namespaces). Apply early — it's a manual review. [Yahoo developer access](https://senders.yahooinc.com/developer/developer-access/), [Yahoo docs](https://senders.yahooinc.com/developer/documentation/), [Yahoo OAuth guide](https://developer.yahoo.com/oauth2/guide/)
- **Zoho Mail**: real **REST API + OAuth 2.0** (messages, folders, tags, attachments, sendAs; self-serve client registration; multi-DC accounts.zoho.{com,eu,in…}); IMAP/POP also work with XOAUTH2 via self-registered clients (`ZOHOMAIL.PROTOCOL.IMAP`/`.POP` scopes). The friendliest API after Fastmail. [Zoho OAuth guide](https://www.zoho.com/mail/help/api/using-oauth-2.html), [Zoho API index](https://www.zoho.com/mail/help/api/), [Zoho XOAUTH2](https://www.zoho.com/mail/help/sasl-xoauth2-smtp-imap-pop-protocol-exchange.html)
- **GMX / Web.de / Posteo / mailbox.org** (Germany): IMAP/SMTP/CalDAV/CardDAV with **app-specific passwords** (GMX/Web.de, Posteo, mailbox.org "Login 2.0" app passwords); no third-party OAuth. Standard backend + guided app-password UX. [GMX app passwords](https://support.gmx.com/security/2fa/application-specific-passwords.html), [Posteo](https://posteo.de/en/help/app-passwords), [mailbox.org](https://kb.mailbox.org/en/private/security-and-privacy/application-passwords-for-external-programs/)
- **QQ Mail / NetEase 163** (China): IMAP/POP/SMTP enabled per-account in webmail settings + **authorization code** (授权码) instead of password; same pattern at Sina/Aliyun (unverified for those two). [NetEase/163 via Aliyun docs](https://help.aliyun.com/en/model-studio/connector/netease-mail), [QQ settings](https://smtpedia.com/qq-com-email-settings-pop3-imap-and-smtp/)
- **Migadu / Purelymail / Fastmail-class indies**: plain IMAP+SMTP (+app passwords); covered by the generic backend.
- **HEY (37signals)**: no IMAP/POP/SMTP at all, any plan — by design ("screening/Imbox/Paper Trail can't be represented as folders"). Only official apps + one-shot **MBOX export**. A third-party report mentions a `hey-cli`/MCP tool shipped by 37signals (unverified — couldn't confirm on hey.com). **Import-only.** [HEY on IMAP](https://help.hey.com/article/719-can-i-use-hey-to-check-my-other-email-accounts), [HEY export](https://help.hey.com/article/730-can-i-export-my-email-and-contacts)
- **POP3**: still a real account type (providers still offer it; Proton notably doesn't). Support "download + keep on server" modes; POP3 has no folders — flatten to Inbox.
- **Local stores as accounts**: mbox (the de-facto archive format — Thunderbird, Apple Mail export, HEY export) and Maildir (Evolution, dovecot, public-inbox). Read-write local folders double as the "never lose data" safety net (auto-copy sent/deleted mail to a local folder = Eudora-style safety).
- **Mailing lists / archives**: NNTP is alive in dev communities — **public-inbox** serves lore.kernel.org over NNTP, IMAP (anonymous), POP3, Atom, git clone, mbox download; the `lei` tool does saved searches. A read-only NNTP/public-inbox "archive" account type is cheap and delights the kernel-dev crowd. [public-inbox README](https://public-inbox.org/README), [lore.kernel.org docs](https://kernel.org/doc/projects/korg/lore.html)

### 2.7 How existing clients structure multi-backend support

- **Thunderbird** — account objects bind an incoming-server implementation to folder/message abstractions; the UI talks to protocol-agnostic `nsIMsg*` interfaces. New protocols (EWS) slot in as Rust crates behind XPCOM (`IExchangeClient`, `ews_xpcom`/`graph_xpcom`); shared local store (mbox + global-messages-db for search) stays protocol-agnostic. Lesson: protocol code isolated behind a narrow interface; reuse Thunderbird's MPL crates where possible. [Exchange architecture](https://source-docs.thunderbird.net/en/latest/exchange/architecture.html), [OAuth module docs](https://source-docs.thunderbird.net/en/latest/backend/oauth.html)
- **Evolution (GNOME)** — `CamelProvider` plugin mechanism: each backend registers a `CamelStore` (folders/messages) and/or `CamelTransport` (send); EDS handles contacts/calendar similarly via `EBackend`s. Providers: imapx, pop3, nntp, mbox, maildir, EWS (evo-ews), MAPI (legacy). Lesson: clean store/transport split, but the all-in-process plugin model ties backends to Camel's lifetime. [Camel.Provider](https://wiki.gnome.org/Apps(2f)Evolution(2f)Camel(2e)Provider.html), [EDS architecture](https://wiki.gnome.org/Apps/Evolution/EDS_Architecture)
- **Mailspring** — most instructive modern design: **one `mailsync` C++ process per account** (mailcore2 IMAP/SMTP + SQLite-in-JSON store); the Electron UI is *read-only on the DB* and sends "tasks" (local-write + retryable remote-op) over stdin; the engine streams JSON deltas on stdout that drive observable queries. Foreground IDLE worker + background folder resync (CONDSTORE). Lesson: process isolation = crash/auth-failure containment; task queue = offline-resilient writes; observable DB = live UI without races. [Mailspring CLAUDE.md](https://github.com/Foundry376/Mailspring/blob/master/CLAUDE.md), [Mailspring-Sync](https://github.com/Foundry376/Mailspring-Sync)
- **Spark** — server-centric: credentials live on Readdle's AWS; their servers poll the mailbox for pushes/send-later/read-receipts/team features; device caches ~30 days. Great mobile UX, heavy privacy trust ask — Sweetbriar's local-first stance is the deliberate opposite. [Spark privacy](https://support.readdle.com/spark/privacy/privacy-explained)
- **Apple Mail** — two Exchange code paths: **EAS on iOS, EWS on macOS** (both OAuth-able for O365 since ~2018-2020); currently transitioning Mac apps to Graph. [Apple deployment](https://support.apple.com/guide/deployment/integrate-with-microsoft-exchange-dep158966b23/web)
- **Outlook for Mac** — *new* Outlook: cloud-first sync through Microsoft's own service (mailbox state cached in Microsoft's cloud; IMAP accounts get bridged through it too). *Classic* Mac Outlook: EWS. [MC1485116](https://mc.merill.net/message/MC1485116)
- **eM Client** — protocol matrix: IMAP, POP3, EWS, AirSync (consumer MSA), MS Graph (v11); server rules for Exchange/IceWarp; their 2-year Graph bake-off is a warning about Graph edge-cases. [eM Client compare](https://www.emclient.com/how-we-compare?lang=en), [forum](https://forum.emclient.com/t/microsoft-graph-support/109139)

## 3. Options compared

### 3.1 Microsoft 365 / Exchange Online access paths

| Path | Status 2026-10 | Pros | Cons |
|---|---|---|---|
| **Microsoft Graph** | Required going forward | Modern REST+OAuth, delta sync, MIME $value, search/rules/categories, shared mailboxes | Webhooks need public endpoint; throttling 10k/10min; parity gaps vs EWS; admin-consent friction |
| **EWS** | Blocking started Oct 1, 2026; dead Apr 1, 2027 | Rich, mature, on-prem forever; Thunderbird's Rust impl is MPL | Dead end for Exchange Online except allow-listed grace window |
| **IMAP+SMTP OAuth** | Alive | Simple, lowest Graph-permission ask (IMAP.AccessAsUser.All, SMTP.Send) | Tenants may disable protocols; no server-side rules/calendar; basic-ish |
| **EAS** | Alive, licensed | Push, mature | License program (fees, legal), mobile-centric, legacy |
| **DavMail gateway** | Works, Graph backend incomplete | No app registration needed locally | Java sidecar, OWA/EWS-based modes on the same retirement clock |

### 3.2 Gmail access paths

| Path | Auth friction | Push | Quotas | Notes |
|---|---|---|---|---|
| **IMAP + XOAUTH2** | Restricted scope `mail.google.com` → app verification; no CASA if local-only | IDLE | Provider-level only | X-GM-* extensions recover labels/threads/search |
| **IMAP + app password** | None (user-generated) | IDLE | Provider-level | Bypasses OAuth UX; Workspace admins may block |
| **Gmail API** | Same restricted-scope verification; CASA only if your servers touch data | watch→Pub/Sub (needs your relay) | 1.2M/min/project, 6k/min/user, **80M/day → billing** | Threads/labels/settings native; send 100u, watch 100u |

### 3.3 Provider matrix

| Provider | Read | Send | Auth | Notes |
|---|---|---|---|---|
| Exchange Online | Graph, IMAP | Graph, SMTP OAuth | Entra OAuth | EWS dying; admin consent may block |
| Exchange on-prem | EWS | EWS/SMTP | Basic/NTLM/OAuth | Long-lived backend |
| Outlook.com (consumer) | Graph/IMAP/EAS | same | MSA OAuth | AirSync covers calendar for some clients |
| Gmail | IMAP/API | SMTP/API | OAuth or app pw | LSA dead since 2025 |
| iCloud | IMAP | SMTP | app pw (OAuth: partners only) | no POP |
| Yahoo/AOL | IMAP/POP | SMTP | app pw / OAuth by approval | apply early |
| Proton | Bridge→local IMAP | Bridge→SMTP / SMTP tokens | Bridge pw | paid only; direct API unsanctioned |
| Tuta | — | — | — | impossible; import only |
| HEY | — | — | — | impossible; MBOX import only |
| Fastmail | JMAP/IMAP/POP | JMAP/SMTP | OAuth/API token/app pw | friendliest |
| Zoho | IMAP/API | SMTP/API | OAuth self-serve | full REST API |
| GMX/Web.de/Posteo/mailbox.org | IMAP/POP | SMTP | app pw | Germany, no OAuth |
| QQ/163 | IMAP/POP | SMTP | auth code | per-account enable + code |
| Stalwart/Cyrus/James | JMAP/IMAP | JMAP/SMTP | app creds | self-host market |
| public-inbox (lists) | NNTP/IMAP | — | anonymous | read-only archives |
| local | mbox/Maildir | — | fs | archive + safety net |

## 4. Recommendations for Sweetbriar

### 4.1 Internal data model: yes, shape it like JMAP

Define one canonical object model in the Rust core — Mailbox / Thread / Email / EmailSubmission / Identity / Blob / VacationResponse — with JMAP semantics, then map each backend onto it:

- **`mailboxIds` (set membership) is the killer feature.** It expresses IMAP folders (exactly one), Gmail labels (many), Graph folders (one) + categories (many, orthogonal flag set), and Proton labels — one column, one index. Flags → `keywords` (`$seen`, `$flagged`, `$draft`, `$answered`, plus custom keywords for tags).
- **Blob/binary separation**: message metadata vs. body parts vs. attachment blobs with lazy fetch — matches JMAP `blobId`, Graph attachments, and MIME parts identically.
- **Changes/sync states**: model each mailbox's sync cursor opaquely per backend (IMAP `UIDVALIDITY`+`HIGHESTMODSEQ`, Graph `deltaToken`, Gmail `historyId`, JMAP `state`). One `SyncState` type, backend-serialized payloads.
- **Don't speak JMAP on the wire internally** — just reuse the data shapes and `/changes` semantics so the real JMAP backend becomes trivial and the model stays honest. Keep a `raw` escape hatch (RFC 822 + provider IDs) for fidelity.

Strongest counter-argument: JMAP's model is a straitjacket — Graph messages, Proton conversations, and IMAP all leak provider-specific features (Focused Inbox, Proton labels+E2E state, Exchange sensitivity headers). Mitigate with an extensible `providerData` JSON sidecar per object rather than bloating the schema; keep the common path protocol-clean.

### 4.2 Capability model

Each backend implements `AccountBackend` (folders/mailboxes CRUD, message fetch, send) plus a runtime-declared capability set — a struct of booleans + trait objects, queried by the UI:

```
capabilities {
  container_model: Folders | Labels | FoldersPlusTags   // drives UI
  multi_membership: bool                               // labels vs copy-to
  server_search: None | Syntax(query_lang)             // IMAP SEARCH / KQL / X-GM-RAW / JMAP Filter / Zoho / Proton
  push: None | Idle | WebSocket | LongPoll             // IMAP IDLE / JMAP WS / none → poll
  threads: Server | ReferencesOnly
  categories: bool | NamedTags                          // Graph categories, X-GM-LABELS, IMAP KEYWORDS
  rules: None | ServerSide(engine)                      // messageRules / filters API / Sieve
  send_later: bool | LocalOnly                          // Graph/Exchange submit-time? mostly local
  mime_fetch: bool, mime_send: bool
  quotas: bool, shared_mailboxes: bool
  archive_semantics: AllMail | ArchiveFolder | None
}
```

UI greys out what a backend can't do instead of guessing — and sync drivers each get their own Tokio task per account, writing exclusively to the store (Mailspring's proven pattern), with an op-log/task queue for offline-resilient mutations and every UI write going through `Task{local_op, remote_op}`.

### 4.3 Implementation order

1. **IMAP4rev2+rev1 client + SMTP submission** with X-GM-EXT-1, CONDSTORE, QRESYNC, IDLE, MOVE, SPECIAL-USE, LIST-EXTENDED, OAUTHBEARER/XOAUTH2. One backend covers Gmail, iCloud, Yahoo, Fastmail, Zoho, Proton-via-Bridge, QQ/163, GMX, Migadu, dovecot hosts — i.e., everything non-Microsoft. Include per-provider preset profiles + app-password UX.
2. **Microsoft Graph** (Exchange Online + Outlook.com): delta-sync engine, MIME $value, sendMail, upload sessions, KQL search, categories; multi-tenant Entra app + publisher verification early (lead time!). This is where the market hole opened on Oct 1.
3. **EWS** for on-prem Exchange + allow-listed tenants — evaluate lifting Thunderbird's MPL `ews`/`ews_xpcom` Rust crates; also future-proofs Graph parity work.
4. **JMAP** — Fastmail, Stalwart, Cyrus (contacts now; calendars when RFC lands; it lands your model 1:1).
5. **Proton**: Bridge-aware IMAP profile now (port probe, cert handling, label folders); direct API only if Proton opens the SDK or blesses third parties — monitor `ProtonMail/clients`.
6. **Local**: mbox/Maildir import + local-folder accounts; POP3 fetch; **public-inbox/NNTP** read-only archive source.
7. Stretch: Zoho REST backend (cheap once OAuth plumbing exists), Yahoo OAuth after approval, iCloud OAuth if Apple admits Sweetbriar.

### 4.4 Legal/policy/cost risk per provider

| Provider | Risk |
|---|---|
| Microsoft | Entra publisher verification (CPP account, time); tenant admin-consent walls; Graph throttling; EWS hard-stop Apr 2027 |
| Google | Restricted-scope verification (weeks); CASA mandatory only if your servers touch mail data — keep architecture local-only; new per-project daily quota → future billing |
| Apple | OAuth closed to partners; app-password UX friction; iCloud quirks (no POP, username nuances) |
| Yahoo | OAuth approval is manual/uncertain — ship app-password path first |
| Proton | No sanctioned third-party API; Bridge = paid users only; direct API = ToS gray-zone (clause on non-standard client behavior) |
| Tuta/HEY | Impossible by policy/design — set expectations, offer importers |
| Zoho | Low — self-serve OAuth, documented scopes |
| China providers | Per-account enablement + SMS-gated auth codes; docs mostly in Chinese |
| EAS | Paid license + legal review — avoid entirely |

## 5. Risks and open questions

- **Graph edge-cases**: eM Client's 2-year Graph holdback (distribution lists on outlook.com, "buggy…changing all the time") — budget soak time; verify shared-mailbox send + large-attachment known issues. [forum](https://forum.emclient.com/t/microsoft-graph-support/109139)
- **Webhook alternative for desktops**: Graph offers Event Hubs delivery for notifications — worth checking if it still requires cloud resources (likely yes; unverified for delegated desktop scenarios).
- **Does iCloud OAuth admit new partners?** Apple docs only say "supported third-party apps"; enrollment path unknown — contact Apple Developer Relations.
- **Proton Mail SDK openness**: the clients monorepo is a sanitized mirror; unclear when/if the Rust SDK becomes buildable or licensed for third-party use — watch the repo; alternatively evaluate `go-proton-api`-equivalent in Rust (proton-core shows feasibility; ToS risk stands).
- **Gmail billing model "later in 2026"**: exact pricing for >80M units/day not yet published — keep Gmail API optional so a price shock can't hurt you.
- **Yahoo OAuth approval**: timeline/approval odds for a new client unknown; mail-api@yahooinc.com contact needed.
- **Microsoft Graph vs consumer Outlook.com**: verify Outlook.com supports all needed Graph delegated scopes via the MSA+Org multi-tenant app type (personal accounts historically had gaps — e.g., delta on some resources).
- **Tuta/HEY**: any change in stance? Recheck at launch; both actively market lock-in.
- **Public-inbox IMAP anonymous auth** is nonstandard — test early.
- **Exchange SE / on-prem OAuth**: NTLM still common; Thunderbird itself lacks NTLM yet — scope the effort.
- **Apple Mail Graph migration in macOS 27** may partially close the M365-on-Mac window; still leaves on-prem+EWS and every non-Apple client gap.

## 6. Sources

Microsoft:
- [Deprecation of EWS in Exchange Online — Microsoft Learn](https://learn.microsoft.com/en-us/exchange/clients-and-mobile-in-exchange-online/deprecation-of-ews-exchange-online)
- [Retirement of EWS in Exchange Online — TechCommunity](https://techcommunity.microsoft.com/blog/exchange/retirement-of-exchange-web-services-in-exchange-online/3924440)
- [MC1485116 — EWSAllowedAppIDs enforcement](https://mc.merill.net/message/MC1485116), [MC1227454](https://mc.merill.net/message/MC1227454), [MC1447678](https://mc.merill.net/message/MC1447678), [MC786329](https://mc.merill.net/message/MC786329)
- [Updated SMTP AUTH basic-auth deprecation timeline](https://techcommunity.microsoft.com/blog/exchange/updated-exchange-online-smtp-auth-basic-authentication-deprecation-timeline/4489835), [original post](https://techcommunity.microsoft.com/blog/exchange/exchange-online-to-retire-basic-auth-for-client-submission-smtp-auth/4114750), [Office365ITPros](https://office365itpros.com/2026/01/29/smtp-auth-basic-retirement/)
- [Deprecation of basic auth in Exchange Online](https://learn.microsoft.com/en-us/exchange/clients-and-mobile-in-exchange-online/deprecation-of-basic-authentication-exchange-online)
- Graph: [delta query messages](https://learn.microsoft.com/en-us/graph/delta-query-messages), [message: delta](https://learn.microsoft.com/en-us/graph/api/message-delta?view=graph-rest-1.0), [change notifications overview](https://learn.microsoft.com/en-us/graph/outlook-change-notifications-overview), [webhook delivery](https://learn.microsoft.com/en-us/graph/change-notifications-delivery-webhooks), [mail API overview](https://learn.microsoft.com/en-us/graph/api/resources/mail-api-overview?view=graph-rest-1.0), [createUploadSession](https://learn.microsoft.com/en-us/graph/api/attachment-createuploadsession?view=graph-rest-1.0), [large attachments](https://learn.microsoft.com/en-us/graph/outlook-large-attachments), [throttling limits](https://learn.microsoft.com/en-us/graph/throttling-limits), [throttling guidance](https://learn.microsoft.com/en-us/graph/throttling), [EWS→Graph mappings](https://learn.microsoft.com/en-us/graph/migrate-exchange-web-services-api-mapping)
- Entra: [register an app](https://learn.microsoft.com/en-us/entra/identity-platform/quickstart-register-app), [consent experience](https://learn.microsoft.com/en-us/entra/identity-platform/application-consent-experience), [publisher verification](https://learn.microsoft.com/en-us/entra/identity-platform/publisher-verification-overview), [establish applications](https://learn.microsoft.com/en-us/entra/architecture/establish-applications)
- EAS: [developer support/licensing](https://devblogs.microsoft.com/premier-developer/client-side-exchange-activesync-eas-developers-support-options/), [EAS intro](https://devblogs.microsoft.com/premier-developer/microsoft-exchange-activesync/), [2008 licensing expansion](https://news.microsoft.com/source/2008/12/18/microsoft-expands-exchange-active-sync-licensing-program/), [ZDNet fees](https://www.zdnet.com/article/in-apples-footsteps-google-licenses-microsoft-activesync/)
- Apple/others: [Apple Exchange integration](https://support.apple.com/guide/deployment/integrate-with-microsoft-exchange-dep158966b23/web), [EWS payload](https://support.apple.com/guide/deployment/exchange-web-services-ews-payload-settings-depd86788a3/web), [MacRumors Oct 1 2026](https://www.macrumors.com/2026/10/01/apple-mail-mac-stop-sync-microsoft-365/), [Outlook Q&A](https://learn.microsoft.com/en-us/answers/questions/5785155/outlook-sync-issues), [eM Client compare](https://www.emclient.com/how-we-compare?lang=en), [eM Client EWS blog](https://www.emclient.com/blog/microsoft-is-retiring-exchange-web-services-ews-831), [eM Client forum](https://forum.emclient.com/t/microsoft-graph-support/109139), [DavMail 6.5.0](https://sourceforge.net/p/davmail/news/2025/10/davmail-650-released/), [davmail repo](https://github.com/mguessan/davmail/)
- Thunderbird: [Exchange support blog](https://blog.thunderbird.net/2025/11/thunderbird-adds-native-microsoft-exchange-email-support/), [Adventures in Rust](https://blog.thunderbird.net/2024/04/adventures-in-rust-bringing-exchange-support-to-thunderbird/), [MozillaWiki Exchange](https://wiki.mozilla.org/Thunderbird:Exchange), [code architecture](https://source-docs.thunderbird.net/en/latest/exchange/architecture.html), [OAuth docs](https://source-docs.thunderbird.net/en/latest/backend/oauth.html)

Google:
- [Gmail API usage limits](https://developers.google.com/workspace/gmail/api/reference/quota), [push notifications](https://developers.google.com/workspace/gmail/api/guides/push), [users.watch](https://developers.google.com/workspace/gmail/api/reference/rest/v1/users/watch), [history.list](https://developers.google.com/workspace/gmail/api/reference/rest/v1/users.history/list)
- [Gmail IMAP extensions](https://developers.google.com/gmail/imap/imap-extensions)
- [Restricted scope verification](https://developers.google.com/identity/protocols/oauth2/production-readiness/restricted-scope-verification), [GCP security assessment](https://support.google.com/cloud/answer/13465431?hl=en), [Nylas scope guide](https://developer.nylas.com/docs/provider-guides/google/google-verification-security-assessment-guide/), [Nango guide](https://nango.dev/docs/api-integrations/google-shared/google-security-review), [CASA cost blog](https://bright-softwares.com/blog/en/google-workspace/the-50000-gmail-add-on-myth-what-google-s-casa-certification-really-costs)
- [Less-secure-apps transition](https://support.google.com/a/answer/14114704?hl=en), [Workspace Updates blog](https://workspaceupdates.googleblog.com/2023/09/winding-down-google-sync-and-less-secure-apps-support.html), [LSA help](https://support.google.com/accounts/answer/6010255)

Proton / Tuta:
- [proton-bridge](https://github.com/ProtonMail/proton-bridge), [gluon](https://github.com/protonmail/gluon), [Gluon blog](https://proton.me/blog/gluon-imap-library), [go-proton-api](https://github.com/ProtonMail/go-proton-api), [rclone fork](https://github.com/rclone/go-proton-api)
- [IMAP/SMTP setup](https://proton.me/support/imap-smtp-and-pop3-setup), [Bridge page](https://proton.me/mail/bridge), [why Bridge](https://proton.me/support/why-you-need-bridge), [ports](https://proton.me/support/port-already-occupied-error), [Bridge settings guide](https://proton.me/support/comprehensive-guide-to-bridge-settings)
- [Proton ToS](https://proton.me/legal/terms), [Mail v7 blog](https://proton.me/blog/new-mail-apps), [Engineering Transformation](https://proton.me/blog/next-generation-proton-mail-mobile-apps), [ios-mail](https://github.com/ProtonMail/ios-mail/), [clients monorepo](https://github.com/ProtonMail/clients), [rust-mail (archived)](https://github.com/ProtonMail/rust-mail), [protonmail-rs](https://github.com/filippofinke/protonmail-rs), [proton-core](https://crates.io/crates/proton-core)
- Tuta: [desktop clients](https://tuta.com/blog/desktop-clients-tutanota), [security page](https://tuta.com/security), [offline mode](https://tuta.com/blog/offline-support), [Thunderbird add-ons](https://tuta.com/blog/tuta-add-on-in-thunderbird), [tutabridge](https://github.com/spartanz51/tutabridge)

JMAP:
- [Fastmail dev](https://www.fastmail.com/dev/), [API tokens](https://www.fastmail.help/hc/en-us/articles/5254602856719-API-tokens), [app passwords](https://www.fastmail.help/hc/en-us/articles/360058752854-App-passwords), [API Developer Policy](https://www.fastmail.com/policies/api-developer-policy/)
- [draft-ietf-jmap-calendars](https://datatracker.ietf.org/doc/draft-ietf-jmap-calendars/), [JSCalendar bis](https://datatracker.ietf.org/doc/draft-ietf-calext-jscalendarbis/19/), [Stalwart](https://stalw.art/), [stalwart repo](https://github.com/stalwartlabs/mail-server/), [Vandelay migration tool](https://stalw.art/blog/jmap-account-migration/), [Cyrus JMAP](https://www.cyrusimap.org/imap/download/installation/http/jmap.html), [Apache James](https://james.apache.org/)

Other providers:
- iCloud: [server settings](https://support.apple.com/en-us/102525), [app passwords](https://support.apple.com/en-us/102654), [third-party authorization](https://support.apple.com/en-us/121539), [How-To Geek iCloud OAuth](https://www.howtogeek.com/outlook-finally-has-better-icloud-support/)
- Yahoo: [developer access](https://senders.yahooinc.com/developer/developer-access/), [documentation](https://senders.yahooinc.com/developer/documentation/), [OAuth guide](https://developer.yahoo.com/oauth2/guide/), [Nylas Yahoo guide](https://developer.nylas.com/docs/provider-guides/yahoo-authentication/)
- Zoho: [OAuth guide](https://www.zoho.com/mail/help/api/using-oauth-2.html), [API index](https://www.zoho.com/mail/help/api/), [getting started](https://www.zoho.com/mail/help/api/getting-started-with-api.html), [XOAUTH2](https://www.zoho.com/mail/help/sasl-xoauth2-smtp-imap-pop-protocol-exchange.html)
- Europe: [GMX app passwords](https://support.gmx.com/security/2fa/application-specific-passwords.html), [Posteo](https://posteo.de/en/help/app-passwords), [mailbox.org config](https://kb.mailbox.org/en/private/e-mail/e-mail-configuration/), [mailbox.org app passwords](https://kb.mailbox.org/en/private/security-and-privacy/application-passwords-for-external-programs/), [mailbox.org Login 2.0](https://mailbox.org/en/news/the-new-login/)
- China: [NetEase/163 via Aliyun](https://help.aliyun.com/en/model-studio/connector/netease-mail), [QQ settings](https://smtpedia.com/qq-com-email-settings-pop3-imap-and-smtp/)
- HEY: [no IMAP](https://help.hey.com/article/719-can-i-use-hey-to-check-my-other-email-accounts), [export](https://help.hey.com/article/730-can-i-export-my-email-and-contacts), [import policy](https://help.hey.com/article/754-can-we-import-our-old-emails)
- Lists: [public-inbox README](https://public-inbox.org/README), [lore.kernel.org](https://kernel.org/doc/projects/korg/lore.html), [lore+lei](https://kernel.org/doc/projects/korg/lore-lei.html)

Client architectures:
- [Mailspring CLAUDE.md](https://github.com/Foundry376/Mailspring/blob/master/CLAUDE.md), [Mailspring-Sync](https://github.com/Foundry376/Mailspring-Sync), [Spark privacy](https://support.readdle.com/spark/privacy/privacy-explained), [Spark storage](https://support.readdle.com/spark/spark-onboarding/email-storage-and-backups), [Readdle account info blog](https://readdle.com/blog/how-we-handle-your-account-information-in-spark), [Camel.Provider](https://wiki.gnome.org/Apps(2f)Evolution(2f)Camel(2e)Provider.html), [EDS architecture](https://wiki.gnome.org/Apps/Evolution/EDS_Architecture)

*Report compiled 2026-10-11. Facts dated as verified on that date; policies shift — re-verify before committing to a provider roadmap.*
