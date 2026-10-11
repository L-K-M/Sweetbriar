# 04 — Data safety: never lose mail, drafts, settings or sends

Synthesis of [raw/data-safety/devin.md](raw/data-safety/devin.md) and
[raw/data-safety/codex.md](raw/data-safety/codex.md), with input from the
architecture, performance, classic-client and standards reports. The
SQLite facts (WAL-reset bug, fullfsync behaviour) were checked on
sqlite.org. Checked 2026-10-11.

## Summary

- **Safety is a contract about acknowledged operations.**
  - "Saved" means durably committed.
  - "Submitted" means accepted by the submission service. Generic SMTP
    can't guarantee exactly-once delivery after a lost acceptance reply,
    so uncertainty is a visible state, never a silent retry.
  - "Deleted" only ever happens through a specific, durable, authorized
    intent.
- **Three kinds of data with explicit authority:**
  1. Immutable original messages and attachments.
  2. Authoritative mutable state in SQLite: intents, receipts, draft
     revisions, settings, local metadata, sync cursors.
  3. Rebuildable projections: summaries, threads, search, previews.

  Rebuilding projections can never rewrite or delete originals or user
  state.
- **Persist intent before any network activity.** Replay is a
  convergence check. Retry only what is known absent or safely
  idempotent.
- **No bare EXPUNGE, ever,** and no implicit expunge through CLOSE.
  Copy, verify, then delete. Mass disappearance pauses the queue and asks.
- **Durability on macOS needs care.** Bundled SQLite ≥ 3.51.3 (WAL-reset
  fix), WAL with `synchronous=FULL`, `fullfsync`, checked error handling,
  consistent backups, verified migrations.
- **Hostile mail gets layered controls.** Content JavaScript off,
  HTML/CSS sanitized, all network requests denied by default, attachments
  isolated. JS blocking alone doesn't stop CSS exfiltration or EFAIL.
- **Prove the invariants.** Deterministic simulation, provider-specific
  real-server tests, fault injection at every crash boundary, restore
  drills. A passing kill -9 test doesn't prove power-loss durability.

## What went wrong elsewhere

| Incident | Mechanism | Prevention |
|---|---|---|
| Apple Mail, Catalina (2019) | A migration reported success without checking; moves left header-only copies on the server, and other devices synced the deletion back. Tsai (2023) reproduced: reapplying a move rule to the same IMAP mailbox deleted the mail | Verify migrations by inventory and bytes; same-destination moves are no-ops; never propagate a local reconstruction's gaps as deletions |
| Thunderbird compaction (2022–24; tracker 1890230) | mbox rewritten in place; downloads accepted while the store was locked; ignored error returns; an interrupted compaction left an empty index, so intact mail looked missing | Per-message immutable files; store ownership enforced at the lowest layer; errors propagate; "index unavailable" ≠ "message absent" |
| Antivirus quarantine | Whole mailbox file quarantined over one suspect message | Per-message storage limits the blast radius; detect missing bytes |
| Proton Bridge | Clients deleted the source after a failed APPEND; "Recovered Messages" is device-local and lost on reset; UIDs changed without UIDVALIDITY changing | Copy → verify → delete; recovery data lives in backups; epoch-bound UID mappings |
| K-9 / Thunderbird Android | 9.0 settings migration silently made deletes permanent (#9034); one failing APPEND blocked the whole queue (#1155) | Migrations never escalate destructiveness; poisoned ops park in isolation |
| Outlook PST/OST | Single-file format with size ceilings; repair drops items | Many small files; preserve the original before any repair |
| Eudora (source audit) | The TOC mixed index and irreplaceable metadata; rebuilds lost labels and edits and resurrected deleted mail; compaction could discard work in progress; autosave was off by default | Separate metadata from projections; no compaction of originals; autosave always on |
| Mailsmith 2.3 | Reconstruct from message files lost notes, labels and status | Journal metadata separately; mirror to keywords where possible |
| Claris Emailer 2.0 | Monolithic database corruption, whole-file backups, painful export | No opaque monolith; incremental backup; export on day one |
| Musashi | Mail lost on forced quit or a cancelled user switch; deletes bypassed Trash; an "auto-answer Yes" dialog timer | Never auto-confirm destructive actions |
| Mailspring | Drops in-flight tasks on restart instead of replaying; header-hash ids can collide | Replay with convergence checks; local ids never derived from headers alone |
| Gmail IMAP | `autoExpunge` and `expungeBehavior` can archive, trash or destroy mail when the last label goes | Model label removal, archive, Trash and destruction as separate operations |
| Airmail 3 (CVE-2018-15667/15669) | URL-scheme `send` with path traversal; deny-list WebView policy | Confirm URL-scheme actions; allowlist rendering policy; no predictable exfiltration paths |

## Storage layout and authority

```
~/Library/Application Support/Sweetbriar/    (Linux: ~/.local/share/sweetbriar/)
  state.sqlite      authoritative: accounts, logical messages, memberships,
                    remote identities + epochs, intents, receipts, draft
                    revisions, outbox attempts, recipient results, settings,
                    rules, labels, notes, overlays, sync cursors, backup gens
  objects/<account>/<shard>/<hash>    immutable original EML + attachment blobs
  revisions/        draft/outbox snapshots and envelope manifests
  recovery/         preserved originals and disputed versions
  backups/<gen>/    consistent DB snapshot + referenced objects + manifest
~/Library/Caches/Sweetbriar/                 (rebuildable; excluded from Time Machine)
  search index, previews, thumbnails, embeddings
```

- **Object publication:** write to a temp file in the same directory →
  flush (`F_FULLFSYNC` on macOS, `fdatasync` on Linux) → `rename` → fsync
  the directory → *then* commit the DB reference. A crash before the commit
  leaves an orphan for GC, never a dangling reference.
- **GC** respects live references, pending operations, recovery
  retention and backup pins. It commits the reference removal before
  unlinking, and never erases disputed data to clear an error.
- **Metadata is primary data.** A periodic online backup (SQLite backup
  API) and an append-only metadata journal mean a rebuild from objects
  keeps labels, notes, statuses and overlays. Mirror to IMAP and JMAP
  keywords and RFC 9979 flags where the server allows.
- **Store user-owned formats.** Original bytes are kept even when parsing
  fails. A local UUID is independent of Message-ID. Export to EML,
  Maildir and mbox (with documented `From ` escaping) from day one, with a
  metadata manifest.
- **Imports** (Apple Mail `.mbox`/`.emlx`, Thunderbird, Eudora, PST
  read-only) produce counts, byte totals, checksums, rejected items and
  metadata-loss warnings. The source is never removed automatically.
  Engst's two Eudora converters disagreed by 121,077 messages.
- **The counter-argument, taken seriously:** SQLite BLOBs remove the
  multi-file crash protocol, and SQLite measured small BLOBs 35% faster
  than files. Codex: adopt immutable files only after the publication,
  backup and GC protocol passes fault injection. If that proof is too
  costly, use BLOBs plus verified EML export; reliable export beats a
  nominally open store. Devin: files, because a single database file
  concentrates corruption risk and makes mail unreadable without the app.
  **Recommendation:** files, behind a blob abstraction with the protocol
  proven by fault injection before release, and a size cutoff open for
  small objects.

## SQLite durability

| Setting | Why |
|---|---|
| Bundled SQLite ≥ 3.51.3 (or 3.44.6 / 3.50.7 backports); check `sqlite_version()` in every host and extension | The WAL-reset bug (3.7.0–3.51.2) can lose commits when two connections write or checkpoint at once. Tailscale found it in production; Antithesis reproduced it in about 15 minutes |
| Never the system SQLite on macOS | Apple's build substitutes `F_BARRIERFSYNC` for `fullfsync` (bonsaidb, Tsai 2025) |
| WAL, `synchronous=FULL` for authoritative state | WAL + NORMAL can roll back acknowledged commits after power loss |
| `PRAGMA fullfsync=ON`, `checkpoint_fullfsync=ON` on macOS | macOS `fsync` doesn't flush the drive cache |
| One writer connection that also owns checkpoints; readers through it | Avoids the WAL-reset trigger only if no other connection checkpoints; the fixed version is still required. One owner for migrations |
| Exclusive process ownership lease on the live store | A second instance uses the existing core or stays read-only |
| `user_version` migrations, each transactional; `VACUUM INTO` a snapshot before destructive ones; verify counts and checksums after | Catalina and K-9 lessons |
| `integrity_check` scheduled and after migrations; SHA-256 per object, verified on read | Detects corruption; on detection, pause dangerous work and preserve evidence, never "repair" by deleting |
| Scheduled TRUNCATE checkpoints when idle; never hold transactions across I/O | Avoids WAL growth and checkpoint starvation |
| Store kept out of iCloud Drive, Dropbox, OneDrive and network mounts; warn if moved there | Sync daemons and network filesystems break SQLite's locking and fsync assumptions |
| Time Machine: back up `state.sqlite` (via snapshots) and objects; exclude caches (`isExcludedFromBackup`, reapplied after writes) | Backups contain what matters, not 10 GB of index |

**Open item.** Codex reports that macOS 27 changed app-data access
permissions. Import from other apps' containers must use user-selected
files or exports (verify).

## Sync engine correctness

- **Intent journal.** Each intent row is written in the same transaction
  as its local effect:
  `{id, account, op, args, state, attempts, idempotency_key, origin}`.
  Workers drain the journal. Per-account ordering applies only where the
  protocol demands it. A poisoned op parks after N attempts and surfaces
  to the user without blocking the rest.
- **Replay is convergence.** "Ensure UID X in epoch E has `\Deleted` and
  is UID-expunged", "ensure the message with hash H is in B and not in A",
  "ensure submission S reached state Accepted". Never a blind re-issue.
- **IMAP deletes and moves.** UID MOVE where advertised; else COPY →
  verify COPYUID → `UID STORE +FLAGS.SILENT (\Deleted)` → `UID EXPUNGE`
  (UIDPLUS). Without UIDPLUS: flag only and don't expunge; let the
  server's own policy or the user decide. The IMAP layer has **no bare
  EXPUNGE verb and no CLOSE after flagging**.
- **Default delete = move to Trash.** "Delete permanently" is a separate,
  confirmed command.
- **Epoch-bound identity.** Every remote locator carries
  `(mailbox, UIDVALIDITY)`. On a UIDVALIDITY change, suspend intents for
  that mailbox, resync, rematch by hash and Message-ID, then resume. Stale
  UIDs can never reach STORE or EXPUNGE.
- **Incomplete observations don't prove absence.** Page through a staged
  inventory and publish the cursor atomically. An expired cursor or a
  failed page never counts as deletion.
- **Tombstones** for local deletes until the server confirms and one clean
  sync passes. They also keep a crash from resurrecting a message as
  "new".
- **Circuit breaker:** pause and ask when, since the last sync, a delete
  set exceeds a threshold (start at > 50 messages or > 20% of a folder),
  or a UIDVALIDITY change or account reconfiguration would delete in
  bulk. Log the trigger.
- **Retain local copies when remote mail disappears** for a recovery
  period (configurable; an explicit permanent purge exists). This
  conflicts with erasure expectations; it's a product decision.
- **Provider dialects:**
  - **Gmail:** read `ImapSettings` (`autoExpunge`, `expungeBehavior`) or
    assume archive semantics; use `X-GM-MSGID` as identity; model label
    removal separately from destruction.
  - **Graph:** immutable ids, per-folder delta, a move is copy plus
    delete.
  - **JMAP:** `mailboxIds` updates are atomic per record, but `Email/set`
    isn't batch-atomic; reconcile `notUpdated`.
  - **Proton Bridge:** treat its UIDs with suspicion; detect epoch changes
    by content.
- **Mailspring's approach (drop in-flight tasks) is explicitly rejected.**

## Drafts and sending

- **Drafts** are immutable revisions with conflict branches: two clients
  editing offline keep both versions. Autosave is debounced (1–2 s) and
  also runs on blur and quit. "Saved" appears only after the durable
  commit. Attachments are copied into the object store before the draft
  counts as saved.
- **Server draft sync:** one stable Message-ID per draft; APPEND +
  APPENDUID; REPLACE (RFC 8508) where advertised.
- **Outbox state machine:**

  ```
  Held (undo window, serialized cancel/claim) → Queued → Submitting
       → Accepted  → [Sent filing: separate state machine]
       → Failed (5xx; user action)
       → OutcomeUnknown (drop after DATA, crash before receipt) → user resolves
  ```

  - Every submission carries the same Message-ID on any retry.
  - After an ambiguous drop, look up Sent by Message-ID before offering a
    resend. Never resend automatically.
  - 4xx: exponential backoff with jitter. 5xx: fail to the user.
  - SIZE is checked first. A link-upload path covers large attachments.
  - Graph's `202 Accepted` is accepted, not delivered.
  - **Filing the Sent copy can never trigger resubmission.**
  - **Restoring a backup never auto-dispatches** old outbox items.
- **Undo send** is a client hold queue ("sending in 7 s"); there's no wire
  recall. Outlook recall works only inside one Exchange organization.

## Credentials and hostile content

- **Credentials:** tokens in the Keychain with
  `kSecAttrAccessibleWhenUnlockedThisDeviceOnly` and non-synchronizable;
  libsecret on Linux.
  - Never in files or logs; scan diagnostics and exports for token
    patterns in CI.
  - `invalid_grant` means sign in again, and it never touches mail.
- **Encryption at rest:** FileVault covers a powered-off machine. Optional
  app-level encryption (SQLCipher for state; separate handling for
  objects and index files) needs a recoverable key story: losing the only
  key destroys availability. Decide per product.
- **Rendering** (see [08-macos-ui](08-macos-ui.md)):
  - **Script and network:** JavaScript off for every navigation; a
    deny-all `WKContentRuleList` compiled before display (fail closed); a
    nonpersistent data store.
  - **Sanitizing:** Rust sanitizer with an allowlist; CSS constrained
    (no remote `url()`, `@import` or `@font-face`; attribute-selector
    exfiltration considered).
  - **Remote content:** explicit consent, fetched through an app-controlled
    rewrite path.
  - **EFAIL rule:** decrypted content and remote loads never share a
    render context.
- **Attachments:** written with `com.apple.quarantine` so Gatekeeper treats
  them like downloads. Opened through a sandboxed path. Filename and path
  traversal checks.
- **Links:** show the real destination; warn on IDN confusables, raw IPs,
  userinfo and display/target mismatch (Eudora's ScamWatch).
  `Authentication-Results` only behind a provider boundary that strips
  injected copies; the `authserv-id` alone is forgeable text (RFC 8601).
  BIMI is branding, not trust.
- **URL schemes and automation** can never send or exfiltrate without
  confirmation (Airmail lesson).
- **Parsers:** fuzz MIME, charset, base64, QP, TNEF and PKCS#7 paths
  (cargo-fuzz, afl); cap depth and size. A panic renders "message could
  not be displayed", never kills the process; this needs `panic =
  "unwind"` and `catch_unwind` at the parser boundary, and OOM needs
  limits, not panics. Stalwart's CVE-2026-26312 (unbounded traversal of
  cyclic MIME, fixed in Stalwart 0.15.5) shows that Rust prevents memory
  bugs, not resource exhaustion; bound every consumer of the parse tree.

## Invariants and how each is proven

| Invariant | Enforcement | Test |
|---|---|---|
| Acknowledged local revisions survive crash and modeled power loss | FULL commits, platform barriers, no premature "Saved" | Crash before and after commit; discard unflushed writes; verify every acknowledged revision |
| A committed object reference points at complete, unchanged bytes | Publish and flush before reference; length and hash | Short write, flush failure, rename failure, torn object, orphan recovery |
| Rebuilding projections can't alter originals or user state | Separate ownership; read-only inputs | Delete or corrupt every cache; rebuild; compare inventories |
| Every remote destructive action has a durable intent | Planner-only writes, provenance, UID scoping | Rogue server responses, another client's `\Deleted`, same-folder move, account removal |
| Old mailbox identities can't target new messages | Epoch-bound mappings | UIDVALIDITY change with UID reuse during an offline delete |
| Incomplete observations can't establish absence | Staged inventory, atomic cursor publication | Fail each page; expired cursor; limited Gmail visibility |
| A move can't destroy the only complete copy | Destination verification, retained source | Drop after COPY/APPEND; quota failure at the destination |
| Draft conflicts keep both versions | Immutable revisions, conflict branches | Two offline editors, both reconnect orders, crash mid-merge |
| Send uncertainty is preserved and visible | Durable phases and receipts | Lose the final reply; crash before the receipt commit; restore an old backup |
| Undo can't race into unreported dispatch | Serialized cancel/claim | Concurrent cancel and claim; restart during hold; clock jump |
| Credential errors can't erase mail or leak secrets | Native secret adapters, staged replacement, redacted logs | Locked keychain, rotation failure, log scans |
| Untrusted content gets no automatic network or file access | Restricted renderer, sanitizer, capabilities | CSS, EFAIL and MIME fixtures; canary HTTP/DNS endpoints; CID traversal |
| Restore or migration can't silently lose data or replay effects | Verified generations, inventory comparison, disconnected recovery mode | Fail every migration boundary; restore old pending sends and deletes |
| Settings migrations never escalate destructiveness | Golden-file upgrade tests | Every persisted setting survives with equal or safer semantics |

## Testing strategy

- **Deterministic simulation from day one.** The sync engine is a state
  machine. Clock, network and disk are injectable. Seeded fake IMAP, JMAP
  and Graph servers inject faults: drops after COPY/APPEND/DATA, rogue
  responses, UIDVALIDITY resets, throttling, partial pages. Failing seeds
  replay exactly. Precedents: FoundationDB, TigerBeetle VOPR, and Dropbox
  Nucleus (a Rust client sync engine: three trees, remote/local/synced,
  that must converge; tens of millions of seeded runs nightly). Antithesis
  found SQLite's bug in about 15 minutes; production took Tailscale
  months.
- **Model-based tests:** proptest state machines against an independent
  in-memory IMAP model (RFC 9051 semantics plus UIDVALIDITY), audited
  against the RFCs so the model doesn't encode the same wrong assumptions.
- **Real servers in CI:** Dovecot (with `imaptest`), Cyrus (Cassandane),
  Stalwart, Greenmail, plus periodic runs against Gmail, Microsoft 365,
  iCloud, Fastmail and Yahoo test accounts. Mailspring's 2026 rewrite added
  exactly this ("fake problematic IMAP servers" in Docker).
- **Fault injection:** kill -9 at every journal state, ENOSPC, fsync
  errors, toxiproxy and `tc netem` latency, resets. Power-loss simulation
  (drop unflushed writes) separately from process crashes.
- **Restore drills:** backups restored on a clean machine; no dispatch of
  old sends; inventory comparison.
- **Protocol trace audit:** CI fails on any unaccounted destructive command
  in a recorded trace.

## Build order (codex)

1. **Local storage, drafts and outbox durability.** No network.
2. **Read-only sync and complete export/restore.** The app can't destroy
   anything yet.
3. **Reversible mutations** (flags, moves with verification) with
   reconciliation.
4. **Narrowly authorized destructive operations and submission.**

Each milestone adds its failure traces to the permanent regression corpus.

**Release gate:**
- backups restored on clean machines
- network loss and disk full tested with real accounts
- complete vs header-only status visible
- no automatic dispatch after restore
- no secrets in diagnostics
- no unaccounted destructive commands in traces

## Open questions

- **What "never" means.** Mail never downloaded can't be recovered locally.
  Offer a complete-archive mode, label incomplete messages, and state the
  recovery point of backups.
- **Retention of remotely deleted mail vs user erasure expectations.**
- **Single writer under load:** large imports must not delay draft commits
  (scheduler priority, small batches).
- **Encryption keys:** recovery story.
- **macOS 27 app-data import restrictions** (verify).

## Key sources

- [SQLite WAL and the WAL-reset bug](https://sqlite.org/wal.html), [SQLite pragmas](https://sqlite.org/pragma.html), [How to corrupt SQLite](https://sqlite.org/howtocorrupt.html), [bonsaidb: ACID on Apple](https://bonsaidb.io/blog/acid-on-apple/), [Tailscale](https://tailscale.com/blog/sqlite-wal-reset-bug), [Antithesis](https://antithesis.com/blog/2026/wal-reset-bug/)
- [RFC 4315 UIDPLUS](https://www.rfc-editor.org/rfc/rfc4315), [RFC 6851 MOVE](https://www.rfc-editor.org/rfc/rfc6851.html), [RFC 9051](https://www.rfc-editor.org/rfc/rfc9051.html), [RFC 8508 REPLACE](https://www.rfc-editor.org/rfc/rfc8508.txt), [RFC 5321](https://datatracker.ietf.org/doc/html/rfc5321)
- [Tsai: Catalina Mail data loss](https://mjtsai.com/blog/2019/10/11/mail-data-loss-in-macos-10-15/), [Thunderbird tracker 1890230](https://bugzilla.mozilla.org/show_bug.cgi?id=1890230), [Proton Recovered Messages](https://proton.me/support/what-is-the-recovered-messages-folder-in-bridge), [K-9 #9034](https://github.com/thunderbird/thunderbird-android/issues/9034), [Gmail ImapSettings](https://developers.google.com/workspace/gmail/api/reference/rest/v1/ImapSettings)
- [EFAIL](https://efail.de/), [Blind CSS exfiltration](https://portswigger.net/research/blind-css-exfiltration), [Airmail CVE-2018-15667](https://versprite.com/advisories/airmail-3-for-mac/)
- [Dropbox: testing the sync engine](https://dropbox.tech/infrastructure/-testing-our-new-sync-engine), [TigerBeetle VOPR](https://github.com/tigerbeetle/tigerbeetle/blob/main/docs/internals/vopr.md), [imaptest](https://dovecot.github.io/imaptest/)
