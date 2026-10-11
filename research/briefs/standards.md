# Topic: Email standards and plumbing a modern client must get right

Questions (cite RFCs and current provider docs):
1. Account setup: Thunderbird ISPDB / autoconfig, Microsoft Autodiscover,
   RFC 6186 SRV records, MX-based heuristics, the IETF "mailautoconfig"
   draft or successors, how Apple Mail and Thunderbird decide settings.
2. Authentication: OAuth 2.0 for IMAP/SMTP (SASL XOAUTH2, OAUTHBEARER
   RFC 7628), loopback redirect vs custom scheme vs ASWebAuthenticationSession,
   PKCE, token storage and refresh, client IDs and provider registration
   for Google, Microsoft, Yahoo, AOL, Fastmail, iCloud (app passwords).
   SCRAM, app passwords, client certificates.
3. Message format edge cases: RFC 5322/2045-2049, RFC 2047/2231 encoded
   words and parameters, broken charsets, 8BITMIME, SMTPUTF8 / EAI (RFC
   6530-6533), UTF8=ACCEPT, format=flowed (RFC 3676), TNEF, multipart
   /related and /alternative selection, calendar parts, message/rfc822
   attachments, S/MIME and PGP/MIME structures.
4. Threading and conversations: JWZ, RFC 5256 THREAD, References /
   In-Reply-To, subject-based fallbacks, provider thread ids.
5. Sending: SMTP submission (port 587 vs 465 implicit TLS per RFC 8314),
   DSN, Sent-folder handling (Gmail and Exchange save automatically),
   BURL, send later, undo send.
6. Security and trust: TLS requirements, MTA-STS/DANE relevance to
   clients, displaying DKIM/DMARC/ARC results, BIMI and VMC/CMC logos,
   Autocrypt / OpenPGP (rpgp, Sequoia) / S/MIME, Proton and other E2EE
   interop.
7. Productivity standards: List-Unsubscribe and one-click (RFC 8058,
   Gmail/Yahoo bulk sender rules), iMIP/iTIP calendar invitations (RFC
   6047/5546), CardDAV/CalDAV, ManageSieve (RFC 5804) and Sieve (RFC 5228
   + extensions) for server-side rules, IMAP METADATA/ANNOTATE, keywords
   ($Junk, $NotJunk, $Forwarded, $MDNSent, $Phishing; IANA registry),
   MDN read receipts (RFC 8098), message flags interop with Apple Mail
   colour flags.
8. Produce a checklist of standards Sweetbriar should support at launch,
   soon after, and later, with reasons.
