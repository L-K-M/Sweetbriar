# M4 — Send

**Outcome:** the owner composes and sends through Bridge, from the right
identity, with the identity strip, the cross-account check and an
inspectable Out Box. Unknown outcomes are never retried automatically.

### M4.1 Identities and From resolution — M, linux

- **Goal:** identities and the default-identity rules for new, reply,
  reply-all and forward.
- **Spec:** 05 (Identities and From resolution), 02 (`identities`).
- **Depends on:** M3 complete.
- **Deliverables:**
  - `upsert_identity` and `set_default_identity`;
  - a pure resolver function with its inputs (context, source
    occurrence, recorded send attempt, identities);
  - cross-account check validation in the core.
- **Acceptance** (table-driven tests):
  - replying to own sent mail restores the recorded alias A, not
    default B;
  - replying to received mail picks the identity in To, Cc or
    `Delivered-To` of that exact occurrence;
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

### M4.3 Outbox and SMTP — L, linux

- **Goal:** the outbox state machine with SMTP through Bridge, per
  recipient results and `outcome_unknown`.
- **Spec:** 05 (Outbox state machine, The Sent copy, Restore and send),
  02 (`send_attempts`, `send_recipients`).
- **Depends on:** M4.2.
- **Deliverables:**
  - `send_draft`, `cancel_send`, `resolve_unknown_send` and
    `confirm_send_route`;
  - identity promotion from `provisional` to `validated` on Bridge's
    first acceptance (05);
  - `sb-smtp` on `lettre`, with the pinned-cert TLS from M2.2;
  - per-recipient RCPT tracking;
  - startup recovery: `submitting` becomes `outcome_unknown`;
  - Sent reconciliation by `Message-ID`, display only;
  - the fake SMTP server in `sb-test-support`, counting submissions;
  - `seen_addresses` sent-to counts updated on `accepted`;
  - an SMTP suite in the real-Bridge lane (M2.7). It submits to the mock
    Proton API and classifies which Bridge errors occur before any
    Proton submission: the evidence for the qualified pre-submission
    list (05).
- **Acceptance:**
  - crash cuts at every transition give **zero duplicate submissions**
    (fake counter) across 200 seeds;
  - a connection drop after `DATA` gives `outcome_unknown` and no retry;
  - **a Bridge 554 final reply to `DATA` gives `outcome_unknown`**, not
    `failed`, unless its class is on the qualified pre-submission list
    (empty until M2.7 evidence adds entries);
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
  - `discard_draft` is refused while an attempt is `held`, `queued`,
    `submitting` or `outcome_unknown`;
  - "Send again" reuses the unknown attempt's frozen bytes (same
    `Message-ID`);
  - `confirm_send_route` adopts the new `config_generation`, and fails
    if the identity is no longer sendable;
  - `a@X.NET` in To and `a@x.net` in Cc produce one `RCPT TO`;
  - a provisional identity becomes `validated` after its first accepted
    send (fake server);
  - partial RCPT rejection keeps accepted recipients `sent` and offers a
    new attempt only for rejected ones;
  - a definite pre-acceptance failure gives `failed`, with retry only on
    request;
  - no `APPEND` to Sent is issued for Bridge accounts (trace);
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
  - the From `NSPopUpButton` with swatches and provisional marking;
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
  - Undo within 7 s cancels with no SMTP connection (fake);
  - Differentiate Without Color shows the stronger monogram;
  - **owner:** the strip reads clearly in light, dark, Increase Contrast
    and Reduce Transparency.

### M4.Q Owner qualification

Recorded in `plan/qualification/M4.md` from the Bridge checklist, item 9:
1. Send from the primary identity and from secondary or custom-domain
   identities actually used. To, Cc and Bcc. Attachments near the
   limits.
2. One SMTP submission produces exactly one Sent copy, with no
   Sweetbriar APPEND.
3. Reply from the correct identity for mail received at an alias.
4. The cross-account banner appears where expected.
5. Simulated lost final reply (fixture only): the Out Box shows "Outcome
   unknown" with no resend.
