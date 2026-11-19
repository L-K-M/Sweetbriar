//! `subscribe_new_mail` (spec/07 "Subscriptions"): service-owned DTO
//! mirrors, converted field by field (spec/01 rule 3). The Proton
//! event loop feeds one account's Inbox arrivals; a bootstrap, a resync
//! and imports never do (spec/10 "Notifications").

use crate::{Core, CoreError};

/// One account's arrivals since the subscription's last answer
/// (spec/07). `count` is every arrival; `message_ids` the newest 20,
/// so none is dropped silently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewMail {
    pub account_id: String,
    pub count: u32,
    pub message_ids: Vec<i64>,
}

impl NewMail {
    fn from_engine(mail: sb_engine::NewMail) -> Self {
        Self {
            account_id: mail.account_id,
            count: mail.count,
            message_ids: mail.message_ids,
        }
    }
}

impl Core {
    /// `subscribe_new_mail` (spec/07): one account's Inbox arrivals
    /// since the subscription's last answer, pulled with `next`.
    /// Subscribe first, then pull, and no arrival after subscribing is
    /// missed. In memory only.
    pub fn subscribe_new_mail(&self) -> NewMailSubscription {
        NewMailSubscription(self.engine().subscribe_new_mail())
    }
}

/// The new-mail stream (spec/07): one in-flight `next` at a time, in
/// memory only.
pub struct NewMailSubscription(sb_engine::NewMailSubscription);

impl NewMailSubscription {
    /// Wait for one account's arrivals since the last answer; several
    /// are one answer, with `count` all of them and `message_ids` the
    /// newest 20. A second `next` while one waits is `InvalidInput`; a
    /// cancelled call moves nothing; shutdown ends a wait with
    /// `Unavailable`.
    pub async fn next(&self) -> Result<NewMail, CoreError> {
        Ok(NewMail::from_engine(self.0.next().await?))
    }
}
