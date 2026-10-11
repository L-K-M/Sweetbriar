# Topic: Supporting non-standard email sources (Exchange/Microsoft 365, Proton, Gmail API, JMAP, ...)

Verify every date and policy against current official sources (as of
2026-10-11). Questions:

1. Microsoft.
   - Exchange Web Services (EWS) retirement in Exchange Online: exact dates
     and what is blocked when; any exemptions or extensions. Status of EWS on
     on-premises Exchange Server (Exchange SE).
   - Microsoft Graph mail API: delta queries for sync, change notifications
     (do webhooks need a public endpoint? alternatives for a desktop client),
     MIME access ($value), sending, attachments and upload sessions, search,
     categories, Focused Inbox, rules, shared/delegated mailboxes, throttling
     limits.
   - IMAP/POP/SMTP with OAuth 2.0 on Microsoft 365: SMTP AUTH basic auth
     retirement dates, tenants that disable IMAP, consumer Outlook.com.
   - Exchange ActiveSync: is it usable and licensable by a new client?
   - App registration: multi-tenant Entra ID app, admin consent problems in
     enterprises, publisher verification.
   - How Thunderbird added Exchange support (EWS in Rust, 2025) and whether
     it plans Graph; how other third-party clients handle Microsoft 365.
     DavMail as a gateway.
2. Google: Gmail API vs IMAP; OAuth restricted scopes and the CASA security
   assessment (who needs it, cost and tiers in 2026, annual renewal, does
   IMAP via https://mail.google.com/ also require it?); label model vs
   folders; X-GM-EXT-1 IMAP extensions; push (Pub/Sub watch) vs polling
   history.list for a desktop app; quotas.
3. Proton: how Proton Mail Bridge works (local IMAP/SMTP, paid plans only,
   license, Go code: go-proton-api, gluon). Has Proton built a shared Rust
   core or SDK for its 2025-2026 apps, and is any of it open source or
   usable by third parties? Proton's API terms for third-party clients. Is
   direct integration feasible and allowed, or must Sweetbriar go through
   Bridge? OpenPGP libraries involved.
4. Tuta (Tutanota): any third-party access? Legal and technical feasibility.
5. JMAP: Fastmail (API tokens, OAuth for third-party apps), Stalwart, other
   servers; JMAP for calendars/contacts.
6. iCloud Mail (app-specific passwords, OAuth availability), Yahoo/AOL
   (OAuth access approval), Zoho Mail API, GMX/Web.de, HEY (no IMAP?), and
   other popular providers in Europe and Asia. POP3 and local mbox/Maildir
   as account types. Mailing lists via NNTP or public-inbox.
7. How existing clients structure multi-backend support (Thunderbird,
   Evolution, Mailspring, Spark, Outlook for Mac, Apple Mail with Exchange,
   eM Client).
8. Recommend an account-backend architecture for the Rust core: a
   capability model (folders vs labels, server-side search, push, threads,
   categories, send-later), a unified internal data model (should it be
   shaped like JMAP?), per-backend sync drivers, and an order in which to
   implement backends. Include legal/policy/cost risks per provider.
