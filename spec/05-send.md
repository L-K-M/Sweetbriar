# 05 — Sending

Rests on research [04-data-safety](../research/04-data-safety.md)
(drafts and sending), D3, [15-account-colors](../research/15-account-colors.md)
(compose behaviour), [12-markdown-first](../research/12-markdown-first.md)
and [raw/plan-proton-bridge/codex.md](../research/raw/plan-proton-bridge/codex.md).

## Identities and From resolution

- **The From list** offers the account's identities with `sendable` set
  to `validated` or `provisional`.
  - For Proton, identities are the Proton addresses the user adds; Bridge
    rejects addresses the account doesn't own.
  - At setup the user enters them; Sweetbriar suggests the Bridge
    username address.
  - Bridge's acceptance on the first send promotes an identity from
    `provisional` to `validated`.
  - SimpleLogin and reverse aliases are qualified separately, later.
- **Default identity for a new draft,** in order:
  1. **New message:** the identity of the selected account or mailbox's
     account, otherwise the global default.
  2. **Reply or Reply All to a message the user sent:** the identity
     recorded on that message's send attempt, or the identity matching
     its From.
  3. **Reply or Reply All to received mail:** the identity whose address
     appears in the original's To, Cc or `Delivered-To` of that exact
     occurrence's message, otherwise that account's default.
  4. **Forward:** as for reply.
  5. If no valid identity results, the draft opens with "Choose a sending
     identity" and Send disabled.
- **Cross-account check:** a reply whose identity's account differs from
  the receiving occurrence's account shows a banner (08). If it wasn't
  acknowledged for this draft revision's identity and recipients, Send
  asks once more. The acknowledgement lives in the compose window and
  isn't persisted: after a relaunch, Send asks again.

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
  files. Exceeding them blocks Send with an explanation. Bridge's SMTP
  `SIZE` advertises no number, so Sweetbriar checks itself.

## Outbox state machine

```
            send_draft
 editing ──────────────► held ──(undo window ends)──► queued ──► submitting
                          │ cancel_send                   │ cancel_send │
                          ▼                               ▼             ├─► accepted ──► (Sent reconciliation)
                      cancelled ◄─────────────────────────┘             ├─► failed       (qualified pre-submission failure)
                                                                    └─► outcome_unknown (connection lost after DATA,
                                                                                         crash before final reply recorded)
```

Every transition below that changes the draft's state does so only if
the attempt's revision is still the draft's current revision (02).
Otherwise the draft keeps its state.

1. **`send_draft(draft_id, revision_id)`:**
   - `revision_id` must be the draft's `current_revision_id`, otherwise
     `Conflict`: only what the user last saw is sent.
   - **First,** outside any transaction, build the MIME and publish it to
     the object store (03). If publishing fails, nothing else happens.
   - **Then,** in one transaction:
     - insert `send_attempts` (`held`, `hold_until = now + undo window`)
       and `send_recipients` (`pending`, one row per address, 02);
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
   - On startup, any attempt found in `submitting` becomes
     `outcome_unknown`: never retried automatically.
4. **SMTP session** (through `lettre`, or the transport chosen in 01):
   - **Connect and authenticate.** Connect to Bridge's SMTP port, run
     STARTTLS (or implicit TLS) with the pinned certificate, then
     `AUTH PLAIN` or `LOGIN` with the Bridge password.
   - **Send the envelope.** `MAIL FROM:<identity address>`. Each
     `RCPT TO` reply is recorded per recipient as `rcpt_accepted` or
     `rejected_permanent`/`rejected_transient`.
   - **`DATA`:**
     - **250 final reply** → attempt `accepted`. Every `rcpt_accepted`
       recipient becomes `sent` (accepted by Bridge, not delivered). The
       draft becomes `sent` only if its `current_revision_id` still
       equals the attempt's revision (02, drafts with live attempts).
     - **Any other final reply to `DATA`, on a Bridge account** →
       `outcome_unknown`. Bridge submits to Proton (creates a draft, then
       calls `SendDraft`) before it answers. Its generic 554 can follow a
       lost Proton reply after Proton already accepted the message, so a
       negative DATA reply doesn't prove nothing was sent. The reply text
       is shown verbatim.
     - **Exception:** a reply whose text matches a **qualified
       pre-submission class** becomes `failed` with recipients `unsent`.
       The classes are invalid sender, invalid recipient, and message too
       large, each confirmed in the real-Bridge test lane (M2.7) to occur
       before any Proton submission. The list starts empty and grows only
       with that evidence.
     - **Generic IMAP accounts** (*later*): definite 5xx → `failed`; 4xx →
       `failed` with a retry offer.
     - **Connection lost or timeout after `DATA` was sent** →
       `outcome_unknown`.
   - **Partial RCPT rejection** with DATA accepted: rejected recipients
     stay `rejected_*` and are offered as a new attempt to those
     recipients only. Accepted recipients are never re-sent.
5. **`outcome_unknown`** shows in the Out Box with the frozen message, the
   time and what is known. The user resolves it with
   `resolve_unknown_send`:
   - **Mark as sent** (e.g. after finding it in Sent): the draft becomes
     `sent`;
   - **Send again**: after an explicit duplicate warning, creates a new
     attempt with the unknown attempt's frozen revision, bytes and
     recipients (the same `Message-ID`, so recipients' clients can spot
     the duplicate); the draft is `queued` again;
   - **Keep as draft**: the draft returns to `editing`.
   - Each of these changes the draft's state only under the
     current-revision rule (02): a newer revision stays `editing`.
   - **A `failed` attempt** also returns its draft to `editing`.
   - Sweetbriar may show evidence: a Sent occurrence whose headers carry
     the attempt's `Message-ID`. Evidence is shown, never acted on
     automatically.
6. **Retries:** only `failed` attempts with a definite pre-acceptance
   failure, and only on user request.
   - Automatic retry with backoff applies only to failures *before*
     `DATA`: connection refused, `AUTH` temporarily unavailable.
   - **Nothing after `DATA` is retried automatically.**

7. **`discard_draft`** is refused with `Conflict` while the draft has an
   attempt in `held`, `queued`, `submitting` or `outcome_unknown`. The
   user cancels or resolves the attempt first, so discarded mail is
   never sent.

## The Sent copy

- **Proton Bridge saves the Sent copy itself** (`server_saves_sent=1`).
  Sweetbriar **never** APPENDs to Sent for Bridge accounts.
- **Reconciliation:** after `accepted`, the attempt's `sent_copy` is
  `server_saves`. The next Sent sync links the server's Sent occurrence
  to the attempt by `Message-ID` header (display only).
  - A Sent copy missing after 24 h shows a soft warning in the Out Box.
  - **It never triggers a resend.**
- **Generic IMAP** (*later*): `server_saves_sent` comes from the
  provider table. When 0, an `append` intent files the frozen bytes into
  the Sent mailbox after `accepted`. If that fails, it is repaired by
  retrying the APPEND, never the SMTP submission.

## Drafts on the server

Drafts live locally in the pilot. Server Drafts sync (APPEND to Drafts
with replacement) is *later*: Bridge's draft replacement and deletion
semantics need their own qualification.

## Restore and send

After a restore (03), every attempt in `held`, `queued` or `submitting`
becomes `outcome_unknown`. Nothing is sent until the user resolves it.
