ALTER TABLE d_index_queue ADD COLUMN revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1);
CREATE INDEX d_index_queue_oldest ON d_index_queue(enqueued_at, doc_id);
