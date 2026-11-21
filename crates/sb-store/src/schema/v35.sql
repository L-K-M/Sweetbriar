-- Migration 35 (spec/02 "Intents"): the M5 filing intent. It rebuilds
-- intents in place, since SQLite cannot alter a CHECK; the runner has
-- foreign_keys off for this transaction and checks them afterwards
-- (spec/02 "Migrations"). Every row and id is copied, and the
-- runnable index is recreated exactly. The old table is renamed aside
-- with legacy_alter_table so the child tables keep referencing the
-- final name, then the new table is created under that name.

PRAGMA legacy_alter_table = ON;
ALTER TABLE intents RENAME TO intents_old;
PRAGMA legacy_alter_table = OFF;
CREATE TABLE intents (
  intent_id          TEXT PRIMARY KEY,
  account_id         TEXT NOT NULL REFERENCES accounts(account_id),
  config_generation  INTEGER NOT NULL,             -- account generation it was planned against
  op                 TEXT NOT NULL CHECK (op IN ('set_flags','move','copy','delete_to_trash','remove_label','expunge_scoped','append','submit')), -- 'expunge_scoped', 'append', 'submit': reserved, not produced in the pilot (sends use send_attempts, 05)
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
