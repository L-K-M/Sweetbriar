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

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use sb_resources::{
    AttachmentRef, ComposeFormat, DraftAttachment, DraftRecipient, DraftSource, DraftSummary,
    IdentityResolutionInputs, PublishedObject, ReaderAddress, Receipt, ReplySource, RevisionInput,
    StoreError, StoredDraft, WritePriority, create_draft, discard_draft, draft, drafts,
    identity_resolution_inputs, replay, reply_source, run_once, save_draft,
};

use crate::engine::validate_idempotency_key;
use crate::identity::{
    ComposeContext, ResolvedIdentity, address_key, reply_all_recipients, reply_recipients,
    resolve_identity,
};
use crate::{Engine, EngineError};

/// The spec/05 inline Content-ID length limit in bytes.
const MAX_CONTENT_ID_BYTES: usize = 250;

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
        let revision_id = self.resources().new_uuid_string();
        let now = self.resources().now();
        let key = idempotency_key.to_owned();
        let (compose_context, occurrence_id) = match context {
            DraftSource::New => (ComposeContext::New, None),
            DraftSource::Reply { occurrence_id } => (ComposeContext::Reply, Some(occurrence_id)),
            DraftSource::ReplyAll { occurrence_id } => {
                (ComposeContext::ReplyAll, Some(occurrence_id))
            }
            DraftSource::Forward { occurrence_id } => {
                (ComposeContext::Forward, Some(occurrence_id))
            }
        };
        #[cfg(test)]
        let hook = self.drafts_write_hook();
        self.command(move |worker| async move {
            #[cfg(test)]
            draft_write_point(&hook, DraftWritePoint::BeforeWrite);
            #[cfg(test)]
            let write_hook = hook.clone();
            // A replayed key answers its first draft before any source
            // read, which a changed context would now refuse (spec/07).
            let replay_key = key.clone();
            let replayed = worker
                .blocking(move |resources| {
                    resources.read(|tx| replay(tx, &replay_key, CREATE_DRAFT))
                })
                .await??;
            if let Some(receipt) = replayed {
                return Ok(DraftCreated {
                    draft_id: receipt_id(&receipt, "draft_id")?,
                    receipt,
                });
            }
            // A reply or forward gathers its source's stored headers
            // and its acquired plain text before the write: the
            // rendition is rendered in memory, never published as a
            // lease, and a missing occurrence stays `NotFound`
            // (spec/05).
            let prepared = match occurrence_id {
                Some(occurrence_id) => {
                    let source = worker
                        .blocking(move |resources| {
                            resources.read(|tx| reply_source(tx, occurrence_id))
                        })
                        .await??;
                    let message_id = source.message_id;
                    // Not acquired is the attribution alone (spec/05);
                    // any other failure is the command's.
                    let text = match worker
                        .blocking(move |resources| resources.acquired_plain_text(message_id))
                        .await?
                    {
                        Ok(text) => Some(text),
                        Err(StoreError::Unavailable { .. }) => None,
                        Err(error) => return Err(error.into()),
                    };
                    Some((occurrence_id, source, text))
                }
                None => None,
            };
            let id = draft_id.clone();
            let receipt = worker
                .resources()
                .write(WritePriority::User, move |tx| {
                    let receipt = run_once(tx, &key, CREATE_DRAFT, now, |tx| {
                        #[cfg(test)]
                        draft_write_point(&write_hook, DraftWritePoint::InsideBeforeEffect);
                        create_draft(tx, &id, &context, now)?;
                        if let Some((occurrence_id, source, text)) = prepared.as_ref() {
                            let inputs =
                                identity_resolution_inputs(tx, Some(*occurrence_id), None, None)?;
                            let identity_id = match resolve_identity(compose_context, &inputs) {
                                ResolvedIdentity::Chosen(identity) => Some(identity),
                                ResolvedIdentity::ChooseSendingIdentity => None,
                            };
                            let email = identity_id.as_ref().and_then(|identity| {
                                inputs
                                    .identities
                                    .iter()
                                    .find(|route| &route.identity_id == identity)
                                    .map(|route| route.email.clone())
                            });
                            let (to, cc) =
                                draft_recipients(compose_context, &inputs, email.as_deref());
                            let (subject, body_source) = match compose_context {
                                ComposeContext::Forward => (
                                    forward_subject(source.subject.as_deref()),
                                    forward_body(source, text.as_deref()),
                                ),
                                _ => (
                                    reply_subject(source.subject.as_deref()),
                                    reply_body(source, text.as_deref()),
                                ),
                            };
                            save_draft(
                                tx,
                                &id,
                                None,
                                RevisionInput {
                                    revision_id: revision_id.clone(),
                                    identity_id,
                                    format: ComposeFormat::Plain,
                                    plain_flowed: true,
                                    subject,
                                    to,
                                    cc,
                                    bcc: Vec::new(),
                                    body_source,
                                    // Forwarded attachments are a later
                                    // step (spec/05).
                                    attachments: Vec::new(),
                                },
                                now,
                            )?;
                        }
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

    /// Stream a selected regular file to the same durable publication path
    /// as [`Engine::publish_draft_attachment`]. The final component is not
    /// followed if it is a symbolic link. File I/O runs on the blocking pool.
    pub async fn publish_draft_attachment_file(
        &self,
        path: std::path::PathBuf,
    ) -> Result<PublishedObject, EngineError> {
        self.read_through(move |resources| resources.publish_draft_attachment_file(&path))
            .await
            .map_err(|error| match error {
                EngineError::InvalidInput { message, .. } => {
                    EngineError::invalid_input("path", message)
                }
                other => other,
            })
    }

    /// `republish_draft_attachment(draft_id, revision_id, reference)`
    /// (spec/07): re-publish one stored attachment reference from an
    /// exact immutable draft revision. An older revision, or one from
    /// a terminal draft, remains valid because this creates a new
    /// publication, never a mutation of that draft.
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

/// A reply or reply-all first revision's recipients (spec/05): Reply
/// puts its whole list in To; Reply All puts the reply list and the
/// original's To in To and only the entries that appear in the original
/// Cc and nowhere else in Cc. The identity's address is excluded by
/// [`reply_all_recipients`]; an entry without an address is dropped.
fn draft_recipients(
    context: ComposeContext,
    inputs: &IdentityResolutionInputs,
    exclude: Option<&str>,
) -> (Vec<DraftRecipient>, Vec<DraftRecipient>) {
    let Some(source) = inputs.source.as_ref() else {
        return (Vec::new(), Vec::new());
    };
    let headers = &source.headers;
    match context {
        ComposeContext::Reply => (
            reply_recipients(headers)
                .iter()
                .filter_map(reader_recipient)
                .collect(),
            Vec::new(),
        ),
        ComposeContext::ReplyAll => {
            let reply_list = reply_recipients(headers);
            let reply_keys = address_keys(&reply_list);
            let to_keys = address_keys(&headers.to);
            let cc_keys = address_keys(&headers.cc);
            let mut to = Vec::new();
            let mut cc = Vec::new();
            for entry in reply_all_recipients(headers, exclude.unwrap_or("")) {
                let key = entry.address.as_deref().and_then(address_key);
                let cc_only = key
                    .as_deref()
                    .map(|key| {
                        cc_keys.contains(key) && !reply_keys.contains(key) && !to_keys.contains(key)
                    })
                    .unwrap_or(false);
                if cc_only {
                    if let Some(recipient) = reader_recipient(&entry) {
                        cc.push(recipient);
                    }
                } else if let Some(recipient) = reader_recipient(&entry) {
                    to.push(recipient);
                }
            }
            (to, cc)
        }
        _ => (Vec::new(), Vec::new()),
    }
}

fn address_keys(addresses: &[ReaderAddress]) -> BTreeSet<String> {
    addresses
        .iter()
        .filter_map(|entry| entry.address.as_deref().and_then(address_key))
        .collect()
}

/// One stored header address as a draft recipient; an entry without
/// an address has nothing to send to and is dropped (spec/05).
fn reader_recipient(entry: &ReaderAddress) -> Option<DraftRecipient> {
    let address = entry
        .address
        .clone()
        .filter(|address| !address.is_empty())?;
    Some(DraftRecipient {
        address,
        name: entry.name.clone(),
    })
}

/// A reply's subject: trimmed, after `Re: ` unless it already starts
/// with that prefix compared without case; empty becomes `Re:`
/// (spec/05). A forward's is the same rule with `Fwd: `.
fn reply_subject(subject: Option<&str>) -> String {
    prefixed_subject(subject, "Re: ", "re:")
}

fn forward_subject(subject: Option<&str>) -> String {
    prefixed_subject(subject, "Fwd: ", "fwd:")
}

fn prefixed_subject(subject: Option<&str>, prefix: &str, marker: &str) -> String {
    let trimmed = subject.unwrap_or("").trim();
    if trimmed.is_empty() {
        return prefix.trim_end().to_owned();
    }
    let already = trimmed
        .get(..marker.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(marker));
    if already {
        trimmed.to_owned()
    } else {
        format!("{prefix}{trimmed}")
    }
}

/// A reply's body: two empty lines, the attribution line, then each
/// rendition line prefixed `> ` (an empty line becomes `>`); without a
/// rendition the attribution stands alone (spec/05).
fn reply_body(source: &ReplySource, text: Option<&str>) -> String {
    let attribution = format!(
        "\n\nOn {}, {} wrote:",
        source.date_text.as_deref().unwrap_or(""),
        address_text(&source.from)
    );
    let quoted = text.map(quote_rendition).unwrap_or_default();
    if quoted.is_empty() {
        attribution
    } else {
        format!("{attribution}\n{quoted}")
    }
}

/// A forward's body: two empty lines, the forwarded-message block with
/// the original's non-empty From, Date, Subject, To and Cc lines, then
/// the rendition unquoted; without one the block stands alone
/// (spec/05).
fn forward_body(source: &ReplySource, text: Option<&str>) -> String {
    let mut lines = vec!["---------- Forwarded message ----------".to_owned()];
    address_line(&mut lines, "From:", &source.from);
    if let Some(date) = source.date_text.as_deref().filter(|date| !date.is_empty()) {
        lines.push(format!("Date: {date}"));
    }
    if let Some(subject) = source
        .subject
        .as_deref()
        .map(str::trim)
        .filter(|subject| !subject.is_empty())
    {
        lines.push(format!("Subject: {subject}"));
    }
    address_line(&mut lines, "To:", &source.to);
    address_line(&mut lines, "Cc:", &source.cc);
    let mut body = format!("\n\n{}", lines.join("\n"));
    let text = text.map(|text| text.trim_end_matches(['\n', '\r']));
    if let Some(text) = text.filter(|text| !text.is_empty()) {
        body.push_str("\n\n");
        body.push_str(text);
    }
    body
}

fn address_line(lines: &mut Vec<String>, label: &str, addresses: &[ReaderAddress]) {
    let text = address_text(addresses);
    if !text.is_empty() {
        lines.push(format!("{label} {text}"));
    }
}

/// `Name <address>` or the bare address, joined with `, ` (spec/05);
/// entries without an address are skipped.
fn address_text(addresses: &[ReaderAddress]) -> String {
    addresses
        .iter()
        .filter_map(|entry| {
            let address = entry.address.as_deref()?;
            let name = entry.name.as_deref().filter(|name| !name.is_empty());
            Some(match name {
                Some(name) => format!("{name} <{address}>"),
                None => address.to_owned(),
            })
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The rendition quoted: trailing newlines trimmed, each line prefixed
/// `> `, an empty line just `>`. A CRLF or a lone CR ends a line too.
fn quote_rendition(text: &str) -> String {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let trimmed = normalized.trim_end_matches(['\n', '\r']);
    if trimmed.is_empty() {
        return String::new();
    }
    trimmed
        .split('\n')
        .map(|line| {
            if line.is_empty() {
                ">".to_owned()
            } else {
                format!("> {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
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
    let mut content_ids = BTreeSet::new();
    for attachment in &content.attachments {
        let (filename, media_type, content_id) = match attachment {
            DraftAttachment::New {
                filename,
                media_type,
                content_id,
                ..
            } => (filename, media_type, content_id.as_deref()),
            DraftAttachment::Retained(reference) => (
                &reference.filename,
                &reference.media_type,
                reference.content_id.as_deref(),
            ),
        };
        let Some(content_id) = content_id else {
            continue;
        };
        if content_id.len() > MAX_CONTENT_ID_BYTES {
            return Err(EngineError::invalid_input(
                "attachments",
                format!(
                    "attachment {filename:?}: the Content-ID is longer than \
                     {MAX_CONTENT_ID_BYTES} bytes"
                ),
            ));
        }
        if !valid_content_id(content_id) {
            return Err(EngineError::invalid_input(
                "attachments",
                format!("attachment {filename:?}: the Content-ID is not dot-atom@dot-atom"),
            ));
        }
        if !content_ids.insert(content_id) {
            return Err(EngineError::invalid_input(
                "attachments",
                format!("attachment {filename:?}: the Content-ID is already used in this draft"),
            ));
        }
        // Media types compare without regard to case (RFC 2045).
        if !matches!(
            media_type.to_ascii_lowercase().as_str(),
            "image/png" | "image/jpeg" | "image/gif"
        ) {
            return Err(EngineError::invalid_input(
                "attachments",
                format!(
                    "attachment {filename:?}: the Content-ID needs an image/png, \
                     image/jpeg or image/gif media type"
                ),
            ));
        }
    }
    Ok(())
}

/// The spec/05 inline Content-ID shape: a dot-atom `@` dot-atom of
/// RFC 5322 atext ASCII, with exactly one `@`.
fn valid_content_id(id: &str) -> bool {
    let Some((left, right)) = id.split_once('@') else {
        return false;
    };
    if right.contains('@') {
        return false;
    }
    valid_dot_atom(left) && valid_dot_atom(right)
}

/// One or more non-empty atoms joined by single dots.
fn valid_dot_atom(atoms: &str) -> bool {
    if atoms.is_empty() {
        return false;
    }
    atoms
        .split('.')
        .all(|atom| !atom.is_empty() && atom.bytes().all(is_atext))
}

/// RFC 5322 atext: ASCII letters, digits and the spec/05 punctuation.
fn is_atext(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'/'
                | b'='
                | b'?'
                | b'^'
                | b'_'
                | b'`'
                | b'{'
                | b'|'
                | b'}'
                | b'~'
        )
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
pub(crate) fn encode_receipt_id(field: &str, id: &str) -> String {
    let mut body = serde_json::Map::new();
    body.insert(field.to_owned(), id.into());
    serde_json::Value::Object(body).to_string()
}

/// The id a stored receipt names under `field` (spec/07): a replayed
/// command answers this stored id, never a fresh argument. Malformed
/// stored JSON or a missing, empty or non-string id is `Corrupt` —
/// never an invented id.
pub(crate) fn receipt_id(receipt: &Receipt, field: &str) -> Result<String, EngineError> {
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
