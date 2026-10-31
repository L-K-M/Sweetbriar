-- Immutable export capture; no output file or manifest is implied by ready.
CREATE TABLE export_jobs (
  job_id TEXT PRIMARY KEY CHECK (length(CAST(job_id AS BLOB)) BETWEEN 1 AND 128),
  state TEXT NOT NULL CHECK (state IN ('capturing','ready','running','completed','cancelled','failed')),
  request_json TEXT NOT NULL CHECK (
    length(CAST(request_json AS BLOB)) <= 4096 AND json_valid(request_json)
    AND json_type(request_json) = 'object' AND json_type(request_json, '$.v') IS 'integer'
    AND json_extract(request_json, '$.v') IS 1),
  checkpoint_json TEXT NOT NULL CHECK (
    length(CAST(checkpoint_json AS BLOB)) <= 4096 AND json_valid(checkpoint_json)
    AND json_type(checkpoint_json) = 'object' AND json_type(checkpoint_json, '$.v') IS 'integer'
    AND json_extract(checkpoint_json, '$.v') IS 1),
  revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
  captured_chunks INTEGER NOT NULL DEFAULT 0 CHECK (captured_chunks >= 0),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
) STRICT;

CREATE TABLE export_job_chunks (
  job_id TEXT NOT NULL REFERENCES export_jobs(job_id),
  ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
  payload BLOB NOT NULL CHECK (length(payload) BETWEEN 1 AND 65536),
  PRIMARY KEY (job_id, ordinal)
) STRICT;

CREATE TRIGGER export_jobs_foreign_id BEFORE INSERT ON export_jobs
  WHEN EXISTS(SELECT 1 FROM import_jobs WHERE job_id = NEW.job_id)
    OR EXISTS(SELECT 1 FROM search_rebuild_jobs WHERE job_id = NEW.job_id)
BEGIN SELECT RAISE(ABORT, 'job id names another kind'); END;
CREATE TRIGGER import_jobs_export_id BEFORE INSERT ON import_jobs
  WHEN EXISTS(SELECT 1 FROM export_jobs WHERE job_id = NEW.job_id)
BEGIN SELECT RAISE(ABORT, 'job id names an export'); END;
CREATE TRIGGER search_rebuild_jobs_export_id BEFORE INSERT ON search_rebuild_jobs
  WHEN EXISTS(SELECT 1 FROM export_jobs WHERE job_id = NEW.job_id)
BEGIN SELECT RAISE(ABORT, 'job id names an export'); END;
CREATE TRIGGER import_jobs_export_id_update BEFORE UPDATE OF job_id ON import_jobs
  WHEN EXISTS(SELECT 1 FROM export_jobs WHERE job_id = NEW.job_id)
BEGIN SELECT RAISE(ABORT, 'job id names an export'); END;
CREATE TRIGGER search_rebuild_jobs_export_id_update BEFORE UPDATE OF job_id ON search_rebuild_jobs
  WHEN EXISTS(SELECT 1 FROM export_jobs WHERE job_id = NEW.job_id)
BEGIN SELECT RAISE(ABORT, 'job id names an export'); END;

CREATE TRIGGER export_jobs_identity BEFORE UPDATE OF job_id, request_json, created_at ON export_jobs
  WHEN NEW.job_id <> OLD.job_id OR NEW.request_json <> OLD.request_json
    OR NEW.created_at <> OLD.created_at
BEGIN SELECT RAISE(ABORT, 'export identity is immutable'); END;
CREATE TRIGGER export_jobs_terminal BEFORE UPDATE ON export_jobs
  WHEN OLD.state IN ('completed','cancelled','failed')
BEGIN SELECT RAISE(ABORT, 'export job has finished'); END;
CREATE TRIGGER export_jobs_kept BEFORE DELETE ON export_jobs
BEGIN SELECT RAISE(ABORT, 'export history is retained'); END;

-- Check the indexed tail, never count the whole capture on every append.
CREATE TRIGGER export_chunks_append BEFORE INSERT ON export_job_chunks
  WHEN (SELECT state FROM export_jobs WHERE job_id = NEW.job_id) <> 'capturing'
    OR NEW.ordinal <> (SELECT coalesce(max(ordinal) + 1, 0) FROM export_job_chunks
                       WHERE job_id = NEW.job_id)
BEGIN SELECT RAISE(ABORT, 'export chunk is sealed or noncontiguous'); END;
CREATE TRIGGER export_chunks_immutable BEFORE UPDATE ON export_job_chunks
BEGIN SELECT RAISE(ABORT, 'export payload is immutable'); END;
CREATE TRIGGER export_chunks_kept BEFORE DELETE ON export_job_chunks
BEGIN SELECT RAISE(ABORT, 'export payload is retained'); END;
