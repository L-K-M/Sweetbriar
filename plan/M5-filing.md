# M5 — Filing

**Revised 2026-11-18 (owner decision, 2026-11-15):** filing acts
through Sweetbriar's own Proton API client: a move labels the message
with its destination folder, and labels are added and removed with
`label` and `unlabel` (spec/04-sync-proton "Intents and the mutation
matrix"). No M5 step had merged before this revision. The generic IMAP
policy's filing (`UID MOVE`, copy-and-delete with UIDPLUS, Dovecot) is
deferred with generic accounts (*later*, like M2.5 and M2.6).

**Outcome:** archive, move to folder, move to Trash and add label, with
undo for moves (add label has none in the pilot; 07). Every action is checked against the Proton mutation matrix and
verified before it completes.

### M5.1 Proton mutation matrix — L, linux

- **Goal:** `move_messages`, `delete_to_trash`, `add_label` and
  `remove_label` under the `proton_api` policy. Local tags (`add_tag`)
  never touch the server.
- **Spec:** 04-sync-proton (Intents and the mutation matrix), 04-sync-imap
  (Intents: execution, Undo), 07 (Messages commands).
- **Depends on:** M4 complete.
- **Deliverables:**
  - a spec commit first: 07 gains `remove_label(selection,
    label_mailbox)`, which the 04-sync-proton matrix allows, and its
    note on `add_label` drops the EXPUNGE reason (label undo stays out
    of the pilot);
  - policy checks: the matrix's allowed destinations (Inbox, Archive,
    Spam, Trash or a folder) and source ≠ destination; a move acts on
    the message whichever view it was chosen in (Starred, All Mail or a
    label), since Proton keeps one location; any action in Scheduled is
    refused with an explanation;
  - pre-acquisition: the body is published before any move;
  - execution with `label` to the destination folder, confirmed by a
    metadata read that shows the new location before `done`;
  - `add_label` and `remove_label` with a user label;
  - `delete_to_trash` as a move to Trash under every move rule;
  - undo intents with the "moved elsewhere since" refusal;
  - the breaker's Explained class covers this client's moves (M3.1);
  - the mutation-matrix suite added to the `go-proton-api` lane
    (M2.17).
- **Acceptance:**
  - no test sends a permanent delete or empties a folder (a global
    assertion in the fake server);
  - a move from Starred, All Mail or a label view moves the message's
    folder, and its user labels stay;
  - an action in Scheduled is refused, and same-folder filing sends
    nothing;
  - `add_label` and `remove_label` return no undo token;
  - in the local archive, `delete_to_trash` moves to a local Trash
    mailbox, sends nothing and deletes nothing;
  - a move without an acquired body fetches it first;
  - a destination the metadata doesn't confirm leaves the intent
    pending for reconciliation rather than marking it done;
  - a drop after the fake applied a move reconciles without re-issuing
    it (mutation counter);
  - crash cuts across move steps never lose or duplicate a message
    (fake server state check);
  - the `go-proton-api` lane passes the matrix.

### M5.2 Filing in the app — M, macos-ci + owner

- **Goal:** archive, delete (to Trash), move, and add and remove labels
  in the UI,
  with type-to-file and an undo toast.
- **Spec:** 08 (Message list keyboard; Keyboard filing).
- **Depends on:** M5.1.
- **Deliverables:**
  - toolbar and menu commands;
  - drag to sidebar mailboxes;
  - the filing palette (`⇧⌘M`, Move to…) ranked by recent use;
  - the undo toast (10 s);
  - an explanation for actions the policy refuses;
  - no undo offered after adding or removing a label.
- **Acceptance:**
  - XCUITests: archive, then undo, restores the message; the palette
    moves the selection; a refused action shows its explanation;
  - **owner** (agent-run): filing works with the keyboard alone.

### M5.Q Qualification

Agent-run on the owner's Mac (plan/README.md), recorded in
`plan/qualification/M5.md`, **using expendable test messages only**,
which the owner names; the agent asks before each effect:
1. Each allowed source/destination pair results in the expected state
   in Proton's web app.
2. Adding and removing a label keeps the folder.
3. Trash, then undo, restores the message.
4. A move from Starred resolves correctly.
5. A lost-completion case (fixture) reconciles without duplicating.
6. Same-folder filing is a no-op.
