//! Drafts (spec/02 "Drafts", spec/05, spec/07): immutable revisions
//! under one draft row, the route snapshot on each revision, and the
//! object-store references a save's attachments were published for.
//! The schema is migration 1's; these are its reads and writes.

use std::sync::Arc;

use rusqlite::{OptionalExtension, Transaction, params};
use sb_types::time::Timestamp;

use crate::StoreError;
use crate::objects::{PublishedObject, ReservationRelease, release_reservation};
use crate::schema::classify;

/// The spec/02 draft state names, read back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DraftState {
    Editing,
    Queued,
    Sent,
    Discarded,
}

impl DraftState {
    fn from_stored(name: &str) -> Result<Self, StoreError> {
        match name {
            "editing" => Ok(Self::Editing),
            "queued" => Ok(Self::Queued),
            "sent" => Ok(Self::Sent),
            "discarded" => Ok(Self::Discarded),
            _ => Err(StoreError::corrupt(format!("a draft's state is '{name}'"))),
        }
    }

    /// The stored word.
    pub fn name(self) -> &'static str {
        match self {
            Self::Editing => "editing",
            Self::Queued => "queued",
            Self::Sent => "sent",
            Self::Discarded => "discarded",
        }
    }

    /// Whether a save or discard may still act: `sent` and `discarded`
    /// end spec/05's transitions.
    fn writable(self) -> bool {
        matches!(self, Self::Editing | Self::Queued)
    }
}

/// `compose_format`'s words (spec/02, spec/05 "Formats").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposeFormat {
    Plain,
    Markdown,
}

impl ComposeFormat {
    fn from_stored(name: &str) -> Result<Self, StoreError> {
        match name {
            "plain" => Ok(Self::Plain),
            "markdown" => Ok(Self::Markdown),
            _ => Err(StoreError::corrupt(format!(
                "a draft revision's format is '{name}'"
            ))),
        }
    }

    /// The stored word.
    pub fn name(self) -> &'static str {
        match self {
            Self::Plain => "plain",
            Self::Markdown => "markdown",
        }
    }
}

/// A send attempt's state word (spec/02 `send_attempts`). A draft
/// reports only a live one (`held`, `queued`, `submitting`, or
/// `outcome_unknown` not yet resolved, spec/05); the rest never reach
/// the queries below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptState {
    Held,
    Queued,
    Submitting,
    Accepted,
    Failed,
    OutcomeUnknown,
    Cancelled,
}

impl AttemptState {
    fn from_stored(name: &str) -> Result<Self, StoreError> {
        match name {
            "held" => Ok(Self::Held),
            "queued" => Ok(Self::Queued),
            "submitting" => Ok(Self::Submitting),
            "accepted" => Ok(Self::Accepted),
            "failed" => Ok(Self::Failed),
            "outcome_unknown" => Ok(Self::OutcomeUnknown),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(StoreError::corrupt(format!(
                "a send attempt's state is '{name}'"
            ))),
        }
    }

    /// The stored word.
    pub fn name(self) -> &'static str {
        match self {
            Self::Held => "held",
            Self::Queued => "queued",
            Self::Submitting => "submitting",
            Self::Accepted => "accepted",
            Self::Failed => "failed",
            Self::OutcomeUnknown => "outcome_unknown",
            Self::Cancelled => "cancelled",
        }
    }
}

/// One address slot on a revision, the `to_json`/`cc_json`/`bcc_json`
/// shape `{address, name}` (spec/02); `name` is `None` when the address
/// stands alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftRecipient {
    pub address: String,
    pub name: Option<String>,
}

/// A new publication or an unchanged attachment from the base revision.
/// Sharing a publication permits identical command retries; its reservation
/// still authorizes only one new reference.
pub enum DraftAttachment {
    New {
        object: Arc<PublishedObject>,
        filename: String,
        media_type: String,
    },
    Retained(AttachmentRef),
}

/// What `create_draft` records about the message being answered
/// (spec/07 `context`). Reply kinds keep the source occurrence; a
/// forward resolves it to the logical message.
pub enum DraftSource {
    New,
    Reply { occurrence_id: i64 },
    ReplyAll { occurrence_id: i64 },
    Forward { occurrence_id: i64 },
}

/// One save's content (spec/07 `save_draft`'s `content`): the route's
/// identity (`None` is a draft with no sending identity, spec/02),
/// format, fields and the published attachments.
pub struct RevisionInput {
    pub revision_id: String,
    pub identity_id: Option<String>,
    pub format: ComposeFormat,
    /// `plain_flowed = false` is the per-draft literal option (spec/05).
    pub plain_flowed: bool,
    pub subject: String,
    pub to: Vec<DraftRecipient>,
    pub cc: Vec<DraftRecipient>,
    pub bcc: Vec<DraftRecipient>,
    pub body_source: String,
    pub attachments: Vec<DraftAttachment>,
}

/// The current revision's full content, as `draft()` returns it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredRevision {
    pub revision_id: String,
    pub seq: i64,
    pub base_revision_id: Option<String>,
    pub account_id: Option<String>,
    pub identity_id: Option<String>,
    pub format: ComposeFormat,
    pub plain_flowed: bool,
    pub subject: String,
    pub to: Vec<DraftRecipient>,
    pub cc: Vec<DraftRecipient>,
    pub bcc: Vec<DraftRecipient>,
    pub body_source: String,
    pub attachments: Vec<AttachmentRef>,
    pub created_at: Timestamp,
}

/// One stored attachment reference (`attachments_json`), a published
/// object's identity and size (spec/02).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentRef {
    pub object_sha256: String,
    pub filename: String,
    pub media_type: String,
    pub size: u64,
}

/// A live send attempt as a draft reports it (spec/07 "any live
/// attempt's state"); the query's where-clause means only live states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveAttempt {
    pub attempt_id: String,
    pub state: AttemptState,
}

/// One draft with its current revision, as `draft(draft_id)` returns
/// it (spec/07).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredDraft {
    pub draft_id: String,
    pub state: DraftState,
    pub reply_to_occurrence_id: Option<i64>,
    pub forward_of_message_id: Option<i64>,
    pub revision: Option<StoredRevision>,
    pub attempt: Option<LiveAttempt>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// One `drafts()` row (spec/07): state, route, subject, recipients,
/// update time and any live attempt's state: the sidebar's shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftSummary {
    pub draft_id: String,
    pub state: DraftState,
    pub account_id: Option<String>,
    pub identity_id: Option<String>,
    pub subject: String,
    pub to: Vec<DraftRecipient>,
    pub cc: Vec<DraftRecipient>,
    pub bcc: Vec<DraftRecipient>,
    pub updated_at: Timestamp,
    pub attempt: Option<LiveAttempt>,
}

/// `create_draft` (spec/07): a new `editing` draft with no revision
/// yet. The source occurrence must exist (`NotFound`); `Forward`
/// resolves it to its logical message.
pub fn create_draft(
    tx: &Transaction,
    draft_id: &str,
    source: &DraftSource,
    now: Timestamp,
) -> Result<(), StoreError> {
    let (reply_to, forward_of) = match *source {
        DraftSource::New => (None, None),
        DraftSource::Reply { occurrence_id } | DraftSource::ReplyAll { occurrence_id } => {
            require_occurrence(tx, occurrence_id)?;
            (Some(occurrence_id), None)
        }
        DraftSource::Forward { occurrence_id } => {
            (None, Some(occurrence_message(tx, occurrence_id)?))
        }
    };
    tx.execute(
        "INSERT INTO drafts (draft_id, state, reply_to_occurrence_id, forward_of_message_id,
             current_revision_id, created_at, updated_at)
         VALUES (?1, 'editing', ?2, ?3, NULL, ?4, ?4)",
        params![draft_id, reply_to, forward_of, now.as_millis()],
    )
    .map_err(|e| classify(e, "create a draft"))?;
    Ok(())
}

/// `save_draft`'s revision insert (spec/02, 07): `base` must be the
/// draft's current revision (`None` before the first save), else
/// `Conflict`; `sent` and `discarded` refuse the same. `identity_id`
/// `Some` resolves its account (`NotFound` when unknown); `None`
/// stores the draft-with-no-route revision spec/02 allows. One
/// transaction: the revision row, each new attachment's reservation
/// release, `current_revision_id`, `editing`.
pub fn save_draft(
    tx: &Transaction,
    draft_id: &str,
    base: Option<&str>,
    revision: RevisionInput,
    now: Timestamp,
) -> Result<(), StoreError> {
    let draft = find_draft(tx, draft_id)?;
    if !draft.state.writable() {
        return Err(conflict_on_state(draft_id, draft.state));
    }
    if draft.current_revision_id.as_deref() != base {
        return Err(StoreError::conflict(format!(
            "draft {draft_id}'s current revision is not its base {}: save again over the current revision",
            describe_base(base)
        )));
    }
    let (account_id, identity_id) = match revision.identity_id.as_deref() {
        Some(identity) => (Some(identity_account(tx, identity)?), Some(identity)),
        None => (None, None),
    };
    let seq = next_seq(tx, draft_id)?;
    let to = recipients_json(&revision.to)?;
    let cc = recipients_json(&revision.cc)?;
    let bcc = recipients_json(&revision.bcc)?;
    let base_attachments = match base {
        Some(id) => self::revision(tx, draft_id, id)?.attachments,
        None => Vec::new(),
    };
    let attachments = attachments_json(&revision.attachments, &base_attachments)?;
    tx.execute(
        "INSERT INTO draft_revisions (revision_id, draft_id, seq, base_revision_id,
             account_id, identity_id, compose_format, plain_flowed, subject,
             to_json, cc_json, bcc_json, body_source, attachments_json, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            revision.revision_id,
            draft_id,
            seq,
            base,
            account_id,
            identity_id,
            revision.format.name(),
            i64::from(revision.plain_flowed),
            revision.subject,
            to,
            cc,
            bcc,
            revision.body_source,
            attachments,
            now.as_millis(),
        ],
    )
    .map_err(|e| classify(e, "save a draft revision"))?;
    // The revision now names each published object: spend this
    // publication's reservation in the same transaction (spec/03).
    for attachment in &revision.attachments {
        if let DraftAttachment::New { object, .. } = attachment {
            release_reservation(tx, object, ReservationRelease::Required)?;
        }
    }
    tx.execute(
        "UPDATE drafts SET current_revision_id = ?2, state = 'editing', updated_at = ?3
         WHERE draft_id = ?1",
        params![draft_id, revision.revision_id, now.as_millis()],
    )
    .map_err(|e| classify(e, "update a draft's current revision"))?;
    Ok(())
}

/// `discard_draft` (spec/07, 05): refused `Conflict` while a live
/// attempt exists or the draft already sits at a terminal state; the
/// draft and its revisions stay, marked `discarded`.
pub fn discard_draft(tx: &Transaction, draft_id: &str, now: Timestamp) -> Result<(), StoreError> {
    let draft = find_draft(tx, draft_id)?;
    if let Some(attempt) = live_attempt(tx, draft_id, draft.current_revision_id.as_deref())? {
        return Err(StoreError::conflict(format!(
            "draft {draft_id} has a live send attempt ({})",
            attempt.state.name()
        )));
    }
    if !draft.state.writable() {
        return Err(conflict_on_state(draft_id, draft.state));
    }
    tx.execute(
        "UPDATE drafts SET state = 'discarded', updated_at = ?2 WHERE draft_id = ?1",
        params![draft_id, now.as_millis()],
    )
    .map_err(|e| classify(e, "discard a draft"))?;
    Ok(())
}

/// `drafts()` (spec/07): every `editing` or `queued` draft, newest
/// update first, with its route, subject, recipients and any live
/// attempt. A draft with no revision yet reports an empty subject and
/// no recipients.
pub fn drafts(tx: &Transaction) -> Result<Vec<DraftSummary>, StoreError> {
    let err = |e| classify(e, "list drafts");
    let mut statement = tx
        .prepare(
            "SELECT d.draft_id, d.state, d.updated_at,
                    r.account_id, r.identity_id, r.subject, r.to_json, r.cc_json, r.bcc_json,
                    a.attempt_id, a.state
             FROM drafts d
             LEFT JOIN draft_revisions r
                 ON r.revision_id = d.current_revision_id AND r.draft_id = d.draft_id
             LEFT JOIN send_attempts a ON a.attempt_id = (
                 SELECT s.attempt_id FROM send_attempts s
                 WHERE s.draft_id = d.draft_id AND (
                     s.state IN ('held','queued','submitting')
                     OR (s.state = 'outcome_unknown' AND s.resolution IS NULL))
                 ORDER BY (s.revision_id = d.current_revision_id) DESC,
                          s.created_at DESC, s.attempt_id DESC
                 LIMIT 1)
             WHERE d.state IN ('editing','queued')
             ORDER BY d.updated_at DESC, d.draft_id",
        )
        .map_err(err)?;
    let mut rows = statement.query([]).map_err(err)?;
    let mut listed = Vec::new();
    while let Some(row) = rows.next().map_err(err)? {
        let state: String = row.get(1).map_err(err)?;
        // The revision's recipient lists are NULL until the first save.
        let to: Option<String> = row.get(6).map_err(err)?;
        let cc: Option<String> = row.get(7).map_err(err)?;
        let bcc: Option<String> = row.get(8).map_err(err)?;
        let attempt_state: Option<String> = row.get(10).map_err(err)?;
        let parse = |json: Option<String>| match json {
            Some(json) => parse_recipients(&json),
            None => Ok(Vec::new()),
        };
        listed.push(DraftSummary {
            draft_id: row.get(0).map_err(err)?,
            state: DraftState::from_stored(&state)?,
            updated_at: Timestamp::from_millis(row.get(2).map_err(err)?),
            account_id: row.get(3).map_err(err)?,
            identity_id: row.get(4).map_err(err)?,
            subject: row
                .get::<_, Option<String>>(5)
                .map_err(err)?
                .unwrap_or_default(),
            to: parse(to)?,
            cc: parse(cc)?,
            bcc: parse(bcc)?,
            attempt: match row.get::<_, Option<String>>(9).map_err(err)? {
                Some(attempt_id) => Some(LiveAttempt {
                    attempt_id,
                    state: AttemptState::from_stored(
                        &attempt_state
                            .ok_or_else(|| corrupt("a live attempt joined without its state"))?,
                    )?,
                }),
                None => None,
            },
        });
    }
    Ok(listed)
}

/// `draft(draft_id)` (spec/07): the draft with its current revision's
/// full content and any live attempt; `None` when the id is unknown.
/// Reads any state: a compose window restores against it.
pub fn draft(tx: &Transaction, draft_id: &str) -> Result<Option<StoredDraft>, StoreError> {
    let err = |e| classify(e, "read a draft");
    let row = tx
        .query_row(
            "SELECT state, reply_to_occurrence_id, forward_of_message_id,
                    current_revision_id, created_at, updated_at
             FROM drafts WHERE draft_id = ?1",
            params![draft_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            },
        )
        .optional()
        .map_err(err)?;
    let Some((state, reply_to, forward_of, current, created_at, updated_at)) = row else {
        return Ok(None);
    };
    let attempt = live_attempt(tx, draft_id, current.as_deref())?;
    Ok(Some(StoredDraft {
        draft_id: draft_id.to_owned(),
        state: DraftState::from_stored(&state)?,
        reply_to_occurrence_id: reply_to,
        forward_of_message_id: forward_of,
        revision: match current {
            Some(revision_id) => Some(revision(tx, draft_id, &revision_id)?),
            None => None,
        },
        attempt,
        created_at: Timestamp::from_millis(created_at),
        updated_at: Timestamp::from_millis(updated_at),
    }))
}

/// The draft row's state and current revision, or `NotFound`.
struct DraftRow {
    state: DraftState,
    current_revision_id: Option<String>,
}

fn find_draft(tx: &Transaction, draft_id: &str) -> Result<DraftRow, StoreError> {
    let row = tx
        .query_row(
            "SELECT state, current_revision_id FROM drafts WHERE draft_id = ?1",
            params![draft_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
        )
        .optional()
        .map_err(|e| classify(e, "read a draft"))?;
    let Some((state, current_revision_id)) = row else {
        return Err(StoreError::not_found(format!("draft {draft_id}")));
    };
    Ok(DraftRow {
        state: DraftState::from_stored(&state)?,
        current_revision_id,
    })
}

/// One revision row with its parsed content; `Corrupt` when a stored
/// column no longer holds what the writer wrote.
fn revision(
    tx: &Transaction,
    draft_id: &str,
    revision_id: &str,
) -> Result<StoredRevision, StoreError> {
    let err = |e| classify(e, "read a draft revision");
    let row = tx
        .query_row(
            "SELECT seq, base_revision_id, account_id, identity_id, compose_format,
                    plain_flowed, subject, to_json, cc_json, bcc_json, body_source,
                    attachments_json, created_at
             FROM draft_revisions WHERE draft_id = ?1 AND revision_id = ?2",
            params![draft_id, revision_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, String>(10)?,
                    row.get::<_, String>(11)?,
                    row.get::<_, i64>(12)?,
                ))
            },
        )
        .optional()
        .map_err(err)?;
    // `drafts.current_revision_id` named it; a missing row is damage.
    let Some((
        seq,
        base,
        account_id,
        identity_id,
        format,
        plain_flowed,
        subject,
        to,
        cc,
        bcc,
        body_source,
        attachments,
        created_at,
    )) = row
    else {
        return Err(StoreError::corrupt(format!(
            "draft {draft_id}'s current revision {revision_id} is missing"
        )));
    };
    Ok(StoredRevision {
        revision_id: revision_id.to_owned(),
        seq,
        base_revision_id: base,
        account_id,
        identity_id,
        format: ComposeFormat::from_stored(&format)?,
        plain_flowed: plain_flowed != 0,
        subject,
        to: parse_recipients(&to)?,
        cc: parse_recipients(&cc)?,
        bcc: parse_recipients(&bcc)?,
        body_source,
        attachments: parse_attachments(&attachments)?,
        created_at: Timestamp::from_millis(created_at),
    })
}

/// The live attempt a draft reports (spec/07 "any live attempt's
/// state"): `held`, `queued`, `submitting`, or `outcome_unknown`
/// unresolved (spec/05). A draft can hold more than one: an older
/// revision's attempt stays live while a newer revision is itself
/// queued (spec/02). The reported one is the current revision's, else
/// the newest; any of them refuses a discard.
fn live_attempt(
    tx: &Transaction,
    draft_id: &str,
    current_revision_id: Option<&str>,
) -> Result<Option<LiveAttempt>, StoreError> {
    let err = |e| classify(e, "read a draft's live attempt");
    let mut statement = tx
        .prepare(
            "SELECT attempt_id, state FROM send_attempts
             WHERE draft_id = ?1 AND (
                 state IN ('held','queued','submitting')
                 OR (state = 'outcome_unknown' AND resolution IS NULL))
             ORDER BY (revision_id = ?2) DESC, created_at DESC, attempt_id DESC
             LIMIT 1",
        )
        .map_err(err)?;
    let mut rows = statement
        .query(params![draft_id, current_revision_id])
        .map_err(err)?;
    let Some(row) = rows.next().map_err(err)? else {
        return Ok(None);
    };
    Ok(Some(LiveAttempt {
        attempt_id: row.get(0).map_err(err)?,
        state: AttemptState::from_stored(&row.get::<_, String>(1).map_err(err)?)?,
    }))
}

fn require_occurrence(tx: &Transaction, occurrence_id: i64) -> Result<(), StoreError> {
    let exists = tx
        .query_row(
            "SELECT 1 FROM occurrences WHERE occurrence_id = ?1",
            params![occurrence_id],
            |_| Ok(()),
        )
        .optional()
        .map_err(|e| classify(e, "check the draft's source occurrence"))?;
    if exists.is_none() {
        return Err(StoreError::not_found(format!("occurrence {occurrence_id}")));
    }
    Ok(())
}

/// A forward's source message id; its occurrence must exist.
fn occurrence_message(tx: &Transaction, occurrence_id: i64) -> Result<i64, StoreError> {
    tx.query_row(
        "SELECT message_id FROM occurrences WHERE occurrence_id = ?1",
        params![occurrence_id],
        |row| row.get(0),
    )
    .optional()
    .map_err(|e| classify(e, "read the draft's source occurrence"))?
    .ok_or_else(|| StoreError::not_found(format!("occurrence {occurrence_id}")))
}

/// The account a draft's chosen identity belongs to (spec/02's route
/// snapshot is taken at save).
fn identity_account(tx: &Transaction, identity_id: &str) -> Result<String, StoreError> {
    tx.query_row(
        "SELECT account_id FROM identities WHERE identity_id = ?1",
        params![identity_id],
        |row| row.get(0),
    )
    .optional()
    .map_err(|e| classify(e, "read a draft identity"))?
    .ok_or_else(|| StoreError::not_found(format!("identity {identity_id}")))
}

fn next_seq(tx: &Transaction, draft_id: &str) -> Result<i64, StoreError> {
    tx.query_row(
        "SELECT COALESCE(MAX(seq), 0) + 1 FROM draft_revisions WHERE draft_id = ?1",
        params![draft_id],
        |row| row.get(0),
    )
    .map_err(|e| classify(e, "number a draft revision"))
}

/// The recipients' canonical JSON: one `{address, name}` entry each
/// (spec/02), `name` null when absent.
fn recipients_json(recipients: &[DraftRecipient]) -> Result<String, StoreError> {
    let mut list = Vec::with_capacity(recipients.len());
    for recipient in recipients {
        if recipient.address.is_empty() {
            return Err(StoreError::InvalidInput {
                message: "a draft recipient needs an address".to_owned(),
            });
        }
        let mut entry = serde_json::Map::new();
        entry.insert("address".to_owned(), recipient.address.clone().into());
        match &recipient.name {
            Some(name) => entry.insert("name".to_owned(), name.clone().into()),
            None => entry.insert("name".to_owned(), serde_json::Value::Null),
        };
        list.push(serde_json::Value::Object(entry));
    }
    Ok(serde_json::Value::Array(list).to_string())
}

/// The attachments' canonical JSON: `{filename, media_type,
/// object_sha256, size}` each (spec/02; the map's sorted keys are the
/// canonical order).
fn attachments_json(
    attachments: &[DraftAttachment],
    base: &[AttachmentRef],
) -> Result<String, StoreError> {
    let mut list = Vec::with_capacity(attachments.len());
    for attachment in attachments {
        let reference = match attachment {
            DraftAttachment::New {
                object,
                filename,
                media_type,
            } => AttachmentRef {
                object_sha256: object.id().sha256_hex(),
                filename: filename.clone(),
                media_type: media_type.clone(),
                size: object.id().byte_length(),
            },
            DraftAttachment::Retained(reference) => {
                if !base.contains(reference) {
                    return Err(StoreError::conflict(
                        "attachment is not in the base revision",
                    ));
                }
                reference.clone()
            }
        };
        if reference.filename.is_empty() || reference.media_type.is_empty() {
            return Err(StoreError::InvalidInput {
                message: "a draft attachment needs a filename and media type".to_owned(),
            });
        }
        let mut entry = serde_json::Map::new();
        entry.insert("filename".to_owned(), reference.filename.into());
        entry.insert("media_type".to_owned(), reference.media_type.into());
        entry.insert("object_sha256".to_owned(), reference.object_sha256.into());
        entry.insert(
            "size".to_owned(),
            serde_json::Value::Number(reference.size.into()),
        );
        list.push(serde_json::Value::Object(entry));
    }
    Ok(serde_json::Value::Array(list).to_string())
}

/// A stored list back into slots; anything but a list of
/// `{address, name}` entries is damage the writer didn't make.
fn parse_recipients(json: &str) -> Result<Vec<DraftRecipient>, StoreError> {
    let parsed: serde_json::Value = serde_json::from_str(json)
        .map_err(|_| corrupt("a draft revision's recipients are not JSON"))?;
    let list = parsed
        .as_array()
        .ok_or_else(|| corrupt("a draft revision's recipients are not a list"))?;
    let mut recipients = Vec::with_capacity(list.len());
    for entry in list {
        let address = entry["address"]
            .as_str()
            .ok_or_else(|| corrupt("a draft recipient has no address"))?;
        let name = match &entry["name"] {
            serde_json::Value::Null => None,
            serde_json::Value::String(name) => Some(name.clone()),
            _ => return Err(corrupt("a draft recipient's name is not a string or null")),
        };
        recipients.push(DraftRecipient {
            address: address.to_owned(),
            name,
        });
    }
    Ok(recipients)
}

/// A stored `attachments_json` back into references; anything but a
/// list of complete entries is damage the writer didn't make. The
/// backup snapshot's reference read parses through this same shape.
pub(crate) fn parse_attachments(json: &str) -> Result<Vec<AttachmentRef>, StoreError> {
    let parsed: serde_json::Value = serde_json::from_str(json)
        .map_err(|_| corrupt("a draft revision's attachments are not JSON"))?;
    let list = parsed
        .as_array()
        .ok_or_else(|| corrupt("a draft revision's attachments are not a list"))?;
    let mut attachments = Vec::with_capacity(list.len());
    for entry in list {
        let string = |field: &str| {
            entry[field]
                .as_str()
                .ok_or_else(|| corrupt(format!("a draft attachment has no {field}")))
        };
        let size = entry["size"]
            .as_u64()
            .ok_or_else(|| corrupt("a draft attachment has no size"))?;
        attachments.push(AttachmentRef {
            object_sha256: string("object_sha256")?.to_owned(),
            filename: string("filename")?.to_owned(),
            media_type: string("media_type")?.to_owned(),
            size,
        });
    }
    Ok(attachments)
}

fn conflict_on_state(draft_id: &str, state: DraftState) -> StoreError {
    StoreError::conflict(format!("draft {draft_id} is {}", state.name()))
}

fn describe_base(base: Option<&str>) -> String {
    match base {
        Some(revision) => revision.to_string(),
        None => "the first save".to_owned(),
    }
}

fn corrupt(detail: impl Into<String>) -> StoreError {
    StoreError::corrupt(detail)
}

#[cfg(test)]
mod tests;
