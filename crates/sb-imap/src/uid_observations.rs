//! UID inventory and flag observations (plan M2.2b b2a). Explicit
//! `UID SEARCH` and `UID FETCH` frames become owned rows keyed only by
//! UID. No provider or store policy, no store writes, and coverage is
//! never authority to delete anything.

use std::collections::BTreeMap;
use std::num::NonZeroU32;

use io_imap::types::fetch::MessageDataItem;
use io_imap::types::flag::FlagFetch;
use io_imap::types::response::{Data, Response};

use crate::{ImapError, MailboxContext};

/// The complete UID set of one selection, from the explicit single
/// `UID SEARCH ALL` response. An empty set is observed evidence of an
/// empty mailbox; a missing frame is no evidence at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UidInventory {
    /// The exact selection the inventory was read under.
    pub mailbox: MailboxContext,
    /// Sorted, deduplicated UIDs, `u32::MAX` included. Sequence numbers
    /// never appear here.
    pub uids: Vec<NonZeroU32>,
}

/// One flag row, keyed only by the UID item, never by the FETCH
/// sequence number of the frame that carried it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UidFlags {
    /// The UID item's value; a changing sequence number is never a key.
    pub uid: NonZeroU32,
    /// The observed flag spellings. An explicit empty list is known "no
    /// flags" and differs from a missing FLAGS item, which yields no row.
    pub flags: Vec<String>,
}

/// Whether a flag sweep covers its inventory exactly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FlagCoverage {
    /// Every inventory UID has exactly one row, with no ambiguity or
    /// mailbox-change hint observed. Even complete coverage stays subject
    /// to the engine's second inventory and change breaker before any
    /// occurrence is marked absent (spec/04).
    Complete,
    /// Rows may be missing, repeated, unkeyed or updated mid-sweep. An
    /// incomplete sweep is never evidence that a message or its flags are
    /// absent.
    Incomplete,
}

/// A flag sweep paired with the inventory it was taken against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlagSweep {
    /// The normalized inventory the rows are measured against.
    pub inventory: UidInventory,
    /// Rows keyed only by UID, sorted by UID with at most one row each.
    /// An empty FLAGS list is a row; a missing FLAGS item is no row.
    pub rows: Vec<UidFlags>,
    /// [`FlagCoverage::Complete`] only for an unambiguous, unchanged
    /// sweep whose row UIDs equal the normalized inventory exactly.
    pub coverage: FlagCoverage,
}

/// Collects the one required SEARCH frame into a UID inventory: only
/// the codec's numeric values, no sequence-set expansion, no count caps.
#[derive(Default)]
pub(crate) struct SearchBuilder {
    values: Option<Vec<NonZeroU32>>,
    failed: bool,
}

impl SearchBuilder {
    /// Stores the single `Data::Search` frame; any other response is not
    /// inventory evidence and is ignored. A second SEARCH frame is a
    /// static protocol error that leaves no inventory behind.
    pub(crate) fn push(&mut self, response: &Response<'_>) -> Result<(), ImapError> {
        let Response::Data(Data::Search(values, ..)) = response else {
            return Ok(());
        };
        if self.values.is_some() {
            self.failed = true;
            return Err(search_error("duplicate SEARCH response"));
        }
        self.values = Some(values.clone());
        Ok(())
    }

    /// Requires exactly one frame: an explicit empty set is a valid
    /// inventory, a missing frame is an error. The result is sorted and
    /// deduplicated.
    pub(crate) fn finish(self, mailbox: MailboxContext) -> Result<UidInventory, ImapError> {
        if self.failed {
            return Err(search_error("duplicate SEARCH response"));
        }
        let Some(mut uids) = self.values else {
            return Err(search_error("missing SEARCH response"));
        };
        uids.sort_unstable();
        uids.dedup();
        Ok(UidInventory { mailbox, uids })
    }
}

/// Collects UID-keyed flag rows from `UID FETCH` frames. A frame missing
/// its UID or FLAGS is a legal unsolicited update: no row, no failure.
#[derive(Default)]
pub(crate) struct FlagBuilder {
    rows: BTreeMap<NonZeroU32, Vec<String>>,
    ambiguous: bool,
}

impl FlagBuilder {
    /// A frame yields one complete row only with exactly one UID and one
    /// FLAGS item (an empty list is valid); extra typed items are ignored.
    /// A duplicate UID or FLAGS attribute within one frame is a static
    /// protocol error that changes no state, so no partial row survives.
    pub(crate) fn push(&mut self, response: &Response<'_>) -> Result<(), ImapError> {
        let Response::Data(Data::Fetch { items, .. }) = response else {
            return Ok(());
        };
        let mut uid = None;
        let mut flags = None;
        for item in items.as_ref() {
            match item {
                MessageDataItem::Uid(value) => {
                    if uid.replace(*value).is_some() {
                        return Err(fetch_error("duplicate UID attribute"));
                    }
                }
                MessageDataItem::Flags(values) => {
                    let names = values.iter().map(flag_name).collect();
                    if flags.replace(names).is_some() {
                        return Err(fetch_error("duplicate FLAGS attribute"));
                    }
                }
                _ => {}
            }
        }
        match (uid, flags) {
            (Some(uid), Some(flags)) => {
                // A UID seen again is an update, even identical, never a
                // second row: keep the latest complete values.
                if self.rows.insert(uid, flags).is_some() {
                    self.ambiguous = true;
                }
            }
            // Unkeyed or missing-flag frames carry no row, but they can
            // hide an update, so they pin coverage below complete.
            _ => self.ambiguous = true,
        }
        Ok(())
    }

    /// Whether any frame failed to yield exactly one complete new row;
    /// the parent turns this hint into a reconciliation notice.
    pub(crate) fn ambiguous(&self) -> bool {
        self.ambiguous
    }

    /// Normalizes the inventory and returns the rows sorted by UID.
    /// Coverage is complete only when nothing was ambiguous or changed
    /// and the row UIDs equal the normalized inventory exactly.
    pub(crate) fn finish(self, mut inventory: UidInventory, changed: bool) -> FlagSweep {
        inventory.uids.sort_unstable();
        inventory.uids.dedup();
        let rows: Vec<UidFlags> = self
            .rows
            .into_iter()
            .map(|(uid, flags)| UidFlags { uid, flags })
            .collect();
        let complete = !self.ambiguous
            && !changed
            && rows
                .iter()
                .map(|row| row.uid)
                .eq(inventory.uids.iter().copied());
        FlagSweep {
            inventory,
            rows,
            coverage: if complete {
                FlagCoverage::Complete
            } else {
                FlagCoverage::Incomplete
            },
        }
    }
}

/// A static `UID SEARCH` failure; no input text is formatted in.
fn search_error(message: &'static str) -> ImapError {
    ImapError::command("UID SEARCH", message)
}

/// A static `UID FETCH` failure; no input text is formatted in.
fn fetch_error(message: &'static str) -> ImapError {
    ImapError::command("UID FETCH", message)
}

pub(crate) fn flag_name(flag: &FlagFetch<'_>) -> String {
    match flag {
        FlagFetch::Flag(flag) => flag.to_string(),
        FlagFetch::Recent => "\\Recent".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use io_imap::codec::{ResponseCodec, decode::Decoder};

    use super::*;

    fn frame(line: &str) -> Result<Response<'_>, ImapError> {
        let (rest, response) = ResponseCodec::new()
            .decode(line.as_bytes())
            .map_err(|_| ImapError::Malformed)?;
        assert!(rest.is_empty());
        Ok(response)
    }

    fn context() -> MailboxContext {
        MailboxContext {
            name: "INBOX".into(),
            uid_validity: NonZeroU32::new(4100).unwrap(),
        }
    }

    fn inventory(uids: &[u32]) -> UidInventory {
        UidInventory {
            mailbox: context(),
            uids: uids.iter().map(|&uid| uid.try_into().unwrap()).collect(),
        }
    }

    fn row_keys(sweep: &FlagSweep) -> Vec<u32> {
        sweep.rows.iter().map(|row| row.uid.get()).collect()
    }

    fn search_inventory(lines: &[&str]) -> Result<UidInventory, ImapError> {
        let mut builder = SearchBuilder::default();
        for line in lines {
            builder.push(&frame(line)?)?;
        }
        builder.finish(context())
    }

    fn flag_sweep(lines: &[&str], uids: &[u32], changed: bool) -> Result<FlagSweep, ImapError> {
        let mut builder = FlagBuilder::default();
        for line in lines {
            builder.push(&frame(line)?)?;
        }
        Ok(builder.finish(inventory(uids), changed))
    }

    #[test]
    fn one_search_frame_is_required_and_explicit_empty_is_not_missing() {
        let empty = search_inventory(&["* SEARCH\r\n"]).unwrap();
        assert!(empty.uids.is_empty(), "explicit empty is evidence");
        assert!(search_inventory(&[]).is_err(), "no frame is not evidence");
        assert!(
            search_inventory(&["* 2 EXISTS\r\n"]).is_err(),
            "non-SEARCH frames are not evidence"
        );
        let mut builder = SearchBuilder::default();
        builder.push(&frame("* SEARCH 7\r\n").unwrap()).unwrap();
        assert!(builder.push(&frame("* SEARCH 9\r\n").unwrap()).is_err());
        assert!(builder.finish(context()).is_err(), "no partial inventory");
    }

    #[test]
    fn uids_sort_dedup_and_stay_distinct_from_sequence_numbers() {
        let found = search_inventory(&["* SEARCH 7 3 4294967295 3\r\n"]).unwrap();
        assert_eq!(found, inventory(&[3, 7, u32::MAX]));
        let swept = flag_sweep(
            &["* 1 FETCH (RFC822.SIZE 42 UID 4294967295 FLAGS (\\Seen))\r\n"],
            &[u32::MAX],
            false,
        )
        .unwrap();
        assert_eq!(row_keys(&swept), [u32::MAX], "sequence 1 is not the key");
        assert_eq!(swept.rows[0].flags, ["\\Seen"]);
        assert_eq!(swept.coverage, FlagCoverage::Complete);
    }

    #[test]
    fn empty_flags_differ_from_missing_and_unkeyed_frames() {
        let found = flag_sweep(
            &[
                "* 1 FETCH (UID 5 FLAGS ())\r\n",
                "* 2 FETCH (UID 6)\r\n",
                "* 3 FETCH (FLAGS (\\Draft))\r\n",
            ],
            &[5],
            false,
        )
        .unwrap();
        assert_eq!(row_keys(&found), [5], "missing/unkeyed frames yield no row");
        assert!(found.rows[0].flags.is_empty(), "known empty");
        assert_eq!(found.coverage, FlagCoverage::Incomplete);
    }

    #[test]
    fn repeated_uids_keep_the_latest_complete_row_and_stay_incomplete() {
        let found = flag_sweep(
            &[
                "* 1 FETCH (UID 5 FLAGS (\\Seen))\r\n",
                "* 2 FETCH (UID 5 FLAGS (\\Flagged))\r\n",
                "* 3 FETCH (UID 5)\r\n",
            ],
            &[5],
            false,
        )
        .unwrap();
        assert_eq!(row_keys(&found), [5]);
        assert_eq!(found.rows[0].flags, ["\\Flagged"], "latest complete row");
        assert_eq!(found.coverage, FlagCoverage::Incomplete);
        let identical = flag_sweep(
            &[
                "* 1 FETCH (UID 5 FLAGS (\\Seen))\r\n",
                "* 2 FETCH (UID 5 FLAGS (\\Seen))\r\n",
            ],
            &[5],
            false,
        )
        .unwrap();
        assert_eq!(identical.coverage, FlagCoverage::Incomplete);
    }

    #[test]
    fn coverage_completes_only_for_exact_unchanged_sweeps() {
        let matching = flag_sweep(&["* 1 FETCH (UID 5 FLAGS (\\Seen))\r\n"], &[5], false).unwrap();
        assert_eq!(matching.coverage, FlagCoverage::Complete);
        let changed = flag_sweep(&["* 1 FETCH (UID 5 FLAGS (\\Seen))\r\n"], &[5], true).unwrap();
        assert_eq!(changed.coverage, FlagCoverage::Incomplete);
        let short = flag_sweep(&[], &[5], false).unwrap();
        assert_eq!(short.coverage, FlagCoverage::Incomplete);
        let extra = flag_sweep(&["* 1 FETCH (UID 6 FLAGS (\\Seen))\r\n"], &[5], false).unwrap();
        assert_eq!(extra.coverage, FlagCoverage::Incomplete);
    }

    #[test]
    fn duplicate_uid_or_flags_attributes_change_no_state() {
        let mut builder = FlagBuilder::default();
        let good = frame("* 1 FETCH (UID 5 FLAGS (\\Seen))\r\n").unwrap();
        builder.push(&good).unwrap();
        for line in [
            "* 1 FETCH (UID 5 UID 6 FLAGS ())\r\n",
            "* 2 FETCH (UID 7 FLAGS (\\Seen) FLAGS (\\Draft))\r\n",
        ] {
            assert!(builder.push(&frame(line).unwrap()).is_err());
        }
        assert_eq!(builder.rows.len(), 1, "rejected frames left no partial row");
    }
}
