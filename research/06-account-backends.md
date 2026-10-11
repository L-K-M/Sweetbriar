# 06 — Account backends: IMAP, Exchange/Microsoft 365, Gmail, Proton, JMAP and more

Synthesis of [raw/sources/devin.md](raw/sources/devin.md) and
[raw/sources/codex.md](raw/sources/codex.md), with input from the
standards, performance, data-safety and Rust-ecosystem reports. Dates and
policies were checked 2026-10-11. The EWS timeline, Apple Mail's
dependence on EWS, Thunderbird's Graph support and Gmail's end of
third-party Send-as were verified by hand.

## Summary

- **Direct, independent protocol drivers behind one durable Rust store.**
  Every driver implements a common `AccountBackend` trait with declared
  capabilities; none writes the store directly (see
  [01-architecture](01-architecture.md)).
- **Exchange Online means Microsoft Graph, now.** EWS blocking started
  2026-10-01; from the 2026-10-10 rollout, Worldwide tenants keeping or
  re-enabling EWS must list allowed apps in `EWSAllowedAppIDs` (other
  clouds have separate schedules); permanent shutdown
  is 2027-04-01 with no extensions. On-prem Exchange keeps EWS.
  - Apple Mail on Mac still uses EWS. Apple promises Graph in a *future*
    macOS 27 update, with no date.
  - **This is a market opening for a native Mac client today.** Thunderbird
    (Graph since 154, August 2026) and eM Client (11, 2026-10-07) already
    moved.
- **Release gates are approvals, not code.** Start them immediately:
  - Microsoft's managed consent policy (the default for new tenants)
    excludes a listed set of Graph permissions from ordinary user consent
    (`Mail.Read`, `Mail.ReadWrite`, `Mail.ReadBasic` and their shared
    variants, `MailboxItem.Read`, `MailBoxFolder.*`, `MailBoxSettings.*`,
    calendars, contacts and more; `Mail.Send` is not on the list) and
    Exchange `IMAP`/`POP`/`EWS`/`EAS.AccessAsUser.All`. Other tenant
    policies can still grant consent; a second policy lets named
    clients through (Apple Mail, Spark, eM Client, Thunderbird, two
    Android clients). **Sweetbriar won't be listed, and choosing IMAP
    does not escape this**, so enterprise onboarding needs admin consent,
    docs and publisher verification (verified 2026-10-11).
  - Google restricted-scope verification applies to IMAP's
    `https://mail.google.com/` and the Gmail API alike. CASA applies if
    restricted data can be accessed "from or through" third-party servers,
    including the ability to access it. Content-free aggregate diagnostics
    don't automatically trigger it, but crash reports or protocol traces
    carrying mail data, or cloud features, could. Get Google's written
    determination for the actual data flows.
  - Yahoo and AOL OAuth needs manual approval.
- **Proton:** Bridge (local IMAP/SMTP, paid plans) is the only sanctioned
  path. Proton's Rust Mail core is public, but its `mail-uniffi` binding is
  **AGPL-3.0-only**, and there is no third-party API program. Direct
  integration needs Proton's written agreement.
- **Tuta and HEY:** no third-party access by design. Import and export
  workflows only.
- **The internal model uses JMAP vocabulary but is occurrence-based**
  (revised after the debate): immutable acquisitions; remote occurrences
  with occurrence-scoped flags (IMAP mailbox + UIDVALIDITY + UID);
  provider objects keyed by provider ids (Gmail account + message id, JMAP
  Email id, Graph mailbox + immutable id); typed folder/label membership;
  and Graph categories and other tags as a **separate** collection, never
  mailbox membership (a Graph message has one `parentFolderId` plus
  `categories`). Sync cursors are opaque per backend.

## Microsoft

| Path | Status 2026-10-11 | Use |
|---|---|---|
| **Microsoft Graph** | Required for Exchange Online and Outlook.com rich access | Primary Microsoft driver |
| EWS | Exchange Online: blocking from 2026-10-01, dead 2027-04-01. On-prem (Exchange SE): supported | On-prem only, via Thunderbird's MPL `ews` crate. No Exchange Online launch path on EWS |
| IMAP/SMTP + OAuth | Works, but tenants can disable IMAP/POP per mailbox; Security Defaults disables SMTP AUTH; SMTP basic auth off by default from end of Dec 2026 | Fallback; don't ask enterprises to loosen policy |
| EAS (ActiveSync) | Licensable (Microsoft licensing program), mobile-shaped | Skip unless a customer requires it |
| DavMail | 7.0.0 (2026-09-19) strengthens the Graph backend first shipped in 6.8.0 (2026-06-12); Java, GPL | User-configured gateway, not embedded |

**Graph implementation notes** (codex, verified against Microsoft Learn):

- **Folders and messages:** `mailFolders/delta` for the hierarchy;
  per-folder `messages/delta`. Follow `nextLink` until `deltaLink` and keep
  the full URL. Delta doesn't support `$search`. A `receivedDateTime`
  `$filter` caps delta at 5,000 messages: use it only as a recent-mail
  fast path, never as proof of complete coverage (verified 2026-10-11).
- **Ids:** send `Prefer: IdType="ImmutableId"` everywhere. Ids survive
  moves within one mailbox, but not moves into archive mailboxes or
  export/reimport. Namespace them by mailbox.
- **MIME:** `GET /messages/{id}/$value` returns MIME. Keep it with the
  metadata, but don't assume it's byte-identical to the original SMTP
  transaction.
- **Sending:** create a draft (JSON or MIME), persist its immutable id,
  send that draft, then find the Sent copy by the same id. This needs
  `Mail.Send` (not included in `Mail.ReadWrite`). `202 Accepted` means
  accepted, not delivered. A missing Sent copy never authorizes
  resubmission. Direct `sendMail` gives weaker correlation evidence.
- **Attachments:** under 3 MB, a plain POST; 3–150 MB, an upload session
  with ranges under 4 MB. There's a known large-attachment issue in
  shared mailboxes.
- **Search:** `$search` (KQL) returns at most 1,000 results, date-sorted.
  `/search/query` covers bodies and attachments for work accounts, own
  mailbox only. Server search supplements the local index; it doesn't
  replace it.
- **Categories:** a master list with colours; a message has many. These
  map to keywords, not folders. Focused Inbox via `inferenceClassification`.
- **Rules:** `messageRules` with sequence; respect `isReadOnly`.
- **Notifications:** webhooks need a public HTTPS endpoint, and
  subscriptions last under 7 days (under 1 day with resource data).
  **Desktop default: adaptive polling of delta**, with an immediate refresh
  on activation or network recovery. A relay only after privacy, cost and
  consent review.
- **Shared mailboxes:** `Mail.Read(Write).Shared`, plus `Mail.Send.Shared`
  with Send As rights. Delegated scopes can't subscribe to shared-mailbox
  notifications. Shared mailboxes are separate sync namespaces under one
  principal.
- **Throttling:** per app and mailbox: 10,000 requests / 10 min, 4
  concurrent, 150 MB uploaded / 5 min. Mailbox-aware concurrency,
  jittered backoff, `Retry-After`. New mail and sends go before history.
- **Graph parity gaps:** archive CRUD, import/export and non-draft MIME
  creation are targeted for Q4 2026 (estimates). Generic public-folder and
  Group-mailbox CRUD are excluded for good. Declare these as capabilities.
- **App registration:** a native public client (organizational and
  personal accounts), auth code + PKCE, no secret. Start with delegated
  `Mail.ReadWrite` + `Mail.Send`; add shared and settings scopes only when
  a feature needs them.
- **Admin onboarding kit:** client ID, exact scopes, data-flow diagram,
  actionable consent errors. Conditional Access can still block; test
  several tenant configurations.
- eM Client held Graph back for about two years ("very buggy … changing").
  Budget soak time.

## Google

| Path | Auth | Push | Quotas | Fidelity |
|---|---|---|---|---|
| **IMAP + X-GM-EXT-1 + XOAUTH2** | Restricted scope `mail.google.com` → verification (CASA if restricted data can be accessed from or through third-party servers) | IDLE | Workspace: 2,500 MB/day IMAP download, 500 MB/day upload (Google, checked 2026-10-11; consumer limits not documented there) | `X-GM-MSGID` (the API id's value: IMAP returns unsigned 64-bit decimal, the API hexadecimal; normalize without float truncation), `X-GM-THRID`, `X-GM-LABELS`, `X-GM-RAW` search; CONDSTORE rolled out historically; current CONDSTORE and QRESYNC unverified until an authenticated probe |
| **Gmail API** | Same restricted scopes; CASA if restricted data can be accessed from or through third-party servers | `watch` → Pub/Sub needs our infrastructure; Google recommends polling for installed apps | New projects: 6,000 units/user/min, 1.2M/project/min, billing above 80M units/project/day announced, but price and start date pending with 90 days' notice. `messages.get` = 20 units → 1M messages ≥ 55.6 h for one user | Native labels, `history.list` deltas (404 when expired → full resync), threads, settings and filters APIs |
| IMAP + app password | None | IDLE | — | As IMAP; Workspace admins can block it |

- **The disagreement:**
  - Devin and Claude: Gmail over IMAP + X-GM-EXT first. One engine; no
    per-project quotas or future billing; labels via X-GM-LABELS.
  - Codex and GLM: a dedicated Gmail API driver. True label and history
    model; no IMAP quirks such as `expungeBehavior`.
  - Codex's own quota arithmetic argues against API-only bulk import.
- **Resolved in the debate:** the Gmail account type is designed around
  Gmail identity (`X-GM-MSGID` = API id, normalized), so either transport
  can serve it. Ship one transport first, then measure API-only against a
  hybrid (API for deltas, labels and settings; IMAP for bulk bodies) on
  bytes, the Workspace IMAP limits and quota time before building the
  hybrid. A hybrid doubles the failure modes; admins can disable IMAP.
- **Workspace admins** can disable IMAP or app passwords; the API path then
  becomes necessary for those tenants.
- **Consumer Gmail removes third-party Send-as on web and mobile in
  January 2027** (and Gmailify); Workspace Send-as and Gmail aliases are
  unaffected. Users who sent as other addresses through consumer Gmail
  need a client that sends through those accounts directly: an argument
  for first-class identities with per-identity outgoing servers
  ([11-classic-clients](11-classic-clients.md)).

## Proton, Tuta, HEY

- **Proton Bridge:** local IMAP 127.0.0.1:1143 and SMTP 1025, STARTTLS by
  default, a self-signed certificate (trust narrowly on first use or
  import it), paid plans only, GPL-3.0. The app must be running. Plaintext
  then lives in Sweetbriar's cache, index and backups, so our own
  protection applies.
- **Bridge quirks:** UIDs reported changing without UIDVALIDITY changing;
  device-local "Recovered Messages"; orphaned messages in no folder. See
  [04-data-safety](04-data-safety.md).
- **Proton's Rust core** (`ProtonMail/clients`, ~80% shared code across
  their mobile apps):
  - `mail-uniffi` is unpublished and **AGPL-3.0-only**
  - `proton-crypto-rs` is MIT, but not vetted for outside use (its default
    backend needs Go)
  - the service terms restrict automation that deviates from normal
    client behavior (a clause that predates the 2026-10-09 revision,
    which added general service-discontinuation language; no new
    Bridge-specific restriction)

  Technically possible, source-licensed and service-authorized are three
  separate questions. **Ask Proton.**
- **Tuta:** no IMAP/POP/API. Its Thunderbird add-on just embeds the web
  app. **HEY:** no IMAP/POP, MBOX export only. Offer importers.

## JMAP and the rest

| Provider | Access | Notes |
|---|---|---|
| Fastmail | JMAP (mail, submission, contacts) + IMAP/SMTP; OAuth for distributed apps, API tokens, app passwords | Developer-friendly policy; Fastmail's endpoint advertises IMAP4rev2 + QRESYNC |
| Stalwart, Cyrus, Apache James | JMAP + IMAP | Stalwart is the CI reference server; JMAP Contacts is RFC 9610; JMAP Calendars still a draft (2026-10-05) |
| iCloud | IMAP/SMTP, app-specific password | OAuth only for Apple-chosen partners (Outlook got it) |
| Yahoo / AOL | IMAP/SMTP; OAuth by manual approval, else app passwords | Advertises OBJECTID and PARTIAL |
| Zoho | REST API + IMAP XOAUTH2, self-serve OAuth | Friendliest after Fastmail |
| GMX, Web.de, Posteo, mailbox.org | IMAP/SMTP with app passwords | European audience |
| QQ, 163 | IMAP with per-account authorization codes | Docs mostly Chinese; verify current policy |
| public-inbox / NNTP | Read-only archives (lore.kernel.org) | Cheap, delights developers |
| Local | mbox, Maildir, POP3 | Import and local folders; POP defaults to leave-on-server |

## Backend architecture

- **Capabilities describe semantics, permissions and reachability:**
  - `container_model`: Folders | Labels | FoldersPlusCategories
  - `multi_membership`
  - `server_search`: a query language and its limits
  - `push`: Idle | JMAP push | Poll
  - `threads`: Server | Local
  - `keywords` and categories
  - `rules`: None | Sieve | GmailFilters | GraphRules
  - `send_later`: Local | Server
  - `mime_fetch`, `mime_send`
  - `shared_mailboxes`, `quotas`
  - `delete_semantics`: Trash | Archive | Expunge, with Gmail
    `expungeBehavior` read in
  - `reachable_from_cloud`: false for Bridge
  - parity gaps

  The UI greys out what a backend can't do rather than guessing.
- **Independent sync drivers** run per account (a task each, separate
  connections, separate cursors). They emit deltas that the engine
  applies to the single store, so one account's failure never stalls
  another (Mailspring and aerc precedent).
- **Identity:** local UUID per logical message; remote locators per
  backend: IMAP (mailbox, UIDVALIDITY, UID), Graph immutable id per
  mailbox, Gmail message id, JMAP Email id. Never trust Message-ID alone.
- **JMAP as the model, not the wire.** Use JMAP semantics (`mailboxIds`,
  keywords, `threadId`, `EmailSubmission`, `/changes`) so the JMAP driver
  is nearly 1:1 and the model stays honest. The counter: JMAP's model
  doesn't express everything (Exchange categories vs folders, Focused
  Inbox, Proton E2E state, per-folder IMAP flags). Answer: `providerData`
  plus per-locator flags where the backend scopes them.

## Implementation order

Revised after the debate ([debate/04-resolution](debate/04-resolution.md)).

1. **Now, in parallel with engineering:** start Entra registration,
   publisher verification and the admin-consent kit; Google verification
   and a written CASA determination; Yahoo, AOL and Fastmail
   registration. Submit applications as their artifacts become
   available. This workstream continues whichever backend ships first.
2. **Durable local store, import and export** (see the data-safety build
   order). Import is the first slice: EML plus one cohort format (Apple
   Mail's mbox export first; direct `.emlx` when a cohort needs it).
3. **First live backend, chosen at a dated gate** (end of month 1):
   - Graph (one primary mailbox, unfiltered per-folder delta, acquired
     MIME, drafts and draft-based submission, categories, basic filing)
     if three independent tenants authorize Sweetbriar's own app and the
     cohort uses Microsoft 365.
   - If an M365 cohort cannot authorize Sweetbriar, the cohort decision
     reopens; IMAP doesn't solve their problem.
   - Otherwise one demanded IMAP provider plus Dovecot fixtures in CI.
     With no cohort evidence by the gate, the owner's own primary
     provider.
4. **Second backend by demand,** not on a fixed schedule: Graph or
   IMAP with provider presets (Gmail X-GM-EXT, iCloud, Yahoo, Fastmail,
   GMX, Proton Bridge with port probe, certificate handling and
   labels-as-folders).
5. **JMAP** (Fastmail, Stalwart, Cyrus): a narrow mail reference driver;
   tests memberships, states and blobs.
6. **Gmail API driver** (or hybrid), once quota and billing are clear.
7. **On demand:** on-prem EWS (Thunderbird crate), POP, public-inbox/NNTP,
   Zoho REST, local archives.
8. **Deferred until there's an agreement or a customer:** EAS, direct
   Proton, Tuta, HEY, cloud aggregation, JMAP calendars.

**Strongest objection** (codex): four drivers (IMAP, Graph, Gmail API,
JMAP) are four interoperability and security obligations. A small team
could spend years on sync instead of the native experience. Mitigate by
staging drivers and keeping first-release capabilities narrow, not by
promising parity.

## Risks and open questions

- Can ordinary enterprise users consent to Sweetbriar under Microsoft's
  managed policy? Test several tenants, Conditional Access, shared
  mailboxes.
- Google: a written CASA determination; billing price for more than 80M
  units/day.
- iCloud partner OAuth route; Yahoo approval odds and fees.
- Graph parity items in Q4 2026: retest when they ship.
- Keep a dated provider compatibility matrix with an owner who monitors
  policy changes.

## Key sources

- [EWS deprecation in Exchange Online](https://learn.microsoft.com/en-us/exchange/clients-and-mobile-in-exchange-online/deprecation-of-ews-exchange-online), [EWS rollout announcement](https://techcommunity.microsoft.com/blog/exchange/ews-deprecation-is-here-%E2%80%93-what-this-means-to-you/4561431), [MacRumors: Apple Mail and M365](https://www.macrumors.com/2026/10/01/apple-mail-mac-stop-sync-microsoft-365/)
- [Graph message delta](https://learn.microsoft.com/en-us/graph/delta-query-messages), [immutable ids](https://learn.microsoft.com/en-us/graph/outlook-immutable-id), [throttling](https://learn.microsoft.com/en-us/graph/throttling-limits), [webhooks](https://learn.microsoft.com/en-us/graph/change-notifications-delivery-webhooks), [consent policies](https://learn.microsoft.com/en-us/entra/identity/enterprise-apps/manage-app-consent-policies)
- [Thunderbird Graph](https://blog.thunderbird.net/2026/09/thunderbird-desktop-new-protocol-support-microsoft-graph-api/), [eM Client 11](https://www.emclient.com/blog/em-client-11-has-been-released-842), [DavMail](https://davmail.sourceforge.net/)
- [Gmail IMAP extensions](https://developers.google.com/gmail/imap/imap-extensions), [Gmail quotas](https://developers.google.com/workspace/gmail/api/reference/quota), [restricted-scope verification](https://developers.google.com/identity/protocols/oauth2/production-readiness/restricted-scope-verification), [Gmail push](https://developers.google.com/workspace/gmail/api/guides/push)
- [Proton Bridge](https://github.com/ProtonMail/proton-bridge), [Proton mail-uniffi licence](https://raw.githubusercontent.com/ProtonMail/clients/main/project/mail/rust/mail/mail-uniffi/Cargo.toml), [Proton terms](https://proton.me/legal/terms), [Tuta add-on](https://tuta.com/blog/tuta-add-on-in-thunderbird), [HEY FAQ](https://www.hey.com/faqs/)
- [Fastmail developers](https://www.fastmail.com/dev/), [RFC 8621](https://www.rfc-editor.org/rfc/rfc8621.html), [RFC 9610](https://www.rfc-editor.org/rfc/rfc9610.html)
