//! Owned composer catalog over a cancellable query (spec/07, M1.9c2b2).
use crate::{AccountKind, CancelToken, Core, CoreError, DraftRecipient, IdentitySendable};
use std::sync::Arc;

/// Complete retained identity metadata; not inferred from appearance tokens.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
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
impl From<sb_service::ComposeIdentity> for ComposeIdentity {
    fn from(value: sb_service::ComposeIdentity) -> Self {
        Self {
            identity_id: value.identity_id,
            account_id: value.account_id,
            account_name: value.account_name,
            account_kind: match value.account_kind {
                sb_service::AccountKind::Local => AccountKind::Local,
                sb_service::AccountKind::Imap => AccountKind::Imap,
            },
            email: value.email,
            display_name: value.display_name,
            reply_to: value.reply_to,
            signature_source: value.signature_source,
            is_default: value.is_default,
            sendable: match value.sendable {
                sb_service::IdentitySendable::Validated => IdentitySendable::Validated,
                sb_service::IdentitySendable::Provisional => IdentitySendable::Provisional,
                sb_service::IdentitySendable::Unavailable => IdentitySendable::Unavailable,
            },
            revision: value.revision,
        }
    }
}

/// Whole catalog plus its optional, explicitly resolved new-draft default.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ComposeCatalog {
    pub identities: Vec<ComposeIdentity>,
    pub default_identity_id: Option<String>,
}
impl From<sb_service::ComposeCatalog> for ComposeCatalog {
    fn from(value: sb_service::ComposeCatalog) -> Self {
        Self {
            identities: value.identities.into_iter().map(Into::into).collect(),
            default_identity_id: value.default_identity_id,
        }
    }
}

#[uniffi::export]
impl Core {
    /// Metadata/defaults only. Cancellation/failure is never an empty success.
    pub async fn compose_catalog(
        &self,
        token: Arc<CancelToken>,
        preferred_account_id: Option<String>,
    ) -> Result<ComposeCatalog, CoreError> {
        let value = self
            .query(&token, async |core| {
                core.compose_catalog(preferred_account_id.as_deref()).await
            })
            .await?;
        Ok(value.into())
    }

    /// Strict whole-input recipient parsing on the native compute pool.
    pub async fn parse_draft_recipients(
        &self,
        token: Arc<CancelToken>,
        text: String,
    ) -> Result<Vec<DraftRecipient>, CoreError> {
        let values = self
            .query(&token, async |core| {
                core.parse_draft_recipients(&text).await
            })
            .await?;
        Ok(values.into_iter().map(Into::into).collect())
    }

    /// Bounded literal history suggestions, with no partial-answer fallback.
    pub async fn recipient_completions(
        &self,
        token: Arc<CancelToken>,
        prefix: String,
        limit: u32,
    ) -> Result<Vec<DraftRecipient>, CoreError> {
        let values = self
            .query(&token, async |core| {
                core.recipient_completions(&prefix, limit).await
            })
            .await?;
        Ok(values.into_iter().map(Into::into).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CoreConfig, open_core};
    use sb_test_support::{ScratchDir, block_on, execute_sql};
    use std::future::Future;
    use std::task::{Context, Poll, Waker};

    fn config(s: &ScratchDir) -> CoreConfig {
        CoreConfig {
            data_root: s.path().join("data").to_str().unwrap().into(),
            cache_root: s.path().join("cache").to_str().unwrap().into(),
        }
    }
    fn round_trip<T>(value: T) -> T
    where
        T: Clone + uniffi::Lower<crate::UniFfiTag> + uniffi::Lift<crate::UniFfiTag>,
    {
        let mut bytes = Vec::new();
        <T as uniffi::Lower<crate::UniFfiTag>>::write(value, &mut bytes);
        let mut input = bytes.as_slice();
        let value = <T as uniffi::Lift<crate::UniFfiTag>>::try_read(&mut input).unwrap();
        assert!(input.is_empty());
        value
    }

    #[test]
    fn all_catalog_fields_variants_and_options_round_trip_without_narrowing() {
        for (source, expected) in [
            (
                sb_service::IdentitySendable::Validated,
                IdentitySendable::Validated,
            ),
            (
                sb_service::IdentitySendable::Provisional,
                IdentitySendable::Provisional,
            ),
            (
                sb_service::IdentitySendable::Unavailable,
                IdentitySendable::Unavailable,
            ),
        ] {
            let mapped = ComposeCatalog::from(sb_service::ComposeCatalog {
                default_identity_id: Some("i-é".into()),
                identities: vec![sb_service::ComposeIdentity {
                    identity_id: "i-é".into(),
                    account_id: "a".into(),
                    account_name: "Work 東京".into(),
                    account_kind: sb_service::AccountKind::Imap,
                    email: "Local@EXAMPLE.test".into(),
                    display_name: "A \"Name\"".into(),
                    reply_to: Some("reply@example.test".into()),
                    signature_source: Some("**Signature**\nsecond line".into()),
                    is_default: true,
                    sendable: source,
                    revision: i64::MAX,
                }],
            });
            assert_eq!(mapped.default_identity_id.as_deref(), Some("i-é"));
            let i = &mapped.identities[0];
            assert_eq!(i.identity_id, "i-é");
            assert_eq!(i.account_id, "a");
            assert_eq!(i.account_name, "Work 東京");
            assert_eq!(i.account_kind, AccountKind::Imap);
            assert_eq!(i.email, "Local@EXAMPLE.test");
            assert_eq!(i.display_name, "A \"Name\"");
            assert_eq!(i.reply_to.as_deref(), Some("reply@example.test"));
            assert_eq!(
                i.signature_source.as_deref(),
                Some("**Signature**\nsecond line")
            );
            assert!(i.is_default);
            assert_eq!(i.sendable, expected);
            assert_eq!(i.revision, i64::MAX);
            assert_eq!(round_trip(mapped.clone()), mapped);
            let mut absent = i.clone();
            absent.account_kind = AccountKind::Local;
            absent.is_default = false;
            absent.reply_to = None;
            absent.signature_source = None;
            absent.revision = i64::MIN;
            assert_eq!(round_trip(absent.clone()), absent);
        }
        let empty = ComposeCatalog {
            identities: vec![],
            default_identity_id: None,
        };
        assert_eq!(round_trip(empty.clone()), empty);
    }

    #[test]
    fn actual_catalog_query_preserves_populated_fields_and_refuses_corruption() {
        let s = ScratchDir::new("ffi-compose-catalog");
        let core = block_on(open_core(CancelToken::new(), config(&s))).unwrap();
        assert!(
            block_on(core.compose_catalog(CancelToken::new(), None))
                .unwrap()
                .identities
                .is_empty()
        );
        block_on(core.shutdown());
        execute_sql(
            &s.path().join("data/state.sqlite"),
            "INSERT INTO accounts(account_id,kind,provider,display_name,state,sort_order,created_at) VALUES('a','imap','generic_imap','Work','paused',1,0); INSERT INTO identities(identity_id,account_id,email,display_name,is_default,sendable,revision) VALUES('i','a','User@EXAMPLE.test','東京',1,'provisional',9223372036854775807)",
        );
        let core = block_on(open_core(CancelToken::new(), config(&s))).unwrap();
        let catalog = block_on(core.compose_catalog(CancelToken::new(), Some("a".into()))).unwrap();
        assert_eq!(catalog.default_identity_id.as_deref(), Some("i"));
        assert_eq!(catalog.identities.len(), 1);
        assert_eq!(catalog.identities[0].revision, i64::MAX);
        assert_eq!(catalog.identities[0].email, "User@EXAMPLE.test");
        assert_eq!(catalog.identities[0].display_name, "東京");
        assert_eq!(
            catalog.identities[0].sendable,
            IdentitySendable::Provisional
        );
        assert!(matches!(
            block_on(core.compose_catalog(CancelToken::new(), Some("missing".into()))),
            Err(CoreError::NotFound { .. })
        ));
        execute_sql(
            &s.path().join("data/state.sqlite"),
            "UPDATE identities SET display_name=CAST(x'ff' AS TEXT)",
        );
        assert!(matches!(
            block_on(core.compose_catalog(CancelToken::new(), None)),
            Err(CoreError::Corrupt { .. })
        ));
        block_on(core.shutdown());
        assert!(matches!(
            block_on(core.compose_catalog(CancelToken::new(), None)),
            Err(CoreError::Unavailable { .. })
        ));
    }

    #[test]
    fn recipient_queries_preserve_names_literal_prefixes_errors_and_close() {
        let s = ScratchDir::new("ffi-compose-recipients");
        let core = block_on(open_core(CancelToken::new(), config(&s))).unwrap();
        let parsed = block_on(core.parse_draft_recipients(
            CancelToken::new(),
            "\"Doe, Alex\" <User@EXAMPLE.test>; other@example.test".into(),
        ))
        .unwrap();
        assert_eq!(
            parsed,
            [
                DraftRecipient {
                    address: "User@EXAMPLE.test".into(),
                    name: Some("Doe, Alex".into())
                },
                DraftRecipient {
                    address: "other@example.test".into(),
                    name: None
                }
            ]
        );
        assert!(matches!(
            block_on(
                core.parse_draft_recipients(CancelToken::new(), "ok@example.test,broken".into())
            ),
            Err(CoreError::InvalidInput { .. })
        ));
        assert!(matches!(
            block_on(core.recipient_completions(CancelToken::new(), "x".into(), 0)),
            Err(CoreError::InvalidInput { .. })
        ));
        execute_sql(
            &s.path().join("data/state.sqlite"),
            r#"INSERT INTO seen_addresses(canonical,email,display_name,last_seen_at,times_received,times_sent_to) VALUES('literal@example.test','literal@EXAMPLE.test','name_%\x',0,0,1),('second@example.test','second@example.test','Second',0,0,0)"#,
        );
        assert_eq!(
            block_on(core.recipient_completions(CancelToken::new(), "_%\\".into(), 50)).unwrap(),
            [DraftRecipient {
                address: "literal@EXAMPLE.test".into(),
                name: Some("name_%\\x".into())
            }]
        );
        let cancelled = CancelToken::new();
        cancelled.cancel();
        assert!(matches!(
            block_on(core.parse_draft_recipients(cancelled.clone(), "a@example.test".into())),
            Err(CoreError::Cancelled)
        ));
        assert!(matches!(
            block_on(core.recipient_completions(cancelled, "x".into(), 1)),
            Err(CoreError::Cancelled)
        ));
        execute_sql(
            &s.path().join("data/state.sqlite"),
            "UPDATE seen_addresses SET display_name=CAST(x'ff' AS TEXT) WHERE canonical='second@example.test'",
        );
        assert!(matches!(
            block_on(core.recipient_completions(CancelToken::new(), "@example.test".into(), 50)),
            Err(CoreError::Corrupt { .. })
        ));
        block_on(core.shutdown());
        assert!(matches!(
            block_on(core.parse_draft_recipients(CancelToken::new(), "a@example.test".into())),
            Err(CoreError::Unavailable { .. })
        ));
        assert!(matches!(
            block_on(core.recipient_completions(CancelToken::new(), "x".into(), 1)),
            Err(CoreError::Unavailable { .. })
        ));
    }

    #[test]
    fn cancellation_before_and_during_lifecycle_admission_never_becomes_empty_success() {
        let s = ScratchDir::new("ffi-compose-cancel");
        let core = block_on(open_core(CancelToken::new(), config(&s))).unwrap();
        let cancelled = CancelToken::new();
        cancelled.cancel();
        assert!(matches!(
            block_on(core.compose_catalog(cancelled, None)),
            Err(CoreError::Cancelled)
        ));
        block_on(async {
            let locked = core.service.write().await;
            let token = CancelToken::new();
            let mut query = Box::pin(core.compose_catalog(token.clone(), None));
            assert!(matches!(
                query.as_mut().poll(&mut Context::from_waker(Waker::noop())),
                Poll::Pending
            ));
            token.cancel();
            assert!(matches!(
                query.as_mut().poll(&mut Context::from_waker(Waker::noop())),
                Poll::Ready(Err(CoreError::Cancelled))
            ));
            drop(query);
            drop(locked);
        });
        assert!(
            block_on(core.compose_catalog(CancelToken::new(), None))
                .unwrap()
                .identities
                .is_empty()
        );
        block_on(core.shutdown());
    }
}
