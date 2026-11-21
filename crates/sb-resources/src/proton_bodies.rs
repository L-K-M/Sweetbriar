//! Proton body acquisition composition (spec/04-sync-proton "Sync
//! loop" step 3, "Reconstruction"): the store's body writes re-exported
//! for the engine (spec/01 rule 3), the read an open needs, and one
//! function that turns a fetched, decrypted message into its commit.
//!
//! The engine fetches and decrypts; this module reconstructs the
//! original, publishes it, parses its header block and summary, and
//! commits it in one write. Decrypted plaintext stays in
//! [`SecretBuffer`]s until the reconstruction is published, and no
//! failure detail ever carries message content.

use sb_mime::{
    Disposition as MimeDisposition, Limits, RECONSTRUCTION_VERSION, ReconstructError,
    ReconstructInput, ReconstructPart, reconstruct,
};
use sb_proton::{AttachmentMetadata, DecryptError, Disposition, FullMessage, SecretBuffer};
use sb_store::Timestamp;
use sb_store::{ObjectStore, StoreError, WritePriority, await_write};

use crate::sync::header_columns;
use crate::{Resources, acquire};

pub use sb_store::proton::{
    BodyBeforeMove, MAX_PENDING_BODIES, PendingBody, ProtonBody, ProtonBodyCommit,
    ProtonBodyFailureReason, ProtonOpenState, body_before_move, bump_key_generation,
    commit_proton_body, pending_proton_bodies, proton_open_state, record_body_failure,
};

/// Why a fetched body did not become a commit. The content reasons map
/// to `proton_body_failures.reason`; [`BodyFailure::Store`] is the
/// caller's own retry and is never recorded as a message failure.
#[derive(Debug, Clone)]
pub enum BodyFailure {
    /// No unlocked address key exists.
    NoKey,
    /// The body ciphertext is damaged or was not encrypted to these keys.
    Decrypt,
    /// An attachment failed to download or decrypt.
    Attachment,
    /// The body or an attachment exceeded the byte bound.
    TooLarge,
    /// The reconstruction input was invalid.
    Reconstruct,
    /// The object store or the commit failed; the caller retries.
    Store(StoreError),
}

impl BodyFailure {
    /// The reason to record, or `None` for a store failure the caller
    /// retries without recording.
    pub fn reason(&self) -> Option<ProtonBodyFailureReason> {
        match self {
            Self::NoKey => Some(ProtonBodyFailureReason::NoKey),
            Self::Decrypt => Some(ProtonBodyFailureReason::Decrypt),
            Self::Attachment => Some(ProtonBodyFailureReason::Attachment),
            Self::TooLarge => Some(ProtonBodyFailureReason::TooLarge),
            Self::Reconstruct => Some(ProtonBodyFailureReason::Reconstruct),
            Self::Store(_) => None,
        }
    }

    /// A static, redacted detail (spec/10): never message content.
    pub fn detail(&self) -> Option<&'static str> {
        match self {
            Self::NoKey => Some("no unlocked address key exists"),
            Self::Decrypt => Some("the body ciphertext is damaged"),
            Self::Attachment => Some("an attachment could not be downloaded or decrypted"),
            Self::TooLarge => Some("the body or an attachment exceeds the byte limit"),
            Self::Reconstruct => Some("the reconstruction input is invalid"),
            Self::Store(_) => None,
        }
    }
}

impl From<DecryptError> for BodyFailure {
    fn from(error: DecryptError) -> Self {
        match error {
            DecryptError::NoKey => Self::NoKey,
            DecryptError::Damaged => Self::Decrypt,
        }
    }
}

impl From<ReconstructError> for BodyFailure {
    fn from(error: ReconstructError) -> Self {
        match error {
            ReconstructError::TooLarge => Self::TooLarge,
            ReconstructError::InvalidInput(_) => Self::Reconstruct,
        }
    }
}

impl From<StoreError> for BodyFailure {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

/// Reconstruct, publish and commit one fetched Proton message (spec/04
/// "Sync loop" step 3). `body` and `attachments` are the decrypted
/// plaintext; they stay in their zeroizing buffers until the
/// reconstruction is published. The header block and summary come from
/// the reconstruction, as imports build theirs from the parsed message.
pub fn acquire_proton_body(
    resources: &Resources,
    account_id: &str,
    message: &FullMessage,
    body: SecretBuffer,
    attachments: Vec<(AttachmentMetadata, SecretBuffer)>,
    now: Timestamp,
) -> Result<ProtonBodyCommit, BodyFailure> {
    let parts: Vec<ReconstructPart<'_>> = attachments
        .iter()
        .map(|(metadata, data)| ReconstructPart {
            name: &metadata.name,
            mime_type: &metadata.mime_type,
            disposition: match metadata.disposition {
                Disposition::Inline => MimeDisposition::Inline,
                Disposition::Attachment => MimeDisposition::Attachment,
            },
            content_id: metadata.content_id.as_deref(),
            data: data.as_slice(),
        })
        .collect();
    let input = ReconstructInput {
        message_id: &message.metadata.id,
        header: message.header.as_bytes(),
        mime_type: &message.mime_type,
        body: body.as_slice(),
        attachments: &parts,
    };
    let reconstructed = SecretBuffer::new(reconstruct(&input, &Limits::default())?);
    let raw_header = header_block(reconstructed.as_slice());
    let columns = header_columns(raw_header);
    let summary = acquire::summary(&sb_mime::parse(
        reconstructed.as_slice(),
        &Limits::default(),
    ));
    let object = resources
        .store
        .objects()
        .publish(reconstructed.as_slice())?;
    let proton_body = ProtonBody {
        account_id: account_id.to_owned(),
        provider_key: message.metadata.id.clone(),
        object,
        reconstruction_version: RECONSTRUCTION_VERSION,
        raw_header: raw_header.to_vec(),
        columns,
        summary,
    };
    let committed = resources.write(WritePriority::Sync, move |tx| {
        commit_proton_body(tx, &proton_body, now)
    });
    Ok(await_write(committed)?)
}

/// The message's header section: everything before the first blank
/// line (CRLF or LF). A message without one is all header.
fn header_block(message: &[u8]) -> &[u8] {
    let crlf = message.windows(4).position(|window| window == b"\r\n\r\n");
    let lf = message.windows(2).position(|window| window == b"\n\n");
    match (crlf, lf) {
        (Some(crlf), Some(lf)) => &message[..crlf.min(lf)],
        (Some(crlf), None) => &message[..crlf],
        (None, Some(lf)) => &message[..lf],
        (None, None) => message,
    }
}
