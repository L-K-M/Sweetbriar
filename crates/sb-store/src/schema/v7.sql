CREATE TABLE seen_address_messages (
  message_id INTEGER NOT NULL REFERENCES logical_messages(message_id),
  canonical TEXT NOT NULL REFERENCES seen_addresses(canonical),
  PRIMARY KEY (message_id, canonical)
) STRICT, WITHOUT ROWID;
CREATE INDEX occ_by_message_observation ON occurrences(message_id, first_seen_at);

INSERT INTO store_meta(key, value) VALUES
  ('seen_address_target', '1'),
  ('seen_address_inputs', '1'),
  ('seen_address_swept', '0');
