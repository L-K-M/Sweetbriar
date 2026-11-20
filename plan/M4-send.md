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
- **Split (2026-11-18):** the step measured about 2,100 changed lines,
  so it lands as **M4.1a**, the two identity commands through store,
  engine and service (about 970), and **M4.1b**, stacked on it: the
  pure resolver, the header-only read gathering its inputs, reply
  recipients and the cross-account check (about 1,150).

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
  - startup recovery of every Proton `submitting` attempt by
    `send_requested_at` (05): with it, `outcome_unknown` and the
    evidence check; without it, back to `queued` with the recorded
    draft and uploads;
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
    fake), and recorded uploads aren't repeated; an upload whose reply
    the fake dropped is recorded from the draft on retry, so the sent
    message carries each attachment once;
  - a draft creation whose reply the fake dropped isn't reused: the
    retry creates a second draft and sends with it, and the first stays
    in Proton's Drafts (05 "Drafts on the server");
  - a send whose connection was refused, or timed out before it was
    established, stays `queued` with `send_requested_at` cleared, and
    is retried; a timeout after the request began is `outcome_unknown`;
  - `retry_rejected_recipients` on a Proton attempt gets `Conflict` and
    sends nothing (the fake's counter);
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
- **Split (2026-11-19):** a survey of the code put the step at about
  7,800 changed lines, so it lands as seven PRs. M4.3a and M4.3c may
  start in parallel; M4.3b follows M4.3a, M4.3d follows M4.3c, M4.3e
  needs M4.3b and M4.3d, M4.3f follows M4.3e, and M4.3g may start once
  M4.3d merges. Commands reach the FFI with M4.4. The spec PR that
  added this note settled the undo window (`sending.undo_seconds`),
  the `held` to `queued` move, the pilot's single package, the
  submission's place on the account's one worker, the evidence check's
  trigger, the recorded upload fields, the sent-to counts and
  `out_box()`'s rows.

#### M4.3a Outbox queue — linux

- **Deliverables:** the migration adding `proton_draft_id`,
  `proton_uploads_json` and `send_requested_at` (02); the
  `sending.undo_seconds` setting; `send_draft` (current-revision and
  live-attempt checks, compose and publish the frozen bytes, then one
  transaction inserting the `held` attempt and its recipients and
  setting the draft `queued`) and `cancel_send`, through store, engine
  and service.
- **Acceptance:** M4.3's bullets on a non-current revision, `sent` or
  `discarded` drafts, two keys queuing one attempt, one recipient for
  `a@X.NET` and `a@x.net`, the route-snapshot schema refusal,
  `discard_draft` while an attempt is live, and queue R1, save R2
  (R1's cancellation leaves R2 intact).
- **Split (2026-11-19):** the finished step measured 1,902 changed
  lines, so it lands as two PRs. M4.3a1 (about 930 lines) is the store
  layer: migration 34, the `send_draft` and `cancel_send` transactions
  and their schema-refusal tests. M4.3a2 (about 980 lines) follows it
  with the `sending.undo_seconds` setting, the engine's compose,
  publish and replay, and the service commands, carrying the
  acceptance bullets that need a draft saved through the engine.

#### M4.3b Resolution, route and Out Box — linux

- **Deliverables:** `resolve_unknown_send`, `confirm_send_route`,
  `retry_rejected_recipients` (`Conflict` for Proton), the
  `cross_account` ack check in `send_draft`, the sent-to counts on
  `marked_sent`, and `out_box()` (07), through store, engine and
  service.
- **Acceptance:** M4.3's bullets on each resolution (exactly once,
  across a restart, discarding afterwards, a queued R2 untouched),
  recipients `unknown` then `sent`, the cross-account ack, "Send
  again"'s frozen bytes, `confirm_send_route`, a reconfiguration
  keeping the attempt `queued`, and `retry_rejected_recipients`.
- **Split (2026-11-19):** everything but `out_box()` measured 1,441
  changed lines, so it lands as two PRs. M4.3b1 is the resolutions,
  the route confirmation, the retry refusal and the cross-account ack;
  M4.3b2 adds `out_box()` with the reconfiguration bullet.

#### M4.3c Fake server send calls — linux

- **Deliverables:** the fake's key lookup for internal and external
  recipients (with and without keys), draft create and update,
  attachment upload with the draft reporting each attachment's name,
  type and size, and send, recording each recipient's package and
  counting sends; faults that drop the reply after a draft creation,
  an upload or a send was applied, refuse a connection before it is
  established, stall before the TLS handshake, and refuse with a
  chosen code (11).
- **Acceptance:** tests driving the fake directly: each call's
  recorded effect, the counter, and each fault.
- **Split (2026-11-19):** the calls measured about 1,400 changed
  lines, so the faults land as their own PR: M4.3c1 the calls and the
  counter, M4.3c2 the four faults. M4.3d follows M4.3c2.

#### M4.3d Proton submission transport — linux

- **Deliverables:** in `sb-proton`, the recipient key lookup, draft
  create and update, attachment upload and send with one package per
  MIME type, the body and attachments encrypted and signed with
  `proton-crypto`; the sender's address key for signing; errors that
  keep Proton's code for identity refusals and tell a connection never
  established from one lost after the request began.
- **Acceptance:** against the fake, an internal recipient's package
  carries the session keys encrypted to its key and the fake decrypts
  the body; an external recipient without keys gets the clear scheme;
  a dropped send reply is not replayed.
- **Split (2026-11-19):** the step measured about 2,500 changed lines,
  so it lands as two PRs: M4.3d1 the encryption, signing and packages
  (about 970 lines), with the internal and clear-scheme acceptance
  checked through the fake's decryption; M4.3d2 the HTTP calls and
  their error classes (about 1,530 lines), with the round trip, the
  dropped reply and the connect failures. M4.3d2 follows M4.3d1.

#### M4.3e Submission on the account worker — linux

- **Deliverables:** the `held` to `queued` move, at the worker's
  ticks and at startup, the `config_generation` gate, the claim, submission steps 1 to 5 between
  the worker's sync steps, the reply interpretation with the (empty)
  qualified pre-submission list, failures before `send_requested_at`
  with the in-memory backoff, `needs_attention` on an identity or
  session refusal, the sent-to counts on `accepted`, and Sent
  reconciliation by `proton_draft_id`.
- **Acceptance:** M4.3's bullets on a dropped send reply, a refusal of
  the send, draft and upload reuse after a crash, a dropped upload or
  draft-creation reply, a connection never established versus a
  timeout after the request began, an external recipient with keys,
  a definite failure, R1 accepted with R2 saved or queued, and no Sent
  copy uploaded.
- **Split (2026-11-19, re-cut 2026-11-20):** a survey measured about
  3,740 changed lines, so the step lands as PRs that follow one
  another: M4.3e1 the store transitions and the `held` to `queued`
  move (about 1,290); M4.3e2, which measured about 1,715 changed lines
  once it submitted an attempt without attachments, as two: M4.3e2a
  the sender's key, the frozen message's parts, the worker's wait
  ending with an undo window and the wakes from the commands that
  queue an attempt (about 700), then M4.3e2b the submission on the
  worker: keys, draft, send, the reply interpretation, the in-memory
  backoff and `needs_attention` (about 1,340); M4.3e3 the attachment
  uploads and Sent reconciliation (about 960 changed lines). No FFI
  command reaches send before M4.4.

#### M4.3f Recovery, evidence and crash cuts — linux

- **Deliverables:** startup recovery of Proton `submitting` attempts
  by `send_requested_at` (M4.3e queues the `held` ones); the evidence
  check (05); the crash-cut suite over every transition.
- **Acceptance:** zero duplicate sends across 200 seeds; a dropped
  send the fake applied becomes `accepted` by evidence without a second
  send; the restore test turns pending attempts into
  `outcome_unknown`.

#### M4.3g Send suite on the `go-proton-api` lane — linux (nightly)

- **Deliverables:** the lane's send suite and driver support, recording
  which refusals occur before anything is sent (the evidence the
  qualified list needs, 05).
- **Acceptance:** the lane runs green and records a dated artifact.

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
- **Split (2026-11-20):** the send commands' FFI surface alone measured
  1,189 changed lines (bindings excluded), so M4.4 lands as five PRs,
  each keeping its deliverables and the acceptance bullets they carry.
  M4.4a: `send_draft`, `cancel_send`, `resolve_unknown_send`,
  `confirm_send_route`, `retry_rejected_recipients` and `out_box()`
  over FFI, with the bridge's `SendSource`. M4.4b: the send flow in the
  compose window (sendable Proton identities in From, Send, Undo
  Send's hold) and Settings → Sending safety's undo delay, with the
  Undo acceptance; the strip, swatches, disabled identities, Strong
  mode, full screen and Differentiate Without Color shipped with M1.10
  and its hosted tests. M4.4c: the Out Box view and its resolve
  actions. M4.4d: Reply, Reply All and Forward, with the cross-account
  banner and Send's confirmation (the core's revision-bound
  acknowledgement reaches the shell there, since only replies need it)
  and the From-switch XCUITest over a UI-test account that can send.
  M4.4e: the composer extras deferred from M0.11. The owner check runs
  after M4.4e.

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
