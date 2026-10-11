# 07 — Standards and plumbing

Based on [raw/standards/devin.md](raw/standards/devin.md), cross-checked
against the sources, data-safety and performance reports. RFC 9979 and the
Exchange SMTP AUTH timeline were verified by hand. Checked 2026-10-11.

## Summary

- **Account setup is a chain of mechanisms, not one.** The core tries known
  providers, Autodiscover, the Thunderbird ISPDB, `autoconfig.<domain>`,
  SRV records, then hostname probing.
- **Passwords are nearly gone at the big providers.** OAuth 2.0
  (XOAUTH2/OAUTHBEARER) is the main path, and app passwords are the
  fallback.
- **The MIME edge cases decide whether mail looks right.** Broken RFC 2047
  encoded words, three filename encodings, charset lies, TNEF and
  format=flowed all have to be handled.
- **Threading:** JWZ locally, with provider thread ids as hints.
- **Sending:** keep a per-provider "server saves Sent" table, or users get
  duplicate copies in Sent.
- **RFC 9979 (May 2026)** standardized Apple's flag colours and several
  keywords. Round-trip them at launch.
- **Sieve plus ManageSieve** is how server-side rules work outside Gmail
  and Exchange, and almost no client edits them well. That is an opening.

## Account setup

- **Order of attempts:**
  1. Known providers, matched by domain and by MX record. Matching
     `aspmx.l.google.com` or `*.mail.protection.outlook.com` picks the
     authentication path, not just the host names.
  2. Microsoft Autodiscover V1 (POX). V2 (JSON) works but is undocumented.
  3. Thunderbird ISPDB: MPL-2.0 data that covers about 50% of accounts.
  4. `autoconfig.<domain>` and `/.well-known/autoconfig`.
  5. RFC 6186 SRV records: sparsely deployed, but cheap to query.
  6. Hostname probing.
  7. Manual entry.

  Cache the outcome per domain.
- **The autoconfig format is being standardized** as
  `draft-ietf-mailmaint-autoconfig` (-06, revising after last call).
  Implement the deployed format now and track the draft.

## Authentication

- **OAuth on macOS** follows RFC 8252: `ASWebAuthenticationSession` with
  PKCE and a reverse-domain custom URL scheme, falling back to a loopback
  redirect. Google still allows loopback for desktop clients; the old
  out-of-band flow is dead.
- **Client ids are public** and ship in the app; the refresh token is the
  secret.
  - Store tokens in the Keychain as `ThisDeviceOnly` and
    non-synchronizable; use the Secret Service on Linux.
  - Treat `invalid_grant` as "sign in again", never as "delete the
    account".
- **Provider gates** (lead times belong in the plan):

| Provider | Gate |
|---|---|
| Gmail | `https://mail.google.com/` is a restricted scope and needs app verification; the annual CASA assessment applies only if mail passes through our servers |
| Microsoft 365 | Multi-tenant Entra app, publisher verification, possible admin consent |
| Yahoo / AOL | Manual OAuth approval; app passwords until approved |
| iCloud | App-specific passwords only (OAuth is for Apple-chosen partners) |
| Fastmail | OAuth for all protocols |

- **Exchange Online SMTP AUTH basic auth** was re-timed on 2026-01-27. It
  becomes off by default for existing tenants at the end of December 2026,
  and the final removal date will be announced in the second half of 2027.
- **SCRAM-SHA-256** is the right password mechanism where offered
  (Dovecot). GSSAPI/Kerberos and TLS client certificates matter for
  enterprises.

## Message format

- **Parse defensively.** Expect missing boundaries, raw 8-bit headers,
  duplicate headers, NULs and folded `References`.
- **Filenames arrive three ways:** RFC 2231 continuations, RFC 2047 inside
  `filename=`, and raw UTF-8. Decode all three; emit RFC 2231.
- **Charsets:** map `iso-8859-1` to `windows-1252`, as WHATWG does. Detect
  undeclared 8-bit text with `chardetng`, and remap GB2312→GBK,
  `unknown-8bit` and similar labels.
- **Transfer encodings:** use 8BITMIME and BINARY on submission, and
  tolerate broken quoted-printable.
- **EAI/SMTPUTF8 (RFC 6530–6533, UTF8=ACCEPT):** inbound support is broad.
  Parse UTF-8 headers, and send with `SMTPUTF8` only when the server
  advertises it.
- **format=flowed (RFC 3676):** honor it on display, including DelSp and
  space-stuffing. Sending it is a preference (see
  [08-macos-ui](08-macos-ui.md)).
- **TNEF (`winmail.dat`):** write a decoder from [MS-OXTNEF].
- **Multipart:** prefer the most faithful `alternative` part and keep
  `text/plain` authoritative for quoting. Resolve `related` and `cid:`
  per message. Show `message/rfc822` as an attached message. Detect
  `text/calendar` even when it's mislabeled.
- **Crypto parts are structure, not attachments.** Don't offer a
  signature `.p7s` as a download by default. Handle S/MIME (RFC 8551),
  OpenPGP (RFC 9580), PGP/MIME (RFC 3156) and inline PGP.

## Threading

- JWZ (RFC 5256 REFERENCES) locally, with localized reply prefixes
  (Aw:, Sv:, VS:, WG:, Odp:, ...).
- Provider hints: Gmail `X-GM-THRID`; Exchange `Thread-Index`/
  `ConversationIndex`, parseable over IMAP when `References` was stripped.
- Server `THREAD` is an optimization only. `THREAD=REFS` is a dead draft.

## Sending

- Prefer implicit TLS on port 465 (RFC 8314), else 587 with STARTTLS.
  Never send credentials before TLS.
- **Gmail and Microsoft 365 save SMTP-submitted mail to Sent themselves.**
  Keep a per-provider "server saves Sent" flag; otherwise `APPEND` to the
  `\Sent` special-use folder. Thunderbird bug 1431562 shows the duplicates
  this prevents.
- **DSN (RFC 3461/3464)** is nice to have. BURL and FUTURERELEASE are
  rarely deployed.
- Undo send and send later run client-side. RFC 9979 registers a
  `\Scheduled` mailbox attribute.

## Trust display

- **`Authentication-Results`:** show only headers whose `authserv-id`
  matches the account's own provider; anything deeper in the message is
  attacker decoration. Show SPF, DKIM, DMARC (now RFC 9989) and ARC
  verdicts factually.
- **BIMI is still an Internet-Draft.** Treat logos as branding, not trust.
  Apple Mail shows only provider-asserted BIMI; client-side VMC/CMC
  validation would let Sweetbriar show logos Apple Mail can't (later).
- **MTA-STS and DANE** protect server-to-server hops; they don't apply to
  a client talking to a known server.
- **End-to-end encryption:**
  - OpenPGP (RFC 9580) via `rpgp`
  - Autocrypt 1.1 headers
  - S/MIME through the platform trust store

## Keywords and productivity standards

- **IANA keywords:** `$Junk`/`$NotJunk` (mutually exclusive), `$Phishing`,
  `$Forwarded`, `$MDNSent`, `$Important`.
- **RFC 9979** (Informational, May 2026, Fastmail with Apple):
  - `$MailFlagBit0/1/2`, Apple's seven flag colours encoded on top of
    `\Flagged`; clear the bits when unflagging
  - `$hasattachment`, `$muted`, `$notify`, `$canunsubscribe`,
    `$unsubscribed`, `$memo`, `$followed`, `$new` and others
  - Mailbox attributes `\Snoozed`, `\Scheduled`, `\Memos`

  Servers won't set these for you; treat them as client-written
  conventions.
- **Unsubscribe:** `List-Unsubscribe` with one-click POST (RFC 8058),
  which Gmail and Yahoo require of bulk senders. Offer the button; never
  fire it automatically.
- **Calendar (iMIP/iTIP):** show a banner with RSVP, track `SEQUENCE`, and
  write REPLY messages. CalDAV and CardDAV later.
- **Sieve (RFC 5228)** with vacation, imap4flags, variables, body,
  editheader, regex, include and duplicate; ManageSieve on port 4190
  (Dovecot Pigeonhole, Cyrus, mailbox.org). Users ask for an editor and
  nobody ships a good one.
- **MDN (RFC 8098):** parse it; never auto-send receipts.
- **METADATA (RFC 5464):** sync Sweetbriar's own per-mailbox settings where
  servers offer it.

## Launch checklist

| Tier | Standards |
|---|---|
| **Launch** | RFC 5322/MIME parsing with repair; IMAP4rev1/rev2 with UIDPLUS, CONDSTORE/QRESYNC, IDLE, ENABLE, LIST-EXTENDED, SPECIAL-USE, MOVE, SASL-IR, NAMESPACE, ID, LITERAL±; SMTP on 465/587; the full autoconfig chain; XOAUTH2/OAUTHBEARER and app passwords; TLS 1.2+ with hard-fail; JWZ + X-GM-THRID + Thread-Index; per-provider Sent rules; format=flowed; TNEF; one-click unsubscribe; iMIP banner; `$Junk`/`$NotJunk`/`$Forwarded`/`$MDNSent`/`$Phishing` and Apple flag colours; Authentication-Results display; Keychain tokens |
| **Soon after** | SMTPUTF8/EAI; SCRAM-SHA-256; CardDAV/CalDAV; ManageSieve with a Sieve editor; S/MIME verify and decrypt; OpenPGP read with Autocrypt; provider-asserted BIMI; DSN; METADATA; the remaining RFC 9979 keywords |
| **Later** | Full OpenPGP key management; client-verified BIMI; NOTIFY; BURL; FUTURERELEASE; server THREAD as an optimization |

JMAP, Graph and EWS backends are scheduled in
[06-account-backends](06-account-backends.md).

## Key sources

- [Thunderbird autoconfig](https://developer.mozilla.org/en-US/docs/Mozilla/Thunderbird/Autoconfiguration), [draft-ietf-mailmaint-autoconfig](https://datatracker.ietf.org/doc/draft-ietf-mailmaint-autoconfig/), [Autodiscover](https://learn.microsoft.com/en-us/exchange/client-developer/exchange-web-services/autodiscover-for-exchange)
- [RFC 8252](https://www.rfc-editor.org/rfc/rfc8252.html), [RFC 7628](https://www.rfc-editor.org/rfc/rfc7628.html), [Google loopback migration](https://developers.google.com/identity/protocols/oauth2/resources/loopback-migration)
- [RFC 3676](https://www.rfc-editor.org/rfc/rfc3676.html), [MS-OXTNEF](https://learn.microsoft.com/en-us/openspecs/exchange_server_protocols/ms-oxtnef/1f0544d7-30b7-4194-b58f-adc82f3763bb), [RFC 5256](https://www.rfc-editor.org/rfc/rfc5256.html)
- [RFC 8314](https://www.rfc-editor.org/rfc/rfc8314.html), [Gmail IMAP client settings](https://support.google.com/mail/answer/78892)
- [RFC 9979](https://www.rfc-editor.org/rfc/rfc9979.html), [IANA keywords](https://www.iana.org/assignments/imap-jmap-keywords), [RFC 8058](https://www.rfc-editor.org/rfc/rfc8058.html), [RFC 5804](https://www.rfc-editor.org/rfc/rfc5804.html)
- [Apple BIMI](https://developer.apple.com/support/bimi/), [Autocrypt](https://docs.autocrypt.org/level1.html)
