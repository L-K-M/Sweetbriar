//! Owned composer query DTOs; no leaf or engine type crosses L2 (spec/07).
use crate::{AccountKind, Core, CoreError, DraftRecipient, IdentitySendable};

/// Complete retained identity metadata, independent of its appearance tokens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposeIdentity {
    pub identity_id: String,
    pub account_id: String,
    pub account_name: String,
    pub account_kind: AccountKind,
    pub email: String,
    pub display_name: String,
    pub reply_to: Option<String>,
    pub signature_source: Option<String>,
    pub is_default: bool,
    pub sendable: IdentitySendable,
    pub revision: i64,
}

/// One immutable whole catalog and its optional new-draft default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposeCatalog {
    pub identities: Vec<ComposeIdentity>,
    pub default_identity_id: Option<String>,
}

impl Core {
    /// `compose_catalog`: no mutation, arbitrary first-sender fallback or secret.
    pub async fn compose_catalog(
        &self,
        preferred_account_id: Option<&str>,
    ) -> Result<ComposeCatalog, CoreError> {
        let catalog = self.engine().compose_catalog(preferred_account_id).await?;
        Ok(ComposeCatalog {
            default_identity_id: catalog.default_identity_id().map(str::to_owned),
            identities: catalog
                .identities()
                .iter()
                .map(|i| ComposeIdentity {
                    identity_id: i.identity_id().to_owned(),
                    account_id: i.account_id().to_owned(),
                    account_name: i.account_name().to_owned(),
                    account_kind: match i.account_kind() {
                        sb_engine::AccountKind::Local => AccountKind::Local,
                        sb_engine::AccountKind::Imap => AccountKind::Imap,
                    },
                    email: i.email().to_owned(),
                    display_name: i.display_name().to_owned(),
                    reply_to: i.reply_to().map(str::to_owned),
                    signature_source: i.signature_source().map(str::to_owned),
                    is_default: i.is_default(),
                    revision: i.revision(),
                    sendable: match i.sendable() {
                        sb_engine::IdentitySendable::Validated => IdentitySendable::Validated,
                        sb_engine::IdentitySendable::Provisional => IdentitySendable::Provisional,
                        sb_engine::IdentitySendable::Unavailable => IdentitySendable::Unavailable,
                    },
                })
                .collect(),
        })
    }

    /// `recipient_completions`: strict whole-answer literal history matches.
    pub async fn recipient_completions(
        &self,
        prefix: &str,
        limit: u32,
    ) -> Result<Vec<DraftRecipient>, CoreError> {
        Ok(self
            .engine()
            .recipient_completions(prefix, limit)
            .await?
            .into_iter()
            .map(DraftRecipient::from_engine)
            .collect())
    }

    /// `parse_draft_recipients`: no permissive recovery or discarded members.
    pub async fn parse_draft_recipients(
        &self,
        text: &str,
    ) -> Result<Vec<DraftRecipient>, CoreError> {
        Ok(self
            .engine()
            .parse_draft_recipients(text)
            .await?
            .into_iter()
            .map(DraftRecipient::from_engine)
            .collect())
    }
}
