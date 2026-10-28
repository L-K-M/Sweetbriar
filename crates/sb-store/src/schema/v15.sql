CREATE INDEX d_list_rows_by_date ON d_list_rows(sort_date DESC, occurrence_id DESC)
  WHERE representative = 1;
CREATE INDEX d_list_rows_account_by_date ON d_list_rows(account_id, sort_date DESC, occurrence_id DESC)
  WHERE representative = 1;
