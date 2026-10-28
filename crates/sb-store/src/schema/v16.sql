CREATE INDEX d_list_rows_account_order ON d_list_rows(account_id, occurrence_id DESC)
  WHERE representative = 1;
CREATE INDEX d_list_rows_mailbox_order ON d_list_rows(mailbox_id, occurrence_id DESC);
