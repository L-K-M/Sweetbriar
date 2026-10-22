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
INSERT INTO store_meta(key,value) VALUES('mime_repair_target','2');
