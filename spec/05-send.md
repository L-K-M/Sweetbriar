# 05 — Sending

**Revised 2026-11-18 (owner decision, 2026-11-15):** Proton accounts
send through Sweetbriar's own Proton API client, not SMTP through
Bridge: a draft is created on Proton at submission, its body and
attachments encrypted to the sender's address keys, then sent with one
package per MIME type that carries each recipient's key packets
(research/proton-api mail-sync.md §7, keys-crypto.md §4–5). Local
drafts, the outbox state machine, identities and From resolution stand.
SMTP stays for generic accounts (*later*).

Rests on research [04-data-safety](../research/04-data-safety.md)
(drafts and sending), D3, [15-account-colors](../research/15-account-colors.md)
(compose behaviour), [12-markdown-first](../research/12-markdown-first.md)
and [raw/plan-proton-bridge/codex.md](../research/raw/plan-proton-bridge/codex.md).

## Identities and From resolution

- **The From list** offers the account's identities with `sendable` set
  to `validated` or `provisional`.
  - For Proton, identities are the account's Proton addresses, refreshed
    at sign-in and on address events (04-sync-proton). An address is
    `validated` when Proton allows sending from it and one of its keys
    is unlocked, otherwise `unavailable`; Proton identities are never
    `provisional`.
  - SimpleLogin and reverse aliases are qualified separately, later.
- **Default identity for a new draft,** in order:
  1. **New message:** the identity of the selected account or mailbox's
     account, otherwise the global default (setting `identity.default`;
     each account has at most one `is_default` identity, 02).
  2. **Reply or Reply All to a message the user sent:** the identity
     recorded on that message's send attempt, or the identity matching
     its From.
  3. **Reply or Reply All to received mail:** the identity whose address
     appears in the original's To, Cc or `Delivered-To` of that exact
     occurrence's message, otherwise that account's default.
  4. **Forward:** as for reply.
  5. If no valid identity results, the draft opens with "Choose a sending
     identity" and Send disabled.
- **Reply recipients** are resolved separately from the identity: Reply
  goes to the original's `Reply-To` if present, else its From; Reply All
  adds its To and Cc, minus the chosen identity's address. Offline, these
  come from `message_headers` (02).
- **Reply and forward drafts** (M4.4d): `create_draft` for a reply,
  reply-all or forward writes the draft's first revision in the same
  transaction, so the window opens on it like any saved draft:
  - **identity:** resolved as above; none leaves the revision unrouted;
  - **recipients:** Reply puts its list in To. Reply All puts the reply
    list and the original's To in To and the original's Cc in Cc, minus
    the identity's address and repeats. Forward has none. Bcc is never
    copied;
  - **subject:** the original's, trimmed, after `Re: ` (replies) or
    `Fwd: ` (forward), unless it already starts with that prefix,
    compared without case; an empty one becomes `Re:` or `Fwd:`;
  - **format:** `plain`, flowed, as a new message's;
  - **body:** two empty lines, then, when the original's body is
    acquired locally, its plain-text rendition:
    - a reply quotes it under `On <Date>, <From> wrote:`, each line
      prefixed `> ` (an empty line becomes `>`);
    - a forward puts it, unquoted, under a
      `---------- Forwarded message ----------` block with the original's
      From, Date, Subject, To and Cc lines (empty ones left out).

    `<Date>` is the stored Date as an RFC 5322 date in UTC (02 keeps
    it as UTC milliseconds) and `<From>` the stored From, shown as
    `Name <address>` or the bare address. An original whose body
    isn't acquired gets the attribution or block alone, and no
    acquisition is requested. Forwarded attachments are a later step.
- **Cross-account check:** a reply whose identity's account differs from
  the receiving occurrence's account shows a banner (08). Replies to
  local-archive messages are exempt: that account can't send. If it wasn't
  acknowledged for this draft revision's identity and recipients, Send
  asks once more. The acknowledgement lives in the compose window and
  isn't persisted: after a relaunch, Send asks again. The window passes
  it with the send request (`send_draft`'s `acks`, 07); the core checks
  it against the revision and never stores it.
- **Recipient check** (owner decision 2026-11-24, M6.7): a send from
  account A is **blocked** for a recipient (To, Cc or Bcc) who has
  written to another sending account but never to A, unless the user
  allowed that address for A.
  - "Has written to" an account means the address is the first From of
    a message in that account (02 "Account senders"), matched by
    canonical address. The local archive is never counted: imported
    mail doesn't say which sending account received it.
  - An address that has written to A, or to no account at all, passes.
  - It applies to new messages, replies, reply-alls and forwards alike,
    next to the cross-account check, which still covers replies.
  - **Blocking is enforced by the core:** `send_draft` refuses a
    revision with a blocked recipient with `Conflict` naming
    `recipient_fence` and the addresses. `acks` are not consulted for
    it, so no acknowledgement overrides it. The ways through are changing From to an account the address
    has written to, removing the recipient, or `allow_recipient` for
    the route's account (07), a lasting decision listed in Settings and
    revocable there.
  - The check reads the account senders when Send is pressed, as far
    as the received-address sweep has recorded them (02). Until the
    first pass after the upgrade has recorded the existing mail, the
    check isn't ready and blocks nothing; a partial pass could
    otherwise block an address that wrote to A. Once ready, a first
    message to A that no pass has reached yet is still missing; Allow
    covers it. An attempt already accepted
    for sending keeps its frozen route and isn't checked again.

## Composing

- **Formats:** `plain` or `markdown` (02 `draft_revisions.compose_format`).
- **The Markdown dialect:** CommonMark plus GFM tables, strikethrough and
  autolinks, rendered with `pulldown-cmark`. **Raw HTML is disabled**:
  HTML in the source renders as literal text.
- **Outgoing MIME,** built with `mail-builder`:
  - **plain:** `text/plain; charset=utf-8; format=flowed` by default.
    A per-draft "literal" option sends without flowed wrapping, for
    patches and code.
  - **markdown:** `multipart/alternative` with:
    - `text/plain`: **text rendered from the Markdown** (emphasis markers
      removed, lists and quotes kept as plain-text conventions, links as
      `text <url>`), not the raw source. The source stays local in the
      draft revision (research 12);
    - `text/html`: rendered, sanitized by the same allow-list as the
      reader, with inline CSS from a built-in stylesheet;
  - **attachments:** `multipart/mixed`; inline images go in
    `multipart/related` with `cid:`;
  - **inline images (M4.4e2):** an attachment with a `content_id` (02)
    is inline. Its Content-ID is a dot-atom `@` dot-atom of printable
    ASCII, at most 250 bytes, unique within the revision, and its media
    type is `image/png`, `image/jpeg` or `image/gif`; `save_draft`
    refuses anything else with `InvalidInput` naming the attachment.
    The stored `content_id` is the bare value; the shell mints a
    lowercase UUID followed by `@sweetbriar.invalid`. Only the
    `Content-ID` header wraps it in angle brackets; a `cid:` reference
    (RFC 2392) and Proton's `ContentID` use it bare. In a Markdown
    draft, inline images join the HTML part in `multipart/related`,
    referenced by an image whose URL is `cid:` and the bare value, as in
    `![Chart](cid:0f8fad5b-d9cb-469f-a165-70867728950e@sweetbriar.invalid)`.
    The text part shows a
    `cid:` image as its alt text, and the HTML drops a `cid:` source
    that names none of the revision's inline images. A plain draft has
    no HTML, so its inline images go as ordinary attachments. Inline
    images count toward the limits below;
  - no `text/markdown` part (research D16).
- **Headers:**
  - `Message-ID: <uuid@domain>`, where `domain` is the identity's
    address domain;
  - `Date`;
  - `In-Reply-To` and `References` from the original;
  - `User-Agent: Sweetbriar/<version>`;
  - **no `Bcc` header.** Bcc recipients exist only in the envelope
    (`send_recipients`).
- **Local limits (Proton):** 25 MB total attachments and at most 100
  files. Exceeding them blocks Send with an explanation; Sweetbriar
  checks before Proton does.
- **The frozen MIME is the record.** For a Proton account the attempt's
  published MIME is what the Out Box shows and what the submission
  sends: the draft's subject, recipients, bodies and attachments are
  read from it, never from the editable draft.

## Outbox state machine

```
            send_draft
 editing ──────────────► held ──(undo window ends)──► queued ──► submitting
                          │ cancel_send                   │ cancel_send │
                          ▼                               ▼             ├─► accepted ──► (Sent reconciliation)
                      cancelled ◄─────────────────────────┘             ├─► failed       (qualified pre-submission failure)
                                                                    └─► outcome_unknown (connection lost after the send
                                                                                         request, crash before its reply recorded)
```

Every transition below that changes the draft's state does so only if
the attempt's revision is still the draft's current revision (02).
Otherwise the draft keeps its state.

1. **`send_draft(draft_id, revision_id, acks)`:**
   - `revision_id` must be the draft's `current_revision_id`, otherwise
     `Conflict`: only what the user last saw is sent.
   - The draft must be `editing`, and the revision must have no live
     attempt and none `accepted` or resolved `marked_sent`. Otherwise
     `Conflict`. A second call with the same idempotency key returns the
     first result (07). These checks run again inside the transaction
     below, so two calls with different keys can't queue one revision
     twice; the `send_attempts_live` index (02) backs them up.
   - **First,** outside any transaction, build the MIME and publish it to
     the object store (03). If publishing fails, nothing else happens.
   - **Then,** in one transaction:
     - insert `send_attempts` (`held`, `hold_until = now + undo window`,
       the `sending.undo_seconds` setting, 02) and `send_recipients`
       (`pending`, one row per address, 02);
     - set the draft to `queued`.
     - If this transaction fails, the published object is an orphan, and
       GC handles it.
   - The frozen bytes, identity, envelope and recipients never change
     afterwards. Editing the draft again creates a new revision and,
     if sent, a new attempt.
2. **`held` and `queued`:** `cancel_send` moves the attempt to `cancelled`
   and the draft back to `editing`. That includes an attempt waiting for
   `confirm_send_route`. The claim to submit and a cancel are serialized
   by the writer; whichever commits first wins. Once `submitting`, an
   attempt can't be cancelled.
   - Once `hold_until` has passed, the account's worker moves a `held`
     attempt to `queued` in one write, guarded on `held` so a cancel
     that committed first wins. At startup a `held` attempt whose window
     passed while the app was closed is queued the same way; one whose
     window is still open stays `held` until it ends.
3. **`queued` → `submitting`** commits before any network I/O.
   - **Before claiming,** the engine checks that the attempt's
     `config_generation` equals the account's current one. If the account
     was reconfigured (endpoint, credentials, identity) since, the attempt
     stays `queued`, marked "Account changed; confirm sending with the new
     settings". It is submitted only after the user confirms
     (`confirm_send_route`, 07) or cancels.
   - **Confirming** checks that the attempt's identity is still
     sendable, then sets the attempt's `config_generation` to the
     account's current one in the same transaction. If the identity is
     no longer sendable, confirming fails and only cancelling remains.
   - The draft must have a valid sending identity. Drafts without one
     can't be sent.
   - On startup, a Proton attempt found in `submitting` with
     `send_requested_at` set becomes `outcome_unknown`: never retried
     automatically. One without it never issued the send request, so it
     returns to `queued` and keeps its recorded `proton_draft_id` (02).
     A generic attempt (*later*) found in `submitting` becomes
     `outcome_unknown` whatever its progress, until SMTP records its own
     marker that `DATA` began.
4. **Proton submission** (04-sync-proton's transport, on the account's
   one worker between its sync steps, so the attempt's requests are the
   worker's and one body fetch stays the only other request in flight;
   an attempt's requests are sequential):
   1. **Recipient keys.** `GET /core/v4/keys?Email=` for each recipient.
      An internal recipient (Proton) is encrypted to its keys. An
      external recipient for whom the endpoint returns no keys is sent
      in clear, as Proton's own clients do by default; the Out Box says
      so (the recipient's `scheme`, 02). An external recipient for whom
      it returns keys isn't supported in the pilot: the attempt becomes
      `failed` before anything is created on Proton, that recipient
      `rejected_permanent` with the reason and the others `unsent`, so
      Sweetbriar never downgrades a key the endpoint knows to clear
      text. Keys the endpoint doesn't return (a WKD it doesn't consult,
      keys pinned in Proton contacts) aren't looked up in the pilot;
      that lookup is *later*. Password-protected sending isn't in the
      pilot.
   2. **Create the draft.** `POST /mail/v4/messages` with the sender
      address, the recipient lists, the subject, the body encrypted to
      the sender's address key, and, for a reply or forward of a
      message in the same Proton account, its `ParentID` and `Action`.
      The returned draft id is committed to the attempt
      (`proton_draft_id`) before the next request; a crash or a lost
      reply in that window leaves a draft Sweetbriar can't name, which
      stays in Proton's Drafts ("Drafts on the server"). A retried attempt
      reuses a recorded draft, updating it (`PUT`) rather than creating
      another.
   3. **Upload the attachments** to the draft (`POST
      /mail/v4/attachments`): key packets, data packet and a detached
      signature each, in the frozen MIME's order. An inline image of a
      Markdown draft uploads with `Disposition` `inline` and its
      `ContentID`; every other one with `attachment` and an empty
      `ContentID`. An upload already
      recorded on the attempt isn't repeated. A retry first reads the
      draft: an attachment it carries that the attempt didn't record
      (its reply was lost) is recorded when its name, type, size (as
      the draft reports it, the measure the fake and the lane pin, 11)
      and disposition match the next pending one, and for an inline
      one its `ContentID` too; an unrecorded draft attachment
      with no pending counterpart, and any other mismatch, makes the
      attempt `failed` before the send request, naming the draft.
      Attachments the attempt recorded are expected and pass.
   4. **Send.** Build one package per MIME type the recipients need
      (`text/html` for a Markdown draft, `text/plain` for a plain
      draft; recipients' MIME preferences live in Proton contacts,
      which the pilot doesn't read, so the pilot needs one package and
      the frozen `text/plain` alternative of a Markdown draft goes in
      a second one only once those preferences are read, *later*):
      the body's data packet, and per recipient its scheme (internal:
      the session keys encrypted to its key; clear: the session keys in
      the package), signed as Proton's clients sign. Then commit
      `send_requested_at` with each recipient's scheme (`internal` or
      `clear`, 02), then `POST /mail/v4/messages/{draft id}`.
   5. **Interpret the reply:**
      - **Proton's success reply** (`Code` 1000 with the `Sent`
        message) → attempt `accepted`; every recipient `sent` (accepted
        by Proton, not delivered). The draft becomes `sent` only if its
        `current_revision_id` still equals the attempt's revision (02).
        The sent message's id is the draft id. Each recipient counts
        as sent to in `seen_addresses` (02), in the same transaction.
      - **A refusal whose code is in a qualified pre-submission class**
        → `failed`, recipients `unsent`, Proton's code and message shown.
        A class is qualified only when the `go-proton-api` lane (11) or
        research shows Proton refuses before anything is sent; the list
        starts empty and grows only with that evidence.
      - **Any other reply,** and any connection loss or timeout once
        `send_requested_at` is committed, whether or not the request
        was fully written → `outcome_unknown`, recipients `unknown`.
        Only a connection that was never established (refused, an
        unresolved host, a connect timeout, a failed TLS handshake)
        proves nothing was
        sent: `send_requested_at` is cleared and the attempt stays
        `queued`.
      - Proton sends a message as a whole, so there is no partial
        acceptance: `retry_rejected_recipients` (07) applies to generic
        accounts only.
   - **Failures before `send_requested_at`** (offline, 429 or 503, a
     session to refresh, keys not unlocked) send nothing. The attempt
     stays `queued` and the outbox retries with an in-memory backoff;
     a refusal that will persist (the address can no longer send, the
     draft was refused) is `failed`.
   - A refusal of Sweetbriar's identity (2064, 5003) or a session
     refused (10013) also puts the account in `needs_attention`
     (04-sync-proton "Account states and health").
5. **SMTP session** (generic accounts, *later*; through `lettre`, or the
   transport chosen in 01):
   - **Connect and authenticate.** Connect to the server's SMTP port,
     run STARTTLS (or implicit TLS), then `AUTH PLAIN` or `LOGIN`.
   - **Send the envelope.** `MAIL FROM:<identity address>`. Each
     `RCPT TO` reply is recorded per recipient as `rcpt_accepted` or
     `rejected_permanent`/`rejected_transient`.
   - **`DATA`:**
     - **250 final reply** → attempt `accepted`. Every `rcpt_accepted`
       recipient becomes `sent` (accepted by the server, not delivered).
       The draft becomes `sent` only if its `current_revision_id` still
       equals the attempt's revision (02, drafts with live attempts).
     - Definite 5xx → `failed`; 4xx → `failed` with a retry offer.
     - **Connection lost or timeout after `DATA` was sent** →
       `outcome_unknown`.
     - **On `outcome_unknown`,** `rcpt_accepted` recipients become
       `unknown`; definite RCPT rejections keep their state. A recipient
       whose RCPT reply wasn't recorded before a crash becomes `unknown`.
   - **Partial RCPT rejection** with DATA accepted: rejected recipients
     stay `rejected_*` and are offered as a new attempt to those
     recipients only (`retry_rejected_recipients`, 07): the same frozen
     bytes, an envelope with just them. Accepted recipients are never
     re-sent.
6. **`outcome_unknown`** shows in the Out Box with the frozen message, the
   time and what is known. The user resolves it with
   `resolve_unknown_send`:
   - **Mark as sent** (e.g. after finding it in Sent): its `unknown`
     recipients become `sent`, and the draft becomes `sent`;
   - **Send again**: after an explicit duplicate warning, creates a new
     attempt with the unknown attempt's frozen revision, bytes and
     recipients (the same `Message-ID`, so recipients' clients can spot
     the duplicate), in the same transaction that records the
     resolution; the draft is `queued` again;
   - **Keep as draft**: the draft returns to `editing`. The warning says
     that sending the same revision again may duplicate it.
   - The resolution is stored on the attempt (`resolution`, 02) and can't
     be changed. A resolved attempt is history: it no longer counts as
     live for sending or discarding, and the Out Box lists it under
     history.
   - Each of these changes the draft's state only under the
     current-revision rule (02): a newer revision keeps its state,
     including `queued`.
   - **A `failed` attempt** also returns its draft to `editing`.
   - **Proton evidence:** when a Proton attempt becomes
     `outcome_unknown` (from a reply, at startup or after a restore),
     and again at each of the account worker's polls while it is
     unresolved, the engine reads the draft's metadata by
     `proton_draft_id`; `out_box()` only reports what was observed. If Proton shows it sent (it carries
     Sent, `7`, and no longer Drafts, `8`), the attempt becomes
     `accepted`, its `unknown` recipients `sent` and the draft `sent`
     under the current-revision rule (02): that is Proton's own record, and
     nothing is sent again. Any other observation, a draft still in
     Drafts included, proves nothing and is only shown; the user
     resolves the attempt.
   - Generic accounts: Sweetbriar may show a Sent occurrence whose
     headers carry the attempt's `Message-ID`. Evidence is shown, never
     acted on automatically.
7. **Retries:** only `failed` attempts with a definite pre-acceptance
   failure, and only on user request.
   - Automatic retry with backoff applies only to failures *before* the
     send request (Proton: before `send_requested_at`; SMTP: before
     `DATA`). These keep the attempt `queued`; the outbox retries it
     with an in-memory backoff, so there's nothing to persist.
   - **Nothing after the send request is retried automatically.**

8. **`discard_draft`** is refused with `Conflict` while the draft has a
   live attempt (`held`, `queued`, `submitting`, or unresolved
   `outcome_unknown`). The
   user cancels or resolves the attempt first, so discarded mail is
   never sent.

## The Sent copy

- **Proton saves the Sent copy itself:** the sent draft becomes the
  Sent message (`sent_copy = 'server_saves'` at insert). Sweetbriar never uploads or
  imports a Sent copy for Proton accounts.
- **Reconciliation:** after `accepted`, the attempt's `sent_copy` is
  `server_saves`. The events link the Sent message to the attempt by
  its id, `proton_draft_id` (display only).
  - A Sent copy missing after 24 h shows a soft warning in the Out Box.
  - **It never triggers a resend.**
- **Generic IMAP** (*later*): `server_saves_sent` comes from the
  provider table. When 0, an `append` intent files the frozen bytes into
  the Sent mailbox after `accepted`. If that fails, it is repaired by
  retrying the APPEND, never the SMTP submission.

## Drafts on the server

Drafts live locally in the pilot. A Proton draft exists only from
submission step 2 and becomes the sent message. An attempt that ends
`failed` or is resolved after its draft was created leaves that draft in
Proton's Drafts; the Out Box names it, and Sweetbriar doesn't delete it
in the pilot (no permanent deletes, 04-sync-proton). Syncing local
drafts to Proton's Drafts is *later*.

## Restore and send

After a restore (03), every attempt in `held`, `queued` or `submitting`
becomes `outcome_unknown`, whatever its `send_requested_at`: the backup
may predate a send request the original store made later, so its
`send_requested_at` proves nothing. Nothing is
sent until the user resolves it; for a Proton attempt with a
`proton_draft_id` the evidence check above runs first.
