# M4 — Send

**Revised 2026-11-18 (owner decision, 2026-11-15):** Proton accounts
send through Sweetbriar's own Proton API client (spec/05, revised the
same day), not SMTP through Bridge. No M4 step had merged before this
revision.

**Outcome:** the owner composes and sends through Proton, from the right
identity, with the identity strip, the cross-account check and an
inspectable Out Box. Unknown outcomes are never retried automatically.

### M4.1 Identities and From resolution — M, linux

- **Goal:** identities and the default-identity rules for new, reply,
  reply-all and forward.
- **Spec:** 05 (Identities and From resolution), 02 (`identities`).
- **Depends on:** M3 complete.
- **Deliverables:**
  - `set_default_identity`, and `upsert_identity` for accounts whose
    identities aren't Proton's (Proton identities come from its
    addresses, M2.12);
  - a pure resolver function with its inputs (context, source
    occurrence, recorded send attempt, identities);
  - cross-account check validation in the core.
- **Acceptance** (table-driven tests):
  - replying to own sent mail restores the recorded alias A, not
    default B;
  - replying to received mail picks the identity in To, Cc or
    `Delivered-To` of that exact occurrence;
  - offline and header-only, Reply goes to `Reply-To` when it differs
    from From;
  - no valid identity gives "Choose a sending identity";
  - after a restart, offline, a reply to a message whose body wasn't
    downloaded resolves its identity from `message_headers` (including
    `Delivered-To`);
  - a cross-account reply raises the check, and acknowledgement is
    invalidated by identity or recipient edits.

### M4.2 Outgoing MIME — M, linux

- **Goal:** build outgoing messages from draft revisions.
- **Spec:** 05 (Composing).
- **Depends on:** M4.1.
- **Deliverables:**
  - `sb-mime::compose`:
    - plain: flowed;
    - Markdown: `pulldown-cmark` with raw HTML disabled; a
      `multipart/alternative` with a plain-text rendering of the Markdown
      (not the raw source) and sanitized HTML with inline CSS;
    - the per-draft literal plain-text option, without flowed wrapping;
    - attachments: `multipart/mixed` and `related`;
    - headers per 05;
  - the local Proton limit checks (25 MB, 100 files).
- **Acceptance:**
  - `insta` snapshots for representative drafts;
  - HTML in Markdown source appears as literal text in the HTML part;
  - the `text/plain` alternative contains no Markdown syntax markers for
    emphasis or headings (snapshot);
  - generated messages re-parse with `sb-mime::parse` to the same
    content;
  - limits block with an explanation;
  - a draft with Bcc recipients produces MIME with no `Bcc` header.

### M4.3 Outbox and Proton submission — L, linux

- **Goal:** the outbox state machine submitting through the Proton API,
  with per-recipient encryption and `outcome_unknown`.
- **Spec:** 05 (Outbox state machine, Proton submission, The Sent copy,
  Drafts on the server, Restore and send), 02 (`send_attempts` with its
  Proton columns, `send_recipients`), 10 (keys), 11 (the fake's send
  calls).
- **Depends on:** M4.2.
- **Deliverables:**
  - `send_draft`, `cancel_send`, `resolve_unknown_send`,
    `confirm_send_route`, and `retry_rejected_recipients` (refused with
    `Conflict` for Proton attempts, which have no partial acceptance);
  - the migration adding `proton_draft_id`, `proton_uploads_json` and
    `send_requested_at` (02);
  - the transport: recipient key lookup, draft create and update,
    attachment upload, and send with one package per MIME type, the
    body and attachments encrypted and signed with `proton-crypto`;
  - the fake server's support: key lookup for internal and external
    recipients, drafts, uploads and sends, recording each recipient's
    package and counting sends, with faults that drop the connection
    after a send is applied and that refuse with a chosen code;
  - startup recovery by `send_requested_at` (05);
  - the Proton evidence check for `outcome_unknown`;
  - Sent reconciliation by `proton_draft_id`, display only;
  - `seen_addresses` sent-to counts updated on `accepted`;
  - a send suite on the `go-proton-api` lane (M2.17) that records which
    refusals occur before anything is sent: the evidence for the
    qualified pre-submission list (05).
- **Acceptance:**
  - crash cuts at every transition give **zero duplicate sends** (the
    fake's counter) across 200 seeds;
  - a connection drop after the send request gives `outcome_unknown`
    and no retry; when the fake had applied the send, the evidence check
    makes the attempt `accepted` without a second send;
  - a refusal of the send request gives `outcome_unknown`, not `failed`,
    unless its code is on the qualified pre-submission list (empty until
    lane evidence adds entries);
  - an internal recipient's package carries the session keys encrypted
    to its key, and the fake decrypts the body with that recipient's
    key; an external recipient without keys gets the clear scheme; an
    external recipient with keys fails the attempt before any draft is
    created (the fake holds none);
  - a crash after the draft was created reuses it (one draft on the
    fake), and recorded uploads aren't repeated;
  - an account reconfiguration between queueing and submission keeps the
    attempt `queued` until `confirm_send_route`, and `cancel_send` works
    while it waits;
  - queue R1, save R2, then R1 is accepted: the draft stays `editing` with
    R2, which isn't marked sent. Cancelling R1 likewise leaves R2 intact;
  - queue R1, save and queue R2: R1's acceptance, cancellation, or its
    resolution as sent or kept as draft leaves the draft `queued` with
    R2;
  - an attempt whose account or identity doesn't match its revision's
    route snapshot is rejected by the schema (negative test);
  - `send_draft` with a revision other than the current one gets
    `Conflict`;
  - two `send_draft` calls with different keys, concurrent or in turn,
    queue one attempt; the second gets `Conflict`. Sending a `sent` or
    `discarded` draft gets `Conflict`;
  - each `resolve_unknown_send` choice resolves the attempt exactly once
    and survives a restart. After "Mark as sent" or "Keep as draft",
    discarding succeeds when no other attempt is live; after "Send
    again", it gets `Conflict` until the new attempt ends. With a queued
    R2, any choice leaves R2 `queued`;
  - recipients become `unknown` on `outcome_unknown`; "Mark as sent"
    makes them `sent`;
  - a cross-account reply without the `cross_account` ack gets
    `Conflict`; with it, it sends; an identity or recipient edit (a new
    revision) needs a new ack;
  - `discard_draft` is refused while an attempt is `held`, `queued`,
    `submitting` or `outcome_unknown`;
  - "Send again" reuses the unknown attempt's frozen bytes (same
    `Message-ID`);
  - `confirm_send_route` adopts the new `config_generation`, and fails
    if the identity is no longer sendable;
  - `a@X.NET` in To and `a@x.net` in Cc produce one recipient in the
    package;
  - a definite pre-acceptance failure gives `failed`, with retry only on
    request;
  - no Sent copy is uploaded or imported (request log);
  - the restore test turns pending attempts into `outcome_unknown`.

### M4.4 Compose window, identity strip and Out Box — L, macos-ci + owner

- **Goal:** the full compose experience with account colors and the Out
  Box.
- **Spec:** 08 (Composer, Identity strip and account colors), 05, research
  15.
- **Depends on:** M4.3, M1.10.
- **Deliverables:**
  - the identity strip (`NSTitlebarAccessoryViewController`, opaque,
    `fullScreenMinHeight`, accessibility label);
  - the From `NSPopUpButton` with swatches, unavailable identities
    disabled;
  - the cross-account banner and send confirmation;
  - Strong mode;
  - Undo Send with a 7 s hold;
  - the Out Box view (queued, held, unknown, failed) with the resolve
    actions;
  - Reply, Reply All and Forward commands;
  - composer extras deferred from M0.11: paste as Markdown with a preview
    and a lossy-conversion warning, inline images (`multipart/related`),
    an external editor round trip;
  - Settings → Sending safety: cross-account check, always choose
    identity, undo-send delay.
- **Acceptance:**
  - an XCUITest switches From: the strip label and color change, and the
    accessibility announcement is posted;
  - a full-screen compose window keeps the strip visible;
  - Undo within 7 s cancels with no request to Proton (fake request
    log);
  - Differentiate Without Color shows the stronger monogram;
  - **owner:** the strip reads clearly in light, dark, Increase Contrast
    and Reduce Transparency.

### M4.Q Qualification

Agent-run on the owner's Mac (plan/README.md), recorded in
`plan/qualification/M4.md`. The agent asks the owner before every real
send, and for its recipients and content.

1. Send from the primary identity and from secondary or custom-domain
   identities actually used. To, Cc and Bcc. Attachments near the
   limits.
2. One send produces exactly one Sent copy, and Sweetbriar uploads none.
3. A message to a Proton recipient arrives encrypted; one to an
   external recipient arrives in clear, as the Out Box said.
4. Reply from the correct identity for mail received at an alias.
5. The cross-account banner appears where expected.
6. Simulated lost reply (fixture only): the Out Box shows "Outcome
   unknown" with no resend.
