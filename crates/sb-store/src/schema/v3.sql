CREATE UNIQUE INDEX d_list_rows_one_representative ON d_list_rows(message_id)
  WHERE representative = 1;
CREATE INDEX logical_messages_by_acquisition ON logical_messages(acquisition_id);
