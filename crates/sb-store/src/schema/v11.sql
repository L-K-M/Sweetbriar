CREATE TABLE d_message_bindings (
  message_id INTEGER PRIMARY KEY REFERENCES logical_messages(message_id),
  acquisition_id INTEGER NOT NULL REFERENCES acquisitions(acquisition_id),
  parser_target INTEGER NOT NULL CHECK (parser_target BETWEEN 1 AND 4294967295),
  bound_at INTEGER NOT NULL,
  FOREIGN KEY (acquisition_id,parser_target)
    REFERENCES d_repair_assemblies(acquisition_id,parser_target)
) STRICT;
CREATE TABLE d_activation_deferrals (
  message_id INTEGER PRIMARY KEY REFERENCES logical_messages(message_id),
  acquisition_id INTEGER NOT NULL REFERENCES acquisitions(acquisition_id),
  parser_target INTEGER NOT NULL CHECK (parser_target BETWEEN 1 AND 4294967295),
  prior_target INTEGER CHECK (prior_target BETWEEN 1 AND 4294967295),
  prior_bound_at INTEGER,
  witness BLOB NOT NULL CHECK (length(witness) BETWEEN 16 AND 65536),
  recorded_cost INTEGER NOT NULL CHECK (recorded_cost>2000000),
  recorded_at INTEGER NOT NULL,
  CHECK ((prior_target IS NULL)=(prior_bound_at IS NULL)),
  FOREIGN KEY (acquisition_id,parser_target)
    REFERENCES d_repair_assemblies(acquisition_id,parser_target)
) STRICT;
CREATE INDEX d_list_rows_by_message ON d_list_rows(message_id,occurrence_id);
CREATE TRIGGER d_message_bindings_insert BEFORE INSERT ON d_message_bindings
WHEN NOT EXISTS (
  SELECT 1 FROM logical_messages lm JOIN acquisitions a ON a.acquisition_id=lm.acquisition_id
  JOIN d_repair_assemblies v ON v.acquisition_id=lm.acquisition_id AND v.parser_target=NEW.parser_target
  WHERE lm.message_id=NEW.message_id AND lm.acquisition_id=NEW.acquisition_id
    AND v.completed_at IS NOT NULL AND v.source_sha256=a.content_sha256 AND v.source_bytes=a.byte_length
) OR EXISTS (
  SELECT 1 FROM d_message_bindings WHERE message_id=NEW.message_id
    AND (parser_target>=NEW.parser_target OR acquisition_id IS NOT NEW.acquisition_id)
)
BEGIN SELECT RAISE(ABORT,'invalid MIME activation binding'); END;
CREATE TRIGGER d_message_bindings_update BEFORE UPDATE ON d_message_bindings
WHEN (OLD.message_id,OLD.acquisition_id) IS NOT (NEW.message_id,NEW.acquisition_id)
  OR NEW.parser_target<OLD.parser_target
  OR (NEW.parser_target=OLD.parser_target AND NEW.bound_at IS NOT OLD.bound_at)
  OR NOT EXISTS (
    SELECT 1 FROM logical_messages lm JOIN acquisitions a ON a.acquisition_id=lm.acquisition_id
    JOIN d_repair_assemblies v ON v.acquisition_id=lm.acquisition_id AND v.parser_target=NEW.parser_target
    WHERE lm.message_id=NEW.message_id AND lm.acquisition_id=NEW.acquisition_id
      AND v.completed_at IS NOT NULL AND v.source_sha256=a.content_sha256 AND v.source_bytes=a.byte_length
  )
BEGIN SELECT RAISE(ABORT,'invalid MIME activation binding'); END;
CREATE TRIGGER d_message_bindings_delete BEFORE DELETE ON d_message_bindings
BEGIN SELECT RAISE(ABORT,'MIME activation binding cannot be cleared'); END;
CREATE TRIGGER logical_messages_bound_insert BEFORE INSERT ON logical_messages
WHEN EXISTS (SELECT 1 FROM d_message_bindings
  WHERE message_id=NEW.message_id AND acquisition_id IS NOT NEW.acquisition_id)
BEGIN SELECT RAISE(ABORT,'bound MIME acquisition cannot change'); END;
CREATE TRIGGER logical_messages_bound_update BEFORE UPDATE ON logical_messages
WHEN (OLD.message_id,OLD.acquisition_id) IS NOT (NEW.message_id,NEW.acquisition_id)
  AND EXISTS (SELECT 1 FROM d_message_bindings WHERE message_id=OLD.message_id)
BEGIN SELECT RAISE(ABORT,'bound MIME acquisition cannot change'); END;
