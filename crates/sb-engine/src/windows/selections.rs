//! Selection snapshots (spec/07 "Selection snapshots"): an exact,
//! immutable materialization of one store window's membership — an
//! explicit id list, or a rank bitmap over the window's id index —
//! registered as an engine-bound query handle and paged by ordinal.
//!
//! The index read is the window's own scoped read (`store_index`,
//! `search_window_ids`), so a close before registration stops it, and
//! it never marks a delivered generation: a selection query leaves the
//! source window's observed-generation mark alone.

use std::collections::HashSet;
use std::sync::Arc;

use sb_resources::Scoped;

use super::{MAX_LIMIT, WindowId, WindowSource, Windows};
use crate::engine::Detached;
use crate::{Engine, EngineError};

/// The most occurrences an explicit `Ids` selection names (spec/07).
const MAX_SELECTION_IDS: usize = 1_000;

/// A selection snapshot's handle (spec/07): opaque, never reused, and
/// bound to the issuing engine — the UUID names it globally, so an id
/// another engine or an earlier run minted is simply unknown here.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SelectionId(String);

impl SelectionId {
    /// The id as the opaque string the shell holds.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for SelectionId {
    /// Wrap a shell-held id; a string this engine never minted is
    /// `NotFound` at use, never mistaken for a live one.
    fn from(raw: String) -> Self {
        Self(raw)
    }
}

/// What `open_selection` materializes (spec/07): explicit occurrence
/// ids, or one bit per rank of the window's id index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectionInput {
    /// Up to 1,000 distinct members of the index, in any order; the
    /// snapshot keeps window order.
    Ids(Vec<i64>),
    /// `ceil(index_length / 8)` bytes; rank `r` is bit `r % 8` of byte
    /// `r / 8`, and the unused final bits are zero.
    RankBitmap(Vec<u8>),
}

/// The metadata a delivered selection carries (spec/07): the
/// engine-bound id the caller pages and closes with, the source
/// window, the generation the membership was frozen at, and how many
/// ids it holds. Closing it is the caller's job.
#[derive(Debug)]
pub struct SelectionSnapshot {
    selection_id: SelectionId,
    window_id: WindowId,
    generation: u64,
    count: u64,
}

impl SelectionSnapshot {
    /// The handle `selection_ids` and `close_selection` take.
    pub fn selection_id(&self) -> &SelectionId {
        &self.selection_id
    }

    /// The window the membership was materialized from.
    pub fn window_id(&self) -> WindowId {
        self.window_id
    }

    /// The generation the source window's index was read at.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// How many occurrence ids the snapshot holds.
    pub fn count(&self) -> u64 {
        self.count
    }
}

/// A selection registered on its way to the caller; a dropped one
/// closes itself on the blocking pool, as an `Opened` window does
/// (spec/07: a cancelled open releases an unclaimed handle).
struct OpenedSelection {
    snapshot: Option<SelectionSnapshot>,
    windows: Arc<Windows>,
    cleanup: Detached,
}

impl OpenedSelection {
    /// Hand the snapshot to the caller, who now owns closing it.
    fn into_snapshot(mut self) -> SelectionSnapshot {
        self.snapshot
            .take()
            .expect("an opened selection is handed over once")
    }
}

impl Drop for OpenedSelection {
    fn drop(&mut self) {
        let Some(snapshot) = self.snapshot.take() else {
            return;
        };
        let windows = Arc::clone(&self.windows);
        self.cleanup
            .spawn(move || windows.close_selection(&snapshot.selection_id));
    }
}

/// `input` as occurrence ids in the index's window order, or why it is
/// invalid (spec/07): `Ids` at most [`MAX_SELECTION_IDS`] distinct
/// members, `RankBitmap` exactly `ceil(n / 8)` bytes with zero
/// padding — never an approximation.
fn materialize(input: &SelectionInput, index: &[i64]) -> Result<Vec<i64>, EngineError> {
    match input {
        SelectionInput::Ids(ids) => {
            if ids.len() > MAX_SELECTION_IDS {
                return Err(EngineError::invalid_input(
                    "input",
                    format!("an explicit selection holds at most {MAX_SELECTION_IDS} ids"),
                ));
            }
            let mut wanted = HashSet::with_capacity(ids.len());
            for id in ids {
                if !wanted.insert(*id) {
                    return Err(EngineError::invalid_input(
                        "input",
                        format!("occurrence id {id} is duplicated"),
                    ));
                }
            }
            // One index-order pass against the small input set: the
            // snapshot keeps window order without an index-sized map.
            let mut found = HashSet::with_capacity(ids.len());
            let mut selected = Vec::with_capacity(ids.len());
            for &id in index {
                if wanted.contains(&id) {
                    found.insert(id);
                    selected.push(id);
                }
            }
            if found.len() != ids.len() {
                let missing = ids
                    .iter()
                    .find(|id| !found.contains(id))
                    .expect("a shortfall means an input id is missing");
                return Err(EngineError::invalid_input(
                    "input",
                    format!("occurrence id {missing} is not in the window"),
                ));
            }
            Ok(selected)
        }
        SelectionInput::RankBitmap(bitmap) => {
            let n = index.len();
            let bytes = n.div_ceil(8);
            if bitmap.len() != bytes {
                return Err(EngineError::invalid_input(
                    "input",
                    format!("a rank bitmap is {bytes} bytes over a {n}-row window"),
                ));
            }
            if !n.is_multiple_of(8) && bitmap[n / 8] >> (n % 8) != 0 {
                return Err(EngineError::invalid_input(
                    "input",
                    "the rank bitmap's unused final bits are set",
                ));
            }
            Ok(index
                .iter()
                .enumerate()
                .filter(|(rank, _)| bitmap[rank / 8] & (1 << (rank % 8)) != 0)
                .map(|(_, &id)| id)
                .collect())
        }
    }
}

impl Engine {
    /// `open_selection` (spec/07 "Selection snapshots"): materialize
    /// `input` against the window's id index at `generation` — a
    /// mismatch is `ResetRequired` naming the current one — and
    /// register the frozen snapshot atomically against the source
    /// window's close. `DebugSynthetic` windows are `InvalidInput`:
    /// the source must be store-backed.
    pub async fn open_selection(
        &self,
        window_id: WindowId,
        generation: u64,
        input: SelectionInput,
    ) -> Result<SelectionSnapshot, EngineError> {
        let windows = Arc::clone(self.windows());
        let lookup = Arc::clone(&windows);
        let open = self.compute(move || lookup.window(window_id)).await?;
        let (generation, index) = match open.spec.source() {
            WindowSource::Synthetic => {
                return Err(EngineError::invalid_input(
                    "window_id",
                    "a selection's source is a store-backed window",
                ));
            }
            WindowSource::Search(spec) => {
                let found = self
                    .search_window_ids(spec.clone(), open.stop.clone(), Some(generation))
                    .await?;
                let Scoped::Done(found) = found else {
                    return Err(EngineError::NotFound {
                        what: format!("window {}", window_id.get()),
                    });
                };
                (found.generation, found.ids)
            }
            WindowSource::List(scope) => {
                self.store_index(window_id, open.stop.clone(), scope, Some(generation))
                    .await?
            }
        };
        let selection_id = SelectionId(self.resources().new_uuid_string());
        let cleanup = self.detached();
        let opened = self
            .compute(move || {
                let selected = materialize(&input, &index)?;
                let count = selected.len() as u64;
                windows.register_selection(
                    window_id,
                    selection_id.clone(),
                    generation,
                    selected,
                )?;
                Ok(OpenedSelection {
                    snapshot: Some(SelectionSnapshot {
                        selection_id,
                        window_id,
                        generation,
                        count,
                    }),
                    windows,
                    cleanup,
                })
            })
            .await?;
        Ok(opened.into_snapshot())
    }

    /// `selection_ids` (spec/07): the frozen ids `[from_ordinal,
    /// from_ordinal + limit)`, `limit` 1–1,000. `from_ordinal` at the
    /// count is an empty page; past it is `InvalidInput`, as is a bad
    /// limit — checked before the handle is looked up. A foreign,
    /// unknown or closed id is `NotFound`.
    pub async fn selection_ids(
        &self,
        id: SelectionId,
        from_ordinal: u64,
        limit: u32,
    ) -> Result<Vec<i64>, EngineError> {
        if !(1..=MAX_LIMIT).contains(&limit) {
            return Err(EngineError::invalid_input(
                "limit",
                format!("a selection page is 1–{MAX_LIMIT} ids"),
            ));
        }
        let windows = Arc::clone(self.windows());
        self.compute(move || windows.selection_page(&id, from_ordinal, limit))
            .await
    }

    /// Lease the actual handle's immutable membership for native export.
    /// Foreign/closed IDs fail; later source-window close changes nothing.
    pub async fn selection_for_export(
        &self,
        id: SelectionId,
    ) -> Result<crate::ExportSelection, EngineError> {
        let windows = Arc::clone(self.windows());
        self.compute(move || windows.selection_for_export(&id))
            .await
    }

    /// `close_selection` (spec/07): release the snapshot. Closing
    /// twice, or an id this engine never minted, is harmless.
    pub async fn close_selection(&self, id: SelectionId) {
        let windows = Arc::clone(self.windows());
        let _ = self
            .compute(move || {
                windows.close_selection(&id);
                Ok(())
            })
            .await;
    }
}
