-- Migration 28 (spec/02 "Proton accounts"): the Proton migration. It
-- rebuilds accounts, acquisition_sources and intents in place, since
-- SQLite cannot alter a CHECK; the runner has foreign_keys off for this
-- transaction and checks them afterwards (spec/02 "Migrations"). Every
-- row and id is copied, and each table's indexes are recreated exactly.
-- The old table is renamed aside with legacy_alter_table so the child
-- tables keep referencing the final name, then the new table is created
-- under that name: its stored CREATE statement stays the spec's.

PRAGMA legacy_alter_table = ON;
ALTER TABLE accounts RENAME TO accounts_old;
PRAGMA legacy_alter_table = OFF;
CREATE TABLE accounts (
  account_id        TEXT PRIMARY KEY,                 -- UUID, never changes
  kind              TEXT NOT NULL CHECK (kind IN ('local','imap','proton')),
  provider          TEXT NOT NULL CHECK (provider IN ('local_archive','proton_bridge','generic_imap','proton_api')),
  display_name      TEXT NOT NULL,
  state             TEXT NOT NULL CHECK (state IN ('active','paused','needs_attention','disconnected')),
  state_reason      TEXT,                             -- shown on the health surface
  config_generation INTEGER NOT NULL DEFAULT 1,       -- bumped when server, credentials or identity change
  sort_order        INTEGER NOT NULL,
  created_at        INTEGER NOT NULL,
  revision          INTEGER NOT NULL DEFAULT 1
) STRICT;
INSERT INTO accounts
  (account_id, kind, provider, display_name, state, state_reason,
   config_generation, sort_order, created_at, revision)
SELECT account_id, kind, provider, display_name, state, state_reason,
       config_generation, sort_order, created_at, revision FROM accounts_old;
DROP TABLE accounts_old;
CREATE UNIQUE INDEX accounts_one_local_archive ON accounts(provider)
  WHERE provider = 'local_archive';

PRAGMA legacy_alter_table = ON;
ALTER TABLE acquisition_sources RENAME TO acquisition_sources_old;
PRAGMA legacy_alter_table = OFF;
CREATE TABLE acquisition_sources (                    -- provenance, one row per time a source delivered these bytes
  source_id        INTEGER PRIMARY KEY AUTOINCREMENT,
  acquisition_id   INTEGER NOT NULL REFERENCES acquisitions(acquisition_id),
  account_id       TEXT NOT NULL REFERENCES accounts(account_id),
  source_kind      TEXT NOT NULL CHECK (source_kind IN ('imap_fetch','import_mbox','import_eml','send_attempt','proton_fetch')), -- 'send_attempt' reserved: in the pilot, sent mail arrives through Sent sync
  source_ref       TEXT NOT NULL,                     -- e.g. occurrence id; import source path + sha256 + mailbox + offset (09); attempt id
  completeness     TEXT NOT NULL CHECK (completeness IN ('complete','truncated')),
  acquired_at      INTEGER NOT NULL
) STRICT;
INSERT INTO acquisition_sources
  (source_id, acquisition_id, account_id, source_kind, source_ref, completeness, acquired_at)
SELECT source_id, acquisition_id, account_id, source_kind, source_ref, completeness, acquired_at
  FROM acquisition_sources_old;
DROP TABLE acquisition_sources_old;
CREATE UNIQUE INDEX acquisition_import_identity
  ON acquisition_sources(account_id, source_kind, source_ref)
  WHERE source_kind IN ('import_mbox','import_eml');

PRAGMA legacy_alter_table = ON;
ALTER TABLE intents RENAME TO intents_old;
PRAGMA legacy_alter_table = OFF;
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
  wait_reason        TEXT CHECK (wait_reason IN ('auth','throttled','offline','dependency','breaker','requalify','resync')),
  quarantine_reason  TEXT CHECK (quarantine_reason IN ('epoch_change','restore','ambiguous_match','config_change','breaker')),
  last_error         TEXT,
  evidence_json      TEXT,                         -- server evidence recorded as it arrives, e.g. a COPYUID mapping (04)
  idempotency_key    TEXT NOT NULL UNIQUE,
  origin             TEXT NOT NULL CHECK (origin IN ('user','undo','import','system')),
  created_at         INTEGER NOT NULL,
  updated_at         INTEGER NOT NULL
) STRICT;
INSERT INTO intents
  (intent_id, account_id, config_generation, op, target_json, args_json,
   preconditions_json, depends_on, state, attempts, next_attempt_at,
   wait_reason, quarantine_reason, last_error, evidence_json,
   idempotency_key, origin, created_at, updated_at)
SELECT intent_id, account_id, config_generation, op, target_json, args_json,
       preconditions_json, depends_on, state, attempts, next_attempt_at,
       wait_reason, quarantine_reason, last_error, evidence_json,
       idempotency_key, origin, created_at, updated_at
  FROM intents_old;
DROP TABLE intents_old;
CREATE INDEX intents_runnable ON intents(account_id, state, next_attempt_at);

ALTER TABLE mailboxes ADD COLUMN provider_key TEXT;
CREATE UNIQUE INDEX mailboxes_provider_key ON mailboxes(account_id, provider_key)
  WHERE provider_key IS NOT NULL;

CREATE TABLE proton_accounts (                        -- one per Proton API account
  account_id         TEXT PRIMARY KEY REFERENCES accounts(account_id),
  api_base_url       TEXT NOT NULL,                   -- 'https://mail.proton.me/api' in the pilot
  username           TEXT NOT NULL,                   -- Proton's canonical name after sign-in
  proton_user_id     TEXT NOT NULL UNIQUE,            -- a later sign-in must name the same user; one account per Proton user
  credential_ref     TEXT NOT NULL,                   -- secret store key: session UID, refresh token, key passphrase (10)
  password_mode      INTEGER NOT NULL CHECK (password_mode IN (1,2)), -- 2: a separate mailbox password
  key_generation     INTEGER NOT NULL DEFAULT 1,      -- bumped when key unlocking changes what it unlocked
  event_cursor       TEXT,                            -- last applied event id; NULL until the first bootstrap completes
  pending_cursor     TEXT,                            -- cursor read before the running bootstrap's listing
  listing_state      TEXT NOT NULL CHECK (listing_state IN ('none','listing','complete')),
  listing_checkpoint TEXT,                            -- last committed page's position while listing
  refresh_count      INTEGER NOT NULL DEFAULT 0,      -- Proton refreshes applied
  diagnostics_json   TEXT,                            -- last refusal code and message, redacted (10)
  revision           INTEGER NOT NULL DEFAULT 1,
  CHECK ((listing_state = 'listing') = (pending_cursor IS NOT NULL)),
  CHECK (listing_checkpoint IS NULL OR listing_state = 'listing')
) STRICT;

CREATE TABLE proton_body_failures (                   -- bodies that couldn't be acquired
  message_id     INTEGER PRIMARY KEY REFERENCES logical_messages(message_id),
  reason         TEXT NOT NULL CHECK (reason IN ('no_key','decrypt','attachment','too_large','reconstruct')),
  detail         TEXT,                                -- redacted (10)
  attempts       INTEGER NOT NULL,
  key_generation INTEGER NOT NULL,                    -- retried once the account's key_generation moves past it
  last_at        INTEGER NOT NULL
) STRICT;
