# M5 — Filing

**Outcome:** archive, move to folder, move to Trash and add label, with
undo. Every action is checked against the Proton mutation matrix and
verified before it completes.

### M5.1 Proton mutation matrix — L, linux

- **Goal:** `move_messages`, `delete_to_trash` and `add_label` (via
  `COPY` to `Labels/*`) under the Proton policy; the generic policy for
  Dovecot. Local tags (`add_tag`) never touch the server.
- **Spec:** 04 (Proton policy table, generic IMAP policy, Undo), 07
  (Messages commands).
- **Depends on:** M4 complete.
- **Deliverables:**
  - policy checks: allowed source and destination classes; Starred, All
    Mail and label views resolved to the real folder occurrence or
    refused (04 matrix);
  - pre-acquisition: the body is published before any move;
  - `UID MOVE` execution with destination verification by
    `X-Pm-Internal-Id`;
  - add-label via `UID COPY` to `Labels/*`;
  - the generic policy: `UID MOVE`, or `COPY` + `STORE \Deleted` +
    `UID EXPUNGE` only with UIDPLUS;
  - undo intents with the "moved elsewhere since" refusal;
  - the mutation-matrix suite added to the real-Bridge lane (M2.6), run
    with the folder-unlabel feature flag both ways.
- **Acceptance:**
  - for the Proton policy, the command trace contains **no `EXPUNGE`,
    `UID EXPUNGE` or `CLOSE`** in any test (a global assertion in the
    fake server);
  - moving from a label mailbox resolves to the folder occurrence or is
    refused;
  - a move without an acquired body fetches it first;
  - a destination that doesn't verify leaves the intent pending for
    reconciliation rather than marking it done;
  - crash cuts across move steps never lose or duplicate a message
    (fake server state check);
  - the real-Bridge nightly lane passes the matrix, including the
    folder-unlabel feature flag both ways.

### M5.2 Filing in the app — M, macos-ci + owner

- **Goal:** archive, delete (to Trash), move and add-label in the UI,
  with type-to-file and an undo toast.
- **Spec:** 08 (Message list keyboard; Keyboard filing).
- **Depends on:** M5.1.
- **Deliverables:**
  - toolbar and menu commands;
  - drag to sidebar mailboxes;
  - the filing palette (`⇧⌘M`, Move to…) ranked by recent use;
  - the undo toast (10 s);
  - an explanation for actions the policy refuses.
- **Acceptance:**
  - XCUITests: archive, then undo, restores the message; the palette
    moves the selection; a refused action shows its explanation;
  - **owner:** filing works with the keyboard alone.

### M5.Q Owner qualification

Recorded in `plan/qualification/M5.md` from the Bridge checklist, item 8,
**using expendable test messages only**:
1. Each allowed source/destination pair results in the expected state
   in Proton's web app.
2. Add-label keeps the folder.
3. Trash, then undo, restores the message.
4. A move from Starred resolves correctly.
5. A lost-completion case (fixture) reconciles without duplicating.
6. Same-folder filing is a no-op.
