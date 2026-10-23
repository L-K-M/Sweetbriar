CREATE TRIGGER d_repair_assemblies_immutable_insert
BEFORE INSERT ON d_repair_assemblies
WHEN NEW.completed_at IS NOT NULL OR EXISTS (
  SELECT 1 FROM d_repair_assemblies
  WHERE acquisition_id=NEW.acquisition_id AND parser_target=NEW.parser_target
    AND completed_at IS NOT NULL
)
BEGIN
  SELECT RAISE(ABORT,'completed MIME repair assembly is immutable');
END;
CREATE TRIGGER d_repair_assemblies_immutable_update
BEFORE UPDATE ON d_repair_assemblies
WHEN (OLD.completed_at IS NOT NULL AND
  (OLD.acquisition_id,OLD.parser_target,OLD.source_sha256,OLD.source_bytes,
   OLD.date_header,OLD.has_attachments,OLD.field_lengths,OLD.completed_at) IS NOT
  (NEW.acquisition_id,NEW.parser_target,NEW.source_sha256,NEW.source_bytes,
   NEW.date_header,NEW.has_attachments,NEW.field_lengths,NEW.completed_at))
  OR ((OLD.acquisition_id,OLD.parser_target) IS NOT
      (NEW.acquisition_id,NEW.parser_target) AND EXISTS (
    SELECT 1 FROM d_repair_assemblies
    WHERE acquisition_id=NEW.acquisition_id AND parser_target=NEW.parser_target
      AND completed_at IS NOT NULL
  ))
  OR (OLD.completed_at IS NULL AND NEW.completed_at IS NOT NULL AND
    (OLD.acquisition_id,OLD.parser_target,OLD.source_sha256,OLD.source_bytes,
     OLD.date_header,OLD.has_attachments,OLD.field_lengths) IS NOT
    (NEW.acquisition_id,NEW.parser_target,NEW.source_sha256,NEW.source_bytes,
     NEW.date_header,NEW.has_attachments,NEW.field_lengths))
BEGIN
  SELECT RAISE(ABORT,'completed MIME repair assembly is immutable');
END;
CREATE TRIGGER d_repair_assemblies_immutable_delete
BEFORE DELETE ON d_repair_assemblies WHEN OLD.completed_at IS NOT NULL
BEGIN
  SELECT RAISE(ABORT,'completed MIME repair assembly is immutable');
END;
CREATE TRIGGER d_repair_pieces_immutable_insert
BEFORE INSERT ON d_repair_pieces
WHEN EXISTS (
  SELECT 1 FROM d_repair_assemblies
  WHERE acquisition_id=NEW.acquisition_id AND parser_target=NEW.parser_target
    AND completed_at IS NOT NULL
)
BEGIN
  SELECT RAISE(ABORT,'completed MIME repair pieces are immutable');
END;
CREATE TRIGGER d_repair_pieces_immutable_update
BEFORE UPDATE ON d_repair_pieces
WHEN (OLD.acquisition_id,OLD.parser_target,OLD.field_index,OLD.byte_offset,OLD.text)
  IS NOT (NEW.acquisition_id,NEW.parser_target,NEW.field_index,NEW.byte_offset,NEW.text)
  AND (EXISTS (
    SELECT 1 FROM d_repair_assemblies
    WHERE acquisition_id=OLD.acquisition_id AND parser_target=OLD.parser_target
      AND completed_at IS NOT NULL
  ) OR EXISTS (
    SELECT 1 FROM d_repair_assemblies
    WHERE acquisition_id=NEW.acquisition_id AND parser_target=NEW.parser_target
      AND completed_at IS NOT NULL
  ))
BEGIN
  SELECT RAISE(ABORT,'completed MIME repair pieces are immutable');
END;
CREATE TRIGGER d_repair_pieces_immutable_delete
BEFORE DELETE ON d_repair_pieces
WHEN EXISTS (
  SELECT 1 FROM d_repair_assemblies
  WHERE acquisition_id=OLD.acquisition_id AND parser_target=OLD.parser_target
    AND completed_at IS NOT NULL
)
BEGIN
  SELECT RAISE(ABORT,'completed MIME repair pieces are immutable');
END;
