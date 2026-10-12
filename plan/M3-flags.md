# M3 — Flags (read/unread, star)

**Outcome:** marking read and unread, and starring, syncs to Proton
through durable intents. This is the first remote effect, and it builds
the intent framework every later mutation uses.

### M3.1 Intent framework — L, linux

- **Goal:** durable intents with preconditions, reconcile-before-retry,
  parking, quarantine, undo and health actions.
- **Spec:** 02 (`intents`), 04 (Intents and the mutation matrix:
  execution, flag conflicts, undo), 07 (`release_intents`,
  `cancel_intents`, `undo`).
- **Depends on:** M2 complete.
- **Deliverables:**
  - planning: a command writes the local effect and the intent in one
    transaction;
  - the executor on the account worker: check preconditions, act,
    interpret the response, reconcile after a drop;
  - per-occurrence ordering;
  - `wait_reason` handling (auth, throttled, offline);
  - parking after 5 attempts;
  - undo tokens creating inverse intents;
  - health actions (retry, retarget, cancel).
- **Acceptance:**
  - crash cuts between the local commit, command send, response and
    intent completion never lose an intent, never apply an effect twice
    and never mark `done` without the effect (fake server counts
    commands);
  - a drop after send, then reconnect, reconciles first and doesn't
    re-issue when the effect is present;
  - an epoch change moves pending intents to `quarantined`;
  - **same-epoch UID reuse:** the fake server reassigns a UID to a
    different message without changing UIDVALIDITY; the live
    `X-Pm-Internal-Id` check quarantines the intent
    (`ambiguous_match`), and no STORE is sent;
  - an account reconfiguration (generation bump) quarantines pending
    intents (`config_change`);
  - the proptest model includes intents and converges.

### M3.2 Read and star on Proton — M, linux + macos-ci

- **Goal:** `set_flags` for `\Seen` and `\Flagged` under the Proton
  policy, with the flag-conflict rule, wired into the app.
- **Spec:** 04 (Proton policy rows for read and star; Flag conflicts), 08
  (Message list keyboard; Reader).
- **Depends on:** M3.1.
- **Deliverables:**
  - `set_flags` planning and execution (`UID STORE ±FLAGS.SILENT`);
  - the sweep doesn't overwrite flags with pending intents;
  - a setting for marking read after opening (immediately, after N
    seconds, or manually);
  - macOS: mark read and unread (⇧⌘U), star (⇧⌘L), and toolbar items;
  - the flag suite added to the real-Bridge nightly lane (M2.7).
- **Acceptance:**
  - the command trace shows `+FLAGS.SILENT (\Seen)` exactly once per
    action, followed by a confirming `UID FETCH … (FLAGS)` before the
    intent is `done`;
  - a server that returns only a tagged `OK` to STORE still reaches
    `done`, via the confirming fetch;
  - marking read in INBOX shows the message read at once in its label
    and Starred views, with one STORE sent;
  - another client's later flag change (fake server) is preserved after
    our intent completes;
  - the UI updates instantly (local effect) and stays correct after the
    sweep;
  - the real-Bridge nightly lane passes the flag tests.

### M3.Q Owner qualification

Recorded in `plan/qualification/M3.md`:
1. Read and unread, and star and unstar, from Sweetbriar show in Proton's
   web app within a minute.
2. Changes made in the web app show in Sweetbriar after the next sweep.
3. A conflict case (unstar in the web app while Sweetbriar is offline,
   then star in Sweetbriar) ends in the documented state.
4. All other occurrences of the message converge.
