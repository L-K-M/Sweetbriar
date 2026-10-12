# M2 — Proton Mail read-only through Bridge

**Outcome:** the owner's Proton account syncs read-only through Bridge:
mailboxes, headers, bodies, search, coverage and account health. No
remote changes yet.

**Prerequisite (owner, O6):** Bridge 3.27.1 or later installed and
signed in, with its TLS certificate exported.

### M2.1 IMAP crate spike — M, linux

- **Goal:** choose `io-imap` 0.7.1 or `async-imap` 0.12.0 on evidence.
- **Spec:** 01 (Crates: IMAP), 04 (Connections, Bridge facts).
- **Depends on:** M1 complete.
- **Deliverables:**
  - in `sb-test-support`: the scripted fake IMAP server with the
    **Bridge profile** (capabilities without CONDSTORE, NAMESPACE or
    LITERAL+; LIST with `Folders/` and `Labels/` and role attributes;
    the same message in several mailboxes with `X-Pm-Internal-Id`;
    UIDPLUS receipts) and fault knobs (11);
  - a throwaway spike client with each crate doing: STARTTLS with a
    custom verifier, LOGIN, CAPABILITY, ID, LIST, EXAMINE,
    `UID FETCH … BODY.PEEK[HEADER.FIELDS (…)]`, `UID FETCH 1:* (FLAGS)`,
    IDLE, and a raw UTF-8 ENVELOPE fault;
  - a decision note in the PR, plus updates to spec/01.
- **Acceptance:**
  - the chosen crate passes every spike operation against the fake
    server and Dovecot (Docker, pinned digest);
  - a malformed response closes the connection without applying later
    data;
  - the decision is recorded in spec/01.

### M2.2 IMAP transport — L, linux

- **Goal:** `sb-imap` provides connections and read operations, with
  pinned-certificate TLS.
- **Spec:** 04 (Structure, Connections), 10 (TLS and the Bridge
  certificate).
- **Depends on:** M2.1.
- **Deliverables:**
  - `sb-imap::Connection`: connect to `127.0.0.1`, STARTTLS or implicit
    TLS, a pinned-cert verifier that does full handshake verification
    plus a leaf-hash check, LOGIN, capability refresh, ID;
  - read operations returning observations (mailbox list, epoch, UID
    sets, header literals, flags, body literals as streams);
  - an IDLE watcher;
  - timeouts and backoff;
  - a dialect-free command interface. The provider policies
    (`proton_bridge`, `generic_imap`) live in `sb-core::engine::policy`
    (04) and arrive in M2.4.
- **Acceptance**, against the fake server and Dovecot:
  - a wrong pin is rejected before LOGIN;
  - a missing STARTTLS is rejected;
  - a non-loopback host is rejected for the Bridge preset;
  - command timeouts trigger a reconnect;
  - IDLE is renewed at 25 min (virtual clock);
  - credentials never appear in logs (a log-scan test).

### M2.3 Bridge account setup and secrets — M, linux + macos-ci + owner

- **Goal:** add a Bridge account with its credentials stored in the
  platform secret store and its certificate pinned.
- **Spec:** 02 (`accounts`, `imap_endpoints`, `identities`), 10
  (Credentials, TLS), 07 (`add_bridge_account`).
- **Depends on:** M2.2.
- **Deliverables:**
  - `sb-platform::SecretStore`: `keyring-core` with the Apple
    `protected` store on macOS and Secret Service on Linux, plus a fake
    store for tests;
  - `add_bridge_account`: probe the endpoint, import `cert.pem` or
    confirm the fingerprint, verify login, store the credential by
    reference, create identities;
  - the macOS onboarding sheet: Bridge instructions with host and ports
    prefilled, username, Bridge password, certificate import, test
    connection.
- **Acceptance:**
  - setup against the fake server stores no secret in SQLite (schema
    test plus log scan);
  - a Keychain error pauses rather than deletes (fault test with the
    fake store);
  - **owner (O6):** adding the real Bridge account succeeds;
  - **data-protection Keychain:** the `protected` store needs an
    entitlement-signed app. With ad-hoc builds this may fail with
    `-34018`. If it does, the step falls back to the file-based Keychain
    item **for the pilot only**, flagged in Settings, until O8 provides
    a signing identity. Record which happened.

### M2.4 Discovery and header sync — L, linux

- **Goal:** mailboxes and headers sync with checkpoints, grouped into
  logical messages by `X-Pm-Internal-Id`.
- **Spec:** 04 (Sync loop steps 1–3 and 8, Account states), 02
  (`message_headers`, occurrences).
- **Depends on:** M2.3.
- **Deliverables:**
  - engine per-account tasks (IDLE watcher, worker);
  - mailbox discovery and mapping (kinds and roles: Starred `flagged`,
    All Mail `all`, Scheduled `virtual` without a role);
  - header batches with `uidnext` checkpoints in one transaction;
  - logical message grouping with evidence;
  - account state transitions.
- **Acceptance:**
  - **UID reuse:** the old occurrence becomes `superseded` and the new
    one is inserted;
  - `message_headers` persists the header literal and parsed fields,
    including `Delivered-To` and `X-Original-To`, in the batch's
    transaction;
  - crash cuts during header batches resume without gaps or
    duplicates;
  - schema-level cross-account rejection: inserting an occurrence whose
    message belongs to another account fails.

### M2.5 Flag sweep, inventory, breaker and epochs — L, linux

- **Goal:** flags converge, disappearances are confirmed safely, and
  epoch changes and mass disappearances stop for review.
- **Spec:** 04 (Sync loop steps 5–7, Epoch changes, The breaker).
- **Depends on:** M2.4.
- **Deliverables:**
  - the flag sweep; inventory with `vanish_pending` and `vanished`;
  - the Proton identity re-check (step 7);
  - the breaker with `breaker_events`;
  - epoch-change handling with quarantine.
- **Acceptance:**
  - a proptest model against an in-memory IMAP model with random
    sequences of server changes (new, flag change, move between
    mailboxes, expunge, epoch change, disconnects) converges to the
    model's state;
  - an occurrence never vanishes on an incomplete observation;
  - the breaker opens on the default thresholds;
  - **UID reused below `uidnext`:** with UIDNEXT and flags unchanged and
    A's body already acquired, the identity re-check finds B at A's UID,
    supersedes A and inserts B, with no mutation or body fetch;
  - an epoch change quarantines targeting intents (inserted directly in
    the test).

### M2.6 Body acquisition, coverage and health — M, linux + macos-ci

- **Goal:** bodies download once per logical message, coverage is
  visible, and the account health surface exists.
- **Spec:** 04 (Sync loop step 4, Account states and health), 06
  (Coverage), 08 (Activity / account health window).
- **Depends on:** M2.5.
- **Deliverables:**
  - the body queue (opened messages first, then newest) using
    `BODY.PEEK[]`, publish, link and index;
  - `coverage()` and `health()`;
  - `update_account` (endpoint, credentials, certificate re-trust; keeps
    the account UUID and bumps `config_generation`), `pause_account`,
    `resume_account` and `disconnect_account`, tested through the public
    API, including resuming after a restore;
  - the "Recovery: messages no longer on the server" list (`epoch_stale`
    and `vanished` occurrences with no counterpart);
  - the diagnostics package (redacted, previewed, saved by the user; 10),
    used by M2.Q item 3;
  - new-mail notifications through `UNUserNotificationCenter`, naming the
    account in text, with previews only if enabled (10);
  - the macOS Activity window: per account state, coverage, breaker
    events with Accept and Keep, parked and quarantined intents with
    actions.
- **Acceptance:**
  - a message in INBOX plus two labels is fetched exactly once (fake
    server counts fetches);
  - **UID reuse between header and body fetch:** message B's body,
    fetched at A's old UID, is never attached to A's logical message;
  - the body-attach identity check per policy: on the Bridge profile a
    different `X-Pm-Internal-Id` is refused; on Dovecot a body whose
    `Message-ID` differs from `message_headers` is refused;
  - with headers but no body, `from:` `is`/`domain` predicates match
    from `message_headers`, and `contains` and `text` count the message
    as unknown, also under negation;
  - reading never sends `\Seen` (command trace);
  - coverage numbers match fixture expectations;
  - breaker Accept and Keep work end to end (UI test with the fake),
    leaving the event `accepted` or `kept`; an event whose messages
    reappear becomes `cleared`.

### M2.7 Real-Bridge CI lane — L, linux (nightly)

- **Goal:** run Sweetbriar's engine against **real Bridge code** with
  Proton's mock API, without credentials.
- **Spec:** 11 (Test layers), research
  [raw/plan-proton-bridge](../research/raw/plan-proton-bridge/codex.md)
  §2.7.
- **Depends on:** M2.5.
- **Deliverables:**
  - a separate CI-only component (its own directory, licensed per
    Bridge's GPLv3, never linked into Sweetbriar);
  - a small Go driver inside a pinned Bridge v3.27.1 checkout that
    provisions mock users and messages, starts the Bridge core, and
    prints ports and the test certificate;
  - a nightly job runs the Rust read-only sync suite against it;
  - the Go toolchain pinned by Bridge's `go.mod` (Go 1.26.x) and Bridge's
    native (CGO) build prerequisites from its `BUILDS.md`, without the
    Qt GUI, installed in that job;
  - the lane is built to grow: M3.2, M4.3 and M5.1 each add their suites
    to it.
- **Acceptance:**
  - the nightly lane runs green;
  - its results include the actual capability set, LIST output and
    `X-Pm-Internal-Id` behaviour, recorded as a dated artifact;
  - the README states that the component is GPL test tooling, not part
    of the app.

### M2.Q Owner qualification (read-only)

Recorded in `plan/qualification/M2.md` from the Bridge checklist, items
1–7:
1. **Setup and lifecycle:** Bridge GUI startup and open-on-startup;
   Sweetbriar started before Bridge; Bridge quit and restart; sleep and
   wake; network loss; offline reading survives all of them.
2. **TLS:** ports, modes and fingerprints recorded; certificate import;
   a replaced certificate produces a recoverable error.
3. **Protocol:** CAPABILITY before and after login, LIST attributes,
   SELECT data and IDLE, captured in a diagnostics export.
4. **Coverage and identity:** one known message in its folder, two
   labels, All Mail and Starred, with matching `X-Pm-Internal-Id`;
   Spam, Trash and drafts present.
5. **Epoch lifecycle:** identity inventory after a Bridge restart and
   after logout and login. A mode switch and a repair are tested only
   after a backup.
6. **MIME:** PGP-signed, encrypted, S/MIME, HTML with CID, nested
   messages, bad charsets.
7. **Performance:** time to first inbox, headers complete, bodies
   complete, RAM and disk, and new-mail latency at p50 and p95.
