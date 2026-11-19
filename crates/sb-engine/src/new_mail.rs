//! `subscribe_new_mail`'s arrivals (spec/07 "Subscriptions", spec/10
//! "Notifications"). In memory only: the Proton event loop records a
//! new logical message that lands in the account's Inbox role mailbox,
//! and a bootstrap, a refresh or resumed account's resync, or an
//! import never does. A slow consumer's answer counts every arrival
//! since its last one and names the newest 20 ids, so none is dropped
//! silently.

use std::collections::VecDeque;
use std::sync::Mutex;

use crate::Engine;

/// How many message ids one answer carries (spec/07): the count names
/// the rest, so none is dropped.
pub(crate) const MAX_MESSAGE_IDS: usize = 20;

/// One account's arrivals since a subscription's last answer
/// (spec/07). `count` is every arrival; `message_ids` the newest
/// [`MAX_MESSAGE_IDS`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewMail {
    pub account_id: String,
    pub count: u32,
    pub message_ids: Vec<i64>,
}

/// One subscription's unanswered arrivals: per-account batches,
/// coalesced, oldest first. Each subscription has its own, so one
/// consumer's backlog is its own.
pub(crate) struct Subscriber {
    pending: Mutex<VecDeque<(String, VecDeque<i64>)>>,
}

impl Subscriber {
    /// A fresh, empty queue (spec/07 `subscribe_new_mail`).
    pub(crate) fn new() -> Self {
        Self {
            pending: Mutex::new(VecDeque::new()),
        }
    }

    /// Append one account's arrivals to its batch, or open a new one.
    pub(crate) fn push(&self, account_id: &str, message_ids: &[i64]) {
        let mut pending = self
            .pending
            .lock()
            .expect("the new-mail queue lock poisoned");
        match pending.iter_mut().find(|(id, _)| id == account_id) {
            Some((_, ids)) => ids.extend(message_ids.iter().copied()),
            None => {
                let ids = message_ids.iter().copied().collect();
                pending.push_back((account_id.to_owned(), ids));
            }
        }
    }

    /// The oldest account's arrivals, or `None` when none are pending.
    /// The count keeps them all; the ids are the newest 20.
    pub(crate) fn pop(&self) -> Option<NewMail> {
        let mut pending = self
            .pending
            .lock()
            .expect("the new-mail queue lock poisoned");
        let (account_id, mut ids) = pending.pop_front()?;
        let count = u32::try_from(ids.len()).unwrap_or(u32::MAX);
        let message_ids = if ids.len() > MAX_MESSAGE_IDS {
            ids.split_off(ids.len() - MAX_MESSAGE_IDS)
                .into_iter()
                .collect()
        } else {
            ids.into_iter().collect()
        };
        Some(NewMail {
            account_id,
            count,
            message_ids,
        })
    }
}

impl Engine {
    /// `subscribe_new_mail` (spec/07): one account's Inbox arrivals
    /// since the subscription's last answer, pulled with `next`.
    /// Subscribe first, then pull, and no arrival after subscribing is
    /// missed. In memory only.
    pub fn subscribe_new_mail(&self) -> crate::windows::NewMailSubscription {
        self.windows().subscribe_new_mail()
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::sync::Arc;

    use crate::windows::Windows;

    #[test]
    fn two_accounts_arrivals_answer_one_after_the_other() {
        let windows = Arc::new(Windows::new());
        let subscription = windows.subscribe_new_mail();
        windows.new_mail_arrived("a", vec![1]);
        windows.new_mail_arrived("b", vec![2]);

        let first = sb_test_support::block_on(subscription.next()).expect("a answers");
        assert_eq!(
            (first.account_id.as_str(), first.message_ids),
            ("a", vec![1])
        );

        // The second account's batch is already pending: it answers
        // on the first poll, without waiting for another arrival.
        let mut next = std::pin::pin!(subscription.next());
        let polled = next
            .as_mut()
            .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()));
        let std::task::Poll::Ready(second) = polled else {
            panic!("b's pending batch waits for another arrival");
        };
        let second = second.expect("b answers");
        assert_eq!(
            (second.account_id.as_str(), second.message_ids),
            ("b", vec![2])
        );
    }
}
