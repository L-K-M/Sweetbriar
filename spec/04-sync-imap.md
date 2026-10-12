# 04 — IMAP sync (Proton Bridge first)

Rests on research [04-data-safety](../research/04-data-safety.md), D2/D3
in [debate/04-resolution](../research/debate/04-resolution.md), and
[raw/plan-proton-bridge/codex.md](../research/raw/plan-proton-bridge/codex.md).
The Bridge facts below come from Bridge **v3.27.1** source and Proton's
docs (checked 2026-10-12). They must be requalified on the owner's Mac
(the manual checklist in the plan) before mutations are enabled.

## Structure

- **The generic IMAP transport** (the `sb-imap` crate):
  connections, TLS, the IMAP codec, command and response handling, and
  capability discovery. It knows nothing about Proton.
- **A provider policy** declares the dialect: multi-membership,
  COPY-may-move, server-saves-Sent, the allowed mutation matrix, and
  identity headers.
  - The pilot has two policies, `proton_bridge` and `generic_imap`
    (Dovecot in CI).
  - **Policies live in the engine** (`sb-core::engine::policy`). The
    engine consults them before planning any remote effect and tells the
    transport exactly which commands to send. `sb-imap` stays
    dialect-free.
- **The engine** (01) owns planning, reconciliation, intents and the
  breaker. The transport never writes the store; it returns observations
  that the engine applies.

## Proton Bridge facts the engine relies on

| Fact | Consequence |
|---|---|
| Listens on `127.0.0.1`. IMAP 1143 and SMTP 1025 by default (configurable). STARTTLS by default; "SSL" means implicit TLS on the displayed port | The account preset fills these in; the user copies the values from Bridge |
| Self-signed certificate, IP SAN `127.0.0.1` | Pin it (10). Always connect to the literal `127.0.0.1`, never `localhost` |
| Credentials are a Bridge-generated password, not the Proton password | Never ask for the Proton password |
| Capabilities: IMAP4rev1, IDLE (can be disabled remotely), UIDPLUS, MOVE, UNSELECT, ID. **No CONDSTORE/QRESYNC, NAMESPACE, LITERAL+ or SPECIAL-USE** (roles still appear as LIST attributes) | No modseq-based sync. Flags are swept. Use synchronizing literals. Read roles from LIST attributes |
| Mailboxes: INBOX, Drafts, Sent, Starred, Archive, Spam, Trash, All Mail, Scheduled (hidden when empty); `Folders/<path>` and `Labels/<name>`; `Folders` and `Labels` are `\Noselect`; delimiter `/` | `kind='folder'` or `'label'` from the prefix. Starred, All Mail and Scheduled are `kind='virtual'`. Roles: Starred `flagged`, All Mail `all`, Scheduled none (read-only in the pilot). Roles come from LIST attributes, else from these names |
| One Proton message can appear in its folder, several `Labels/…`, All Mail and Starred | One occurrence per mailbox appearance, grouped by `X-Pm-Internal-Id` |
| Bridge injects `X-Pm-Internal-Id` (the Proton message id) into fetched headers | `logical_messages.provider_key` for the Proton account. Its persistence is unverified, so it is evidence, not authority |
| BODY[] is **reconstructed** from Proton's data. MIME boundaries are random since v3.25.0 | The same message fetched twice gives different bytes. Fetch a body once per logical message, and never compare bytes for identity |
| Bridge can return a synthetic "Message failed to build" substitute | Don't classify by subject. Coverage reports Bridge-visible inventory, not all Proton mail |
| All Mail can be hidden, and can exclude Spam and Trash per Proton settings | Never treat All Mail as complete inventory. Sync every selectable mailbox |
| UIDVALIDITY is assigned at mailbox creation. Bridge repair, cache loss and address-mode changes rebuild mailboxes (new epochs) | An epoch change is normal after a repair; handle it as described below |
| Permanent flags: `\Seen`, `\Flagged`, `\Deleted`, `$Forwarded`, `Forwarded`; Proton metadata also yields `\Answered` and `\Draft`. `\Seen` maps to read/unread; `\Flagged` maps to Starred. Arbitrary keywords aren't synced to Proton | Only `\Seen` and `\Flagged` are written in the pilot; the others are read-only. Tags stay local |
| COPY to `Labels/L` adds a label. **COPY to `Folders/F` can move.** MOVE folder → label keeps the folder. MOVE label → folder files and removes the label. MOVE from or to All Mail, and APPEND to All Mail, are disallowed. MOVE between Inbox and Sent is disallowed | The mutation matrix below |
| **EXPUNGE on a label mailbox unlabels. On Trash or Drafts it can delete permanently** | **No EXPUNGE of any kind in the Proton policy** (pilot) |
| SMTP saves the Sent copy itself. Its duplicate suppression is in memory only | `server_saves_sent=1`. Never APPEND to Sent (05) |
| Bridge is GPLv3 | Never bundle or embed Bridge. It is user-installed |

## Connections

- **Per account:** one IDLE connection (on INBOX), plus one worker
  connection for sync and commands, then at most two body-fetch
  connections, added only after measurements.
- **Setup:**
  - TLS is established before any credential; capabilities are
    re-read after STARTTLS and after login.
  - `ID` sends `("name" "Sweetbriar" "version" "<version>")`, honestly
    identifying the client.
- **Mailbox state:** `EXAMINE` (read-only) unless a write is planned.
  Leave with `UNSELECT`, never `CLOSE`.
- **Reads:** body fetches use `BODY.PEEK[]`, so reading never sets
  `\Seen` remotely. Read state changes only through an explicit
  `set_flags` intent.
- **Timeouts:**
  - 30 s per command, 120 s per body fetch, scaled for large literals;
  - IDLE is renewed every 25 min;
  - reconnects back off exponentially (1 s to 5 min).
- **Fault handling:** a malformed response (e.g. raw UTF-8 in an ENVELOPE,
  Gluon issue #498) closes the connection. Nothing parsed after the fault
  is applied. Header fields are read from `BODY.PEEK[HEADER.FIELDS (…)]`
  literals, not from ENVELOPE.

## Sync loop

There is no CONDSTORE, so sync per mailbox works like this:

1. **Discover mailboxes.** `LIST "" "*"` maps names, roles and kinds, and
   is reconciled with the `mailboxes` rows.
   - New mailboxes are added.
   - Missing ones are marked `gone`; their occurrences are kept.
2. **Check the epoch.** `EXAMINE` returns `UIDVALIDITY` and `UIDNEXT`.
   - If UIDVALIDITY differs from the stored value: see
     [Epoch changes](#epoch-changes).
3. **Fetch new messages.** UIDs from the stored `uidnext` upward, in
   batches of 250, fetching `(UID FLAGS INTERNALDATE RFC822.SIZE
   BODY.PEEK[HEADER.FIELDS (DATE FROM REPLY-TO TO CC SUBJECT MESSAGE-ID
   IN-REPLY-TO REFERENCES LIST-ID DELIVERED-TO X-ORIGINAL-TO
   X-PM-INTERNAL-ID)])`.
   - The fetch also backfills UIDs below `uidnext` that the last full
     UID list reported but that have no occurrence (Bridge can
     materialize messages late).
   - For each, the engine finds or creates the logical message by
     `(account, X-Pm-Internal-Id)`, stores the header literal and parsed
     fields in `message_headers` (02), inserts the occurrence and updates
     `d_list_rows`.
   - If the locator `(mailbox, uidvalidity, uid)` already has a current
     occurrence for a **different** `X-Pm-Internal-Id`, that occurrence
     becomes `superseded` (UID reuse, 02) and the new one is inserted.
   - **Each batch commits with its `uidnext` checkpoint in one
     transaction,** so a crash resumes without gaps or duplicates.
4. **Acquire bodies.** A separate queue fetches `BODY.PEEK[]` for logical
   messages without an acquisition.
   - Order: messages the user opened first, then newest first.
   - **Before attaching a body,** the engine checks that the mailbox's
     epoch is unchanged, then applies the policy's identity check:
     - **`proton_bridge`:** the fetched message's `X-Pm-Internal-Id`
       equals the logical message's `provider_key`;
     - **`generic_imap`:** `OBJECTID`/`EMAILID` match where advertised
       and qualified. Otherwise the body's `Message-ID` and `Date` must
       equal those in `message_headers`. This is weaker: it relies on
       RFC 3501's rule that a UID isn't reused within an epoch.
   - On a mismatch the body isn't attached: the mailbox gets a header
     resync and any intents on that occurrence are quarantined
     (`ambiguous_match`).
   - Each body is published (03), then the acquisition is inserted,
     linked to the logical message, and queued for search.
   - A body is never fetched again for another occurrence of the same
     logical message.
5. **Sweep flags.** A periodic `UID FETCH 1:* (UID FLAGS)` per mailbox:
   INBOX every 60 s while active, others every 15 min, plus after IDLE
   notifications. Changed flags update occurrences. Pending local flag
   intents take precedence (below).
6. **Check the inventory.** After the flag sweep's full UID list, UIDs
   that are stored as `present` but absent from the server become
   `vanish_pending`.
   - Only after a **second complete inventory** (a later full sweep, or
     `EXPUNGE` responses seen in the session) do they become `vanished`.
   - Disappearances pass through the breaker first.
7. **Re-check identities (`proton_bridge`).** Steps 3 and 5 never read
   an existing UID's provider id, so a UID reused below `uidnext` would
   go unseen. Every 6 h per mailbox, and after each reconnect, `UID FETCH
   1:* (UID BODY.PEEK[HEADER.FIELDS (X-PM-INTERNAL-ID)])` compares each
   UID's id with its current occurrence. A mismatch takes step 3's
   UID-reuse path (supersede, insert, header fetch) and quarantines
   intents on the old occurrence (`ambiguous_match`). The live target
   check still guards every mutation in between.
8. **Use IDLE notifications** (`EXISTS`, `EXPUNGE`, `FETCH`) as triggers
   to run steps 3–6 for that mailbox. They are not a journal.

**Pace:** the inbox is polled every 60 s when IDLE is unavailable;
everything else follows the intervals above. Interactive fetches (opening
a message) always run before background work.

**Coverage** per mailbox records:
- `headers_complete`: every UID the server reported below `uidnext` (in
  the last full UID list) has an occurrence;
- the count of logical messages with and without bodies.

The UI says "Bridge-visible messages synced", never "all Proton mail".

## Epoch changes

When a mailbox's UIDVALIDITY changes:

A NULL stored `uidvalidity` (after a restore, 03) counts as a change:
every present occurrence of the mailbox belongs to the old epoch, and a
NULL `uidnext` means fetching starts at UID 1.

1. **Before anything else,** mark every occurrence of the old epoch
   `epoch_stale`. Suspend every intent targeting those occurrences: they
   become `quarantined` with `quarantine_reason='epoch_change'`.
2. **Resync the mailbox** as new: new occurrences under the new epoch,
   grouped to logical messages by `X-Pm-Internal-Id`.
3. **Rematch.** Old and new occurrences with the same `X-Pm-Internal-Id`
   are linked to the same logical message, so tags and notes carry over.
   - An old occurrence with no new counterpart stays `epoch_stale` and
     is shown in "Recovery: messages no longer on the server". It is not
     deleted. That list also holds messages whose last occurrence is
     `vanished` or `superseded` (UID reuse), each with its reason.
4. **Quarantined intents are never resumed automatically.** The user
   reviews them on the health surface: retarget to the new occurrence,
   or cancel.
5. **Breaker:** an epoch change on more than one mailbox at once (a
   Bridge repair or mode change) records one `breaker_event` of kind
   `epoch_change`, and the account shows "Bridge rebuilt its mailboxes;
   reconciling".

## The breaker

Classify each disappearance seen in an inventory:

| Class | Example | Action |
|---|---|---|
| **Explained** | A move or delete this client performed (matching `done` intent) | Apply |
| **Single-folder move** | The `X-Pm-Internal-Id` reappears in another mailbox in the same sync pass | Apply as a move |
| **Unexplained disappearance** | Gone, with no reappearance and no intent | Breaker rules below |
| **Incomplete observation** | Connection dropped, partial FETCH, Bridge still syncing | Never counts. Keep `present` |

**Pause, conservative defaults:** a breaker event is opened when any of
these holds. It pauses destructive reconciliation for the account, and
GC globally (03):
- more than 25 unexplained disappearances in one mailbox in one pass;
- unexplained disappearances of more than 50% of a mailbox holding at
  least 8 messages;
- a mailbox going from at least 1 message to 0;
- an epoch change on more than one mailbox at once.

An epoch change on a single mailbox opens no event. It still pauses
destructive reconciliation for that mailbox until its resync and rematch
finish.

**While paused:** downloads, reading, drafting and other accounts
continue. The user sees counts and examples and chooses **Accept**
(apply the disappearances; the event becomes `accepted`) or **Keep**
(leave occurrences marked `vanish_pending` and keep watching; `kept`).
An open or kept event whose messages reappear on a later inventory
becomes `cleared`. A kept event can still be accepted later, from the
health surface. Thresholds are settings, tuned from
traces later.

## Intents and the mutation matrix

**Execution:** each intent row (02) runs on the account's worker
connection.

1. **Check preconditions.**
   - The intent's `config_generation` equals the account's current one.
     If the account was reconfigured since, the intent becomes
     `quarantined` with `quarantine_reason='config_change'`.
   - The target occurrence's epoch equals the mailbox's current
     UIDVALIDITY, and the occurrence is `present`.
   - `delete_to_trash` runs as a `move` to the account's Trash mailbox
     and follows every `move` rule below.
   - **Live target check, defined by the provider policy,** before every
     mutation (`set_flags`, `move`, `copy`):
     - **`proton_bridge`:** in the selected mailbox, `UID FETCH <uid>
       (BODY.PEEK[HEADER.FIELDS (X-PM-INTERNAL-ID)])` must return the
       logical message's `provider_key`. A UID can be reused within the
       same epoch, as Bridge has done historically.
     - **`generic_imap`:** the live `UIDVALIDITY` must equal the
       occurrence's epoch, and the UID must still exist (`UID FETCH <uid>
       (UID)`). `OBJECTID`/`EMAILID` (RFC 8474) are compared only where
       advertised and qualified.
     - A mismatch, or missing or ambiguous evidence, quarantines the
       intent with `quarantine_reason='ambiguous_match'`.
   - For `move`: the occurrence is still in the source mailbox, as of the
     last inventory.
   - For `set_flags`: none needed; flags are applied as deltas.
   - A target that isn't `present`, or a move source that no longer holds
     it, quarantines the intent (`epoch_change` for `epoch_stale`,
     otherwise `ambiguous_match`).
   - **On startup,** `in_flight` intents return to `pending` with the
     reconcile-before-retry flag (step 4).
2. **Act** with the policy's command.
3. **Interpret the response,** per operation:
   - **`set_flags`** (`UID STORE … FLAGS.SILENT`, which returns no flag
     data): after the tagged `OK`, confirm with `UID FETCH <uid> (FLAGS)`,
     plus the live target check, that the requested delta holds → `done`;
   - **`move`:** tagged `OK` with `COPYUID` data or an untagged `EXPUNGE`,
     then destination verification (the matrix) → `done`;
   - **`copy`** (add label): tagged `OK` with `COPYUID`, then the
     destination occurrence verified → `done`;
   - a `NO` that will persist → `parked`;
   - a connection drop → back to `pending`, with a reconcile-before-retry
     flag.
4. **Reconcile before retry,** per policy. After a drop, the engine
   first re-reads the target and re-issues only if the effect is
   provably absent:
   - flags: `UID FETCH <uid> (FLAGS)` in the source mailbox;
   - **`proton_bridge`** move or copy: in the destination, fetch
     `BODY.PEEK[HEADER.FIELDS (X-PM-INTERNAL-ID)]` for UIDs at or above
     the destination's `uidnext` recorded in `evidence_json` before the
     action, and look for the `provider_key`;
   - **`generic_imap`** move or copy: the `COPYUID` mapping recorded in
     `intents.evidence_json` as it arrived, or a qualified `EMAILID`.
     Without either, a move whose source UID is gone is `done`;
     anything else is quarantined (`ambiguous_match`). The engine never
     re-issues `COPY`, or goes on to `STORE \Deleted`, without that
     evidence.
5. **Order:** intents for the same occurrence run in order. Independent
   intents may run in any order.

**Proton policy (pilot)**:

| User action | Allowed when | Command | Notes |
|---|---|---|---|
| Mark read or unread | Any present occurrence in a selectable mailbox | `UID STORE <uid> +FLAGS.SILENT (\Seen)` or `-FLAGS.SILENT` | Proton keeps read state per message. The local effect applies to every present occurrence of the logical message; the intent targets the occurrence acted on. The sweep confirms the other occurrences |
| Star or unstar | Same | `UID STORE` with `\Flagged` | Same convergence |
| Archive, move to folder, move to Trash | Source is INBOX, Archive, Spam, Trash or `Folders/*`; destination is INBOX, Archive, Spam, Trash or `Folders/*`; source ≠ destination | `UID MOVE <uid> <dest>` | **Before the move,** the logical message's body must be acquired and published (keep a local copy of anything leaving view). Afterwards, verify the destination occurrence appears with the same `X-Pm-Internal-Id`, and only then mark `done` |
| Add label | Source is any selectable non-virtual mailbox; destination is `Labels/*` | `UID COPY <uid> <label>` | Adds membership. The folder stays |
| Remove label | — | Not in the pilot | Would need a qualified `UID EXPUNGE` on the label mailbox; deferred |
| Delete permanently, empty Trash | — | Not in the pilot | No EXPUNGE in the Proton policy |
| Move or add label from Starred or All Mail | — | Resolved to the message's real folder occurrence first, then `UID MOVE` or `UID COPY` of that occurrence | If there's none, or it's ambiguous, the action is refused with an explanation |
| Any action in Scheduled | — | Not in the pilot | Scheduled sends are read-only |
| Archive, move or trash from a label view (`Labels/*`) | — | Resolved to the message's folder occurrence (same `X-Pm-Internal-Id`, in INBOX, Archive, Spam, Trash or `Folders/*`), then `UID MOVE` of that occurrence | Proton keeps labels on a folder-to-folder move, so the message stays in the label view. The UI says so. If no folder occurrence is known, the action is refused |

**Generic IMAP policy** (Dovecot in CI; generic accounts later):
- `UID MOVE` when `MOVE` is advertised. Otherwise copy and delete: `UID
  COPY`, check `COPYUID`, then `UID STORE +FLAGS.SILENT (\Deleted)` and
  `UID EXPUNGE <uid>` (UIDPLUS required).
- Without UIDPLUS, no automatic expunge.
- **Completion:** a `copy` is `done` when `COPYUID`, with the
  destination's current UIDVALIDITY, names the new UID and `UID FETCH
  <uid> (UID)` in the destination finds it. A `move` also needs the
  source UID gone:
  - `UID MOVE` removes it. Without `COPYUID` (no UIDPLUS), the move is
    `done` when the source UID is gone; the destination arrives with the
    next sync.
  - The copy-and-delete fallback records each phase (copied, flagged
    `\Deleted`, expunged) in `intents.evidence_json`. After a crash or
    drop it resumes from the last proven phase, under the live target
    check, and is `done` only when the source UID is gone.
- Never a bare `EXPUNGE`, and never `CLOSE` after flagging.

**Flag conflicts:** with no CONDSTORE, a flag intent is a delta (add or
remove one flag). While a local flag intent is pending or in flight, the
sweep doesn't overwrite that flag on the occurrences it covers (for
Proton, every occurrence of the logical message). Once the intent is
done, the next sweep wins: another client's later change is preserved.

**Undo:** undo creates the inverse intent, for example move back to the
recorded source mailbox, with its own preconditions. If another client
moved the message since, undo is refused with "Moved elsewhere since".

## Account states and health

| Condition | Account state | User sees |
|---|---|---|
| Connection refused | `needs_attention` | "Bridge isn't running" (retries continue) |
| Login rejected | `needs_attention` | "Bridge password changed or account signed out in Bridge" |
| Certificate pin mismatch | `needs_attention` | "Bridge's certificate changed", with the re-trust flow (10) |
| Epoch change or breaker event | `active` with an open event | "Bridge rebuilt mailboxes; reconciling" or "Review N disappeared messages" |
| Healthy | `active` | Last sync time and coverage |

None of these deletes local data. The health surface lists parked and
quarantined intents with **Retry**, **Retarget** (after an epoch change)
and **Cancel**.

## Compatibility ledger

On every connect, the engine records the capabilities, the mailbox list
with roles and the server `ID` in `imap_endpoints.capabilities_json`, plus
a dated diagnostics entry.

If the capabilities or the mailbox roles change (the `ID` response's
version, when present, is recorded as a best-effort hint only), mutations
for the account are suspended until a check passes (pending intents
wait with `wait_reason='requalify'`):
- the capabilities the account's policy uses are still advertised:
  MOVE and UIDPLUS for `proton_bridge`; for `generic_imap`, MOVE or
  UIDPLUS for the move path it chose (with neither, moves stay disabled
  and flags continue);
- the role mapping is unchanged.

Read-only sync continues meanwhile. A failed check shows on the health
surface with the old and new values. A new role mapping takes effect
only when the user confirms it, which records it as the new baseline
and resumes mutations.
