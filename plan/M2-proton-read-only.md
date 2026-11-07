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
  - **M2.1a — the permanent fake server (this change):** in
    `sb-test-support`, the scripted fake IMAP server with the
    **Bridge profile** (capabilities without CONDSTORE, NAMESPACE or
    LITERAL+; LIST with `Folders/` and `Labels/` and role attributes;
    the same message in several mailboxes with `X-Pm-Internal-Id`;
    UIDPLUS receipts) and every fault knob (11), with tests for each;
    the raw-UTF-8 ENVELOPE fault fixture (`RAW_UTF8_ENVELOPE`), kept
    public for M2.1b and undecodable by imap-codec;
  - **M2.1b — the crate spike (follows):** a throwaway spike client
    with each crate doing: STARTTLS with a custom verifier, LOGIN,
    CAPABILITY, ID, LIST, EXAMINE,
    `UID FETCH … BODY.PEEK[HEADER.FIELDS (…)]`, `UID FETCH 1:* (FLAGS)`,
    IDLE, and a raw UTF-8 ENVELOPE fault, committed as
    `crates/sb-imap/tests/spike_operations.rs`; the same suite against
    Dovecot (Docker, pinned digest) through `cargo xtask ci --docker`
    and the `dovecot` CI job; a decision note in the PR, plus updates
    to spec/01.
- **Acceptance:**
  - **M2.1a:** every knob and the Bridge profile have tests that fail
    if the behaviour breaks; `cargo test -p sb-test-support` passes;
  - **M2.1b:** the chosen crate passes every spike operation against
    the fake server and Dovecot (Docker, pinned digest); a malformed
    response closes the connection without applying later data; and
    the decision is recorded in spec/01.

### M2.2 IMAP transport — L, linux

- **Goal:** `sb-imap` provides connections and read operations, with
  pinned-certificate TLS.
- **Spec:** 04 (Structure, Connections), 10 (TLS and the Bridge
  certificate).
- **Depends on:** M2.1b.
- **Prerequisite split (2026-11-07):** extract **M2.2a0 Pinned TLS and
  secret primitives (M)** before integrating the connection artifact.
  This keeps its cancellation regression and all existing proof within
  the 1,450-line limit. a0 provides the exact DER SHA-256 pin, the private
  rustls verifier/configuration with validity and handshake-signature
  checks, and the non-cloning redacted password wrapper. Native tests
  cover exact pins, mismatch evidence, validity and secret formatting.
  No socket, credential exchange or complete transport is exposed by a0;
  the following connection, STARTTLS, read and lifecycle artifacts retain
  every deliverable and acceptance check below.

The whole is larger than the size limit, so it runs as four steps,
each with its own PR. Every deliverable and acceptance item is kept;
the provider policies (`proton_bridge`, `generic_imap`) live in
`sb-engine` (module `policy`) (04) and arrive in M2.4.

#### M2.2a1 Implicit pinned-TLS connection — L, linux

- **Goal:** `sb-imap::Connection` sets up an implicit pinned-TLS session.
- **Deliverables:** loopback policy, SHA-256 leaf pin and validity and
  handshake signature checks, strict response gate, greeting, CAPABILITY,
  LOGINDISABLED refusal, LOGIN, post-login CAPABILITY, ID, best-effort
  LOGOUT, typed errors and redacted credentials.
- **Acceptance**, against the fake server:
  - a wrong pin is rejected before LOGIN;
  - a non-loopback host is rejected for the Bridge preset;
  - a malformed response closes the connection and nothing after it is
    delivered;
  - credentials never appear in formatted output (the log-scan test).

#### M2.2a2 STARTTLS and Dovecot setup — L, linux

- **Deliverables:** STARTTLS capability, command and pinned upgrade;
  refusal of trailing plaintext; Dovecot setup wiring.
- **Acceptance**, against the fake server and Dovecot:
  - a missing STARTTLS is rejected before credentials;
  - plaintext injected after the tagged OK is refused;
  - setup, ID and best-effort LOGOUT succeed against Dovecot.

#### M2.2a3 Correlated setup exchanges — M, linux

- **Correctness prerequisite (2026-11-07):** native regressions show the
  pinned sender can replace a LOGIN or CAPABILITY refusal with a later
  tagged OK, and does not check the tag. Before b1, feed setup coroutines
  one complete response at a time and correlate the first tagged reply
  with the encoded command. Keep synchronizing literals, legitimate
  unsolicited updates, cancellation ownership and STARTTLS's stricter
  plaintext boundary. This adds no read observations or provider policy.
- **Acceptance:** native tests reproduce refusal replacement before the
  fix, reject mismatched completions and prevent buffered extra tagged
  replies from authorizing a following command; implicit TLS, STARTTLS,
  LOGIN literals, ID and best-effort LOGOUT still pass the fake server
  and pinned Dovecot suite. Malformed replies and cancelled futures close
  the connection. Run the full local gate and Dovecot suite.

#### M2.2b Read operations — L, linux

- **Split before implementation (2026-11-07):** retain the complete read
  artifact as four bounded PRs. **b0 Strict mailbox-name codec (M)** supplies
  private Unicode/modified-UTF-7 conversion with RFC 3501 section 5.1.3
  vectors, lossless round trips, and rejection of malformed/noncanonical
  encodings instead of replacement characters or aliases. It opens no socket
  and yields no read observation. **b1 Mailbox observations (L)** adds LIST
  and EXAMINE with decoded UTF-8 names and checked epoch fields; **b2 Header,
  UID-set and flag observations (L)** adds the corresponding reads; **b3 Body
  streams (L)** adds bounded literal streaming under the full message budget.
  Every read must correlate its first tagged completion, fail closed on
  malformed/cancelled exchanges, and keep the fake-server and Dovecot
  acceptance below. b1-b3 consume the preceding merged contracts; no provider
  policy or store writes enter this leaf.
- **b1 contract:** `list_mailboxes` and `examine` return owned observations
  together with coalesced reconciliation notices. Buffered pre-command data,
  including a partially received frame, stays under its previous selection
  context and cannot supply a new selection's epoch. Unsolicited data is
  preserved as invalidation evidence, not applied as a row journal. Each new
  selection must supply FLAGS, EXISTS, RECENT, UIDVALIDITY and the tagged
  READ-ONLY code; absent newer optional codes remain explicitly unknown.
  Selection switches first use a correlated UNSELECT barrier when advertised;
  otherwise the transport requires a fresh connection. This excludes old
  in-flight epoch data as well as buffered frames, without CLOSE or EXPUNGE.
  Conflicting epochs and decreasing UIDNEXT are refused. A metadata exchange
  has a bounded response budget; the separate b3 streaming contract is kept.
- **b2 split before implementation (2026-11-07):** **b2a UID inventory and
  flags (L)** precedes **b2b Header observations (L)** so their independent
  wire and coverage proofs stay within the artifact limit. b2a requires an
  explicit UID SEARCH ALL response, distinguishing an empty set from missing
  evidence. Its flag sweep pairs that inventory with UID FETCH 1:* (UID FLAGS),
  keyed only by the UID item, never the FETCH sequence number. Missing flags
  remain unknown; mismatched inventories, unkeyed or repeated FETCH updates
  and mailbox-change hints produce explicit incomplete coverage. These are
  observations, never authority to delete rows. Contradictory or malformed
  epoch evidence closes the session. Buffered data remains a notice under
  the established selection. b2b retains every header-field/literal
  deliverable; b3 retains the full body budget and streaming requirement.
  Both b2 artifacts keep the first-tag, cancellation, fake Bridge profile
  and actual pinned Dovecot checks, with no provider policy or store writes.
  While selected, CAPABILITY refreshes use the same epoch checks and retain
  reconciliation notices for the next read; a refresh cannot hide an epoch
  change from a later UID observation or leave the capability cache stale.
- **Goal:** the dialect-free command interface returns the read
  observations.
- Body streaming must support spec/10's 100 MB message budget without
  inheriting setup's 50 MiB response-framing cap; oversized MIME still
  needs partial-parse/coverage evidence, not an endless reconnect loop.
- **Deliverables:**
  - a dialect-free command interface;
  - read operations returning observations (mailbox list, epoch, UID
    sets, header literals, flags, body literals as streams).
- **Acceptance**, against the fake server and Dovecot:
  - each observation is returned from the fake server's Bridge profile
    and from the container.

#### M2.2c IDLE, timeouts and reconnect — L, linux

- **Goal:** the connection stays live and recovers.
- **Deliverables:**
  - an IDLE watcher (renewed at 25 min);
  - timeouts and backoff (30 s per command, 120 s per body fetch,
    scaled for large literals; reconnects back off exponentially, 1 s
    to 5 min).
- **Acceptance**, against the fake server and Dovecot:
  - command timeouts trigger a reconnect;
  - IDLE is renewed at 25 min (virtual clock).

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
  - the provider policy modules (`sb_engine::policy::proton_bridge`,
    `generic_imap`) with their mailbox mapping and grouping rules; later
    steps add their mutation rules;
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
  - M1.8's restore test now also stops an active account worker before
    the swap;
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

### M2.6 Body acquisition and coverage — M, linux

- **Goal:** bodies download once per logical message, and coverage and
  recovery are queryable.
- **Spec:** 04 (Sync loop step 4, Epoch changes: Recovery), 06
  (Coverage).
- **Depends on:** M2.5.
- **Deliverables:**
  - the body queue (opened messages first, then newest) using
    `BODY.PEEK[]`, publish, link and index;
  - `coverage()`;
  - the "Recovery: messages no longer on the server" list (`epoch_stale`,
    `vanished` and `superseded` occurrences with no present
    counterpart, each with its reason); a message all of whose
    occurrences were superseded keeps its body, tags and notes there.
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
  - complete header rows preserve 02/06's absent-versus-unidentifiable JSON
    convention; a present invalid field cannot become a known-empty NULL;
  - reading never sends `\Seen` (command trace);
  - coverage numbers match fixture expectations.

### M2.7 Account health and the Activity window — M, linux + macos-ci

- **Goal:** accounts can be managed and repaired, and their health is
  visible in the app.
- **Spec:** 04 (Account states and health), 07 (Accounts commands), 08
  (Activity / account health window), 10 (diagnostics, notifications).
- **Depends on:** M2.6.
- **Deliverables:**
  - `health()`;
  - `update_account` (endpoint, credentials, certificate re-trust; keeps
    the account UUID and bumps `config_generation`), `pause_account`,
    `resume_account` and `disconnect_account`, tested through the public
    API, including resuming after a restore;
  - the diagnostics package (redacted, previewed, saved by the user; 10),
    used by M2.Q item 3;
  - new-mail notifications through `UNUserNotificationCenter`, naming the
    account in text, with previews only if enabled (10);
  - the macOS Activity window: per account state, coverage, breaker
    events with Accept and Keep, parked and quarantined intents with
    actions, and the Recovery list.
- **Acceptance:**
  - breaker Accept and Keep work end to end (UI test with the fake),
    leaving the event `accepted` or `kept`; an event whose messages
    reappear becomes `cleared`;
  - a re-trusted certificate and repaired credentials keep the account's
    UUID and data;
  - the diagnostics package contains no secret (log-scan test).

### M2.8 Real-Bridge CI lane — L, linux (nightly)

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
