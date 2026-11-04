CREATE INDEX object_pins_by_owner ON object_pins (pin_kind, pin_ref, object_sha256)
  WHERE pin_kind = 'backup_in_progress';
