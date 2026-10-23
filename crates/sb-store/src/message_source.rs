//! Current BASE message edges shared by runtime consumers, without changing precedence.

pub(crate) const MESSAGES: &str = "logical_messages lm";
pub(crate) const SUMMARY: &str = "d_message_summary s ON s.acquisition_id = lm.acquisition_id";
pub(crate) const HEADERS: &str = "message_headers h ON h.message_id = lm.message_id";
