# Topic: Proton Mail via Proton Bridge as Sweetbriar's first live backend

The owner's own mail is **Proton Mail**. The project rule (see
`research/debate/04-resolution.md`, `research/06-account-backends.md`) is
that, without pilot-cohort evidence, the first live backend is the
owner's own provider. Proton's only sanctioned third-party path is
**Proton Mail Bridge** (local IMAP/SMTP). The project snapshot is in
`./research/` (start with `research/00-summary.md`; the data model is the
occurrence-based model in D2; the IMAP rules are in
`research/04-data-safety.md`).

An implementation plan is about to be written. Give the concrete facts it
needs, verified against primary sources (Proton docs, the
`ProtonMail/proton-bridge` repository and its release notes, issues):

1. **Current Bridge**: version and release date; platforms (macOS, Linux
   packages/Flatpak?); plan requirements (paid plans only?); whether it
   runs headless/CLI (`--cli`, `--noninteractive`), as a login item, or
   both; how a user starts it.
2. **Endpoints and security**: default IMAP/SMTP host and ports, STARTTLS
   vs SSL modes, Bridge's self-signed certificate (how clients are meant
   to trust it; exporting the cert; pinning), the Bridge-generated
   password vs the Proton password, split vs combined address mode.
3. **IMAP behaviour and quirks** (cite source/issues): capabilities
   advertised (IDLE, UIDPLUS, MOVE, CONDSTORE/QRESYNC, SPECIAL-USE,
   NAMESPACE, ID, LITERAL+ …); how labels and folders map (Labels/…,
   Folders/…, All Mail, Starred), so the same message appears in several
   mailboxes; UIDVALIDITY stability and when it changes (resync, cache
   rebuild, re-login); flag behaviour (\\Seen, \\Flagged, \\Deleted,
   keywords); EXPUNGE and MOVE semantics on label vs folder mailboxes;
   APPEND to Sent/Drafts; message size and attachments; BODY[] fidelity
   (is it the original MIME or a reconstruction? signatures and PGP);
   known bugs relevant to a sync client.
4. **SMTP**: submission behaviour, whether Bridge saves Sent copies
   itself (so the client must not APPEND), sending from addresses and
   aliases, attachments limits, error behaviour.
5. **Sync and performance**: initial sync time and how Bridge caches;
   events/IDLE latency; rate limits; behaviour while Bridge is still
   syncing (partial mailboxes); restart and update effects on clients.
6. **Terms and policy** for a third-party client using Bridge (the 2026
   terms revision is covered in `research/raw/fact-check-1/codex.md`).
7. **Testing strategy**: how to test against Bridge in CI without real
   credentials (no fake Bridge exists? a Dovecot fixture configured to
   mimic Bridge's quirks?), and what must be qualified manually on the
   owner's Mac.
8. **Recommendation**: the concrete first-backend scope for Sweetbriar
   (read-only sync first, then flags/filing, then send), Bridge-specific
   rules for the engine (e.g. identity across label mailboxes, never
   EXPUNGE on label folders, no Sent APPEND), and the risks.
