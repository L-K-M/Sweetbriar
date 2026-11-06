//! The drafts domain (spec/02 "Drafts", spec/05 "Composing", spec/07):
//! `create_draft`, `save_draft` and `discard_draft` — one counted
//! command each under its idempotency key, so a repeated key returns
//! the stored receipt and a dropped caller cannot strand an accepted
//! write — and the `drafts()`/`draft()` queries the sidebar's Drafts
//! (local) list reads. Attachment bytes publish through
//! [`Engine::publish_draft_attachment`], or republish from a stored
//! revision through [`Engine::republish_draft_attachment`], on the
//! blocking pool, before the revision that references them: no file
//! I/O ever runs inside a write transaction.

use std::sync::{Arc, Mutex};

use sb_resources::{
    AttachmentRef, ComposeFormat, DraftAttachment, DraftRecipient, DraftSource, DraftSummary,
    PublishedObject, Receipt, RevisionInput, StoreError, StoredDraft, WritePriority, create_draft,
    discard_draft, draft, drafts, run_once, save_draft,
};

use crate::engine::validate_idempotency_key;
use crate::{Engine, EngineError};

/// The commands' names in `command_receipts` (spec/07).
const CREATE_DRAFT: &str = "create_draft";
const SAVE_DRAFT: &str = "save_draft";
const DISCARD_DRAFT: &str = "discard_draft";

/// A hook fired at each [`DraftWritePoint`] of a draft command's
/// write (spec/11 crash cuts); `None` outside tests.
#[cfg(test)]
pub(crate) type DraftWriteHook = Arc<dyn Fn(DraftWritePoint) + Send + Sync>;

/// A point at or inside a draft command's write transaction (spec/11
/// cuts "before, inside and after each write transaction"): the write
/// does no filesystem operations — SQLite does its own I/O — so a
/// hook must fire a counted one for a `CutFs` sweep to reach inside.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DraftWritePoint {
    /// The write is about to be submitted; its transaction has not begun.
    BeforeWrite,
    /// Inside the transaction, before the command's effect.
    InsideBeforeEffect,
    /// Inside it after the effect, before the receipt row.
    InsideBeforeReceipt,
    /// Inside it after the receipt row, before commit.
    InsideBeforeCommit,
    /// The write resolved: its transaction committed.
    AfterWrite,
}

/// Fire `point` on the snapshot a command took of [`Engine::drafts_hook`].
#[cfg(test)]
fn draft_write_point(hook: &Option<DraftWriteHook>, point: DraftWritePoint) {
    if let Some(hook) = hook {
        hook(point);
    }
}

/// What `create_draft` answers: its receipt and the draft id the
/// receipt names — the stored id, so a replayed key still names the
/// first command's draft.
#[derive(Debug, Clone)]
pub struct DraftCreated {
    receipt: Receipt,
    draft_id: String,
}

impl DraftCreated {
    /// The durable receipt (`{"draft_id": ...}`).
    pub fn receipt(&self) -> &Receipt {
        &self.receipt
    }

    /// The created draft's id.
    pub fn draft_id(&self) -> &str {
        &self.draft_id
    }
}

/// What `save_draft` answers: its receipt and the revision id the
/// receipt names — the stored id on a replayed key.
#[derive(Debug, Clone)]
pub struct DraftSaved {
    receipt: Receipt,
    revision_id: String,
}

impl DraftSaved {
    /// The durable receipt (`{"revision_id": ...}`).
    pub fn receipt(&self) -> &Receipt {
        &self.receipt
    }

    /// The written revision's id.
    pub fn revision_id(&self) -> &str {
        &self.revision_id
    }
}

/// One save's content (spec/07 `save_draft`'s `content`): everything a
/// revision records but its ids — the engine assigns the revision id
/// and checks the base — over the store's [`RevisionInput`]. `to`,
/// `cc` and `bcc` entries need an address; `attachments` entries a
/// filename and a media type, and each object must carry its live
/// publication reservation into the save.
pub struct DraftContent {
    /// The sending identity the route snapshots (`None` keeps the
    /// revision unrouted: a draft with no sending identity, spec/02).
    pub identity_id: Option<String>,
    pub format: ComposeFormat,
    /// `false` is the per-draft literal plain-text option (spec/05).
    pub plain_flowed: bool,
    pub subject: String,
    pub to: Vec<DraftRecipient>,
    pub cc: Vec<DraftRecipient>,
    pub bcc: Vec<DraftRecipient>,
    /// Markdown or plain source as composed (spec/05); the outgoing
    /// MIME is built from it at send.
    pub body_source: String,
    /// The published objects this revision references.
    pub attachments: Vec<DraftAttachment>,
}

impl Engine {
    /// `create_draft(context)` (spec/07): a new `editing` draft, its id
    /// minted here. `context` is `DraftSource::New` or a reply,
    /// reply-all or forward source occurrence, which must exist
    /// (`NotFound`); a forward resolves to the logical message. The
    /// receipt answers `{"draft_id": ...}` and replays it.
    pub async fn create_draft(
        &self,
        idempotency_key: &str,
        context: DraftSource,
    ) -> Result<DraftCreated, EngineError> {
        validate_idempotency_key(idempotency_key)?;
        let draft_id = self.resources().new_uuid_string();
        let now = self.resources().now();
        let key = idempotency_key.to_owned();
        #[cfg(test)]
        let hook = self.drafts_write_hook();
        self.command(move |worker| async move {
            #[cfg(test)]
            draft_write_point(&hook, DraftWritePoint::BeforeWrite);
            #[cfg(test)]
            let write_hook = hook.clone();
            let id = draft_id.clone();
            let receipt = worker
                .resources()
                .write(WritePriority::User, move |tx| {
                    let receipt = run_once(tx, &key, CREATE_DRAFT, now, |tx| {
                        #[cfg(test)]
                        draft_write_point(&write_hook, DraftWritePoint::InsideBeforeEffect);
                        create_draft(tx, &id, &context, now)?;
                        #[cfg(test)]
                        draft_write_point(&write_hook, DraftWritePoint::InsideBeforeReceipt);
                        Ok(encode_receipt_id("draft_id", &id))
                    })?;
                    #[cfg(test)]
                    draft_write_point(&write_hook, DraftWritePoint::InsideBeforeCommit);
                    Ok(receipt)
                })
                .await?;
            #[cfg(test)]
            draft_write_point(&hook, DraftWritePoint::AfterWrite);
            Ok(DraftCreated {
                draft_id: receipt_id(&receipt, "draft_id")?,
                receipt,
            })
        })
        .await
    }

    /// `save_draft(draft_id, base_revision_id, content)` (spec/07):
    /// append an immutable revision and make it the draft's current
    /// one, in the transaction that also spends each attachment's
    /// publication reservation. `base` must name the current revision
    /// (`None` before the first save); anything else is `Conflict`.
    /// An unknown draft or identity is `NotFound`; a `sent` or
    /// `discarded` draft refuses the same way as a wrong base. The
    /// receipt answers `{"revision_id": ...}` and replays it.
    pub async fn save_draft(
        &self,
        idempotency_key: &str,
        draft_id: &str,
        base_revision_id: Option<&str>,
        content: DraftContent,
    ) -> Result<DraftSaved, EngineError> {
        validate_idempotency_key(idempotency_key)?;
        let revision_id = self.resources().new_uuid_string();
        let now = self.resources().now();
        let (key, id, base) = (
            idempotency_key.to_owned(),
            draft_id.to_owned(),
            base_revision_id.map(str::to_owned),
        );
        #[cfg(test)]
        let hook = self.drafts_write_hook();
        self.command(move |worker| async move {
            #[cfg(test)]
            draft_write_point(&hook, DraftWritePoint::BeforeWrite);
            #[cfg(test)]
            let write_hook = hook.clone();
            let revision = revision_id.clone();
            let validation = Arc::new(Mutex::new(None));
            let write_validation = Arc::clone(&validation);
            let receipt = worker
                .resources()
                .write(WritePriority::User, move |tx| {
                    let receipt = run_once(tx, &key, SAVE_DRAFT, now, |tx| {
                        record_validation(
                            &write_validation,
                            validate_draft(&id, base.as_deref(), &content),
                        )?;
                        #[cfg(test)]
                        draft_write_point(&write_hook, DraftWritePoint::InsideBeforeEffect);
                        let result = encode_receipt_id("revision_id", &revision);
                        save_draft(
                            tx,
                            &id,
                            base.as_deref(),
                            RevisionInput {
                                revision_id: revision,
                                identity_id: content.identity_id,
                                format: content.format,
                                plain_flowed: content.plain_flowed,
                                subject: content.subject,
                                to: content.to,
                                cc: content.cc,
                                bcc: content.bcc,
                                body_source: content.body_source,
                                attachments: content.attachments,
                            },
                            now,
                        )?;
                        #[cfg(test)]
                        draft_write_point(&write_hook, DraftWritePoint::InsideBeforeReceipt);
                        Ok(result)
                    })?;
                    #[cfg(test)]
                    draft_write_point(&write_hook, DraftWritePoint::InsideBeforeCommit);
                    Ok(receipt)
                })
                .await;
            let receipt = finish_validation(receipt, &validation)?;
            #[cfg(test)]
            draft_write_point(&hook, DraftWritePoint::AfterWrite);
            Ok(DraftSaved {
                revision_id: receipt_id(&receipt, "revision_id")?,
                receipt,
            })
        })
        .await
    }

    /// `discard_draft(id)` (spec/07, 05): refuse `Conflict` while a
    /// live attempt exists (`held`, `queued`, `submitting` or
    /// unresolved `outcome_unknown`) or the draft already sits at a
    /// terminal state; otherwise mark it `discarded`, keeping every
    /// revision. The receipt answers `{"draft_id": ...}`.
    pub async fn discard_draft(
        &self,
        idempotency_key: &str,
        draft_id: &str,
    ) -> Result<Receipt, EngineError> {
        validate_idempotency_key(idempotency_key)?;
        let now = self.resources().now();
        let (key, id) = (idempotency_key.to_owned(), draft_id.to_owned());
        #[cfg(test)]
        let hook = self.drafts_write_hook();
        self.command(move |worker| async move {
            #[cfg(test)]
            draft_write_point(&hook, DraftWritePoint::BeforeWrite);
            #[cfg(test)]
            let write_hook = hook.clone();
            let validation = Arc::new(Mutex::new(None));
            let write_validation = Arc::clone(&validation);
            let receipt = worker
                .resources()
                .write(WritePriority::User, move |tx| {
                    let receipt = run_once(tx, &key, DISCARD_DRAFT, now, |tx| {
                        record_validation(&write_validation, validate_draft_id(&id))?;
                        #[cfg(test)]
                        draft_write_point(&write_hook, DraftWritePoint::InsideBeforeEffect);
                        discard_draft(tx, &id, now)?;
                        #[cfg(test)]
                        draft_write_point(&write_hook, DraftWritePoint::InsideBeforeReceipt);
                        Ok(encode_receipt_id("draft_id", &id))
                    })?;
                    #[cfg(test)]
                    draft_write_point(&write_hook, DraftWritePoint::InsideBeforeCommit);
                    Ok(receipt)
                })
                .await;
            let receipt = finish_validation(receipt, &validation)?;
            #[cfg(test)]
            draft_write_point(&hook, DraftWritePoint::AfterWrite);
            receipt_id(&receipt, "draft_id")?;
            Ok(receipt)
        })
        .await
    }

    /// Publish one attachment's bytes for a later save (spec/02:
    /// attachments are published to the object store before the
    /// revision that references them). The returned
    /// [`PublishedObject`]'s reservation pins the object until the
    /// save's transaction spends it — a revision cannot reference an
    /// unpublished object. Blocking file I/O on the blocking pool,
    /// never inside a write transaction.
    pub async fn publish_draft_attachment(
        &self,
        bytes: impl std::io::Read + Send + 'static,
    ) -> Result<PublishedObject, EngineError> {
        self.read_through(move |resources| resources.publish_draft_attachment(bytes))
            .await
    }

    /// `republish_draft_attachment(draft_id, revision_id, reference)`
    /// (spec/07): re-publish one stored attachment reference from an
    /// exact immutable draft revision — an older, noncurrent or
    /// terminal source revision is a valid source because this
    /// creates a new publication, never a mutation of that draft.
    /// `reference` must match one of that revision's stored
    /// attachments in hash, size, filename and media type: a revision
    /// the draft does not have is `NotFound`, a reference that is not
    /// a member is `Conflict`. The returned [`PublishedObject`]
    /// carries a fresh reservation a following save spends once, and
    /// each call reserves independently. No idempotency key and no
    /// receipt: only the publication and its reservation are written.
    /// Blocking file I/O on the blocking pool, like
    /// [`Engine::publish_draft_attachment`].
    pub async fn republish_draft_attachment(
        &self,
        draft_id: &str,
        revision_id: &str,
        reference: AttachmentRef,
    ) -> Result<PublishedObject, EngineError> {
        let (draft_id, revision_id) = (draft_id.to_owned(), revision_id.to_owned());
        self.read_through(move |resources| {
            resources.republish_draft_attachment(&draft_id, &revision_id, &reference)
        })
        .await
    }

    /// `drafts()` (spec/07): every `editing` or `queued` draft, newest
    /// update first — state, route, subject, recipients and any live
    /// attempt's state, the sidebar's Drafts (local) rows.
    pub async fn drafts(&self) -> Result<Vec<DraftSummary>, EngineError> {
        self.read_through(|resources| resources.read(drafts)).await
    }

    /// `draft(draft_id)` (spec/07): the draft with its current
    /// revision's full content and any live attempt, in any state — a
    /// compose window restores against it. An unknown id is `NotFound`.
    pub async fn draft(&self, draft_id: &str) -> Result<StoredDraft, EngineError> {
        let id = draft_id.to_owned();
        self.read_through(move |resources| resources.read(|tx| draft(tx, &id)))
            .await?
            .ok_or_else(|| EngineError::NotFound {
                what: format!("draft {draft_id}"),
            })
    }

    /// The installed draft-write hook, `None` outside tests.
    #[cfg(test)]
    pub(crate) fn drafts_write_hook(&self) -> Option<DraftWriteHook> {
        self.drafts_hook
            .lock()
            .expect("a panicking thread held the drafts hook")
            .clone()
    }
}

/// `save_draft`'s fresh-command validation: the fields the store would
/// otherwise report under `data_root` (its only invalid-input field),
/// named for the caller instead.
fn validate_draft(
    draft_id: &str,
    base_revision_id: Option<&str>,
    content: &DraftContent,
) -> Result<(), EngineError> {
    validate_draft_id(draft_id)?;
    if let Some("") = base_revision_id {
        return Err(EngineError::invalid_input(
            "base_revision_id",
            "a base revision id is required when a base is named",
        ));
    }
    if let Some("") = content.identity_id.as_deref() {
        return Err(EngineError::invalid_input(
            "identity_id",
            "an identity id is required when one is chosen",
        ));
    }
    for (field, recipients) in [
        ("to", &content.to),
        ("cc", &content.cc),
        ("bcc", &content.bcc),
    ] {
        if recipients.iter().any(|r| r.address.is_empty()) {
            return Err(EngineError::invalid_input(
                field,
                "a draft recipient needs an address",
            ));
        }
    }
    if content.attachments.iter().any(|a| match a {
        DraftAttachment::New {
            filename,
            media_type,
            ..
        } => filename.is_empty() || media_type.is_empty(),
        DraftAttachment::Retained(reference) => {
            reference.filename.is_empty() || reference.media_type.is_empty()
        }
    }) {
        return Err(EngineError::invalid_input(
            "attachments",
            "a draft attachment needs a filename and media type",
        ));
    }
    Ok(())
}

fn validate_draft_id(draft_id: &str) -> Result<(), EngineError> {
    if draft_id.is_empty() {
        return Err(EngineError::invalid_input(
            "draft_id",
            "a draft id is required",
        ));
    }
    Ok(())
}

// Return a Store error to roll back, retaining the typed field for the caller.
const VALIDATION_ROLLBACK: &str = "draft validation rejected the command";

fn record_validation(
    slot: &Mutex<Option<EngineError>>,
    result: Result<(), EngineError>,
) -> Result<(), StoreError> {
    if let Err(error) = result {
        *slot.lock().expect("draft validation lock poisoned") = Some(error);
        // The writer returns a closure error unchanged and never retries it.
        // This private marker only rolls back; `finish_validation` recovers
        // the caller-facing error from `slot` before it can escape.
        return Err(StoreError::internal(VALIDATION_ROLLBACK));
    }
    Ok(())
}

fn finish_validation(
    result: Result<Receipt, StoreError>,
    slot: &Mutex<Option<EngineError>>,
) -> Result<Receipt, EngineError> {
    let recorded = slot.lock().expect("draft validation lock poisoned").take();
    match (result, recorded) {
        (Ok(receipt), None) => Ok(receipt),
        (Err(error), None) => Err(error.into()),
        (_, Some(error)) => Err(error),
    }
}

/// `{"<field>":"<id>"}`: the receipt's body (spec/07). The id is
/// encoded as JSON, never pasted into a template — a caller-supplied
/// draft id can hold anything.
fn encode_receipt_id(field: &str, id: &str) -> String {
    let mut body = serde_json::Map::new();
    body.insert(field.to_owned(), id.into());
    serde_json::Value::Object(body).to_string()
}

/// The id a stored receipt names under `field` (spec/07): a replayed
/// command answers this stored id, never a fresh argument. Malformed
/// stored JSON or a missing, empty or non-string id is `Corrupt` —
/// never an invented id.
fn receipt_id(receipt: &Receipt, field: &str) -> Result<String, EngineError> {
    let parsed: serde_json::Value = serde_json::from_str(receipt.result_json())
        .map_err(|_| EngineError::corrupt("a stored command receipt is not JSON"))?;
    match parsed[field].as_str() {
        Some(id) if !id.is_empty() => Ok(id.to_owned()),
        _ => Err(EngineError::corrupt(format!(
            "a stored command receipt names no {field}"
        ))),
    }
}

#[cfg(test)]
mod tests;
