//! UID reads retain the selection established by EXAMINE. They never use
//! the FETCH prefix (a changing sequence number) as message identity.
use io_imap::types::command::CommandBody;
use io_imap::types::core::Vec1;
use io_imap::types::fetch::MessageDataItemName;
use io_imap::types::search::SearchKey;

use super::{Connection, MailboxContext, MailboxNotice, ReadKind, ReadObservation, changed};
use crate::uid_observations::{FlagBuilder, SearchBuilder};
use crate::{FlagCoverage, FlagSweep, ImapError, UidInventory};

impl Connection {
    /// Read the selected mailbox's complete UID SEARCH ALL result. The
    /// required SEARCH frame distinguishes an empty set from absent data.
    /// Notices require reconciliation; this is never deletion authority.
    pub async fn uid_inventory(&mut self) -> Result<ReadObservation<UidInventory>, ImapError> {
        let context = self.uid_context()?;
        let body = CommandBody::search(None, Vec1::from(SearchKey::All), true);
        let mut builder = SearchBuilder::default();
        let notices = self
            .read_metadata(body, ReadKind::UidSearch, |response| builder.push(response))
            .await?;
        let observation = match builder.finish(context) {
            Ok(observation) => observation,
            Err(error) => {
                self.wire = None;
                return Err(error);
            }
        };
        Ok(ReadObservation {
            observation,
            notices,
        })
    }

    /// Pair UID SEARCH ALL with UID FETCH 1:* (UID FLAGS). Coverage is
    /// explicitly incomplete if messages or updates cannot be reconciled.
    /// Missing flags are unknown, while a returned empty flag list is known.
    /// Even complete coverage requires the Engine's later-inventory and
    /// breaker checks before marking an occurrence absent (spec/04).
    pub async fn flag_sweep(&mut self) -> Result<ReadObservation<FlagSweep>, ImapError> {
        let inventory = self.uid_inventory().await?;
        let mut notices = inventory.notices;
        let body = CommandBody::fetch(
            ..,
            vec![MessageDataItemName::Uid, MessageDataItemName::Flags],
            true,
        )
        .expect("static full UID flag sweep");
        let mut builder = FlagBuilder::default();
        // There is no suspension point between the successful search and
        // taking the wire for this exchange. Each pending command future
        // exclusively owns the socket, so cancelling either phase closes it.
        let fetched = self
            .read_metadata(body, ReadKind::FlagSweep, |response| builder.push(response))
            .await?;
        for notice in fetched {
            if !notices.contains(&notice) {
                notices.push(notice);
            }
        }
        let has_changes = notices
            .iter()
            .any(|notice| matches!(notice, MailboxNotice::Changed { .. }));
        let ambiguous = builder.ambiguous();
        let observation = builder.finish(inventory.observation, has_changes);
        if ambiguous || observation.coverage == FlagCoverage::Incomplete {
            changed(Some(&observation.inventory.mailbox), &mut notices);
        }
        Ok(ReadObservation {
            observation,
            notices,
        })
    }

    fn uid_context(&self) -> Result<MailboxContext, ImapError> {
        if self.wire.is_none() {
            return Err(ImapError::Closed);
        }
        self.selection.clone().ok_or(ImapError::NotSelected)
    }
}
