CREATE UNIQUE INDEX accounts_one_local_archive ON accounts(provider)
  WHERE provider = 'local_archive';

CREATE UNIQUE INDEX acquisition_import_identity
  ON acquisition_sources(account_id, source_kind, source_ref)
  WHERE source_kind IN ('import_mbox','import_eml');

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
