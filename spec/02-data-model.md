# 02 — Data model (SQLite schema v2)

Rests on research D1, D2, D3, D14, D22, D23 and D31
([00-summary](../research/00-summary.md),
[debate/04-resolution](../research/debate/04-resolution.md),
[15-account-colors](../research/15-account-colors.md)).

## Rules

1. **One database file, two kinds of table.**
   - **Authoritative** tables hold what can't be rebuilt from originals:
     accounts, identities, occurrences and their flags, user metadata,
     drafts, intents, send attempts, settings.
   - **Derived** tables (prefix `d_`) can be dropped and rebuilt from
     acquisitions and authoritative tables: parsed summaries, threads,
     search.
   - A rebuild never touches authoritative tables.
2. **Every application-owned ordinary table is `STRICT`.** SQLite's
   internal tables (`sqlite_*`) and FTS5 virtual and shadow tables can't
   be. Every connection runs with `foreign_keys=ON` (03-storage).
3. **Ids:**
   - Entities the user or other machines may refer to (accounts,
     identities, drafts, intents, tags, notes, views) use a `TEXT` UUID
     (lowercase, hyphenated).
   - High-volume rows (acquisitions, occurrences, logical messages) use
     `INTEGER PRIMARY KEY AUTOINCREMENT`, so ids are never reused.
4. **Timestamps** are `INTEGER` milliseconds since the Unix epoch, UTC.
5. **Booleans** are `INTEGER` with `CHECK (x IN (0,1))`.
6. **Enumerations** are `TEXT` with a `CHECK` listing the allowed values.
   Adding a value is a migration.
7. **JSON** columns are `TEXT` holding canonical JSON (sorted keys, no
   insignificant whitespace), validated by the core before writing.
8. **Never delete authoritative rows to "clean up".** Rows carry a state;
   only an explicit user action or a retention policy removes them, and
   always through an intent or a recorded operation.
9. **Revisions:** every row holding user-authored settings or metadata
   (accounts, identities, appearance, tags, notes, views, settings) has
   `revision INTEGER` that increments on change. This reserves the field
   for later cross-device sync (D23). Drafts are versioned by immutable
   revisions instead; intents and send attempts are device-local records
   and never sync.
10. **Colors** are stored as canonical lowercase `#rrggbb`. Inputs,
    including the palette's published uppercase values, are lowercased
    before validation.

## Store metadata

```sql
CREATE TABLE store_meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
) STRICT;
-- keys: 'store_uuid', 'incarnation' (new UUID after every restore),
-- 'created_at', 'app_version_last_opened', 'last_migrated_by' (app version),
-- 'clean_shutdown_at' (03 Integrity: present only between a clean shutdown and the next open),
-- 'list_generation' (an integer in decimal digits; the d_list_rows generation, from migration 3),
-- 'thread_inputs' and 'threads_built' (integers in decimal digits, from migration 4): how often
-- imports added logical messages, and how many of those the last complete thread rebuild read.
-- Migration 5: 'index_inputs', 'search_extractor_version' (06) and
-- 'search_generation' (07), integers in decimal digits; search has its own
-- generation so index-only writes leave ordinary list windows alone.
-- The schema version is PRAGMA user_version, owned by the migration runner;
-- it is not duplicated here.
```

## Accounts and identities

```sql
CREATE TABLE accounts (
  account_id        TEXT PRIMARY KEY,                 -- UUID, never changes
  kind              TEXT NOT NULL CHECK (kind IN ('local','imap')),
  provider          TEXT NOT NULL CHECK (provider IN ('local_archive','proton_bridge','generic_imap')),
  display_name      TEXT NOT NULL,
  state             TEXT NOT NULL CHECK (state IN ('active','paused','needs_attention','disconnected')),
  state_reason      TEXT,                             -- shown on the health surface
  config_generation INTEGER NOT NULL DEFAULT 1,       -- bumped when server, credentials or identity change
  sort_order        INTEGER NOT NULL,
  created_at        INTEGER NOT NULL,
  revision          INTEGER NOT NULL DEFAULT 1
) STRICT;
CREATE UNIQUE INDEX accounts_one_local_archive ON accounts(provider)
  WHERE provider = 'local_archive';

CREATE TABLE imap_endpoints (                         -- one per IMAP account
  account_id        TEXT PRIMARY KEY REFERENCES accounts(account_id),
  imap_host         TEXT NOT NULL,
  imap_port         INTEGER NOT NULL,
  imap_security     TEXT NOT NULL CHECK (imap_security IN ('tls','starttls')),
  smtp_host         TEXT NOT NULL,
  smtp_port         INTEGER NOT NULL,
  smtp_security     TEXT NOT NULL CHECK (smtp_security IN ('tls','starttls')),
  username          TEXT NOT NULL,
  credential_ref    TEXT NOT NULL,                    -- key into the platform secret store, never the secret
  trusted_cert_sha256 TEXT,                           -- pinned leaf certificate (Bridge), hex
  server_saves_sent INTEGER NOT NULL CHECK (server_saves_sent IN (0,1)),
  address_mode      TEXT CHECK (address_mode IN ('combined','split')), -- Proton Bridge only
  capabilities_json TEXT,                             -- last CAPABILITY, ID and LIST roles seen (04 compatibility ledger)
  revision          INTEGER NOT NULL DEFAULT 1
) STRICT;

CREATE TABLE identities (
  identity_id        TEXT PRIMARY KEY,
  account_id         TEXT NOT NULL REFERENCES accounts(account_id),
  email              TEXT NOT NULL,
  display_name       TEXT NOT NULL,
  reply_to           TEXT,
  signature_source   TEXT,                            -- Markdown or plain text
  is_default         INTEGER NOT NULL CHECK (is_default IN (0,1)),
  sendable           TEXT NOT NULL CHECK (sendable IN ('validated','provisional','unavailable')),
  revision           INTEGER NOT NULL DEFAULT 1,
  UNIQUE (account_id, email),
  UNIQUE (identity_id, account_id)                   -- target of account-scoped foreign keys
) STRICT;
CREATE UNIQUE INDEX identities_one_default ON identities(account_id) WHERE is_default = 1;

CREATE TABLE account_appearance (
  account_id   TEXT PRIMARY KEY REFERENCES accounts(account_id),
  color_srgb   TEXT NOT NULL CHECK (color_srgb GLOB '#[0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f]'),
  symbol       TEXT,                                  -- monogram override or symbol name
  assignment   TEXT NOT NULL,                         -- 'palette:v1:<slot>' or 'custom'
  revision     INTEGER NOT NULL DEFAULT 1
) STRICT;

CREATE TABLE identity_appearance (
  identity_id          TEXT PRIMARY KEY REFERENCES identities(identity_id),
  color_override_srgb  TEXT CHECK (color_override_srgb IS NULL OR color_override_srgb GLOB '#[0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f]'),
  symbol_override      TEXT,
  revision             INTEGER NOT NULL DEFAULT 1
) STRICT;
```

- The **local archive** is an account with `kind='local'`. Imported
  messages live there, so every message belongs to exactly one account.
- An account's **state** changes only through the engine.
  `disconnected` keeps all data; it only stops network activity
  (research D3 and D25: removal never deletes).
- **`sendable`:**
  - `validated`: the backend confirmed the address can send.
  - `provisional`: user-provisioned, unconfirmed, as with delegated
    send-as.
  - `unavailable`: the address can't send right now.
  - The From list offers `validated` and `provisional` identities, with
    provisional ones marked (15-account-colors).

## Acquisitions: immutable originals

```sql
CREATE TABLE acquisitions (
  acquisition_id   INTEGER PRIMARY KEY AUTOINCREMENT,
  content_sha256   TEXT NOT NULL UNIQUE,              -- hex of the exact bytes
  byte_length      INTEGER NOT NULL,
  object_state     TEXT NOT NULL CHECK (object_state IN ('published','missing','quarantined')), -- 03: absent, or damaged and moved to recovery/
  first_acquired_at INTEGER NOT NULL
) STRICT;

CREATE TABLE acquisition_sources (                    -- provenance, one row per time a source delivered these bytes
  source_id        INTEGER PRIMARY KEY AUTOINCREMENT,
  acquisition_id   INTEGER NOT NULL REFERENCES acquisitions(acquisition_id),
  account_id       TEXT NOT NULL REFERENCES accounts(account_id),
  source_kind      TEXT NOT NULL CHECK (source_kind IN ('imap_fetch','import_mbox','import_eml','send_attempt')), -- 'send_attempt' reserved: in the pilot, sent mail arrives through Sent sync
  source_ref       TEXT NOT NULL,                     -- e.g. occurrence id; import source path + sha256 + mailbox + offset (09); attempt id
  completeness     TEXT NOT NULL CHECK (completeness IN ('complete','truncated')),
  acquired_at      INTEGER NOT NULL
) STRICT;
CREATE UNIQUE INDEX acquisition_import_identity
  ON acquisition_sources(account_id, source_kind, source_ref)
  WHERE source_kind IN ('import_mbox','import_eml');
```

- An acquisition row is inserted **only after** its object is published
  (03-storage). `missing` means a referenced object failed verification:
  the row stays and the UI shows the problem. Nothing is silently
  dropped.
- Bytes are stored exactly as received, even when parsing fails.
  "Original" means "as acquired from this source", not the SMTP byte
  stream.

## Import jobs (schema v2, M1.2b)

```sql
CREATE TABLE import_jobs (
  job_id          TEXT PRIMARY KEY,
  state           TEXT NOT NULL CHECK (state IN ('inventory','awaiting_confirmation','running','completed','cancelled','failed')),
  request_json    TEXT NOT NULL,
  checkpoint_json TEXT NOT NULL,
  revision        INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
  created_at      INTEGER NOT NULL,
  updated_at      INTEGER NOT NULL
) STRICT;

CREATE TABLE import_job_sources (
  job_id          TEXT NOT NULL REFERENCES import_jobs(job_id),
  ordinal         INTEGER NOT NULL CHECK (ordinal >= 0),
  source_json     TEXT NOT NULL,
  checkpoint_json TEXT NOT NULL,
  PRIMARY KEY (job_id, ordinal)
) STRICT;
```

- Job requests, source inventory and checkpoints are canonical JSON objects
  with `"v": 1`. The engine owns their payload types (09); the store
  validates JSON and version before writing. Source inventory is immutable;
  variant choices and progress live in its checkpoint.
- Inventory is shown before confirmation. Only a `running` job ingests.
  Completed, cancelled and failed jobs are retained as history.
- Every batch commits acquisitions, provenance, occurrences, summaries,
  index-queue entries and both checkpoints in one transaction. Updating
  the job requires its expected revision, so two workers cannot advance
  the same checkpoint. A stale revision is `Conflict` and rolls back.
- Each source has its own row so a batch rewrites only its progress,
  rather than an entire EML-folder inventory.
- Migration 2 adds these tables and both unique indexes. It creates the
  local archive account if none exists, preserving an existing archive
  and all v1 rows. It rewrites or drops no existing data.

## Mailboxes, occurrences and logical messages

```sql
CREATE TABLE mailboxes (
  mailbox_id     INTEGER PRIMARY KEY AUTOINCREMENT,
  account_id     TEXT NOT NULL REFERENCES accounts(account_id),
  remote_name    TEXT NOT NULL,                        -- decoded (UTF-8) IMAP name; local archive uses import names
  delimiter      TEXT,
  role           TEXT CHECK (role IN ('inbox','sent','drafts','trash','junk','archive','all','flagged')), -- Bridge's Starred is 'flagged' (04)
  kind           TEXT NOT NULL CHECK (kind IN ('folder','label','virtual')),
  selectable     INTEGER NOT NULL CHECK (selectable IN (0,1)),
  uidvalidity    INTEGER,                              -- current epoch; NULL for local mailboxes
  uidnext        INTEGER,
  highest_modseq INTEGER,
  sync_state     TEXT NOT NULL CHECK (sync_state IN ('new','backfilling','synced','epoch_changed','gone')),
  revision       INTEGER NOT NULL DEFAULT 1,
  UNIQUE (account_id, remote_name),
  UNIQUE (mailbox_id, account_id)
) STRICT;

CREATE TABLE logical_messages (
  message_id       INTEGER PRIMARY KEY AUTOINCREMENT,
  account_id       TEXT NOT NULL REFERENCES accounts(account_id),
  provider_key     TEXT,                               -- provider object id when one exists; Proton: X-Pm-Internal-Id (04)
  acquisition_id   INTEGER REFERENCES acquisitions(acquisition_id), -- best complete body, NULL until fetched
  created_at       INTEGER NOT NULL,
  UNIQUE (account_id, provider_key),
  UNIQUE (message_id, account_id)
) STRICT;

CREATE TABLE occurrences (
  occurrence_id    INTEGER PRIMARY KEY AUTOINCREMENT,
  account_id       TEXT NOT NULL,                      -- must match both the mailbox's and the message's account
  mailbox_id       INTEGER NOT NULL,
  uidvalidity      INTEGER,                            -- epoch this UID belongs to; NULL for local archive
  uid              INTEGER,                            -- NULL for local archive
  message_id       INTEGER NOT NULL,
  internal_date    INTEGER,
  rfc822_size      INTEGER,
  modseq           INTEGER,
  f_seen           INTEGER NOT NULL DEFAULT 0 CHECK (f_seen IN (0,1)),
  f_flagged        INTEGER NOT NULL DEFAULT 0 CHECK (f_flagged IN (0,1)),
  f_answered       INTEGER NOT NULL DEFAULT 0 CHECK (f_answered IN (0,1)),
  f_draft          INTEGER NOT NULL DEFAULT 0 CHECK (f_draft IN (0,1)),
  f_deleted        INTEGER NOT NULL DEFAULT 0 CHECK (f_deleted IN (0,1)),
  keywords         TEXT NOT NULL DEFAULT '[]',         -- canonical JSON array, sorted ($Forwarded, $MailFlagBit0 …)
  state            TEXT NOT NULL CHECK (state IN ('present','vanish_pending','vanished','epoch_stale','superseded')),
  first_seen_at    INTEGER NOT NULL,
  last_seen_at     INTEGER NOT NULL,
  FOREIGN KEY (mailbox_id, account_id) REFERENCES mailboxes(mailbox_id, account_id),
  FOREIGN KEY (message_id, account_id) REFERENCES logical_messages(message_id, account_id)
) STRICT;
CREATE INDEX occ_by_message ON occurrences(message_id);
-- A locator is unique only among current bindings: a UID reused within the same
-- epoch gets a new row, and the old row becomes 'superseded' (04).
CREATE UNIQUE INDEX occ_current_locator ON occurrences(mailbox_id, uidvalidity, uid)
  WHERE state IN ('present','vanish_pending');

CREATE TABLE message_headers (                    -- headers fetched at sync, before any body; keyed by logical message
  message_id        INTEGER PRIMARY KEY REFERENCES logical_messages(message_id),
  raw_fields        BLOB NOT NULL,                -- the exact header-fields literal fetched (04 step 3)
  header_message_id TEXT,
  subject           TEXT,
  from_json         TEXT,
  reply_to_json     TEXT,
  to_json           TEXT,
  cc_json           TEXT,
  delivered_to_json TEXT,                         -- Delivered-To and X-Original-To
  date_header       INTEGER,
  in_reply_to       TEXT,
  references_json   TEXT,
  list_id           TEXT,
  fetched_at        INTEGER NOT NULL
) STRICT;
```

- **Account scoping is enforced by the schema** where it is fixed:
  - an occurrence's mailbox and logical message must belong to its
    account;
  - a breaker event's mailbox must belong to its account;
  - a draft's identity, and a send attempt's identity, must match their
    account;
  - a send attempt's revision must belong to its draft.
  - a draft's current revision belongs to that draft, and a revision's
    base is in the same draft;
  - each revision snapshots its route (account and identity); a send
    attempt's draft, revision, account and identity must equal that
    revision's snapshot. Historical revisions keep their original route
    when the user later switches From.
- **Flags live on occurrences** (D2). The same Proton message shown in
  `INBOX` and `Labels/Work` through Bridge has two occurrences with
  independent flags.
- **Grouping occurrences into one logical message** requires provider
  evidence:
  - a provider object id (`provider_key`; see 04 for what Bridge
    offers);
  - IMAP `OBJECTID`/`EMAILID` (RFC 8474);
  - a COPY/MOVE result Sweetbriar performed itself.
  - Equal bytes or an equal Message-ID **never** group occurrences. They
    may only be recorded as a correspondence hint (`d_correspondence`,
    later).
- **Headers before bodies.** `message_headers` keeps the header fields
  fetched during sync, so list rows, threading, offline replies and
  reply-identity rules work before a body is acquired. It is refreshed
  when the body arrives. It isn't derived from acquisitions, so it is
  kept and backed up.
  Its address JSON uses the summary's `{"address", "name"}` list shape.
  A complete fetched row covers the required From/To/Cc fields: NULL means
  an absent field or known-empty list. A present but unidentifiable field
  retains an entry with a null address, never collapses to whole-column NULL
  or an empty list. A valid empty group is still known empty (06).
- **UID reuse.** If the same `(mailbox, uidvalidity, uid)` later names a
  different provider message (a different `X-Pm-Internal-Id`), the old
  row becomes `superseded` and a new row is inserted. History isn't
  rewritten.
- **Flags shown in a list** are the row's occurrence's: the occurrence
  in that mailbox, or the representative in per-message windows (07).
  For Proton, local flag changes apply to every occurrence of the
  message (04), so occurrences disagree only briefly: after another
  client's change, until the next sweep.
- **Occurrence states:**
  - `vanish_pending`: absent from the server, not yet confirmed by a
    complete observation.
  - `vanished`: confirmed gone. The occurrence row stays for history.
  - `epoch_stale`: its UIDVALIDITY was replaced (04).
- **Membership is the occurrence** itself, an (occurrence, mailbox) pair,
  so no separate membership table is needed for IMAP. Graph categories
  and Gmail labels get their own tables when those backends arrive
  (*later*).

## Private user metadata

```sql
CREATE TABLE tags (
  tag_id     TEXT PRIMARY KEY,
  name       TEXT NOT NULL UNIQUE,
  color_srgb TEXT CHECK (color_srgb IS NULL OR color_srgb GLOB '#[0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f]'),
  revision   INTEGER NOT NULL DEFAULT 1
) STRICT;

CREATE TABLE message_tags (
  message_id INTEGER NOT NULL REFERENCES logical_messages(message_id),
  tag_id     TEXT NOT NULL REFERENCES tags(tag_id),
  added_at   INTEGER NOT NULL,
  revision   INTEGER NOT NULL DEFAULT 1,           -- rule 9
  PRIMARY KEY (message_id, tag_id)
) STRICT;

CREATE TABLE notes (                              -- one note per message in the pilot
  note_id     TEXT PRIMARY KEY,
  message_id  INTEGER NOT NULL UNIQUE REFERENCES logical_messages(message_id),
  body_source TEXT NOT NULL,                       -- Markdown
  created_at  INTEGER NOT NULL,
  updated_at  INTEGER NOT NULL,
  revision    INTEGER NOT NULL DEFAULT 1
) STRICT;

CREATE TABLE saved_views (
  view_id       TEXT PRIMARY KEY,
  name          TEXT NOT NULL,
  query_json    TEXT NOT NULL,                      -- query AST (06), with "v": 1
  sort          TEXT NOT NULL CHECK (sort IN ('date_desc','date_asc','relevance')),
  sort_order    INTEGER NOT NULL,
  revision      INTEGER NOT NULL DEFAULT 1
) STRICT;
```

Tags and notes attach to **logical messages**, the provider object,
deliberately. They survive the message vanishing from the server, and
they are never written to the server in the pilot.

## Contacts (reservation)

```sql
CREATE TABLE seen_addresses (                     -- derived from mail; feeds completion
  canonical      TEXT PRIMARY KEY,                -- local part as written + '@' + lowercased domain
  email          TEXT NOT NULL,                   -- the address exactly as last seen (local-part case kept, RFC 5321 §2.4)
  display_name   TEXT,
  last_seen_at   INTEGER NOT NULL,
  times_sent_to  INTEGER NOT NULL DEFAULT 0,
  times_received INTEGER NOT NULL DEFAULT 0,
  last_account_id TEXT REFERENCES accounts(account_id)
) STRICT;
```

Two spellings that differ only in the domain's case share a row. Spellings
that differ in the local part's case are separate rows, since local parts
may be case-sensitive (RFC 5321 §2.4). `send_recipients` uses the same
canonical form.

A full `contacts` table (D22) is *later*. `seen_addresses` is
technically derived but kept: rebuilding it would lose sent-to counts
from discarded drafts. It is listed in the backup.

### Received-address population (M1.6)

- Read the summary's first From, Reply-To, To and Cc mailboxes; ignore
  missing/invalid addr-specs and empty group names. No Bcc inference and
  no scraping bodies. Deduplicate canonical addresses within one logical
  message. Preserve local-part case and spelling; lowercase only the
  domain as in 06. `times_received` means appearances in these received
  headers, not the number of acquisitions or labels. M1.6 never changes
  `times_sent_to`. This includes Sent-role messages and the owner's own
  addresses: these are header-observation counts, not inferred direction.
  Imported mailbox roles are unreliable and a message may have several
  labels, so M1.6 applies no role/direction or self-address filter. Completion
  may offer an own address; no automatic identity suppression is implied.
- Migration 7 adds the kept ledger. WITHOUT ROWID stores its canonical key once
  rather than duplicating the composite primary key, within the per-key budget.

```sql
CREATE TABLE seen_address_messages (
  message_id INTEGER NOT NULL REFERENCES logical_messages(message_id),
  canonical TEXT NOT NULL REFERENCES seen_addresses(canonical),
  PRIMARY KEY (message_id, canonical)
) STRICT, WITHOUT ROWID;
CREATE INDEX occ_by_message_observation ON occurrences(message_id, first_seen_at);
```

  For each `(message_id, canonical)` key, in one writer transaction,
  insert/upsert the address row, insert the
  ledger key if absent and increment `times_received` only for a new key.
  Several logical messages sharing bytes each contribute once; several
  occurrences of one message contribute once. Only complete address-source
  summaries (`parse_state='ok'`, `parser_version>=2`) contribute. Partial,
  failed and legacy summaries contribute nothing until a successful eligible
  refresh. Later eligible summaries may add addresses, never remove historical
  counts. Ineligible sources change neither counts/ledger nor display metadata.
  `ok` is the persisted complete parser outcome: truncation or limit warnings
  must persist `partial`, never `ok`. Rebuilding derived tables never clears
  this ledger.
- `last_seen_at` is the earliest occurrence `first_seen_at` of the logical
  message, not its untrusted Date header or the backfill clock. The largest
  observation time wins `email`, `display_name`, `last_account_id`; for
  equal times the lexicographically largest `(email, name-or-empty,
  account_id)` wins. Replay must not change that choice. Missing names
  remain NULL; no invented display name. Address normalization is the
  same as exact-address search (06).
- An additive migration creates an empty ledger, preserving all existing
  address rows and sent history. A resumable engine pass scans messages
  with summaries in bounded message-id batches, and runs after import or
  summary replacement as well as startup. Batch replay is safe without
  a maximum-id watermark; late summaries of old ids must be revisited.
  At M1.6 entry the received population has no earlier writer; a future
  upgrade from a populated version must preserve its ledger with its counts.
  This pass populates completion data; composer presentation remains M1.9.

### Bounded received-address maintenance (M1.6m0)

- Population runs after import acknowledgement, not inside its recipient-sized
  transaction. Import adds only a constant-size owed-input change; population
  failure cannot retroactively fail an acknowledged archive import. One message
  may span address-key transactions without truncating recipients. A key's
  address upsert, ledger insertion and conditional received increment are atomic.
  Replayed keys still fold eligible metadata: the maximum
  `(observation_time, email, name-or-empty, account_id)` wins, comparing time
  numerically and text by UTF8 bytes/BINARY collation. A same-time repaired
  spelling/name wins only if it sorts greater, without another count.
  `last_seen_at` is that winning time; an older observation never moves the
  winner backwards.
- Capture bounded stored sources; normalize/decode outside the read transaction.
  Publish only opaque prepared chunks whose source, message/acquisition/account,
  eligibility and observation-time inputs are rechecked inside the writer.
  Superseded input is recaptured, never counted or acknowledged as newer work.
  A parser-version number alone is not a mutable-source revision fence.
- Durable `seen_address_inputs`/`seen_address_swept` revisions track owed work.
  Relevant committed message/effective-summary/observation-input changes advance
  inputs. When work is owed, a pass captures revision R and scans all logical
  message ids from the beginning with local message/key cursors; no per-input
  row revision filter is implied. It acknowledges only R after exhaustion.
  Later changes to old ids remain owed; restart replays committed chunks safely.
  Ledger writes, diagnostics and acknowledgements do not create new inputs.
  Startup checks owed work and supported target changes; no durable
  maximum-message-id shortcut.
- Newer full summaries are immutable acquisition-scoped repair versions, not
  in-place replacements of the first-wins base. A derived per-message binding
  selects its active version; absent binding means base. One effective-summary
  relation serves runtime list projection, reader metadata, exact predicates,
  threading and population. Preserve each consumer's header/source precedence;
  a repaired NULL is known absence, never a reason to fall back to a base value.
  Do not copy recipient JSON per logical message or add scroll-time repair work.
- Verified retained bytes are parsed outside SQLite with the current MIME
  limits. Stage only complete successful repair versions. Partial/failed parse
  attempts and oversized objects preserve base display, bind nothing and remain
  ineligible, with a terminal source/target diagnosis rather than a busy retry.
  Do not add an oversized-prefix parse that could invent a complete outcome.
  Infrastructure read failures retain the base and are typed, observable and
  retryable. Never overwrite a newer source/parser result or downgrade a target.
- Activate a bounded set of logical messages with their bindings AND all their
  affected list rows in the same transaction. Different messages sharing bytes
  may transition independently; each message stays internally current. A single
  message whose own occurrence/text fan-out exceeds the budget is wholly
  deferred, without a partial binding/list transition; other sharing messages
  continue. This is observable incomplete maintenance state, never successful
  repair. Reconsider that message/candidate only when relevant inputs change.
- Activation advances Search generation once for changed query sources, using
  the existing shared advance when list rows change. Advance list generation
  only for its existing inputs; mark threading owed when its effective source
  or representative date changes. Imports keep first-wins behavior and price
  the summary actually displayed. Historical migration 3 must retain its base
  fill path, never reference an effective relation introduced by a later migration.
- Keep both address tables and sent/history counts through Search/thread rebuild,
  upgrade and replay. Never reconstruct received totals by clearing history or
  replacing totals with the ledger's row count. Additive migration 7 owns ledger
  and sweep state; later additive migrations own repair versions and bindings.
  Executable schema blocks land with those migrations. Older writers refuse a
  newer schema; rollback uses a complete consistent backup, not a schema-number
  downgrade or removal of kept ledger data.

### Current-summary facade (M1.6m1)

Migration 7 seeds `seen_address_target=1`, `seen_address_inputs=1` and
`seen_address_swept=0`; existing eligible messages owe a pass. Target 1 is the
received-publication/key-policy capability, distinct from MIME parser version.
The observation index bounds earliest-occurrence lookup even for many labels.
The facade reads owned `(target, inputs, swept)` revisions and begins an opaque
captured-revision sweep only when work is owed. Newer unsupported targets are
Unavailable at begin/publication; missing, malformed or zero target is Corrupt.

Prepare consumes a sweep, captures one bounded logical message (including
ineligible ones) or EOF, then decodes/normalizes outside SQLite. A large
message's deduplicated keys remain bounded in its opaque continuation across
chunks. Publish returns Continue, quiet no-effect Recapture or Complete only
after successful COMMIT; a failed commit exposes no advanced continuation.
EOF acknowledges only captured R. Prepared private fields and commit-time
exact source/identity/time/capability checks prevent forged or stale observations.

Import records owed work in O(1), without recipient writes or a new metadata
budget failure. The native facade caller qualifies library delivery; automatic
startup/import scheduling remains M1.6m2. Preserve sent/received history and
derive the single legal-key charge from admitted header/decoded text and actual
DDL copies; no admission filter or silent key truncation.

### Verified repair artifacts (M1.6m3a1 through m3c)

The MIME-repair target is durable and distinct from the received key-policy
target. Its migration seeds the parser target supported when introduced;
later builds reconcile it monotonically before repair preparation. Opening
alone does not raise an existing target. Recheck it before every part write,
final publication and activation. A newer unsupported target
is typed Unavailable, with work left owed and imports/reads/Search unaffected.
A failed future-target attempt still prevents an older build from lowering
that intent, even when no completed version exists for the future target.

Completed versions are immutable per `(acquisition, parser target)`, checked
against the acquisition's hash/length. Attempts and diagnoses also fence the
exact captured source; parser number/state alone is insufficient. Same-key
differing completed content is an invariant failure, never an overwrite.

Only after EOF verification and a complete successful parse may bounded,
immutable field pieces be staged. Preserve the full mapped summary, canonical
JSON bytes and NULL/empty/nonempty distinctions without double escaping.
Incomplete assemblies are private: a small final publication record makes a
version visible only after field presence, ordered pieces and byte/count
totals match. Finalization must not copy the full assembled text into a new
row. Interruption/reopen can replay matching pieces, never publish omissions
or overwrite differing pieces. Large admitted output takes more commits,
not a new size refusal; oversized objects remain a separate verified outcome.

m3a1 owns version/piece/publication storage and target metadata; m3a2 adds
persistent attempts/diagnoses. m3a1 returns a typed terminal attempt, including
resolved acquisition/hash/length/availability, captured target and kind, for its
actual caller. m3a2 re-fences that opaque handoff inside its persist transaction;
it never trusts an acquisition id alone or persists raw source/reason text.
m3b owns bindings, effective relation and activation/deferral state. Each lands
its additive schema and executable spec block with its complete implementation.
Inactive version publication alone changes neither display nor query-source
revisions. Existing verified-availability observations retain their own fences.

Migration 8 introduces the inactive publication assembly; its executable DDL
matches the migration through the schema-drift test. Only completed assemblies
are versions. The manifest describes all twelve nullable text-field lengths;
parser/state derive from target/completion, with date/attachments typed below.

```sql
CREATE TABLE d_repair_assemblies (
  acquisition_id INTEGER NOT NULL REFERENCES acquisitions(acquisition_id),
  parser_target INTEGER NOT NULL CHECK (parser_target BETWEEN 1 AND 4294967295),
  source_sha256 TEXT NOT NULL CHECK (length(source_sha256) = 64),
  source_bytes INTEGER NOT NULL CHECK (source_bytes >= 0),
  date_header INTEGER,
  has_attachments INTEGER NOT NULL CHECK (has_attachments IN (0,1)),
  field_lengths TEXT NOT NULL CHECK (length(field_lengths) <= 256),
  completed_at INTEGER,
  PRIMARY KEY (acquisition_id, parser_target)
) STRICT, WITHOUT ROWID;
CREATE TABLE d_repair_pieces (
  acquisition_id INTEGER NOT NULL,
  parser_target INTEGER NOT NULL,
  field_index INTEGER NOT NULL CHECK (field_index BETWEEN 0 AND 11),
  byte_offset INTEGER NOT NULL CHECK (byte_offset >= 0),
  text TEXT NOT NULL CHECK (length(CAST(text AS BLOB)) BETWEEN 1 AND 65536),
  PRIMARY KEY (acquisition_id, parser_target, field_index, byte_offset),
  FOREIGN KEY (acquisition_id, parser_target)
    REFERENCES d_repair_assemblies(acquisition_id, parser_target)
) STRICT, WITHOUT ROWID;
```

m3a2 suppresses an identical terminal attempt both at begin/prepare eligibility
and at persistence: no repeated parse or diagnosis rewrite. Same immutable
`(acquisition, target)` with differing raw hash/length or terminal kind is
Corrupt, not permission to replace evidence; a new timestamp alone is not a new
source fence. Changed target/new source identity permits a new attempt. Native
callers qualify both operations separately; no automatic repair caller ships
between them, and both publication and persistent attempts finish before m3b/m4.

Migration 9 adds one immutable terminal record per acquisition/target. Its kind
comes only from the actual verified terminal handoff. The record has no raw
source, diagnostic reason or path, and adds no metadata key or payload index.

```sql
CREATE TABLE d_repair_diagnoses (
  acquisition_id INTEGER NOT NULL REFERENCES acquisitions(acquisition_id),
  parser_target INTEGER NOT NULL CHECK (parser_target BETWEEN 1 AND 4294967295),
  source_sha256 TEXT NOT NULL CHECK (length(source_sha256) = 64),
  source_bytes INTEGER NOT NULL CHECK (source_bytes >= 0),
  kind TEXT NOT NULL CHECK (kind IN ('partial','failed','oversized')),
  diagnosed_at INTEGER NOT NULL,
  PRIMARY KEY (acquisition_id, parser_target)
) STRICT, WITHOUT ROWID;
```

Begin and preparation eligibility compare the stored hash/length at the current
key; no candidate kind exists before parsing. Validate that fence even for a
recorded unavailable source, but suppress only when availability is Acquired.
Missing/quarantined acquisitions retain their existing typed zero-open failures.
Preparation first checks the existing source/target gate, then completed version,
then diagnosis. Keep this eligibility logic separate from the publisher's gate:
publication must still compare completed immutable content with its prepared plan.

Persist rechecks the original target/source/availability/base gate, then completed
version, then the full stored hash/length/kind tuple in one writer transaction.
Current/newer base or completed version supersedes a terminal without deleting
evidence. Identical tuple returns Current without DML; absent row inserts once
and returns Diagnosed only after COMMIT. Timestamp is not identity. Stale source
availability/deletion or lowered target returns Recapture; unsupported target and
corrupt identity retain typed errors. A failed consumed attempt is retried with
a fresh real terminal, never a fabricated or reused token.

Status reports the fenced stored diagnosis kind/time at the current metadata
target, even if the build does not support that target. A completed version and
retained diagnosis may coexist in explicit status; completion wins eligibility
and persistence. Unchanged diagnoses rewrite neither their timestamp nor input/
generation counters. Begin may still reconcile the existing repair-target key.

Before activation, m3b0 consolidates the current BASE-source selection used by
list projection, reader, query predicates, threading and received population.
It has the existing summary writer and import/read callers, no binding table,
repaired branch or candidate mode. Preserve each current consumer's distinct
header precedence, known-empty values and row-absence behavior. Historical
migration 3 keeps its frozen base fill source. Shared SQL fragments need no
new schema object; point lookups must not materialize the whole corpus.

Import accounting uses the deduplicated set of required displayed-row changes,
not incoming summary field lengths multiplied by sharing count. A selected
sender is its display name or address, not both. Count retained/replacement
text, covering-index copies and ordinary representative-row rewrites before
their DML. The conservative packet estimate remains a batch-builder input;
the writer separates metadata from that list estimate and charges actual
projection changes once. Preserve first-wins summaries, progress atomicity and
all kept history. This current BASE proof does not qualify repaired sources.

Migration 10 enforces completed-publication immutability for the current
Resources publisher/read/status callers. Completion means non-NULL, including
timestamp zero. Freeze every assembly and piece value; reject replacement into
a completed destination even when the incoming marker is NULL. Use OLD and NEW
owners for piece updates. Identical-value UPDATEs remain legal DML; matching
publisher replay still returns Current before any write. Incomplete-to-complete
may change only the marker. Incomplete staging retains its existing reactive
validation: these guards freeze completed facts, not attest arbitrary raw-SQL
fabrication while incomplete. The verified publisher still compares full output.
Diagnoses and acquisition availability retain their existing behavior.

```sql
CREATE TRIGGER d_repair_assemblies_immutable_insert
BEFORE INSERT ON d_repair_assemblies
WHEN NEW.completed_at IS NOT NULL OR EXISTS (
  SELECT 1 FROM d_repair_assemblies
  WHERE acquisition_id=NEW.acquisition_id AND parser_target=NEW.parser_target
    AND completed_at IS NOT NULL
)
BEGIN
  SELECT RAISE(ABORT,'completed MIME repair assembly is immutable');
END;
CREATE TRIGGER d_repair_assemblies_immutable_update
BEFORE UPDATE ON d_repair_assemblies
WHEN (OLD.completed_at IS NOT NULL AND
  (OLD.acquisition_id,OLD.parser_target,OLD.source_sha256,OLD.source_bytes,
   OLD.date_header,OLD.has_attachments,OLD.field_lengths,OLD.completed_at) IS NOT
  (NEW.acquisition_id,NEW.parser_target,NEW.source_sha256,NEW.source_bytes,
   NEW.date_header,NEW.has_attachments,NEW.field_lengths,NEW.completed_at))
  OR ((OLD.acquisition_id,OLD.parser_target) IS NOT
      (NEW.acquisition_id,NEW.parser_target) AND EXISTS (
    SELECT 1 FROM d_repair_assemblies
    WHERE acquisition_id=NEW.acquisition_id AND parser_target=NEW.parser_target
      AND completed_at IS NOT NULL
  ))
  OR (OLD.completed_at IS NULL AND NEW.completed_at IS NOT NULL AND
    (OLD.acquisition_id,OLD.parser_target,OLD.source_sha256,OLD.source_bytes,
     OLD.date_header,OLD.has_attachments,OLD.field_lengths) IS NOT
    (NEW.acquisition_id,NEW.parser_target,NEW.source_sha256,NEW.source_bytes,
     NEW.date_header,NEW.has_attachments,NEW.field_lengths))
BEGIN
  SELECT RAISE(ABORT,'completed MIME repair assembly is immutable');
END;
CREATE TRIGGER d_repair_assemblies_immutable_delete
BEFORE DELETE ON d_repair_assemblies WHEN OLD.completed_at IS NOT NULL
BEGIN
  SELECT RAISE(ABORT,'completed MIME repair assembly is immutable');
END;
CREATE TRIGGER d_repair_pieces_immutable_insert
BEFORE INSERT ON d_repair_pieces
WHEN EXISTS (
  SELECT 1 FROM d_repair_assemblies
  WHERE acquisition_id=NEW.acquisition_id AND parser_target=NEW.parser_target
    AND completed_at IS NOT NULL
)
BEGIN
  SELECT RAISE(ABORT,'completed MIME repair pieces are immutable');
END;
CREATE TRIGGER d_repair_pieces_immutable_update
BEFORE UPDATE ON d_repair_pieces
WHEN (OLD.acquisition_id,OLD.parser_target,OLD.field_index,OLD.byte_offset,OLD.text)
  IS NOT (NEW.acquisition_id,NEW.parser_target,NEW.field_index,NEW.byte_offset,NEW.text)
  AND (EXISTS (
    SELECT 1 FROM d_repair_assemblies
    WHERE acquisition_id=OLD.acquisition_id AND parser_target=OLD.parser_target
      AND completed_at IS NOT NULL
  ) OR EXISTS (
    SELECT 1 FROM d_repair_assemblies
    WHERE acquisition_id=NEW.acquisition_id AND parser_target=NEW.parser_target
      AND completed_at IS NOT NULL
  ))
BEGIN
  SELECT RAISE(ABORT,'completed MIME repair pieces are immutable');
END;
CREATE TRIGGER d_repair_pieces_immutable_delete
BEFORE DELETE ON d_repair_pieces
WHEN EXISTS (
  SELECT 1 FROM d_repair_assemblies
  WHERE acquisition_id=OLD.acquisition_id AND parser_target=OLD.parser_target
    AND completed_at IS NOT NULL
)
BEGIN
  SELECT RAISE(ABORT,'completed MIME repair pieces are immutable');
END;
```

The effective relation selects one whole source per logical message. A repaired
NULL never falls back to base; no row exists when neither base nor active
version exists. Preserve each consumer's existing precedence, including reader
headers fallback only when a summary row is absent. Sharing an acquisition
does not imply sharing a binding. Imports stay first-wins and price the actual
per-message displayed source, including mixed bound/unbound sharing messages.

m3b supplies real single-message atomic activation with all five consumers and
all per-message proof. A budget deferral identifies message/acquisition, current
binding, candidate/target and relevant projection dependencies, including
representative effects. Before m3b code, pin bounded inspection and a typed,
length-framed overbudget witness with cutoff/count: unchanged witnessed required
mutations alone must still prove non-fit. Changes capable of removing that
proof permit reconsideration; no global-generation/count-sum shortcut or tail
blind spot. An unchanged diagnosis does not rewrite itself or advance inputs.

Migration 11 adds the following kept binding and bounded derived evidence. Guards
reject replacement, rekey, clear and downgrade escapes; same-value UPDATEs remain
legal. Completed source identity must match the current acquisition, even when
headers mask every repaired field. Absence selects BASE; invalidity is Corrupt.
Flat binding/assembly/acquisition PK joins and correlated piece-PK expressions
select all twelve fields from one source, preserving repaired NULL and empty.

```sql
CREATE TABLE d_message_bindings (
  message_id INTEGER PRIMARY KEY REFERENCES logical_messages(message_id),
  acquisition_id INTEGER NOT NULL REFERENCES acquisitions(acquisition_id),
  parser_target INTEGER NOT NULL CHECK (parser_target BETWEEN 1 AND 4294967295),
  bound_at INTEGER NOT NULL,
  FOREIGN KEY (acquisition_id,parser_target)
    REFERENCES d_repair_assemblies(acquisition_id,parser_target)
) STRICT;
CREATE TABLE d_activation_deferrals (
  message_id INTEGER PRIMARY KEY REFERENCES logical_messages(message_id),
  acquisition_id INTEGER NOT NULL REFERENCES acquisitions(acquisition_id),
  parser_target INTEGER NOT NULL CHECK (parser_target BETWEEN 1 AND 4294967295),
  prior_target INTEGER CHECK (prior_target BETWEEN 1 AND 4294967295),
  prior_bound_at INTEGER,
  witness BLOB NOT NULL CHECK (length(witness) BETWEEN 16 AND 65536),
  recorded_cost INTEGER NOT NULL CHECK (recorded_cost>2000000),
  recorded_at INTEGER NOT NULL,
  CHECK ((prior_target IS NULL)=(prior_bound_at IS NULL)),
  FOREIGN KEY (acquisition_id,parser_target)
    REFERENCES d_repair_assemblies(acquisition_id,parser_target)
) STRICT;
CREATE TRIGGER d_message_bindings_insert BEFORE INSERT ON d_message_bindings
WHEN NOT EXISTS (
  SELECT 1 FROM logical_messages lm JOIN acquisitions a ON a.acquisition_id=lm.acquisition_id
  JOIN d_repair_assemblies v ON v.acquisition_id=lm.acquisition_id AND v.parser_target=NEW.parser_target
  WHERE lm.message_id=NEW.message_id AND lm.acquisition_id=NEW.acquisition_id
    AND v.completed_at IS NOT NULL AND v.source_sha256=a.content_sha256 AND v.source_bytes=a.byte_length
) OR EXISTS (
  SELECT 1 FROM d_message_bindings WHERE message_id=NEW.message_id
    AND (parser_target>=NEW.parser_target OR acquisition_id IS NOT NEW.acquisition_id)
)
BEGIN SELECT RAISE(ABORT,'invalid MIME activation binding'); END;
CREATE TRIGGER d_message_bindings_update BEFORE UPDATE ON d_message_bindings
WHEN (OLD.message_id,OLD.acquisition_id) IS NOT (NEW.message_id,NEW.acquisition_id)
  OR NEW.parser_target<OLD.parser_target
  OR (NEW.parser_target=OLD.parser_target AND NEW.bound_at IS NOT OLD.bound_at)
  OR NOT EXISTS (
    SELECT 1 FROM logical_messages lm JOIN acquisitions a ON a.acquisition_id=lm.acquisition_id
    JOIN d_repair_assemblies v ON v.acquisition_id=lm.acquisition_id AND v.parser_target=NEW.parser_target
    WHERE lm.message_id=NEW.message_id AND lm.acquisition_id=NEW.acquisition_id
      AND v.completed_at IS NOT NULL AND v.source_sha256=a.content_sha256 AND v.source_bytes=a.byte_length
  )
BEGIN SELECT RAISE(ABORT,'invalid MIME activation binding'); END;
CREATE TRIGGER d_message_bindings_delete BEFORE DELETE ON d_message_bindings
BEGIN SELECT RAISE(ABORT,'MIME activation binding cannot be cleared'); END;
CREATE TRIGGER logical_messages_bound_insert BEFORE INSERT ON logical_messages
WHEN EXISTS (SELECT 1 FROM d_message_bindings
  WHERE message_id=NEW.message_id AND acquisition_id IS NOT NEW.acquisition_id)
BEGIN SELECT RAISE(ABORT,'bound MIME acquisition cannot change'); END;
CREATE TRIGGER logical_messages_bound_update BEFORE UPDATE ON logical_messages
WHEN (OLD.message_id,OLD.acquisition_id) IS NOT (NEW.message_id,NEW.acquisition_id)
  AND EXISTS (SELECT 1 FROM d_message_bindings WHERE message_id=OLD.message_id)
BEGIN SELECT RAISE(ABORT,'bound MIME acquisition cannot change'); END;
```

Witness bytes are little-endian version:u32, count:u32, cutoff:i64, followed by
strictly ascending positive i64 IDs not exceeding cutoff, with exact length and
checked arithmetic. Retain only required mutations; capacity derives from their
minimum actual charge. Fresh capture always decodes and live-reprices evidence;
insufficient, resolved, removed or reassigned evidence restarts bounded inspection.
Keep one record/message, excluded from input observers. Identical live decisions
do not rewrite time; status filters acquisition/target/prior-binding ownership and
labels recorded cost as historical. Activation retains obsolete evidence, never
claims live non-fit from its recorded cost or population's swept revision.

m3c adds bounded corpus discovery/batching and its own durable input/swept
revisions. Publication and relevant message/source/target/projection changes record
owed discovery. Capture R, use local cursors, acknowledge only R at exhaustion
and revisit changed old ids on the next pass. Do not borrow population's swept
acknowledgement or dirty discovery from its own activation/diagnosis/ack writes.
Exhausted captured work is distinct from unresolved deferrals or failed repairs.
Migration 12 adds independent checked decimal counters in `store_meta`:

```sql
INSERT INTO store_meta(key,value) VALUES
  ('repair_inputs','1'),('repair_swept','0'),
  ('repair_deferred','0'),('repair_unready','0');
```

Inputs advance once per successful ordinary transaction with actual relevant
changes, atomically before COMMIT. TEMP triggers cover messages, occurrences,
headers, BASE, acquisitions, cached rows, bindings, mailbox role/kind, root keys
and completed publication, without acquisition fanout. Incomplete staging,
diagnoses, own activation/evidence and acknowledgements never dirty this clock.
Use an infallible atomic dirty flag reset for each writer job; row-value guards
exclude no-ops. A failed job contributes no revision. A malformed/exhausted
counter rejects the relevant transaction rather than acknowledging uncounted work.
Value-changing events later reversed within a successful savepoint transaction
may conservatively schedule another pass; whole-job rollback records no input.

Each pass captures R and a maximum message ID; later IDs and changed old IDs
remain owed for a fresh pass. At exhaustion acknowledge only R, monotonically,
with that pass's live Deferred and Unready totals, never raw evidence-table
counts. Newer input makes these recorded totals historical. Caught-up is
`inputs == swept`; complete also requires zero deferred/unready and a supported
target. An unready message has an acquisition needing repair or unavailable
source; header-only/current messages contribute neither count. Corruption fails
loudly without acknowledgement. Diagnosis does not change unready eligibility.
All three libraries have supported Resources operations and real native callers;
all original m3 behavior and proof finish before m4 worker integration.

## Drafts

```sql
CREATE TABLE drafts (                             -- the route (account, identity) lives only on revisions
  draft_id          TEXT PRIMARY KEY,
  state             TEXT NOT NULL CHECK (state IN ('editing','queued','sent','discarded')),
  reply_to_occurrence_id INTEGER REFERENCES occurrences(occurrence_id),
  forward_of_message_id  INTEGER REFERENCES logical_messages(message_id),
  current_revision_id    TEXT,                     -- latest revision; set in the same transaction as the insert
  created_at        INTEGER NOT NULL,
  updated_at        INTEGER NOT NULL,
  FOREIGN KEY (current_revision_id, draft_id) REFERENCES draft_revisions(revision_id, draft_id)
    DEFERRABLE INITIALLY DEFERRED
) STRICT;

CREATE TABLE draft_revisions (                    -- immutable once written
  revision_id     TEXT PRIMARY KEY,
  draft_id        TEXT NOT NULL REFERENCES drafts(draft_id),
  seq             INTEGER NOT NULL,
  base_revision_id TEXT,                           -- for conflict branches; must be in the same draft
  account_id      TEXT,                           -- route snapshot: the identity's account at this revision
  identity_id     TEXT,                           -- NULL until a sending identity is chosen
  compose_format  TEXT NOT NULL CHECK (compose_format IN ('plain','markdown')),
  plain_flowed    INTEGER NOT NULL DEFAULT 1 CHECK (plain_flowed IN (0,1)), -- 0 = literal plain text (05)
  subject         TEXT NOT NULL,
  to_json         TEXT NOT NULL,
  cc_json         TEXT NOT NULL,
  bcc_json        TEXT NOT NULL,
  body_source     TEXT NOT NULL,
  attachments_json TEXT NOT NULL,                  -- list of {object_sha256, filename, media_type, size}
  created_at      INTEGER NOT NULL,
  UNIQUE (draft_id, seq),
  UNIQUE (revision_id, draft_id),
  UNIQUE (revision_id, draft_id, account_id, identity_id),
  CHECK ((account_id IS NULL) = (identity_id IS NULL)),
  FOREIGN KEY (base_revision_id, draft_id) REFERENCES draft_revisions(revision_id, draft_id),
  FOREIGN KEY (identity_id, account_id) REFERENCES identities(identity_id, account_id)
) STRICT;
```

- **Saving** a draft inserts a revision, updates
  `drafts.current_revision_id` and sets the draft `editing`, in one
  transaction (the foreign key is deferred to commit). `queued` thus
  always means the current revision has a live attempt.
- **Drafts with live attempts.** A draft may be saved again while an
  attempt is `held` or `queued`. An attempt changes its draft's state
  (`sent`, `editing`, `queued`) only if the draft's
  `current_revision_id` still equals the attempt's `revision_id`.
  Otherwise the draft keeps its state, whether its newer revision is
  `editing` or itself `queued`, and nothing newer is lost or marked
  sent. "Saved" is shown only
  after that commit.
- **Discarding** is refused (`Conflict`) while the draft has a live
  attempt: `held`, `queued`, `submitting`, or `outcome_unknown` not yet
  resolved (05).
- **A draft's route** is its current revision's `account_id` and
  `identity_id`. `drafts` keeps no copy, so the two can't disagree.
- **Drafts without an identity** (the current revision's route NULL)
  are allowed, so local drafts work before any sending account
  exists. Sending requires a valid identity (05).
- Attachments are published to the object store **before** the revision
  that references them.
- Old revisions are kept until the draft is sent or discarded, and then
  for a retention period (default 30 days). Revisions referenced by a
  send attempt are kept as long as the attempt; attempts and their
  recipients are the send history and are kept.

## Intents (remote effects)

```sql
CREATE TABLE intents (
  intent_id          TEXT PRIMARY KEY,
  account_id         TEXT NOT NULL REFERENCES accounts(account_id),
  config_generation  INTEGER NOT NULL,             -- account generation it was planned against
  op                 TEXT NOT NULL CHECK (op IN ('set_flags','move','copy','delete_to_trash','expunge_scoped','append','submit')), -- 'expunge_scoped', 'append', 'submit': reserved, not produced in the pilot (sends use send_attempts, 05)
  target_json        TEXT NOT NULL,                -- scoped target: occurrence ids with (mailbox, uidvalidity, uid); every *_json payload here carries "v": 1
  args_json          TEXT NOT NULL,                -- e.g. {"add":["\\Seen"],"remove":[]} or destination mailbox id
  preconditions_json TEXT NOT NULL,                -- what must still be true to act (04)
  depends_on         TEXT REFERENCES intents(intent_id),
  state              TEXT NOT NULL CHECK (state IN ('pending','in_flight','done','parked','quarantined','cancelled')),
  attempts           INTEGER NOT NULL DEFAULT 0,
  next_attempt_at    INTEGER,
  wait_reason        TEXT CHECK (wait_reason IN ('auth','throttled','offline','dependency','breaker','requalify')),
  quarantine_reason  TEXT CHECK (quarantine_reason IN ('epoch_change','restore','ambiguous_match','config_change','breaker')),
  last_error         TEXT,
  evidence_json      TEXT,                         -- server evidence recorded as it arrives, e.g. a COPYUID mapping (04)
  idempotency_key    TEXT NOT NULL UNIQUE,
  origin             TEXT NOT NULL CHECK (origin IN ('user','undo','import','system')),
  created_at         INTEGER NOT NULL,
  updated_at         INTEGER NOT NULL
) STRICT;
CREATE INDEX intents_runnable ON intents(account_id, state, next_attempt_at);
```

- An intent row is written **in the same transaction** as the local
  effect: the occurrence's flags flip, or the message shows in its new
  mailbox.
- **States:**
  - `parked`: the intent failed N times or hit a terminal error; it shows
    on the health surface.
  - `quarantined`: the user must decide. `quarantine_reason` records why:
    an epoch change, a restore, an ambiguous match, a configuration change
    (the account's `config_generation` moved past the intent's), or a
    breaker event. It is set exactly when `state='quarantined'`, which the
    engine enforces.
  - `wait_reason` separates waiting from failing.
- The execution and reconciliation rules are in
  [04-sync-imap](04-sync-imap.md).

## Sending

```sql
CREATE TABLE send_attempts (
  attempt_id        TEXT PRIMARY KEY,
  draft_id          TEXT NOT NULL REFERENCES drafts(draft_id),
  revision_id       TEXT NOT NULL,
  account_id        TEXT NOT NULL REFERENCES accounts(account_id),
  config_generation INTEGER NOT NULL,              -- account configuration this attempt was frozen under
  identity_id       TEXT NOT NULL,
  mime_sha256       TEXT NOT NULL,                 -- frozen outgoing bytes in the object store
  message_id_header TEXT NOT NULL,
  envelope_from     TEXT NOT NULL,
  state             TEXT NOT NULL CHECK (state IN ('held','queued','submitting','accepted','failed','outcome_unknown','cancelled')),
  hold_until        INTEGER,                       -- undo-send window
  final_reply       TEXT,                          -- final SMTP reply (code + text)
  sent_copy         TEXT NOT NULL CHECK (sent_copy IN ('server_saves','pending','filed','failed')), -- at insert: server_saves if the provider saves Sent, else pending
  resolution        TEXT CHECK (resolution IN ('marked_sent','send_again','kept_as_draft')), -- the user's answer to outcome_unknown (05)
  resolved_at       INTEGER,
  created_at        INTEGER NOT NULL,
  updated_at        INTEGER NOT NULL,
  CHECK (resolution IS NULL OR state = 'outcome_unknown'),
  CHECK ((resolution IS NULL) = (resolved_at IS NULL)),
  FOREIGN KEY (revision_id, draft_id, account_id, identity_id)
    REFERENCES draft_revisions(revision_id, draft_id, account_id, identity_id)
) STRICT;
-- At most one live attempt per revision; resolved unknown attempts are history.
CREATE UNIQUE INDEX send_attempts_live ON send_attempts(revision_id)
  WHERE state IN ('held','queued','submitting') OR (state = 'outcome_unknown' AND resolution IS NULL);

CREATE TABLE send_recipients (
  attempt_id   TEXT NOT NULL REFERENCES send_attempts(attempt_id),
  canonical    TEXT NOT NULL,                      -- seen_addresses form: local part as written, domain lowercased
  address      TEXT NOT NULL,                      -- first spelling, as written
  kind         TEXT NOT NULL CHECK (kind IN ('to','cc','bcc')),
  rcpt_reply   TEXT,                               -- RCPT reply code + text
  state        TEXT NOT NULL CHECK (state IN ('pending','rcpt_accepted','rejected_permanent','rejected_transient','sent','unsent','unknown')),
  PRIMARY KEY (attempt_id, canonical)
) STRICT;
```

- **One row per canonical address,** computed when the attempt is
  built. An address appearing in To and Cc is one SMTP recipient (one
  `RCPT TO`): `a@X.NET` in To and `a@x.net` in Cc are one row. `kind` records the most visible field it appeared
  in (`to` > `cc` > `bcc`); the frozen MIME keeps the headers as written.
- The state machine and its rules are in [05-send](05-send.md).

## Commands, receipts and settings

```sql
CREATE TABLE command_receipts (                   -- idempotency for core commands (07)
  idempotency_key TEXT PRIMARY KEY,
  command         TEXT NOT NULL,
  result_json     TEXT NOT NULL,
  created_at      INTEGER NOT NULL
) STRICT;

CREATE TABLE settings (
  key        TEXT PRIMARY KEY,                    -- dotted names, e.g. 'appearance.compose_cue'
  value_json TEXT NOT NULL,
  revision   INTEGER NOT NULL DEFAULT 1,
  updated_at INTEGER NOT NULL
) STRICT;
```

Receipts are kept for 30 days.

**Settings** are a registry: each key has its values and a default.
`set_setting` refuses an unknown key or value with `InvalidInput`,
stores the value's canonical JSON, and bumps `revision` and
`updated_at` (rule 9). `settings()` returns every registered key, with
the default for one never set. The pilot's registry, which later steps
extend:

| Key | Values | Default |
|---|---|---|
| `appearance.compose_cue` | `standard`, `strong` (08 Strong mode) | `standard` |

## Safety records

```sql
CREATE TABLE breaker_events (
  event_id    TEXT PRIMARY KEY,
  account_id  TEXT NOT NULL REFERENCES accounts(account_id),
  mailbox_id  INTEGER,
  kind        TEXT NOT NULL CHECK (kind IN ('bulk_disappearance','folder_emptied','epoch_change','reconfiguration')), -- 'reconfiguration': reserved, not produced in the pilot
  details_json TEXT NOT NULL,                      -- counts, inventory snapshot reference
  state       TEXT NOT NULL CHECK (state IN ('open','accepted','kept','cleared')), -- 04 (The breaker)
  created_at  INTEGER NOT NULL,
  resolved_at INTEGER,
  FOREIGN KEY (mailbox_id, account_id) REFERENCES mailboxes(mailbox_id, account_id)
) STRICT;

CREATE TABLE backup_generations (
  generation_id  TEXT PRIMARY KEY,
  started_at     INTEGER NOT NULL,
  completed_at   INTEGER,
  state          TEXT NOT NULL CHECK (state IN ('in_progress','complete','failed')),
  location       TEXT NOT NULL,                    -- path of the generation directory; rewritten by restore (03)
  manifest_sha256 TEXT,
  object_count   INTEGER,
  total_bytes    INTEGER
) STRICT;

CREATE TABLE object_pins (                        -- objects GC must keep regardless of references
  object_sha256 TEXT NOT NULL,
  pin_kind      TEXT NOT NULL CHECK (pin_kind IN ('backup_in_progress','gc_grace','reservation','send_attempt','quarantine')),
  pin_ref       TEXT NOT NULL,
  created_at    INTEGER NOT NULL,
  PRIMARY KEY (object_sha256, pin_kind, pin_ref)
) STRICT;
```

## Derived tables

```sql
CREATE TABLE d_message_summary (                  -- parsed once per acquisition, rebuildable
  acquisition_id  INTEGER PRIMARY KEY REFERENCES acquisitions(acquisition_id),
  parser_version  INTEGER NOT NULL,
  parse_state     TEXT NOT NULL CHECK (parse_state IN ('ok','partial','failed')),
  header_message_id TEXT,
  subject         TEXT,
  from_addr       TEXT,
  from_name       TEXT,
  reply_to_json   TEXT,
  to_json         TEXT,
  cc_json         TEXT,
  date_header     INTEGER,
  in_reply_to     TEXT,
  references_json TEXT,
  list_id         TEXT,
  delivered_to_json TEXT,                         -- Delivered-To and X-Original-To values, for reply identity (05)
  has_attachments INTEGER NOT NULL CHECK (has_attachments IN (0,1)),
  snippet         TEXT                            -- bounded, plain text
) STRICT;

CREATE TABLE d_threads (
  thread_id     INTEGER PRIMARY KEY AUTOINCREMENT,
  account_scope TEXT,                              -- NULL = cross-account thread
  subject_norm  TEXT,
  last_date     INTEGER
) STRICT;

CREATE TABLE d_thread_members (
  thread_id  INTEGER NOT NULL REFERENCES d_threads(thread_id),
  message_id INTEGER NOT NULL REFERENCES logical_messages(message_id),
  PRIMARY KEY (thread_id, message_id)
) STRICT;
-- Migration 4: a message belongs to at most one thread.
CREATE UNIQUE INDEX d_thread_members_by_message ON d_thread_members(message_id);

-- The list index: one row per present occurrence, maintained by the engine in the
-- same transaction as occurrence changes, so list windows never join at scroll time.
CREATE TABLE d_list_rows (
  occurrence_id INTEGER PRIMARY KEY REFERENCES occurrences(occurrence_id),
  mailbox_id    INTEGER NOT NULL,
  mailbox_role  TEXT,                              -- copied from mailboxes.role, for unified views
  account_id    TEXT NOT NULL,
  sort_date     INTEGER NOT NULL,
  message_id    INTEGER NOT NULL,
  sender        TEXT,
  subject       TEXT,
  snippet       TEXT,
  unread        INTEGER NOT NULL CHECK (unread IN (0,1)),
  flagged       INTEGER NOT NULL CHECK (flagged IN (0,1)),
  has_attachments INTEGER NOT NULL CHECK (has_attachments IN (0,1)),
  representative INTEGER NOT NULL CHECK (representative IN (0,1)) -- the message's row in per-message windows
) STRICT;
CREATE INDEX d_list_rows_window ON d_list_rows(mailbox_id, sort_date DESC, occurrence_id DESC,
  message_id, account_id, sender, subject, snippet, unread, flagged, has_attachments);
CREATE INDEX d_list_rows_unified ON d_list_rows(mailbox_role, sort_date DESC, occurrence_id DESC,
  mailbox_id, message_id, account_id, sender, subject, snippet, unread, flagged, has_attachments);
CREATE INDEX d_list_rows_flagged ON d_list_rows(sort_date DESC, occurrence_id DESC)
  WHERE flagged = 1 AND representative = 1;
-- Migration 3: at most one representative row per message, and the messages
-- sharing an acquisition, whose rows its first summary updates.
CREATE UNIQUE INDEX d_list_rows_one_representative ON d_list_rows(message_id)
  WHERE representative = 1;
CREATE INDEX d_list_rows_by_message ON d_list_rows(message_id,occurrence_id);
CREATE INDEX logical_messages_by_acquisition ON logical_messages(acquisition_id);

-- Migration 14 (M1.6r2d): exact address keys of each summary (parser_target 0)
-- and completed repair assembly, for driven address searches (06), with the
-- indexes their unknown classes and bindings are enumerated through.
CREATE TABLE d_address_keys (
  field          TEXT NOT NULL CHECK (field IN ('from','to','cc')),
  address_key    TEXT NOT NULL,
  acquisition_id INTEGER NOT NULL REFERENCES acquisitions(acquisition_id),
  parser_target  INTEGER NOT NULL CHECK (parser_target BETWEEN 0 AND 4294967295),
  domain_key     TEXT NOT NULL,
  PRIMARY KEY (field, address_key, acquisition_id, parser_target)
) STRICT, WITHOUT ROWID;
CREATE INDEX d_address_keys_domain ON d_address_keys(field, domain_key);
CREATE TABLE d_address_unidentified (
  field          TEXT NOT NULL CHECK (field IN ('from','to','cc')),
  acquisition_id INTEGER NOT NULL REFERENCES acquisitions(acquisition_id),
  parser_target  INTEGER NOT NULL CHECK (parser_target BETWEEN 0 AND 4294967295),
  PRIMARY KEY (field, acquisition_id, parser_target)
) STRICT, WITHOUT ROWID;
CREATE INDEX d_message_summary_unusable ON d_message_summary(acquisition_id)
  WHERE NOT (parse_state = 'ok' AND parser_version >= 2);
CREATE INDEX d_message_bindings_old_target ON d_message_bindings(message_id)
  WHERE parser_target < 2;
CREATE INDEX d_message_bindings_by_assembly ON d_message_bindings(acquisition_id, parser_target);
```

- **`d_list_rows`** is the covering index for keyset-paginated mailbox
  windows (research 03, 13). `sort_date` is the occurrence's
  `internal_date`, else `message_headers.date_header`, else
  `d_message_summary.date_header` (imports), else `first_seen_at`. It is derived, but kept transactionally
  current, because the list must reflect a flag change instantly.
  Unified views ("all inboxes") page over `d_list_rows_unified` by role.
  - Search walks (M1.6r3c–d, 06) add additive indexes with their steps,
    and the SQL block above gains each with its migration. Migration 15:
    `d_list_rows_by_date` on `(sort_date DESC, occurrence_id DESC)` and
    `d_list_rows_account_by_date` on
    `(account_id, sort_date DESC, occurrence_id DESC)`, both
    `WHERE representative = 1`. Migration 16:
    `d_list_rows_account_order` on `(account_id, occurrence_id DESC)
    WHERE representative = 1` and `d_list_rows_mailbox_order` on
    `(mailbox_id, occurrence_id DESC)`.
  - Rows exist for exactly the `present` occurrences.
  - `subject` and `sender` come from `message_headers` when it holds
    them, else from `d_message_summary`. `sender` is the first From
    mailbox's display name, else its address. `snippet` and
    `has_attachments` come from the summary; without one they are NULL
    and 0.
  - The transaction that changes an occurrence, its mailbox or its
    active display sources updates the affected rows. Received-address repair
    activates immutable full summary versions per logical message, atomically
    with that message's rows, as specified above; it never publishes a new source
    followed by stale-row fan-out. An import never replaces a stored base summary,
    even an older parser's. An import that
    stores the first summary of bytes already held updates the rows of
    every message sharing them. Each row an import writes counts toward
    its batch's write budget (03) at the summary it shows; a batch over
    budget fails.
  - `store_meta.list_generation` gains one in each transaction that
    inserts, changes or removes list rows or thread memberships, and
    in no other. Windows bind to it (07).
- **Address keys (M1.6r2d)** index exact address leaves (06). A summary's
  rows use `parser_target` 0; a completed repair assembly's use its target.
  Per field (From is the first mailbox only) a source has one row per
  distinct received-address key with its domain key, and one
  `d_address_unidentified` row when any entry has no address or fails the
  strict received-address grammar, or the list cannot be decoded. The
  transaction that stores a first summary or completes an assembly writes
  its keys; both sources are immutable afterwards. Migration 14 derives
  them for existing sources.
- **`d_message_summary` JSON columns** are canonical JSON arrays.
  Address lists (`reply_to_json`, `to_json`, `cc_json`) hold
  `{"address", "name"}` objects, with `null` for a missing part;
  `references_json` and `delivered_to_json` hold strings. An empty list
  is NULL. `date_header` is UTC milliseconds.
- **The representative occurrence** stands for its logical message in
  per-message windows (`Flagged`, `Search`, `View`, `Thread`; 07). It
  is the message's present occurrence in a `folder` mailbox, else in a
  `label` mailbox, else in a `virtual` one; ties go to the lowest
  `occurrence_id`. Exactly one present occurrence per message has
  `representative = 1`, maintained in the same transaction as occurrence
  changes.
- **Tags in list rows** are fetched per page in one batched query by
  `message_id`, after the window's rows are read. They aren't
  denormalized into `d_list_rows`.
- **Threads** are computed locally (JWZ plus provider hints when present).
  Manual thread merge and split, and their merge log, are *later*.
  - A thread is a set of logical messages. One graph spans every
    account, so a thread may mix accounts.
  - Each message links through its valid References ids, or, without
    any, through its first In-Reply-To id. Ids compare exactly, without case
    folding. A missing or repeated Message-ID gives the message a
    container of its own. As for list rows, `message_headers` fields
    win over the summary's, field by field.
  - A message's own references overrule earlier guesses about its
    parent: the guess is dropped first. A link that would close a cycle
    is dropped too.
  - Hostile chains stay bounded: only a message's last 64 References
    ids count, and a link whose parent is more than 256 levels deep is
    dropped.
  - Roots whose base subjects are equal share a thread, as in JWZ. A
    root standing for a missing message takes the subject of the
    earliest stored message under it (JWZ takes its first child's). The base subject follows RFC 5256: runs of
    spaces, tabs and line breaks become one space, then trailing
    `(fwd)`, leading `Re:`/`Fw:`/`Fwd:` and `[…]` blobs, and a
    `[Fwd: …]` wrapper are removed; it compares in lowercase. An empty
    base subject joins nothing. Unrelated messages with one subject
    share a thread too: the cost of JWZ's heuristic.
  - Membership is rebuilt from all logical messages after each import
    job's turn ends, and at open, whenever imports added messages
    since the last complete rebuild (`thread_inputs` above
    `threads_built`). A message stored since then has no thread yet.
  - A rebuild publishes changed memberships in transactions within the
    write budget (03); each is consistent, though a large regrouping
    shows over several. Each existing thread id goes to the new group
    holding most of its members (ties: the group with the earliest
    message); a group given several keeps the smallest, and an id no
    group is given is removed.
  - `d_threads` holds the earliest member's base subject, the one
    account of all members (NULL when they mix accounts), and the
    newest representative's `sort_date`, as of the last rebuild. A
    rebuild rewrites every thread whose stored values differ from
    these, so one interrupted mid-publication heals at the next.
  - `Thread` windows (07) join a thread's members to their
    representatives' list rows; Search also joins its derived/filter
    sources (06). Ordinary mailbox lists keep the covering-index path.
- **Search tables** are in [06-search](06-search.md).

## Migrations

- **The schema version** is `PRAGMA user_version`, starting at 1, owned by
  the migration runner in `sb-store`. After each migration the engine
  writes `store_meta.last_migrated_by`: the app version, which is the
  workspace `version` in `Cargo.toml`.
- **At open,** after the SQLite version check (03), the runner reads
  `user_version`:
  - 0 in an empty database: a new store; every migration runs;
  - 0 in a database holding any schema object: not a Sweetbriar store.
    `Corrupt`, and the file is left untouched;
  - 1 up to the latest without `store_uuid`, `incarnation` and
    `last_migrated_by` in a `store_meta` table exactly as above: not a
    Sweetbriar store. `Corrupt`, untouched;
  - below the app's latest version: the pending migrations run, in
    order;
  - above it: `NewerStore`, naming `last_migrated_by` (`unknown` if it
    can't be read). Open writes nothing.

  Nothing is written before these checks pass. They run on a
  read-only connection, since closing a read-write one checkpoints
  WAL frames a crash left behind into the file; for the same reason
  the writer doesn't checkpoint on close until open succeeds. Then
  open records `app_version_last_opened`.
- **Each migration:**
  - is one Rust function in one transaction, which also sets
    `user_version` and `last_migrated_by`;
  - is followed, inside that transaction, by `PRAGMA foreign_key_check`
    and `PRAGMA quick_check`. A finding rolls the migration back and
    yields `Corrupt`. After the whole run, a full
    `PRAGMA integrity_check` (03); a finding is `Corrupt`;
  - is forward-only.
- **Migration 1** creates the original v1 tables of this document and of 06, and
  fills `store_meta`: `store_uuid` and `incarnation` (two new UUIDs),
  `created_at`, and both version keys. A test builds a database from
  these two documents' SQL blocks and requires the migrated schema to
  match the complete migration sequence, so the spec and the migrations
  can't drift. Migration 2 adds the import schema described above.
  Migration 3 adds the list-row support indexes and `list_generation`,
  and fills `d_list_rows` for the occurrences stored before it. It
  rewrites or drops no existing data. Migration 4 adds the one-thread
  index and the thread watermarks: both 0, or `thread_inputs` at 1
  when the store already holds messages, so the first open threads
  them and an empty store owes nothing. Migration 5 adds search queue revisions
  and its oldest-first index, initializes the search watermarks, and queues
  existing acquisitions (06), without changing authoritative rows.
  Migration 6 adds the durable search index state and rebuild jobs of 06
  and 07: epoch 0, no job, no pause, and the cross-kind job-id guards,
  again without touching existing data.
- **Before any migration that rewrites or drops data,** the engine takes a
  `VACUUM INTO` snapshot, keeping the last two. This arrives with the
  first such migration; migration 1 has none.
- **A store newer than the app** opens read-only, with an explanation
  naming the app version that last migrated it (research D27). The
  writable open refuses it with `NewerStore`; the read-only open uses
  readers only (plan M0.6).
