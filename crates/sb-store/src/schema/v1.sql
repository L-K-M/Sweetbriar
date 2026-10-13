-- Schema v1 (migration 1): the ```sql blocks of spec/02-data-model.md in
-- document order, then spec/06-search.md's, copied verbatim. Never edit this
-- file; later schema changes are new migrations (spec/02 "Migrations").

CREATE TABLE store_meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
) STRICT;
-- keys: 'store_uuid', 'incarnation' (new UUID after every restore),
-- 'created_at', 'app_version_last_opened', 'last_migrated_by' (app version).
-- The schema version is PRAGMA user_version, owned by the migration runner;
-- it is not duplicated here.
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
CREATE TABLE seen_addresses (                     -- derived from mail; feeds completion
  canonical      TEXT PRIMARY KEY,                -- local part as written + '@' + lowercased domain
  email          TEXT NOT NULL,                   -- the address exactly as last seen (local-part case kept, RFC 5321 §2.4)
  display_name   TEXT,
  last_seen_at   INTEGER NOT NULL,
  times_sent_to  INTEGER NOT NULL DEFAULT 0,
  times_received INTEGER NOT NULL DEFAULT 0,
  last_account_id TEXT REFERENCES accounts(account_id)
) STRICT;
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
CREATE TABLE d_search_docs (             -- one row per acquisition with extracted text
  doc_id            INTEGER PRIMARY KEY, -- = acquisitions.acquisition_id
  extractor_version INTEGER NOT NULL,
  state             TEXT NOT NULL CHECK (state IN ('indexed','partial','failed')),
  truncated_fields  TEXT NOT NULL DEFAULT '[]', -- JSON array of field names cut by a cap
  subject           TEXT,
  from_text         TEXT,
  to_text           TEXT,
  cc_text           TEXT,
  body_authored     TEXT,
  body_quoted       TEXT,
  body_signature    TEXT,
  attachment_names  TEXT
) STRICT;

CREATE VIRTUAL TABLE d_search_fts USING fts5(
  subject, from_text, to_text, cc_text, body_authored, body_quoted, body_signature, attachment_names,
  content='d_search_docs', content_rowid='doc_id',
  tokenize = "unicode61 remove_diacritics 2",
  prefix = '2 3'
);

CREATE TABLE d_index_queue (              -- work for the index job; durable watermark
  doc_id      INTEGER PRIMARY KEY,
  reason      TEXT NOT NULL CHECK (reason IN ('new','reextract','delete')),
  enqueued_at INTEGER NOT NULL
) STRICT;
