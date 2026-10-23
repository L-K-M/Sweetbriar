CREATE TABLE d_repair_diagnoses (
  acquisition_id INTEGER NOT NULL REFERENCES acquisitions(acquisition_id),
  parser_target INTEGER NOT NULL CHECK (parser_target BETWEEN 1 AND 4294967295),
  source_sha256 TEXT NOT NULL CHECK (length(source_sha256) = 64),
  source_bytes INTEGER NOT NULL CHECK (source_bytes >= 0),
  kind TEXT NOT NULL CHECK (kind IN ('partial','failed','oversized')),
  diagnosed_at INTEGER NOT NULL,
  PRIMARY KEY (acquisition_id, parser_target)
) STRICT, WITHOUT ROWID;
