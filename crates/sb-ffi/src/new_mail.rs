//! `subscribe_new_mail` over FFI (spec/07 "Subscriptions"; plan
//! M2.7c4): a Rust-owned subscription the shell pulls for one
//! account's Inbox arrivals.

use std::sync::Arc;

use crate::api::Core;
use crate::cancel::{CancelToken, until_cancelled};
use crate::error::CoreError;

/// One account's arrivals since the subscription's last answer
/// (spec/07). `count` is every arrival; `message_ids` the newest 20,
/// so the shell summarizes the rest and none is dropped.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct NewMail {
    pub account_id: String,
    pub count: u32,
    pub message_ids: Vec<i64>,
}

impl NewMail {
    fn from_service(mail: sb_service::NewMail) -> Self {
        Self {
            account_id: mail.account_id,
            count: mail.count,
            message_ids: mail.message_ids,
        }
    }
}

/// The new-mail stream (spec/07): one in-flight `next` at a time, in
/// memory only.
#[derive(uniffi::Object)]
pub struct NewMailSubscription {
    inner: sb_service::NewMailSubscription,
}

#[uniffi::export]
impl NewMailSubscription {
    /// Wait for one account's arrivals since the last answer; the
    /// shell then re-reads the affected range. One call at a time: a
    /// second is `InvalidInput`. A cancelled call answers `Cancelled`
    /// and moves nothing, so the next call still sees what it missed.
    /// Shutdown ends a wait with `Unavailable`.
    pub async fn next(&self, token: Arc<CancelToken>) -> Result<NewMail, CoreError> {
        let mail = until_cancelled(&token, self.inner.next()).await??;
        Ok(NewMail::from_service(mail))
    }
}

#[uniffi::export]
impl Core {
    /// `subscribe_new_mail` (spec/07 "Subscriptions"): one account's
    /// Inbox arrivals since the subscription's last answer. Subscribe
    /// first, then pull, and none is missed. A query: the token races
    /// the core's lock.
    pub async fn subscribe_new_mail(
        &self,
        token: Arc<CancelToken>,
    ) -> Result<Arc<NewMailSubscription>, CoreError> {
        let inner = self
            .query(&token, async |core| Ok(core.subscribe_new_mail()))
            .await?;
        Ok(Arc::new(NewMailSubscription { inner }))
    }
}

#[cfg(test)]
mod tests;
