-- Migration 6: the durable index-worker state and rebuild jobs of 06's
-- "Queue, versions and recovery" and 07's "Search rebuild controls".
-- Pauses, epochs and job ids are kept operational records, not
-- disposable d_ tables.

CREATE TABLE search_index_state (           -- exactly one row, id = 0
  id           INTEGER PRIMARY KEY CHECK (id = 0),
  epoch        INTEGER NOT NULL CHECK (epoch >= 0),
  user_paused  INTEGER NOT NULL DEFAULT 0 CHECK (user_paused IN (0,1)),
  io_code      TEXT CHECK (io_code IS NULL OR io_code IN
                 ('storage_full','permission_denied','storage_unavailable','other_io')),
  io_message   TEXT CHECK (io_message IS NULL OR length(CAST(io_message AS BLOB)) <= 512),
  io_retry_at  INTEGER CHECK (io_retry_at IS NULL OR io_retry_at >= 0),
  io_attempts  INTEGER NOT NULL DEFAULT 0 CHECK (io_attempts >= 0),
  CHECK ((io_code IS NULL) = (io_message IS NULL)
     AND (io_code IS NULL) = (io_retry_at IS NULL))
) STRICT;
INSERT INTO search_index_state (id, epoch) VALUES (0, 0);

CREATE TABLE search_rebuild_jobs (          -- table membership is the durable job kind
  job_id      TEXT PRIMARY KEY,
  epoch       INTEGER NOT NULL UNIQUE CHECK (epoch >= 1),
  phase       TEXT NOT NULL CHECK (phase IN ('reset','enqueue','index','complete')),
  doc_cursor  INTEGER NOT NULL DEFAULT 0 CHECK (doc_cursor >= 0),
  acq_cursor  INTEGER NOT NULL DEFAULT 0 CHECK (acq_cursor >= 0),
  phase_count INTEGER NOT NULL DEFAULT 0 CHECK (phase_count >= 0)
) STRICT;

-- At most one non-complete rebuild job (07).
CREATE UNIQUE INDEX search_rebuild_jobs_one_active
  ON search_rebuild_jobs((phase <> 'complete')) WHERE phase <> 'complete';

-- The same id never names both kinds; UUID rarity is not the invariant.
CREATE TRIGGER import_jobs_search_rebuild_id BEFORE INSERT ON import_jobs
  WHEN EXISTS(SELECT 1 FROM search_rebuild_jobs WHERE job_id = NEW.job_id)
BEGIN SELECT RAISE(ABORT, 'job id already names a search rebuild job'); END;
CREATE TRIGGER search_rebuild_jobs_import_id BEFORE INSERT ON search_rebuild_jobs
  WHEN EXISTS(SELECT 1 FROM import_jobs WHERE job_id = NEW.job_id)
BEGIN SELECT RAISE(ABORT, 'job id already names an import job'); END;
CREATE TRIGGER import_jobs_search_rebuild_id_update BEFORE UPDATE OF job_id ON import_jobs
  WHEN EXISTS(SELECT 1 FROM search_rebuild_jobs WHERE job_id = NEW.job_id)
BEGIN SELECT RAISE(ABORT, 'job id already names a search rebuild job'); END;
CREATE TRIGGER search_rebuild_jobs_import_id_update BEFORE UPDATE OF job_id ON search_rebuild_jobs
  WHEN EXISTS(SELECT 1 FROM import_jobs WHERE job_id = NEW.job_id)
BEGIN SELECT RAISE(ABORT, 'job id already names an import job'); END;
