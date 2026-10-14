//! Window handles and the debug synthetic source (spec/07 "Queries",
//! "The id index", "Debug source (pilot)").
//!
//! A *window* is a handle over a spec: `open_window` registers it and
//! answers the first page, `read_window` pages it at the generation the
//! caller holds, `window_ids` hands the shell the whole id index, and
//! `close_window` releases the handle. The pilot's only spec is
//! `DebugSynthetic`: one in-memory source per core, replaced atomically
//! by `debug_fill_synthetic`. Everything here is in memory — the window
//! code touches neither the store nor the blocking pool, so the mutex
//! below is taken for microseconds and never across an await.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

use crate::{Engine, EngineError};

/// The largest synthetic fill (spec/07: `count` is 0–1,000,000).
const MAX_FILL: u32 = 1_000_000;

/// The largest page a window answers (spec/07: `limit` is 1–1,000).
const MAX_LIMIT: u32 = 1_000;

/// The `sort_date` of rank 0 (plan M0.10, pinned): 2026-10-01T00:00:00Z
/// in epoch milliseconds.
const BASE_MS: i64 = 1_790_812_800_000;

/// How many rows share one `sort_date`: pairs tie on the date so the
/// order is carried by `occurrence_id` (spec/07).
const ROWS_PER_MINUTE: u64 = 2;

/// A minute in milliseconds — the `sort_date` step per pair of rows.
const MINUTE_MS: i64 = 60_000;

/// The mailbox every synthetic row reports (plan M0.10).
const MAILBOX_ID: i64 = 1;

/// Every `LONG_SUBJECT_MODULUS`-th rank carries the long subject
/// (plan M0.10: every 13th, ~120 chars, so row layouts that truncate
/// get exercised).
const LONG_SUBJECT_MODULUS: u64 = 13;

/// The ~110-char stem a long subject is built from; with the appended
/// ` #<occurrence_id>` the row subject lands at ~120 chars.
const LONG_SUBJECT: &str =
    "A longer synthetic subject line that exercises truncation and wrapping in rows";

/// The synthetic account ids, cycled by `rank % 3` (plan M0.10).
const ACCOUNTS: [&str; 3] = ["synthetic-a", "synthetic-b", "synthetic-c"];

/// The sender pool, cycled by `rank % 40`. Every name is invented for
/// the synthetic source (plan M0.10: no real names, addresses or
/// messages).
const SENDERS: [&str; 40] = [
    "Aline Sorrel",
    "Benno Hartvig",
    "Cerys Ingwald",
    "Dmitri Pavlen",
    "Edda Norquist",
    "Faruk Selwyn",
    "Grete Amberly",
    "Henrik Dovray",
    "Ines Marchell",
    "Joris Falken",
    "Katya Lindros",
    "Leif Arnesson",
    "Maud Feather",
    "Nils Ostrande",
    "Odile Brennec",
    "Pavel Yuriev",
    "Quinn Ashdown",
    "Runa Vinter",
    "Sanne Holmqvist",
    "Tobias Wexley",
    "Ulla Grimson",
    "Viggo Salmen",
    "Wanda Persen",
    "Xander Hollow",
    "Yrsa Dalen",
    "Zeno Markwart",
    "Astrid Bellam",
    "Birger Crowsley",
    "Clara Dunmore",
    "Dagny Everly",
    "Emil Forsgren",
    "Freya Haldane",
    "Gustav Ilmare",
    "Hilda Jarnvik",
    "Ivo Kestrel",
    "Jonah Lidman",
    "Kirsten Morrow",
    "Lars Novare",
    "Mette Orlav",
    "Nilsa Quill",
];

/// The subject stems, cycled by `rank % 25` and suffixed with the
/// row's occurrence id.
const SUBJECT_STEMS: [&str; 25] = [
    "Weekly digest",
    "Invoice attached",
    "Meeting notes",
    "Lunch on Friday",
    "Trip photos",
    "Budget draft",
    "The schedule",
    "Quick question",
    "Deployment summary",
    "Design review",
    "Weekend plans",
    "Book club",
    "Patch notes",
    "Release checklist",
    "Offsite agenda",
    "Garden update",
    "Recipe exchange",
    "Flight confirmation",
    "Delivery notice",
    "Account statement",
    "Newsletter",
    "Lab results",
    "Rehearsal moved",
    "Trail report",
    "API proposal",
];

/// The snippet pool, cycled by `rank % 20`. Each text is 60–200 chars
/// — the range real previews land in.
const SNIPPETS: [&str; 20] = [
    "Just following up on our conversation from last week — the draft is ready when you are.",
    "The report is attached; let me know if the figures for March look right to you.",
    "Can you pick up the package on your way back? It should be at the desk by noon.",
    "Here are the notes from today's session, plus the open questions we parked.",
    "The invoice for this month is attached. Same terms as usual, due in thirty days.",
    "Looks good to me — a couple of minor comments inline, nothing blocking the merge.",
    "Reminder that the rehearsal moved to Thursday; same room, same time as before.",
    "The measurements came back within tolerance, so we can go ahead with the order.",
    "Thanks for sending these over — I'll read through them tonight and reply tomorrow.",
    "A few of us are thinking about lunch near the office on Friday if you're around.",
    "The draft agenda is below. Feel free to add items; we'll trim on the day itself.",
    "Attached are the photos from the weekend. The light on Saturday was something else.",
    "Your booking is confirmed. Check-in opens at three and the desk closes at eleven.",
    "The new firmware fixed the radio issue; battery life looks unchanged so far though.",
    "I've sketched two options for the layout — option B keeps the header much simpler.",
    "The library books are due back next week; the renewal link is in the receipt below.",
    "Minutes from the board call: the budget passed, hiring stays frozen until spring.",
    "The trail is clear past the falls but washed out near the creek — take the ridge.",
    "Here's the proposal we discussed. Numbers are rough; happy to walk through them.",
    "Quick heads-up: the car needs its service booked before the trip next month.",
];

/// A window's handle (spec/07 "Windows are handles"): what the shell
/// holds to read and close a window. Per-core, never reused, never 0.
/// A newtype, so a generation can't be passed where a window id goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WindowId(u64);

impl WindowId {
    /// The raw id, for the layers above that spell it as a `u64`.
    pub fn get(self) -> u64 {
        self.0
    }
}

impl From<u64> for WindowId {
    fn from(raw: u64) -> Self {
        Self(raw)
    }
}

/// Which window a shell is listing (spec/07 "Queries"). For the pilot
/// only `DebugSynthetic` — the debug source below — exists; the real
/// specs land with the store-backed lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowSpec {
    /// The in-memory synthetic source, filled by `debug_fill_synthetic`.
    DebugSynthetic,
}

impl WindowSpec {
    /// The tag a cursor names its spec with (spec/07: "a cursor names
    /// its row, its spec and its generation"), so a cursor carried to
    /// a window over another spec is caught.
    fn tag(self) -> &'static str {
        match self {
            Self::DebugSynthetic => "debug-synthetic",
        }
    }
}

/// An opaque keyset cursor (spec/07 "Paging"): names one row, the spec
/// it was minted on and the source's generation then — never `OFFSET`.
/// The shell only carries it; the engine mints and parses it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor(String);

impl Cursor {
    /// The cursor as the opaque string the shell holds.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for Cursor {
    /// Wrap a shell-held string. The shape is checked when the cursor
    /// is used, never when it crosses a layer boundary.
    fn from(raw: String) -> Self {
        Self(raw)
    }
}

/// Where a window page read starts (spec/07 "Queries").
#[derive(Debug, Clone)]
pub enum Anchor {
    /// The newest rows: ranks `[0, limit)`.
    Top,
    /// At most `limit` rows strictly after the cursor's row.
    After(Cursor),
    /// At most `limit` rows strictly before the cursor's row.
    Before(Cursor),
    /// The page centred on this occurrence id.
    Around(i64),
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

/// One list row (spec/07): display fields only — no MIME tree, no
/// body. The shell resolves the account color from `account_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListRow {
    occurrence_id: i64,
    message_id: i64,
    account_id: String,
    mailbox_id: i64,
    sort_date: i64,
    sender: String,
    subject: String,
    snippet: String,
    unread: bool,
    flagged: bool,
    has_attachments: bool,
    tags: Vec<String>,
}

impl ListRow {
    /// The occurrence id the window sorts and selects by.
    pub fn occurrence_id(&self) -> i64 {
        self.occurrence_id
    }

    /// The logical message the occurrence belongs to.
    pub fn message_id(&self) -> i64 {
        self.message_id
    }

    /// The account the occurrence's mailbox lives in.
    pub fn account_id(&self) -> &str {
        &self.account_id
    }

    /// The mailbox holding the occurrence.
    pub fn mailbox_id(&self) -> i64 {
        self.mailbox_id
    }

    /// The sort key's date part: epoch milliseconds, newest first.
    pub fn sort_date(&self) -> i64 {
        self.sort_date
    }

    /// The sender as the list shows it.
    pub fn sender(&self) -> &str {
        &self.sender
    }

    /// The subject line.
    pub fn subject(&self) -> &str {
        &self.subject
    }

    /// The preview text.
    pub fn snippet(&self) -> &str {
        &self.snippet
    }

    /// Whether the row is unread.
    pub fn unread(&self) -> bool {
        self.unread
    }

    /// Whether the row is flagged.
    pub fn flagged(&self) -> bool {
        self.flagged
    }

    /// Whether the message has attachments.
    pub fn has_attachments(&self) -> bool {
        self.has_attachments
    }

    /// The row's local tags.
    pub fn tags(&self) -> &[String] {
        &self.tags
    }
}

/// One page of a window (spec/07 "Queries"): the rows in window order,
/// the cursors that continue the page, and whether rows lie past each
/// end.
#[derive(Debug, Clone)]
pub struct Window {
    window_id: WindowId,
    generation: u64,
    rows: Vec<ListRow>,
    before: Option<Cursor>,
    after: Option<Cursor>,
    has_more_after: bool,
    has_more_before: bool,
    total: Count,
}

impl Window {
    /// The window this page belongs to.
    pub fn window_id(&self) -> WindowId {
        self.window_id
    }

    /// The source generation the page was read at.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The page's rows in window order, at most `limit` of them.
    pub fn rows(&self) -> &[ListRow] {
        &self.rows
    }

    /// The first row's cursor — `None` when the page is empty.
    pub fn before(&self) -> Option<&Cursor> {
        self.before.as_ref()
    }

    /// The last row's cursor — `None` when the page is empty.
    pub fn after(&self) -> Option<&Cursor> {
        self.after.as_ref()
    }

    /// Whether rows lie after the page (higher ranks, older).
    pub fn has_more_after(&self) -> bool {
        self.has_more_after
    }

    /// Whether rows lie before the page (lower ranks, newer).
    pub fn has_more_before(&self) -> bool {
        self.has_more_before
    }

    /// The window's row count; the synthetic source always knows it.
    pub fn total(&self) -> Count {
        self.total
    }
}

/// A window's id index (spec/07 "The id index"): every row's
/// `occurrence_id` in window order at the reported generation. A list
/// maps a position to an id and reads `Around(id)`, so a scrollbar
/// jump never pages through the rows between.
#[derive(Debug, Clone)]
pub struct WindowIds {
    generation: u64,
    ids: Vec<i64>,
}

impl WindowIds {
    /// The source generation the index was taken at.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Every occurrence id in the window, in window order.
    pub fn ids(&self) -> &[i64] {
        &self.ids
    }
}

/// The window table and the debug source behind `DebugSynthetic`
/// windows. One per engine; all in memory. Windows outlive refills: a
/// fill only moves the source's generation, and the next read answers
/// `ResetRequired` so the shell rebuilds (spec/07).
pub(crate) struct Windows {
    inner: Mutex<WindowsInner>,
}

#[derive(Default)]
struct WindowsInner {
    /// The id the next opened window gets — incremented before use,
    /// so the first window is 1 and 0 never names a window.
    next_id: u64,
    /// The open windows, id → spec.
    open: HashMap<WindowId, WindowSpec>,
    /// The one debug source (spec/07 "Debug source"): its generation —
    /// 0 before the first fill — and its row count.
    synthetic: SyntheticSource,
}

/// The debug source's replaceable state: `(count, generation)` always
/// change together, under the same lock hold.
#[derive(Default, Clone, Copy)]
struct SyntheticSource {
    generation: u64,
    count: u32,
}

impl WindowsInner {
    /// The id a window being opened gets: fresh, never reused.
    fn take_window_id(&mut self) -> WindowId {
        self.next_id += 1;
        WindowId(self.next_id)
    }
}

impl Windows {
    /// A fresh, empty window state: no windows, an empty source at
    /// generation 0.
    pub(crate) fn new() -> Self {
        Self {
            inner: Mutex::new(WindowsInner::default()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, WindowsInner> {
        self.inner
            .lock()
            .expect("a panicking thread held the window lock")
    }

    /// `debug_fill_synthetic` (spec/07): replace the source atomically —
    /// the count and the bumped generation change under the same lock
    /// hold, so no read sees them apart. Returns the new generation.
    fn fill(&self, count: u32) -> Result<u64, EngineError> {
        if count > MAX_FILL {
            return Err(EngineError::invalid_input(
                "count",
                format!("the debug source holds at most {MAX_FILL} rows"),
            ));
        }
        let mut inner = self.lock();
        inner.synthetic.generation += 1;
        inner.synthetic.count = count;
        Ok(inner.synthetic.generation)
    }

    /// `open_window` (spec/07): resolve the anchor against the current
    /// source, register the window and answer its first page. A bad
    /// anchor fails the open without registering anything.
    fn open(&self, spec: WindowSpec, anchor: &Anchor, limit: u32) -> Result<Window, EngineError> {
        check_limit(limit)?;
        let mut inner = self.lock();
        let source = inner.synthetic;
        let range = resolve(spec, anchor, source.generation, source.count, limit)?;
        let window_id = inner.take_window_id();
        inner.open.insert(window_id, spec);
        Ok(page(
            window_id,
            source.generation,
            spec,
            range,
            source.count,
        ))
    }

    /// `read_window` (spec/07): the page `anchor` picks out of an open
    /// window at the caller's generation. An unknown or closed window
    /// is `NotFound`; a generation the source has moved past is
    /// `ResetRequired` naming the current one.
    fn read(
        &self,
        window_id: WindowId,
        generation: u64,
        anchor: &Anchor,
        limit: u32,
    ) -> Result<Window, EngineError> {
        check_limit(limit)?;
        let inner = self.lock();
        let spec = inner
            .open
            .get(&window_id)
            .copied()
            .ok_or_else(|| EngineError::NotFound {
                what: format!("window {}", window_id.get()),
            })?;
        let source = inner.synthetic;
        if generation != source.generation {
            return Err(EngineError::ResetRequired {
                generation: source.generation,
            });
        }
        let range = resolve(spec, anchor, generation, source.count, limit)?;
        Ok(page(window_id, generation, spec, range, source.count))
    }

    /// `window_ids` (spec/07 "The id index"): every row's occurrence
    /// id in window order, with the source's current generation. Only
    /// the snapshot is taken under the lock; the id list — 500k ids
    /// are 4 MB (spec/07) — is built after it.
    fn ids(&self, window_id: WindowId) -> Result<WindowIds, EngineError> {
        let source = {
            let inner = self.lock();
            if !inner.open.contains_key(&window_id) {
                return Err(EngineError::NotFound {
                    what: format!("window {}", window_id.get()),
                });
            }
            inner.synthetic
        };
        let n = u64::from(source.count);
        Ok(WindowIds {
            generation: source.generation,
            ids: (0..n).map(|rank| occurrence_id(n, rank)).collect(),
        })
    }

    /// `close_window` (spec/07): release the handle. Closing twice, or
    /// an id never opened, is harmless.
    fn close(&self, window_id: WindowId) {
        self.lock().open.remove(&window_id);
    }
}

/// Reject a `limit` outside `1..=1000` (spec/07), checked before
/// anything else in an open or a read.
fn check_limit(limit: u32) -> Result<(), EngineError> {
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(EngineError::invalid_input(
            "limit",
            format!("a window page is 1–{MAX_LIMIT} rows"),
        ));
    }
    Ok(())
}

/// The rank range `[start, end)` `anchor` picks out of `n` rows at
/// `generation` (spec/07 "Paging"). Window order is ascending rank —
/// the newest first — so a `Before` page lists its rows in the same
/// order every other page uses.
fn resolve(
    spec: WindowSpec,
    anchor: &Anchor,
    generation: u64,
    n: u32,
    limit: u32,
) -> Result<(u64, u64), EngineError> {
    let n = u64::from(n);
    let limit = u64::from(limit);
    match anchor {
        Anchor::Top => Ok((0, limit.min(n))),
        Anchor::After(cursor) => {
            let rank = cursor_rank(spec, cursor, generation, n)?;
            let start = rank + 1;
            Ok((start, (start + limit).min(n)))
        }
        Anchor::Before(cursor) => {
            let rank = cursor_rank(spec, cursor, generation, n)?;
            Ok((rank.saturating_sub(limit), rank))
        }
        Anchor::Around(id) => {
            let rank = rank_of(*id, n)?;
            let l = limit.min(n);
            let start = rank.saturating_sub(l / 2).min(n - l);
            Ok((start, start + l))
        }
    }
}

/// The rank of `id` in a source of `n` rows — occurrence ids run `n`
/// down to 1 — or `NotFound` when it falls outside `1..=n` (spec/07:
/// `Around` on an id not in the window).
fn rank_of(id: i64, n: u64) -> Result<u64, EngineError> {
    if id < 1 || id > n as i64 {
        return Err(EngineError::NotFound {
            what: format!("occurrence {id}"),
        });
    }
    Ok(n - id as u64)
}

/// The occurrence id of rank `p` in a source of `n` rows: dense and
/// descending — the newest row is `n`, the oldest is 1.
fn occurrence_id(n: u64, rank: u64) -> i64 {
    (n - rank) as i64
}

/// The `sort_date` of rank `p` (plan M0.10): `BASE_MS − (p / 2) ×
/// 60_000` — one minute back per pair, so neighbouring rows tie and
/// order is carried by `occurrence_id` descending.
fn sort_date_of(rank: u64) -> i64 {
    BASE_MS - (rank / ROWS_PER_MINUTE) as i64 * MINUTE_MS
}

/// The cursor format's tag; bump it if the shape ever changes, so a
/// cursor another version minted is `InvalidInput`, never misread.
const CURSOR_TAG: &str = "sbw1";

/// The cursor naming the row at `rank` — `(sort_date, occurrence_id)`
/// for date order — minted on `spec` at `generation` (spec/07). The
/// row's `sort_date` goes in too, so a hand-shaped cursor is caught
/// by the derivation check, not obeyed.
fn mint_cursor(spec: WindowSpec, generation: u64, row: &ListRow) -> Cursor {
    Cursor(format!(
        "{CURSOR_TAG}|{}|{generation}|{}|{}",
        spec.tag(),
        row.sort_date,
        row.occurrence_id
    ))
}

/// The rank `cursor`'s row holds in a source of `n` rows at
/// `generation` — or why it can't be used (spec/07 "Paging"):
/// malformed, or minted on another spec → `InvalidInput`; minted at
/// another generation → `ResetRequired`; a row that isn't there or
/// doesn't match its derivation → `InvalidInput`. The spec is checked
/// before the generation: a foreign cursor is a caller bug, not
/// staleness.
fn cursor_rank(
    spec: WindowSpec,
    cursor: &Cursor,
    generation: u64,
    n: u64,
) -> Result<u64, EngineError> {
    let invalid = |message: &str| EngineError::invalid_input("anchor", message);
    let fields: Vec<&str> = cursor.0.split('|').collect();
    let [tag, cursor_spec, minted, sort_date, id]: [&str; 5] = fields
        .try_into()
        .map_err(|_| invalid("the cursor is malformed"))?;
    if tag != CURSOR_TAG {
        return Err(invalid("the cursor is malformed"));
    }
    if cursor_spec != spec.tag() {
        return Err(invalid("the cursor names a different window spec"));
    }
    let minted_generation = minted
        .parse::<u64>()
        .map_err(|_| invalid("the cursor is malformed"))?;
    let cursor_sort_date = sort_date
        .parse::<i64>()
        .map_err(|_| invalid("the cursor is malformed"))?;
    let cursor_id = id
        .parse::<i64>()
        .map_err(|_| invalid("the cursor is malformed"))?;
    if minted_generation != generation {
        return Err(EngineError::ResetRequired { generation });
    }
    if cursor_id < 1 || cursor_id > n as i64 {
        return Err(invalid("the cursor's row is not in the window"));
    }
    let rank = n - cursor_id as u64;
    if cursor_sort_date != sort_date_of(rank) {
        return Err(invalid("the cursor's row doesn't match the window"));
    }
    Ok(rank)
}

/// Build the window's answer for `[start, end)`: only those ranks'
/// rows are derived — the source holds no per-row memory (spec/07) —
/// plus the edge cursors and the more-rows flags.
fn page(
    window_id: WindowId,
    generation: u64,
    spec: WindowSpec,
    range: (u64, u64),
    n: u32,
) -> Window {
    let (start, end) = range;
    let n = u64::from(n);
    let rows: Vec<ListRow> = (start..end).map(|rank| row_at(n, rank)).collect();
    Window {
        window_id,
        generation,
        before: rows.first().map(|row| mint_cursor(spec, generation, row)),
        after: rows.last().map(|row| mint_cursor(spec, generation, row)),
        has_more_before: start > 0,
        has_more_after: end < n,
        total: Count::Exact(n),
        rows,
    }
}

/// The synthetic row at rank `p` of `n` (spec/07 "Debug source", plan
/// M0.10), all of it derived — the source holds no per-row memory. ids
/// are dense and descending; strings and flags vary deterministically
/// by rank, all invented.
fn row_at(n: u64, rank: u64) -> ListRow {
    let occurrence_id = occurrence_id(n, rank);
    let subject = if rank.is_multiple_of(LONG_SUBJECT_MODULUS) {
        format!("{LONG_SUBJECT} #{occurrence_id}")
    } else {
        format!(
            "{} #{occurrence_id}",
            SUBJECT_STEMS[rank as usize % SUBJECT_STEMS.len()]
        )
    };
    let tags = if rank.is_multiple_of(22) {
        vec!["work".to_owned(), "later".to_owned()]
    } else if rank.is_multiple_of(11) {
        vec!["work".to_owned()]
    } else {
        Vec::new()
    };
    ListRow {
        occurrence_id,
        message_id: occurrence_id,
        account_id: ACCOUNTS[rank as usize % ACCOUNTS.len()].to_owned(),
        mailbox_id: MAILBOX_ID,
        sort_date: sort_date_of(rank),
        sender: SENDERS[rank as usize % SENDERS.len()].to_owned(),
        subject,
        snippet: SNIPPETS[rank as usize % SNIPPETS.len()].to_owned(),
        unread: rank.is_multiple_of(3),
        flagged: rank.is_multiple_of(17),
        has_attachments: rank.is_multiple_of(7),
        tags,
    }
}

impl Engine {
    /// `debug_fill_synthetic` (spec/07 "Debug source"): replace the
    /// synthetic source with `count` rows and bump its generation, so
    /// open synthetic windows answer `ResetRequired`. The replacement
    /// is one lock hold — atomic, and cheap enough to finish in a
    /// single poll once the caller holds the core (07: a cancelled
    /// fill either left the source untouched or returns the new
    /// generation).
    pub async fn debug_fill_synthetic(&self, count: u32) -> Result<u64, EngineError> {
        self.windows().fill(count)
    }

    /// `open_window` (spec/07 "Queries"): register a window over `spec`
    /// and return its first page at the source's current generation.
    /// `After`/`Before` cursors minted on the same spec at that
    /// generation are honoured.
    pub async fn open_window(
        &self,
        spec: WindowSpec,
        anchor: Anchor,
        limit: u32,
    ) -> Result<Window, EngineError> {
        self.windows().open(spec, &anchor, limit)
    }

    /// `read_window` (spec/07): the page `anchor` picks, at the
    /// generation the caller holds.
    pub async fn read_window(
        &self,
        window_id: WindowId,
        generation: u64,
        anchor: Anchor,
        limit: u32,
    ) -> Result<Window, EngineError> {
        self.windows().read(window_id, generation, &anchor, limit)
    }

    /// `window_ids` (spec/07 "The id index"): the window's rows'
    /// occurrence ids in window order, with the source's current
    /// generation.
    pub async fn window_ids(&self, window_id: WindowId) -> Result<WindowIds, EngineError> {
        self.windows().ids(window_id)
    }

    /// `close_window` (spec/07): release the handle. Closing twice is
    /// harmless.
    pub async fn close_window(&self, window_id: WindowId) {
        self.windows().close(window_id);
    }
}

#[cfg(test)]
mod tests;
