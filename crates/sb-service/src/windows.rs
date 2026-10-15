//! Window and list-row DTOs plus `Core`'s window methods (spec/07
//! "Queries", "The id index", "Debug source"). The DTOs are
//! service-owned mirrors of the engine's types — nothing from below
//! crosses into L1 (spec/01 rule 3), so the methods convert field by
//! field.

use sb_engine::WindowId;

use crate::{Core, CoreError};

/// Which window a shell is listing (spec/07). For the pilot only
/// `DebugSynthetic` — the in-memory debug source — exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowSpec {
    /// The synthetic source filled by `Core::debug_fill_synthetic`.
    DebugSynthetic,
}

impl WindowSpec {
    fn to_engine(self) -> sb_engine::WindowSpec {
        match self {
            Self::DebugSynthetic => sb_engine::WindowSpec::DebugSynthetic,
        }
    }
}

/// Where a window page read starts (spec/07 "Queries"). Cursors are
/// the engine's opaque strings; the shell carries them unchanged.
#[derive(Debug, Clone)]
pub enum Anchor {
    /// The newest rows: the first page.
    Top,
    /// At most `limit` rows strictly after the cursor's row.
    After(String),
    /// At most `limit` rows strictly before the cursor's row.
    Before(String),
    /// The page centred on this occurrence id.
    Around(i64),
}

impl Anchor {
    fn into_engine(self) -> sb_engine::Anchor {
        match self {
            Self::Top => sb_engine::Anchor::Top,
            Self::After(cursor) => sb_engine::Anchor::After(cursor.into()),
            Self::Before(cursor) => sb_engine::Anchor::Before(cursor.into()),
            Self::Around(occurrence_id) => sb_engine::Anchor::Around(occurrence_id),
        }
    }
}

/// A window's row count as the caller should treat it (spec/07).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Count {
    /// The exact number of rows.
    Exact(u64),
    /// A lower bound on the number of rows.
    AtLeast(u64),
    /// Not known.
    Unknown,
}

impl Count {
    fn from_engine(count: sb_engine::Count) -> Self {
        match count {
            sb_engine::Count::Exact(n) => Self::Exact(n),
            sb_engine::Count::AtLeast(n) => Self::AtLeast(n),
            sb_engine::Count::Unknown => Self::Unknown,
        }
    }
}

/// One list row (spec/07): display fields only — no MIME tree, no
/// body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListRow {
    pub occurrence_id: i64,
    pub message_id: i64,
    pub account_id: String,
    pub mailbox_id: i64,
    pub sort_date: i64,
    pub sender: String,
    pub subject: String,
    pub snippet: String,
    pub unread: bool,
    pub flagged: bool,
    pub has_attachments: bool,
    pub tags: Vec<String>,
}

impl ListRow {
    fn from_engine(row: &sb_engine::ListRow) -> Self {
        Self {
            occurrence_id: row.occurrence_id(),
            message_id: row.message_id(),
            account_id: row.account_id().to_owned(),
            mailbox_id: row.mailbox_id(),
            sort_date: row.sort_date(),
            sender: row.sender().to_owned(),
            subject: row.subject().to_owned(),
            snippet: row.snippet().to_owned(),
            unread: row.unread(),
            flagged: row.flagged(),
            has_attachments: row.has_attachments(),
            tags: row.tags().to_vec(),
        }
    }
}

/// One page of a window (spec/07): the rows in window order, the
/// cursors continuing the page — `None` when it is empty — and
/// whether rows lie past each end.
#[derive(Debug, Clone)]
pub struct Window {
    pub window_id: u64,
    pub generation: u64,
    pub rows: Vec<ListRow>,
    pub before: Option<String>,
    pub after: Option<String>,
    pub has_more_after: bool,
    pub has_more_before: bool,
    pub total: Count,
}

impl Window {
    fn from_engine(window: &sb_engine::Window) -> Self {
        Self {
            window_id: window.window_id().get(),
            generation: window.generation(),
            rows: window.rows().iter().map(ListRow::from_engine).collect(),
            before: window.before().map(|cursor| cursor.as_str().to_owned()),
            after: window.after().map(|cursor| cursor.as_str().to_owned()),
            has_more_after: window.has_more_after(),
            has_more_before: window.has_more_before(),
            total: Count::from_engine(window.total()),
        }
    }
}

/// A window's id index (spec/07 "The id index"): every row's
/// `occurrence_id` in window order at the reported generation.
#[derive(Debug, Clone)]
pub struct WindowIds {
    pub generation: u64,
    pub ids: Vec<i64>,
}

impl Core {
    /// `debug_fill_synthetic` (spec/07 "Debug source"): replace the
    /// synthetic source with `count` rows and return the new
    /// generation. A pilot diagnostic, not a command: it writes
    /// nothing to the store, takes no idempotency key and returns no
    /// receipt.
    pub async fn debug_fill_synthetic(&self, count: u32) -> Result<u64, CoreError> {
        Ok(self.engine().debug_fill_synthetic(count).await?)
    }

    /// `open_window` (spec/07 "Queries"): register a window over
    /// `spec` and return its first page at the source's current
    /// generation.
    pub async fn open_window(
        &self,
        spec: WindowSpec,
        anchor: Anchor,
        limit: u32,
    ) -> Result<Window, CoreError> {
        let window = self
            .engine()
            .open_window(spec.to_engine(), anchor.into_engine(), limit)
            .await?;
        Ok(Window::from_engine(&window))
    }

    /// `read_window` (spec/07): the page `anchor` picks out of the
    /// window at the generation the caller holds. A stale generation
    /// or cursor is `ResetRequired`; an unknown or closed window is
    /// `NotFound`.
    pub async fn read_window(
        &self,
        window_id: u64,
        generation: u64,
        anchor: Anchor,
        limit: u32,
    ) -> Result<Window, CoreError> {
        let window = self
            .engine()
            .read_window(
                WindowId::from(window_id),
                generation,
                anchor.into_engine(),
                limit,
            )
            .await?;
        Ok(Window::from_engine(&window))
    }

    /// `window_ids` (spec/07 "The id index"): every row's occurrence
    /// id in window order, with the source's current generation.
    pub async fn window_ids(&self, window_id: u64) -> Result<WindowIds, CoreError> {
        let index = self.engine().window_ids(WindowId::from(window_id)).await?;
        Ok(WindowIds {
            generation: index.generation(),
            ids: index.ids().to_vec(),
        })
    }

    /// `close_window` (spec/07): release the handle. Closing twice is
    /// harmless; the shell closes every window it opens.
    pub async fn close_window(&self, window_id: u64) {
        self.engine().close_window(WindowId::from(window_id)).await;
    }
}
