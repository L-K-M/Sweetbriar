# M3 — Flags (read/unread, star)

**Revised 2026-11-18 (owner decision, 2026-11-15):** Sweetbriar
implements the Proton API itself and doesn't depend on Proton Mail
Bridge. Intents act through Sweetbriar's Proton client, as
spec/04-sync-proton "Intents and the mutation matrix" says; the
execution rules of spec/04-sync-imap still apply where that section
defers to them. No M3 step had merged before this revision.

**Outcome:** marking read and unread, and starring, syncs to Proton
through durable intents. This is the first remote effect, and it builds
the intent framework every later mutation uses.

### M3.1 Intent framework — L, linux

- **Goal:** durable Proton intents with preconditions, the live target
  check, confirmation by a metadata read, reconcile-before-retry,
  parking, quarantine, undo and health actions.
- **Spec:** 02 (`intents`), 04-sync-proton (Intents and the mutation
  matrix; Refresh step 1; The breaker: Explained), 04-sync-imap
  (Intents and the mutation matrix: execution, Flag conflicts, Undo), 07
  (`release_intents`, `cancel_intents`, `undo`).
- **Depends on:** M2.16 and M2.7 (code merged; M2.Q may still be open,
  plan/README.md).
- **Deliverables:**
  - the transport's mutation calls: metadata by id
    (`/mail/v4/messages` filtered by `ID`), `PUT /mail/v4/messages/read`
    and `/unread`, `PUT /mail/v4/messages/label` and `/unlabel`, with
    their per-message response codes; the fake server's support, a
    count of mutation calls and a fault that drops the connection after
    a mutation is applied;
  - planning: a command writes the local effect and the intent in one
    transaction;
  - the executor on the Proton account worker: preconditions
    (`config_generation`, a current occurrence), the live target check,
    act, confirm with a metadata read, and reconcile after a drop;
  - per-message ordering;
  - `wait_reason` handling (`auth`, `throttled`, `offline`, `resync`);
  - parking after 5 attempts;
  - undo tokens creating inverse intents;
  - health actions (`release_intents`, `cancel_intents`) and the
    Activity window's intent rows (M2.7d) wired to them;
  - the breaker's Explained class: a delete or move matching a `done`
    intent is applied (M3 creates only read and star intents, so its
    tests insert the move intent directly; M5 brings real ones).
- **Acceptance:**
  - crash cuts between the local commit, the request, the response and
    intent completion never lose an intent, never apply an effect twice
    and never mark `done` without the effect (the fake counts mutation
    calls);
  - a drop after the request, then reconnect, reconciles first and
    doesn't re-issue when the effect is present;
  - a Refresh makes pending intents wait (`resync`) without
    quarantining any; they run once the listing completes;
  - an intent whose message Proton no longer has is quarantined
    (`ambiguous_match`), and no mutation is sent;
  - an account reconfiguration (generation bump) quarantines pending
    intents (`config_change`);
  - five failures park an intent; `release_intents` runs it again,
    and `cancel_intents` ends it so it never runs;
  - undoing a completed star plans the inverse intent, which unstars
    the message on the fake;
  - the proptest model (M2.15c4b) includes intents and converges.
- **Split (2026-11-21):** M3.1 follows a spec revision (02, 04-sync-proton
  "Running an intent", 07) and lands as five PRs, estimated from the
  code it touches at about 5,600 lines in all:
  - M3.1a (~900): the transport's read, unread, label and unlabel
    calls with their response codes, and the fake server's routes, its
    mutation count and the drop-after-apply fault on them;
  - M3.1b (~1,250): planning a flag change with its local effect in one
    transaction, the store's claim, ordering and state transitions,
    `release_intents` and `cancel_intents` in the store, and the
    engine's undo token and inverse planning;
  - M3.1c (~1,400): the executor on the Proton account worker, with
    every acceptance check on execution: the crash cuts, reconcile after
    a drop, `resync`, `ambiguous_match`, `config_change`, parking and
    undoing a star on the fake;
  - M3.1d (~1,150): the health actions through the service and FFI, and
    the Activity window's Retry and Cancel on intent rows;
  - M3.1e (~950): the breaker's Explained class and the proptest model
    with intents.
  M3.1a and M3.1b run in parallel; M3.1c needs both, and M3.1d and
  M3.1e need M3.1b.
- **Split (2026-11-21, M3.1b):** M3.1b measured 2,277 lines, so it
  lands as two PRs: M3.1b1 (~1,350), planning with the local effect,
  the claim and its order, and the transitions with their backoff and
  waits; M3.1b2 (~950), `release_intents`, `cancel_intents` and the
  engine's undo token and inverse planning. M3.1b2 needs M3.1b1.

### M3.2 Read and star on Proton — M, linux + macos-ci

- **Goal:** `set_flags` for read and star under the `proton_api`
  policy, with the flag-conflict rule, wired into the app.
- **Spec:** 04-sync-proton (the mutation matrix's read and star rows),
  04-sync-imap (Flag conflicts), 08 (Message list keyboard; Reader).
- **Depends on:** M3.1 and M2.17 (the `go-proton-api` lane).
- **Deliverables:**
  - `set_flags` planning and execution: `read`/`unread`, and `label`/
    `unlabel` with Starred (`10`);
  - events don't overwrite a flag with a pending or in-flight intent on
    any occurrence of the message;
  - a setting for marking read after opening (immediately, after N
    seconds, or manually);
  - macOS: mark read and unread (⇧⌘U), star (⇧⌘L), and toolbar items;
  - the flag suite added to the `go-proton-api` lane (M2.17).
- **Acceptance:**
  - the request log shows exactly one mutation call per action,
    followed by a confirming metadata read before the intent is `done`;
  - marking read in Inbox shows the message read at once in its label
    and Starred views, with one call sent;
  - another client's later flag change (fake server) is preserved after
    our intent completes;
  - the UI updates instantly (local effect) and stays correct after the
    next events poll;
  - the `go-proton-api` lane passes the flag tests.
- **Split (2026-11-21, M3.2):** the deliverables are estimated at
  about 3,800 lines, so M3.2 lands as four PRs:
  - M3.2a (~900): the flag-conflict rule on Proton events and listings,
    and the engine acceptance on the fake (one call and its confirming
    read, the label and Starred views, another client's later change
    preserved, correct after the next poll);
  - M3.2b (~1,100): `set_flags` through the engine, service and FFI, and
    the `reader.mark_read` setting (spec revision 2026-11-21: 02, 07);
  - M3.2c (~1,200): macOS: the Message menu and toolbar commands, `⇧⌘U`
    and `⇧⌘L`, the reader's mark-read timer and Settings → Reading (08);
  - M3.2d (~600): the flag suite in the `go-proton-api` lane.
  M3.2a and M3.2b need M3.1c and run in parallel; M3.2c and M3.2d need
  M3.2b.

### M3.Q Qualification

Agent-run on the owner's Mac (plan/README.md), recorded in
`plan/qualification/M3.md`. The agent asks the owner before each effect
on the real account and for the messages it may use.

1. Read and unread, and star and unstar, from Sweetbriar show in Proton's
   web app within a minute.
2. Changes made in the web app show in Sweetbriar after the next events
   poll.
3. A conflict case (unstar in the web app while Sweetbriar is offline,
   then star in Sweetbriar) ends in the documented state.
4. All other occurrences of the message converge.
