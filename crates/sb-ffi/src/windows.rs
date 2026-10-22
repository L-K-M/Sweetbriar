//! The window surface over FFI (spec/07 "Queries", "The id index",
//! "Debug source"): records and enums mirroring `sb-service`'s DTOs
//! field by field, and the exported `Core` methods. The queries race
//! the token at every wait (`Core::query`); `debug_fill_synthetic`
//! honours the token only until it holds the core (`Core::command`),
//! matching the spec's "cancellation counts only until the
//! replacement"; `close_window` takes no token — uncancellable, like
//! `shutdown`.

use std::sync::Arc;

use crate::api::Core;
use crate::cancel::CancelToken;
use crate::error::CoreError;
use crate::{SearchSpec, SearchStatus};

/// Which window the shell is listing (spec/07 "Store windows").
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum WindowSpec {
    /// The synthetic source filled by `debug_fill_synthetic`.
    DebugSynthetic,
    /// One mailbox's occurrences.
    Mailbox { mailbox_id: i64 },
    /// Every occurrence in mailboxes of one role.
    Unified { role: MailboxRole },
    /// Flagged logical messages, once each.
    Flagged,
    /// One thread's logical messages, once each.
    Thread { thread_id: i64 },
    /// Logical-message Search hits, with same-snapshot status.
    Search { spec: SearchSpec },
}

impl WindowSpec {
    fn into_service(self) -> sb_service::WindowSpec {
        match self {
            Self::DebugSynthetic => sb_service::WindowSpec::DebugSynthetic,
            Self::Mailbox { mailbox_id } => sb_service::WindowSpec::Mailbox(mailbox_id),
            Self::Unified { role } => sb_service::WindowSpec::Unified(role.to_service()),
            Self::Flagged => sb_service::WindowSpec::Flagged,
            Self::Thread { thread_id } => sb_service::WindowSpec::Thread(thread_id),
            Self::Search { spec } => sb_service::WindowSpec::Search(spec.into_service()),
        }
    }
}

/// A mailbox's special-use role (spec/02 `mailboxes.role`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum MailboxRole {
    Inbox,
    Sent,
    Drafts,
    Trash,
    Junk,
    Archive,
    All,
    /// Bridge's Starred (spec/04).
    Flagged,
}

impl MailboxRole {
    pub(crate) fn from_service(role: sb_service::MailboxRole) -> Self {
        match role {
            sb_service::MailboxRole::Inbox => Self::Inbox,
            sb_service::MailboxRole::Sent => Self::Sent,
            sb_service::MailboxRole::Drafts => Self::Drafts,
            sb_service::MailboxRole::Trash => Self::Trash,
            sb_service::MailboxRole::Junk => Self::Junk,
            sb_service::MailboxRole::Archive => Self::Archive,
            sb_service::MailboxRole::All => Self::All,
            sb_service::MailboxRole::Flagged => Self::Flagged,
        }
    }

    fn to_service(self) -> sb_service::MailboxRole {
        match self {
            Self::Inbox => sb_service::MailboxRole::Inbox,
            Self::Sent => sb_service::MailboxRole::Sent,
            Self::Drafts => sb_service::MailboxRole::Drafts,
            Self::Trash => sb_service::MailboxRole::Trash,
            Self::Junk => sb_service::MailboxRole::Junk,
            Self::Archive => sb_service::MailboxRole::Archive,
            Self::All => sb_service::MailboxRole::All,
            Self::Flagged => sb_service::MailboxRole::Flagged,
        }
    }
}

/// Where a window page read starts (spec/07 "Queries"). Cursors are
/// opaque strings minted by the core; the shell carries them
/// unchanged.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum Anchor {
    /// The newest rows: the first page.
    Top,
    /// At most `limit` rows strictly after the cursor's row.
    After { cursor: String },
    /// At most `limit` rows strictly before the cursor's row.
    Before { cursor: String },
    /// The page centred on this occurrence id.
    Around { occurrence_id: i64 },
}

impl Anchor {
    fn into_service(self) -> sb_service::Anchor {
        match self {
            Self::Top => sb_service::Anchor::Top,
            Self::After { cursor } => sb_service::Anchor::After(cursor),
            Self::Before { cursor } => sb_service::Anchor::Before(cursor),
            Self::Around { occurrence_id } => sb_service::Anchor::Around(occurrence_id),
        }
    }
}

/// A window's row count as the shell should treat it (spec/07).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum Count {
    /// The exact number of rows.
    Exact { n: u64 },
    /// A lower bound on the number of rows.
    AtLeast { n: u64 },
    /// Not known.
    Unknown,
}

impl Count {
    fn from_service(count: sb_service::Count) -> Self {
        match count {
            sb_service::Count::Exact(n) => Self::Exact { n },
            sb_service::Count::AtLeast(n) => Self::AtLeast { n },
            sb_service::Count::Unknown => Self::Unknown,
        }
    }
}

/// One list row (spec/07): display fields only — no MIME tree, no
/// body.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
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
    /// Mirror the service's row field by field.
    fn from_service(row: &sb_service::ListRow) -> Self {
        Self {
            occurrence_id: row.occurrence_id,
            message_id: row.message_id,
            account_id: row.account_id.clone(),
            mailbox_id: row.mailbox_id,
            sort_date: row.sort_date,
            sender: row.sender.clone(),
            subject: row.subject.clone(),
            snippet: row.snippet.clone(),
            unread: row.unread,
            flagged: row.flagged,
            has_attachments: row.has_attachments,
            tags: row.tags.clone(),
        }
    }
}

/// One page of a window (spec/07): the rows in window order, the
/// cursors continuing the page — `None` when it is empty — and
/// whether rows lie past each end.
#[derive(Debug, Clone, uniffi::Record)]
pub struct Window {
    pub window_id: u64,
    pub generation: u64,
    pub rows: Vec<ListRow>,
    pub before: Option<String>,
    pub after: Option<String>,
    pub has_more_after: bool,
    pub has_more_before: bool,
    pub total: Count,
    pub search: Option<SearchStatus>,
}

impl Window {
    /// Mirror the service's page field by field.
    fn from_service(window: &sb_service::Window) -> Self {
        Self {
            window_id: window.window_id,
            generation: window.generation,
            rows: window.rows.iter().map(ListRow::from_service).collect(),
            before: window.before.clone(),
            after: window.after.clone(),
            has_more_after: window.has_more_after,
            has_more_before: window.has_more_before,
            total: Count::from_service(window.total),
            search: window.search.as_ref().map(SearchStatus::from_service),
        }
    }
}

/// A window's id index (spec/07 "The id index"): every row's
/// `occurrence_id` in window order at the reported generation. The
/// list maps a position to an id and reads `Around(id)`, so a
/// scrollbar jump never pages through the rows between.
#[derive(Debug, Clone, uniffi::Record)]
pub struct WindowIds {
    pub generation: u64,
    pub ids: Vec<i64>,
}

#[uniffi::export]
impl Core {
    /// `open_window` (spec/07): register a window over `spec` and
    /// return its first page at the source's current generation. A
    /// query: the token races the core's lock and the work itself.
    pub async fn open_window(
        &self,
        token: Arc<CancelToken>,
        spec: WindowSpec,
        anchor: Anchor,
        limit: u32,
    ) -> Result<Window, CoreError> {
        let window = self
            .query(&token, async |core| {
                core.open_window(spec.into_service(), anchor.into_service(), limit)
                    .await
            })
            .await?;
        Ok(Window::from_service(&window))
    }

    /// `read_window` (spec/07): the page `anchor` picks out of the
    /// window at the generation the shell holds. A stale generation
    /// or cursor is `ResetRequired`; an unknown or closed window is
    /// `NotFound`.
    pub async fn read_window(
        &self,
        token: Arc<CancelToken>,
        window_id: u64,
        generation: u64,
        anchor: Anchor,
        limit: u32,
    ) -> Result<Window, CoreError> {
        let window = self
            .query(&token, async |core| {
                core.read_window(window_id, generation, anchor.into_service(), limit)
                    .await
            })
            .await?;
        Ok(Window::from_service(&window))
    }

    /// `window_ids` (spec/07 "The id index"): every row's occurrence
    /// id in window order, with the source's current generation.
    pub async fn window_ids(
        &self,
        token: Arc<CancelToken>,
        window_id: u64,
    ) -> Result<WindowIds, CoreError> {
        let index = self
            .query(&token, async |core| core.window_ids(window_id).await)
            .await?;
        Ok(WindowIds {
            generation: index.generation,
            ids: index.ids,
        })
    }

    /// `debug_fill_synthetic` (spec/07 "Debug source"): replace the
    /// synthetic source with `count` rows and return the new
    /// generation. Exported in every build, reached only from the
    /// app's Debug menu (spec/08). It runs through `command`: the
    /// token counts only until the fill holds the core — the
    /// replacement is one lock hold, so a cancelled fill either
    /// leaves the source untouched or returns the new generation,
    /// never `Cancelled` after replacing (spec/07).
    pub async fn debug_fill_synthetic(
        &self,
        token: Arc<CancelToken>,
        count: u32,
    ) -> Result<u64, CoreError> {
        self.command(&token, async |core| core.debug_fill_synthetic(count).await)
            .await
    }

    /// `close_window` (spec/07): release the handle. No token — like
    /// `shutdown`, the call can't be cancelled; closing twice, or
    /// after the core shut down, is harmless.
    pub async fn close_window(&self, window_id: u64) {
        let guard = self.service.read().await;
        if let Some(core) = guard.as_ref() {
            core.close_window(window_id).await;
        }
    }
}

#[cfg(test)]
mod search_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{CoreConfig, open_core};
    use sb_test_support::ScratchDir;

    /// A config over a fresh scratch directory, the way the shell
    /// passes roots it was given.
    fn scratch_config(scratch: &ScratchDir) -> CoreConfig {
        CoreConfig {
            data_root: scratch.path().join("store").to_string_lossy().into_owned(),
            cache_root: scratch.path().join("cache").to_string_lossy().into_owned(),
        }
    }

    /// A fresh token, the way the adapter makes one per call.
    pub(super) fn fresh_token() -> Arc<CancelToken> {
        CancelToken::new()
    }

    /// A token already cancelled, as a call under a task cancelled
    /// before it started gets.
    pub(super) fn cancelled_token() -> Arc<CancelToken> {
        let token = fresh_token();
        token.cancel();
        token
    }

    /// A core over a fresh scratch root and the scratch dir kept alive.
    pub(super) fn scratch_core(label: &str) -> (ScratchDir, Arc<Core>) {
        let scratch = ScratchDir::new(label);
        let core = sb_test_support::block_on(open_core(fresh_token(), scratch_config(&scratch)))
            .expect("a fresh store opens");
        (scratch, core)
    }

    /// A pre-cancelled token on each window query answers `Cancelled`
    /// (spec/07: a query stops wherever it waits — the core's lock
    /// included, so the cancel lands before the window is even looked
    /// up) and leaves the core usable for real calls.
    #[test]
    fn a_pre_cancelled_token_on_the_window_queries_answers_cancelled() {
        let (_scratch, core) = scratch_core("ffi-windows-cancelled");

        let err = sb_test_support::block_on(core.open_window(
            cancelled_token(),
            WindowSpec::DebugSynthetic,
            Anchor::Top,
            200,
        ))
        .expect_err("the open was cancelled");
        assert!(matches!(err, CoreError::Cancelled));

        let err =
            sb_test_support::block_on(core.read_window(cancelled_token(), 1, 0, Anchor::Top, 200))
                .expect_err("the read was cancelled");
        assert!(matches!(err, CoreError::Cancelled));

        let err = sb_test_support::block_on(core.window_ids(cancelled_token(), 1))
            .expect_err("the index read was cancelled");
        assert!(matches!(err, CoreError::Cancelled));

        // The cancelled calls touched nothing: a fresh query works.
        let window = sb_test_support::block_on(core.open_window(
            fresh_token(),
            WindowSpec::DebugSynthetic,
            Anchor::Top,
            200,
        ))
        .expect("the core still answers");
        assert_eq!(window.generation, 0);
        assert_eq!(window.total, Count::Exact { n: 0 });

        sb_test_support::block_on(core.shutdown());
    }

    /// `debug_fill_synthetic` with a pre-cancelled token leaves the
    /// source untouched — same generation, same count (spec/07: the
    /// token counts only until the replacement, which here is the
    /// single lock hold the begun fill finishes in).
    #[test]
    fn a_cancelled_fill_leaves_the_source_untouched() {
        let (_scratch, core) = scratch_core("ffi-fill-cancelled");
        let generation = sb_test_support::block_on(core.debug_fill_synthetic(fresh_token(), 10))
            .expect("the first fill runs");
        assert_eq!(generation, 1);

        let err = sb_test_support::block_on(core.debug_fill_synthetic(cancelled_token(), 999))
            .expect_err("the fill was cancelled before it began");
        assert!(matches!(err, CoreError::Cancelled));

        let window = sb_test_support::block_on(core.open_window(
            fresh_token(),
            WindowSpec::DebugSynthetic,
            Anchor::Top,
            5,
        ))
        .unwrap();
        assert_eq!(
            window.generation, 1,
            "the cancelled fill bumped the generation"
        );
        assert_eq!(
            window.total,
            Count::Exact { n: 10 },
            "the cancelled fill replaced the source"
        );

        sb_test_support::block_on(core.shutdown());
    }

    /// Each spec maps to the service's own, payload and role included,
    /// and each role round-trips.
    #[test]
    fn specs_and_roles_map_variant_for_variant() {
        const ROLES: [(MailboxRole, sb_service::MailboxRole); 8] = [
            (MailboxRole::Inbox, sb_service::MailboxRole::Inbox),
            (MailboxRole::Sent, sb_service::MailboxRole::Sent),
            (MailboxRole::Drafts, sb_service::MailboxRole::Drafts),
            (MailboxRole::Trash, sb_service::MailboxRole::Trash),
            (MailboxRole::Junk, sb_service::MailboxRole::Junk),
            (MailboxRole::Archive, sb_service::MailboxRole::Archive),
            (MailboxRole::All, sb_service::MailboxRole::All),
            (MailboxRole::Flagged, sb_service::MailboxRole::Flagged),
        ];
        for (role, service) in ROLES {
            assert_eq!(role.to_service(), service);
            assert_eq!(MailboxRole::from_service(service), role);
            assert_eq!(
                WindowSpec::Unified { role }.into_service(),
                sb_service::WindowSpec::Unified(service)
            );
        }
        assert_eq!(
            WindowSpec::Mailbox { mailbox_id: 7 }.into_service(),
            sb_service::WindowSpec::Mailbox(7)
        );
        assert_eq!(
            WindowSpec::Thread { thread_id: 9 }.into_service(),
            sb_service::WindowSpec::Thread(9)
        );
        assert_eq!(
            WindowSpec::Flagged.into_service(),
            sb_service::WindowSpec::Flagged
        );
        assert_eq!(
            WindowSpec::DebugSynthetic.into_service(),
            sb_service::WindowSpec::DebugSynthetic
        );
    }

    /// Every store spec opens over the exported path; over an empty
    /// store each is empty and knows it (spec/07 "Store windows").
    #[test]
    fn store_windows_open_over_the_exported_path() {
        let (_scratch, core) = scratch_core("ffi-store-windows");
        for spec in [
            WindowSpec::Mailbox { mailbox_id: 1 },
            WindowSpec::Unified {
                role: MailboxRole::Inbox,
            },
            WindowSpec::Flagged,
            WindowSpec::Thread { thread_id: 1 },
        ] {
            let window = sb_test_support::block_on(core.open_window(
                fresh_token(),
                spec.clone(),
                Anchor::Top,
                10,
            ))
            .unwrap();
            assert!(window.rows.is_empty(), "{spec:?}");
            assert_eq!(window.total, Count::Exact { n: 0 }, "{spec:?}");
            sb_test_support::block_on(core.close_window(window.window_id));
        }
        sb_test_support::block_on(core.shutdown());
    }

    /// The pilot's diagnostic shape (plan M0.10): a 500,000-row fill,
    /// then the id index hands back all 500,000 ids in window order.
    #[test]
    fn a_half_million_row_fill_yields_its_id_index_in_order() {
        const N: u32 = 500_000;
        let (_scratch, core) = scratch_core("ffi-500k");
        let generation = sb_test_support::block_on(core.debug_fill_synthetic(fresh_token(), N))
            .expect("the fill runs");
        assert_eq!(generation, 1);

        let window = sb_test_support::block_on(core.open_window(
            fresh_token(),
            WindowSpec::DebugSynthetic,
            Anchor::Top,
            1,
        ))
        .unwrap();
        let index = sb_test_support::block_on(core.window_ids(fresh_token(), window.window_id))
            .expect("the index reads");
        assert_eq!(index.generation, generation);
        assert_eq!(index.ids.len(), usize::try_from(N).unwrap());
        assert!(
            index
                .ids
                .iter()
                .enumerate()
                .all(|(rank, id)| *id == i64::from(N) - rank as i64),
            "the id index is every occurrence id, newest first"
        );

        sb_test_support::block_on(core.shutdown());
    }

    /// The exported path end to end (spec/07): open a page, page it
    /// with its own cursor, watch a refill reset stale generation and
    /// cursor, honour a cursor minted at the current generation, then
    /// close twice without harm.
    #[test]
    fn window_queries_round_trip_over_the_exported_path() {
        let (_scratch, core) = scratch_core("ffi-windows-round-trip");
        let generation =
            sb_test_support::block_on(core.debug_fill_synthetic(fresh_token(), 50)).unwrap();

        let first = sb_test_support::block_on(core.open_window(
            fresh_token(),
            WindowSpec::DebugSynthetic,
            Anchor::Top,
            10,
        ))
        .expect("the first page opens");
        assert_eq!(first.window_id, 1);
        assert_eq!(first.generation, generation);
        assert_eq!(first.rows.len(), 10);
        assert_eq!(first.rows[0].occurrence_id, 50);
        assert_eq!(first.total, Count::Exact { n: 50 });
        assert!(first.before.is_some() && first.after.is_some());
        assert!(first.has_more_after);
        assert!(!first.has_more_before);

        // The page's own cursor continues it.
        let second = sb_test_support::block_on(core.read_window(
            fresh_token(),
            first.window_id,
            generation,
            Anchor::After {
                cursor: first.after.clone().unwrap(),
            },
            10,
        ))
        .expect("the page after the cursor reads");
        assert_eq!(second.rows.len(), 10);
        assert_eq!(second.rows[0].occurrence_id, 40);

        // A cursor minted at this generation opens a new window.
        let continued = sb_test_support::block_on(core.open_window(
            fresh_token(),
            WindowSpec::DebugSynthetic,
            Anchor::Before {
                cursor: second.before.clone().unwrap(),
            },
            10,
        ))
        .expect("a same-generation cursor is honoured");
        assert_eq!(continued.rows.len(), 10);
        assert_eq!(continued.rows[0].occurrence_id, 50);

        // A refill resets the old generation and its cursors alike.
        let current =
            sb_test_support::block_on(core.debug_fill_synthetic(fresh_token(), 60)).unwrap();
        let err = sb_test_support::block_on(core.read_window(
            fresh_token(),
            first.window_id,
            generation,
            Anchor::Top,
            10,
        ))
        .expect_err("the old generation is stale");
        assert!(
            matches!(err, CoreError::ResetRequired { generation } if generation == current),
            "ResetRequired names the current generation: {err}"
        );
        let err = sb_test_support::block_on(core.open_window(
            fresh_token(),
            WindowSpec::DebugSynthetic,
            Anchor::After {
                cursor: first.after.clone().unwrap(),
            },
            10,
        ))
        .expect_err("the old cursor is stale");
        assert!(matches!(err, CoreError::ResetRequired { generation: 2 }));

        // Closing twice is harmless; a closed window is `NotFound`.
        sb_test_support::block_on(core.close_window(first.window_id));
        sb_test_support::block_on(core.close_window(first.window_id));
        let err = sb_test_support::block_on(core.read_window(
            fresh_token(),
            first.window_id,
            current,
            Anchor::Top,
            10,
        ))
        .expect_err("the closed window is gone");
        assert!(matches!(err, CoreError::NotFound { .. }));

        sb_test_support::block_on(core.shutdown());
    }
}
