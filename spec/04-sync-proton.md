# 04 — Proton sync (Sweetbriar's own Proton API client)

Owner decision, 2026-11-15: Sweetbriar talks to Proton through its own
implementation of the Proton API and does not depend on Proton Mail
Bridge (plan/README.md). Rests on
[research/proton-api](../research/proton-api/README.md). Facts marked
*unverified* there are qualified on the owner's account before the
behaviour that depends on them is enabled (plan M2.Q onward).

The generic IMAP engine in [04-sync-imap](04-sync-imap.md) stays as
tested code for generic IMAP accounts (*later*). Its breaker,
intent-state and account-health rules are shared; this document says
where Proton differs.

## Structure

- **The Proton transport** (the `sb-proton` crate, L5): HTTPS to the
  Proton API, JSON types, SRP sign-in, sessions and refresh, rate-limit
  handling, events, labels, messages and attachments, and the OpenPGP
  work: unlocking keys, decrypting bodies and attachments, verifying
  signatures (and, from M4, encrypting and signing). It knows nothing
  about the store; it returns observations and decrypted parts.
- **Reconstruction** lives in `sb-mime` (L5): it turns a decrypted
  message's parts into one RFC 822 message (below). `sb-resources`
  joins the two, as it does for IMAP and SMTP.
- **The provider policy `proton_api`** lives in `sb_engine::policy`, next
  to `generic_imap`. It maps labels to mailboxes, defines identity and
  the mutation matrix, and tells the transport which calls to make.
- **The engine** (01) owns planning, reconciliation, intents and the
  breaker, as for IMAP. The transport never writes the store.

## Proton facts the engine relies on

| Fact | Consequence |
|---|---|
| API host `https://mail.proton.me/api`, JSON over HTTPS; routes under `/core/v4`, `/mail/v4` and `/auth/v4` | One host per account, recorded in `proton_accounts.api_base_url`; the only host in the pilot |
| Every request needs `x-pm-appversion`. Proton has no registration for third-party clients: unknown products are refused (2064) and `Other` is accepted; official product strings are accepted | **Sweetbriar sends `Other` and an honest `User-Agent: Sweetbriar/<version> (macOS <version>)`. It never sends an official Proton client's identity** (10). If Proton refuses `Other`, the account shows why; there is no fallback |
| Proton's terms allow automated access only while it is indistinguishable from a standard client, and Proton may block it without recourse | Traffic stays within the pacing below; no bulk export, no parallel bursts |
| Messages and labels have stable string ids. A message carries `LabelIDs`: at most one folder or system location, any number of user labels, plus All Mail (`5`), which the server adds to every message, and Starred when starred | `logical_messages.provider_key` is the message id and is authoritative. One occurrence per label the message carries (02) |
| System labels: Inbox `0`, Trash `3`, Spam `4`, All Mail `5`, Archive `6`, Sent `7`, Drafts `8`, Starred `10`, Scheduled `12`; also AllDrafts `1`, AllSent `2`, Outbox `9`. User labels have type 1, folders type 3 (folders may nest) | Mapping below |
| Read state is per message (`Unread`); a star is label `10`; `IsReplied`, `IsRepliedAll`, `IsForwarded` and the draft state are per message | Every occurrence of a message carries the same flags |
| Bodies and attachments are OpenPGP-encrypted to the address's keys. Attachments come as key packets plus a separately downloaded data packet | Decryption needs the unlocked address keys (10) |
| There is no raw RFC 822 endpoint. A full message gives the stored header block (`Header`), the decrypted body with its `MIMEType`, and attachment metadata | Sweetbriar reconstructs the message (below); bytes are never compared for identity |
| `GET /core/v4/events/latest` returns a cursor; `GET /core/v4/events/{id}` returns the changes since it, with `More` when there are more. Message events carry full metadata. `Refresh` set means local mail state must be rebuilt | The event cursor is the sync checkpoint (02). A refresh is the Proton equivalent of an epoch change |
| Listing pages through message metadata (`/mail/v4/messages`, up to 150 per page); `/mail/v4/messages/count` gives per-label totals | Bootstrap and inventory listing; coverage compares with the totals |
| Human verification is error 9001 with a token and methods; 2FA is TOTP or FIDO2; some accounts have a separate mailbox password | Sign-in handles each (below). FIDO2-only accounts aren't supported in the pilot |
| 429 and 503 carry `Retry-After`; other refusals carry a numeric `Code` | Backoff below. Codes are mapped in one table in `sb-proton` with the research citation for each |

## Sign-in and sessions

1. **Sign-in** (07 `begin_proton_sign_in`):
   - `POST /auth/v4/info` with the username, then the SRP proofs
     computed by `proton-srp` (01): the server's modulus is accepted
     only with a valid signature from Proton's embedded modulus key, and
     the server proof is checked in constant time before anything is
     trusted. The password never leaves the process and is dropped
     (zeroized) as soon as the proofs and the key passphrase exist.
   - **Human verification (9001):** the sign-in pauses with a
     `human_verification` challenge carrying Proton's verification URL.
     The app shows it in an isolated web view (08) and returns the token
     Proton hands back; the request is retried once with it.
   - **2FA:** a TOTP code is a `second_factor` challenge. An account
     whose only second factor is a security key is refused with an
     explanation (add an authenticator app, or wait for FIDO2 support,
     *later*).
   - **Two-password mode:** a `mailbox_password` challenge.
   - **Keys:** the key passphrase is derived from the (mailbox) password
     and the key salt (`/core/v4/keys/salts`). The user's primary key is
     unlocked with it, then each address key (token-encrypted keys
     through the user key, legacy keys with the passphrase), using
     `proton-crypto-account`. A key that can't be unlocked is recorded
     and shown; it does not fail the sign-in unless no address key
     unlocks.
   - **Identities** come from `/core/v4/addresses`: one per enabled
     address, `sendable='validated'` when Proton allows sending from it
     and an unlocked key exists, else `unavailable`.
   - Only after all of this do the account rows, the identities and the
     secrets commit, as for Bridge accounts (07).
2. **Secrets** (10): the session's UID, refresh token and the key
   passphrase. Never the password. Access tokens live in memory only.
3. **Refresh:** a 401 refreshes once (`POST /auth/v4/refresh`) and
   retries the request. Refreshes are serialized per account, since
   Proton rotates the refresh token: a 401 that arrives while a refresh
   is in flight waits for it and retries with the new access token,
   and the new refresh token is stored before any request uses the new
   access token. A
   refused refresh (10013) puts the account in
   `needs_attention` ("Sign in again"); local data stays.
4. **On every start** the engine restores the session from the secret
   store and unlocks the keys in memory, without the password.
5. **Disconnect** revokes the session (best effort, `DELETE /auth/v4`)
   and deletes the account's secrets. Data stays (02); reconnecting
   signs in again with the same account UUID.

## Connections and pacing

- At most two HTTP/1.1 connections per account, one per request in
  flight, and one kept alive when idle; at most two requests in flight
  per account (the worker and one body fetch), and at most
  four requests per second per account in the background. Interactive
  requests (opening a message) go first.
- **Timeouts:** 30 s per request; 120 s per body or attachment download,
  scaled for large ones.
- **Backoff:** 429 and 503 wait for `Retry-After` (at least 1 s, at most
  1 h), then retry. Network errors back off exponentially from 1 s to
  5 min, and, as for IMAP, a session that hasn't completed a pass
  doesn't reset the backoff. A request that may have reached Proton (a
  timeout, or a connection lost after it was sent) is retried only when
  its method is idempotent; a sign-in or refresh is never replayed. A
  request holds its in-flight slot until its body has been read.
- **Fault handling:** a response that doesn't decode is a failed request,
  never a partial observation. Nothing from it is applied.

## Mailbox mapping

| Proton label | Mailbox | `kind` | `role` |
|---|---|---|---|
| Inbox `0` | Inbox | `folder` | `inbox` |
| Drafts `8` | Drafts | `folder` | `drafts` |
| Sent `7` | Sent | `folder` | `sent` |
| Archive `6` | Archive | `folder` | `archive` |
| Spam `4` | Spam | `folder` | `junk` |
| Trash `3` | Trash | `folder` | `trash` |
| Starred `10` | Starred | `virtual` | `flagged` |
| All Mail `5` | All Mail | `virtual` | `all` |
| Scheduled `12` | Scheduled | `virtual` | none (read-only in the pilot) |
| Folder (type 3) | its path | `folder` | none |
| Label (type 1) | its name | `label` | none |

AllDrafts, AllSent and Outbox aren't mapped. The system mailboxes
exist for every account, listed or not. `mailboxes.provider_key`
holds the label id; `remote_name` the display path, which may change
without changing the mailbox. Folders and labels are separate
namespaces in Proton, but `remote_name` is unique per account: a name
already taken (by a system mailbox, or a folder before a label) gets
its label id appended, as in "Travel (l1)" for the label with id
`l1`, and again while the result is taken too.

## Sync loop

1. **Bootstrap** (a new account, or after a refresh):
   1. read `/core/v4/events/latest` and keep it as the pending cursor
      **before** listing, so nothing that changes during the listing is
      lost;
   2. list labels and folders, reconciled with `mailboxes` (new ones
      added, missing ones `gone`, renames applied in place);
   3. page through message metadata, newest first. **Each page commits
      in one transaction** with its listing checkpoint: the logical
      messages (found or created by `(account, provider_key)`), their
      occurrences, `message_headers` from the metadata (02) and
      `d_list_rows`. A crash resumes from the checkpoint without gaps
      or duplicates;
   4. when the listing completes, compare with the per-label totals;
      the pending cursor becomes the event cursor in the same
      transaction that records the listing complete.
2. **Events:** poll the cursor every 30 s while the app is active and
   every 2 min otherwise, and immediately after a local mutation. Fetch
   until `More` is 0, applying each response in one transaction with
   its new cursor. A response past the write budget (03) is applied in
   chunks, the cursor moving with the last; each change carries the
   message's full state, so a chunk replayed after a crash changes
   nothing:
   - message create, update and flag updates replace the message's
     metadata: occurrences are added for labels it gained, and those for
     labels it lost become `vanished` (a label removal is
     authoritative, not an inference); flags update on every
     occurrence;
   - a message delete is a disappearance and goes through the breaker;
   - label events update mailboxes: the labels are listed again and
     reconciled as in step 1.2, since a mailbox name depends on every
     label (Mailbox mapping);
   - address, user and key events unlock the keys again with the key
     passphrase kept in memory and refresh the identities (as at
     sign-in), before the response's cursor moves, so a failure
     retries the response. An identity that changed bumps the
     account's `config_generation` (02); an event that changed
     none writes nothing. Keys the passphrase no longer unlocks, as
     after a password change elsewhere, put the account in
     `needs_attention` ("Sign in again") with the response unapplied;
     local data stays;
   - **`Refresh` with the mail bit** starts a bootstrap (below, Refresh).
3. **Acquire bodies.** A queue fetches full messages for logical
   messages without an acquisition: messages the user opened first,
   then newest first.
   - `GET /mail/v4/messages/{id}`, then each attachment; decrypt with
     the address keys; reconstruct; publish (03); insert the acquisition
     linked to the logical message; refresh `message_headers` from the
     stored header block; queue for search.
   - The id in the response must equal `provider_key`; anything else is
     refused and the message's metadata is re-read by id
     (`/mail/v4/messages` filtered by `ID`, the live target check's
     read). The metadata it returns updates the message's occurrences
     as an event's would, without moving the event cursor or counting
     for the breaker. An empty answer writes nothing, since only a
     delete event or a complete listing removes a message; an answer
     naming another message is refused and writes nothing either. The
     store applies a re-read only once the bootstrap listing is
     complete and at the account revision the queue read; otherwise
     it refuses it and the run retries. The body isn't fetched again
     in that run.
   - A body that can't be decrypted gets no acquisition. The message
     records the failure in `proton_body_failures` (02) and shows "Couldn't decrypt this message";
     it is retried after a key change.
   - A body is fetched once per logical message, never per occurrence.
   - Reading never marks a message read remotely; only an explicit
     `set_flags` intent does (M3).

**Coverage** per account records whether the bootstrap listing is
complete, per-mailbox local counts against Proton's totals, and the
counts of messages with and without bodies (and with decryption
failures). The UI says "Proton messages synced", with these numbers.

## Reconstruction

`sb_mime::reconstruct` builds one RFC 822 message from the decrypted
parts, deterministically: the same parts always give the same bytes.

- **Header:** Proton's stored header block, unchanged, except that its
  `Content-Type` and `Content-Transfer-Encoding` are replaced by those
  of the reconstructed body. It gains `X-Pm-Internal-Id: <id>`.
- **Body:** a `multipart/*` `MIMEType` means the decrypted body is
  already the MIME entity and is used as is. Otherwise the body is a
  `text/plain` or `text/html` part; inline attachments with a
  `Content-ID` join it in `multipart/related`, and the other
  attachments follow in `multipart/mixed`, base64-encoded, with their
  names, types and dispositions.
- **Boundaries** derive from the message id and the part's position, so
  they are stable and can't collide with the content (checked).
- Its `acquisition_sources` row has `source_kind='proton_fetch'` and
  `source_ref` `<message id>#r<reconstruction version>` (02), so a later,
  better reconstruction can re-acquire a message without losing the old
  original.
- Fixtures cover each shape (plain, HTML, related images, mixed, PGP/MIME
  bodies, non-UTF-8 charsets, attachments with hostile names).

## Refresh

A `Refresh` with the mail bit invalidates what the client knows:

1. Pause destructive reconciliation for the account. Pending intents
   wait (`wait_reason='resync'`); none are quarantined, since ids
   don't change.
2. Run the bootstrap again. Messages are matched by id, so tags, notes
   and bodies stay with their logical messages.
3. When the listing completes, messages stored as present but absent
   from it become `vanish_pending`. They become `vanished` only after a
   second complete listing or a delete event, through the breaker.
   - Each listed page notes its messages (`proton_listed`, 02), so a
     listing resumed after a crash misses nothing it listed.
   - The missed messages are withheld in budget-sized writes before the
     listing completes, and recorded in `proton_disappearances` as
     unconfirmed at the listing's cursor.
   - An unconfirmed disappearance isn't classified. A delete event
     confirms it into that response's pass; a later complete listing
     that misses it again confirms it, and the breaker classifies it as
     that listing completes. A message listed again is present again.
4. A refresh that would make more messages disappear than the
   breaker's thresholds allow records one `breaker_event` of kind
   `epoch_change` (a Proton refresh is that account's epoch change, 02);
   otherwise none.
   - The thresholds apply per mailbox of the missed messages'
     locations; Trash and Spam don't count (retention).
   - The event holds every message that listing missed, and pauses the
     account like any other: Accept applies them, Keep keeps them
     withheld through later listings.

## The breaker

As in 04-sync-imap, with Proton's evidence:

| Class | Example | Action |
|---|---|---|
| **Explained** | A move or delete this client performed (matching `done` intent) | Apply |
| **Retention** | A delete event for a message whose last location was Trash or Spam (Proton can empty them automatically) | Apply |
| **Label change** | The message lost a label but still exists | Apply (the occurrence `vanished`, the message stays) |
| **Unexplained disappearance** | Any other delete event, or a message absent from a complete listing | Breaker thresholds |
| **Incomplete observation** | A failed request, an interrupted listing | Never counts |

The thresholds, the pause, Accept, Keep and `cleared` are those of
04-sync-imap, counted per mailbox of the message's last location.

A message whose last occurrence became `vanished` stays in the archive
and is listed in the Recovery list, as 04-sync-imap's is (07
`recovery`). Proton ids don't change, so Proton has no `epoch_stale`
or `superseded` occurrences and no intent needs `retarget_intent`.

- **A pass** is one events response. A delete withholds the message's
  current occurrences as `vanish_pending` and records it in
  `proton_disappearances` (02); the response's last chunk classifies
  the recorded deletes no event holds, so a response in chunks is one
  pass.
- **The location** is the message's one folder when it was deleted;
  All Mail, Starred and labels are views of it. A delete with no
  folder counts only against the per-mailbox count.
- **While an event is `open`,** deletes are recorded but not classified.
  Accept or Keep classifies them, except those of a response still being
  applied in chunks, which its last chunk classifies.
- **`cleared`:** a held message that reappears, in a later response or
  listing, is present again and no longer held; an event left holding
  none becomes `cleared`.
- **Explained** deletes need Proton intents (M3); until then none are
  explained.

## Intents and the mutation matrix

Execution follows 04-sync-imap's intent rules, with Proton's evidence:

- **Live target check:** the message's metadata is read by id
  (`/mail/v4/messages` filtered by `ID`) before every mutation. A
  missing message, or a move whose source no longer holds it, quarantines
  the intent (`ambiguous_match`).
- **Act** with the call below, one message per intent in the pilot.
- **Confirm** by reading the metadata again: the requested state holds →
  `done`.
- **Reconcile before retry:** Proton's calls set state rather than
  toggle it, so after a dropped connection the engine re-reads the
  metadata and re-issues only if the state isn't there yet.
- **Flag conflicts** follow 04-sync-imap with events in place of the
  sweep: while a flag intent is pending or in flight, events don't
  overwrite that flag on any occurrence of the message; once it is
  done, the next event wins, so another client's later change is
  kept. Events apply in cursor order and the intent's own change
  arrives as a later event, so an older event delivered after the
  intent is `done` is overtaken by it, at the latest on the next poll.
- **Undo** is the inverse intent, as for IMAP. Proton's undo tokens
  aren't used.

| User action | Call | Notes |
|---|---|---|
| Mark read or unread (M3) | `PUT /mail/v4/messages/read` or `/unread` | Applies to the message, so to every occurrence |
| Star or unstar (M3) | `PUT /mail/v4/messages/label` or `/unlabel` with `10` | Same |
| Archive, move to a folder or to Trash (M5) | label with the destination folder or system label | Proton keeps one location; user labels stay. Confirm the new location before `done`. The body must be acquired first, as for IMAP |
| Add a label (M5) | label with the user label | The location stays |
| Remove a label (M5) | unlabel | Allowed: a label removal deletes nothing on Proton |
| Delete permanently, empty Trash | — | Not in the pilot |
| Any action in Scheduled | — | Not in the pilot |

M3 and M5 qualify each call on the fake server and the
`go-proton-api` lane (11) before enabling it, and on the owner's account
(plan M3.Q, M5.Q).

## Account states and health

| Condition | Account state | User sees |
|---|---|---|
| Offline, Proton unreachable, 5xx | `active` (retrying) | "Offline" or "Proton isn't answering", with the next retry |
| Rate limited | `active` (waiting) | "Proton asked Sweetbriar to slow down", with the time |
| Refresh refused (10013), password changed | `needs_attention` | "Sign in again" |
| Human verification asked for during sync | `needs_attention` | "Proton asks you to verify", opening the verification sheet |
| App identity refused (2064, 5003) | `needs_attention` | "Proton refused this version of Sweetbriar", with Proton's message |
| A key couldn't be unlocked | `active` with a warning | Which address is affected; its messages show "Couldn't decrypt" |
| Secret-store error | `needs_attention` | As for every account (10) |
| Refresh or breaker event | `active` with an open event | "Proton asked Sweetbriar to resync" or "Review N disappeared messages" |
| Healthy | `active` | Last sync time and coverage |

None of these deletes local data. Every refusal records Proton's code
and message in the account's diagnostics (redacted, 10): the worker
keeps Proton's numeric code when it maps a refusal. A paused account
(07 `pause_account`) runs no worker and makes no request; its row reads
"Paused". `account_health` (07) answers this table.
