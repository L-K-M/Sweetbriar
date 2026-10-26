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
