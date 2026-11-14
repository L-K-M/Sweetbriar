//! Composer reads through the native public API, synthetic roots only.
use sb_service::{
    AccountKind, Core, CoreConfig, CoreError, DraftRecipient, IdentitySendable, open_core,
};
use sb_test_support::{ScratchDir, block_on, execute_sql};

fn open(s: &ScratchDir) -> Core {
    block_on(open_core(CoreConfig {
        data_root: s.path().join("store"),
        cache_root: s.path().join("cache"),
    }))
    .unwrap()
}
fn sql(s: &ScratchDir, text: &str) {
    execute_sql(&s.path().join("store/state.sqlite"), text);
}
fn seeded() -> (ScratchDir, Core) {
    let s = ScratchDir::new("native-compose-services");
    let core = open(&s);
    block_on(core.shutdown());
    sql(
        &s,
        r#"
        INSERT INTO accounts(account_id,kind,provider,display_name,state,sort_order,created_at)
          VALUES('a','imap','generic_imap','Work','active',2,0),('b','imap','generic_imap','Personal','paused',1,0);
        INSERT INTO identities(identity_id,account_id,email,display_name,reply_to,signature_source,is_default,sendable,revision)
          VALUES('a-default','a','Local@EXAMPLE.test','Alex 東京','reply@example.test','**Signature**',1,'validated',9223372036854775807),
                ('a-other','a','other@example.test','Other',NULL,NULL,0,'provisional',2),
                ('a-hidden','a','hidden@example.test','Hidden',NULL,NULL,0,'unavailable',3),
                ('b-default','b','private@example.test','Private',NULL,NULL,1,'validated',4);
        INSERT INTO settings(key,value_json,revision,updated_at) VALUES('identity.default','"b-default"',1,0);
    "#,
    );
    let core = open(&s);
    (s, core)
}
fn recipient(address: &str, name: Option<&str>) -> DraftRecipient {
    DraftRecipient {
        address: address.into(),
        name: name.map(str::to_owned),
    }
}

#[test]
fn no_sender_has_no_invented_default_and_unknown_account_refuses() {
    let s = ScratchDir::new("compose-no-sender");
    let core = open(&s);
    let account = block_on(core.accounts()).unwrap().remove(0);
    let catalog = block_on(core.compose_catalog(Some(&account.account_id))).unwrap();
    assert!(catalog.identities.is_empty());
    assert_eq!(catalog.default_identity_id, None);
    assert!(matches!(
        block_on(core.compose_catalog(Some("missing"))),
        Err(CoreError::NotFound { .. })
    ));
    block_on(core.shutdown());
}

#[test]
fn exact_identity_fields_and_default_precedence_survive_reopen() {
    let (s, core) = seeded();
    let catalog = block_on(core.compose_catalog(Some("a"))).unwrap();
    assert_eq!(catalog.default_identity_id.as_deref(), Some("a-default"));
    assert_eq!(
        catalog
            .identities
            .iter()
            .map(|i| i.identity_id.as_str())
            .collect::<Vec<_>>(),
        ["b-default", "a-default", "a-hidden", "a-other"]
    );
    let identity = &catalog.identities[1];
    assert_eq!(identity.account_id, "a");
    assert_eq!(identity.account_name, "Work");
    assert_eq!(identity.account_kind, AccountKind::Imap);
    assert_eq!(identity.email, "Local@EXAMPLE.test");
    assert_eq!(identity.display_name, "Alex 東京");
    assert_eq!(identity.reply_to.as_deref(), Some("reply@example.test"));
    assert_eq!(identity.signature_source.as_deref(), Some("**Signature**"));
    assert!(identity.is_default);
    assert_eq!(identity.sendable, IdentitySendable::Validated);
    assert_eq!(identity.revision, i64::MAX);
    assert_eq!(
        catalog.identities[2].sendable,
        IdentitySendable::Unavailable
    );
    assert_eq!(
        catalog.identities[3].sendable,
        IdentitySendable::Provisional
    );
    assert_eq!(catalog.identities[3].reply_to, None);
    assert_eq!(catalog.identities[3].signature_source, None);
    assert_eq!(
        block_on(core.compose_catalog(None))
            .unwrap()
            .default_identity_id
            .as_deref(),
        Some("b-default")
    );
    let local = block_on(core.accounts())
        .unwrap()
        .into_iter()
        .find(|a| a.kind == AccountKind::Local)
        .unwrap();
    assert_eq!(
        block_on(core.compose_catalog(Some(&local.account_id)))
            .unwrap()
            .default_identity_id
            .as_deref(),
        Some("b-default")
    );
    block_on(core.shutdown());
    let reopened = open(&s);
    assert_eq!(
        block_on(reopened.compose_catalog(Some("a"))).unwrap(),
        catalog
    );
    block_on(reopened.shutdown());
}

#[test]
fn dangling_unavailable_or_absent_global_default_never_picks_the_first_sender() {
    let (s, core) = seeded();
    for value in [r#""missing""#, r#""a-hidden""#, r#""""#] {
        sql(
            &s,
            &format!("UPDATE settings SET value_json='{value}' WHERE key='identity.default'"),
        );
        assert_eq!(
            block_on(core.compose_catalog(None))
                .unwrap()
                .default_identity_id,
            None
        );
        assert_eq!(
            block_on(core.compose_catalog(Some("a")))
                .unwrap()
                .default_identity_id
                .as_deref(),
            Some("a-default")
        );
    }
    sql(&s, "DELETE FROM settings WHERE key='identity.default'");
    assert_eq!(
        block_on(core.compose_catalog(None))
            .unwrap()
            .default_identity_id,
        None
    );
    sql(
        &s,
        "UPDATE identities SET sendable='unavailable' WHERE identity_id='a-default'",
    );
    assert_eq!(
        block_on(core.compose_catalog(Some("a")))
            .unwrap()
            .default_identity_id,
        None
    );
    block_on(core.shutdown());
}

#[test]
fn bad_utf8_or_default_json_refuses_the_whole_catalog() {
    let (s, core) = seeded();
    sql(
        &s,
        "UPDATE identities SET display_name=CAST(x'ff' AS TEXT) WHERE identity_id='a-other'",
    );
    assert!(matches!(
        block_on(core.compose_catalog(None)),
        Err(CoreError::Corrupt { .. })
    ));
    sql(
        &s,
        "UPDATE identities SET display_name='Other' WHERE identity_id='a-other'; UPDATE settings SET value_json='null' WHERE key='identity.default'",
    );
    assert!(matches!(
        block_on(core.compose_catalog(None)),
        Err(CoreError::Corrupt { .. })
    ));
    block_on(core.shutdown());
}

#[test]
fn orphaned_identity_refuses_instead_of_disappearing_from_the_catalog() {
    let (s, core) = seeded();
    sql(
        &s,
        "PRAGMA foreign_keys=OFF; INSERT INTO identities(identity_id,account_id,email,display_name,is_default,sendable) VALUES('orphan','missing-account','orphan@example.test','Orphan',0,'validated')",
    );
    assert!(matches!(
        block_on(core.compose_catalog(None)),
        Err(CoreError::Corrupt { .. })
    ));
    block_on(core.shutdown());
}

#[test]
fn recipient_errors_do_not_retain_private_input() {
    let s = ScratchDir::new("compose-error-privacy");
    let core = open(&s);
    for input in [
        "PRIVATE-SENTINEL-without-domain",
        "PRIVATE-SENTINEL@example..test",
    ] {
        let error = block_on(core.parse_draft_recipients(input)).unwrap_err();
        assert!(matches!(error, CoreError::InvalidInput { .. }));
        assert!(!error.to_string().contains("PRIVATE-SENTINEL"));
    }
    block_on(core.shutdown());
}

#[test]
fn oversized_catalog_is_a_refusal_not_a_partial_answer() {
    let (s, core) = seeded();
    sql(
        &s,
        "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<1021) INSERT INTO identities(identity_id,account_id,email,display_name,is_default,sendable) SELECT 'many-'||x,'a','many-'||x||'@example.test','Many',0,'validated' FROM n",
    );
    assert!(matches!(
        block_on(core.compose_catalog(None)),
        Err(CoreError::Unavailable { .. })
    ));
    block_on(core.shutdown());
    let (s, core) = seeded();
    sql(
        &s,
        "UPDATE identities SET signature_source=replace(hex(zeroblob(2097153)),'0','x') WHERE identity_id='a-default'",
    );
    assert!(matches!(
        block_on(core.compose_catalog(None)),
        Err(CoreError::Unavailable { .. })
    ));
    block_on(core.shutdown());
}

#[test]
fn oversized_stored_default_is_not_parsed_or_returned_as_an_empty_success() {
    let (s, core) = seeded();
    sql(
        &s,
        "UPDATE settings SET value_json='\"'||replace(hex(zeroblob(2097153)),'0','x')||'\"' WHERE key='identity.default'",
    );
    assert!(matches!(
        block_on(core.compose_catalog(None)),
        Err(CoreError::Unavailable { .. })
    ));
    block_on(core.shutdown());
}

#[test]
fn account_input_is_bounded_before_the_blocking_read() {
    let s = ScratchDir::new("compose-account-input");
    let core = open(&s);
    for id in [String::new(), "x".repeat(129), "x\0".to_owned()] {
        assert!(
            matches!(block_on(core.compose_catalog(Some(&id))), Err(CoreError::InvalidInput { field, .. }) if field == "preferred_account_id")
        );
    }
    block_on(core.shutdown());
}

#[test]
fn completion_is_literal_bounded_ordered_and_lossless() {
    let (s, core) = seeded();
    sql(
        &s,
        r#"INSERT INTO seen_addresses(canonical,email,display_name,last_seen_at,times_received,times_sent_to) VALUES
        ('first@example.test','first@EXAMPLE.test','First',1,0,9223372036854775807),
        ('second@example.test','second@example.test','Second',2,0,3),
        ('literal@example.test','literal@example.test','percent_%\name',3,0,0);"#,
    );
    assert_eq!(
        block_on(core.recipient_completions("_%\\", 50)).unwrap(),
        [recipient("literal@example.test", Some("percent_%\\name"))]
    );
    assert_eq!(
        block_on(core.recipient_completions("@example.test", 1)).unwrap(),
        [recipient("first@EXAMPLE.test", Some("First"))]
    );
    assert_eq!(
        block_on(core.recipient_completions("Second", 50)).unwrap(),
        [recipient("second@example.test", Some("Second"))]
    );
    assert!(
        block_on(core.recipient_completions("", 50))
            .unwrap()
            .is_empty()
    );
    for (prefix, limit, field) in [
        ("x".into(), 0, "limit"),
        ("x".into(), 51, "limit"),
        ("x".repeat(1025), 1, "prefix"),
        ("x\0".into(), 1, "prefix"),
    ] {
        assert!(
            matches!(block_on(core.recipient_completions(&prefix,limit)),Err(CoreError::InvalidInput{field:f,..}) if f==field)
        );
    }
    sql(
        &s,
        "UPDATE seen_addresses SET display_name=CAST(x'ff' AS TEXT) WHERE canonical='second@example.test'",
    );
    assert!(matches!(
        block_on(core.recipient_completions("@example.test", 50)),
        Err(CoreError::Corrupt { .. })
    ));
    block_on(core.shutdown());
}

#[test]
fn modern_mailbox_lists_preserve_names_quotes_and_unicode() {
    let s = ScratchDir::new("compose-input");
    let core = open(&s);
    let input =
        r#""Doe, Alex \"A\"" <User@EXAMPLE.test>; 東京 <"a,b"@例子.测试>, bare@example.test,"#;
    assert_eq!(
        block_on(core.parse_draft_recipients(input)).unwrap(),
        [
            recipient("User@EXAMPLE.test", Some("Doe, Alex \"A\"")),
            recipient("\"a,b\"@例子.测试", Some("東京")),
            recipient("bare@example.test", None)
        ]
    );
    assert!(
        block_on(core.parse_draft_recipients("   "))
            .unwrap()
            .is_empty()
    );
    block_on(core.shutdown());
}

#[test]
fn malformed_input_never_returns_a_valid_prefix_or_drops_a_member() {
    let s = ScratchDir::new("compose-invalid-input");
    let core = open(&s);
    for input in [
        "a@example.test,broken",
        ",a@example.test",
        "a@example.test,,",
        "a@example.test (comment)",
        "Group: a@example.test;",
        "\"unterminated <a@example.test>",
        "Name <a@example.test",
        "Name <a@example.test>>",
        "a@example.test\r\nBcc: hidden@example.test",
        "a@example.test\0",
    ] {
        assert!(
            matches!(block_on(core.parse_draft_recipients(input)),Err(CoreError::InvalidInput{field,..}) if field=="recipients"),
            "accepted {input:?}"
        );
    }
    let many = (0..256)
        .map(|i| format!("a{i}@example.test"))
        .collect::<Vec<_>>()
        .join(",");
    assert_eq!(
        block_on(core.parse_draft_recipients(&many)).unwrap().len(),
        256
    );
    for input in [format!("{many},last@example.test"), "a".repeat(16385)] {
        assert!(matches!(
            block_on(core.parse_draft_recipients(&input)),
            Err(CoreError::InvalidInput { .. })
        ));
    }
    block_on(core.shutdown());
}

#[test]
fn queries_release_the_native_core_and_preserve_results_across_reopen() {
    let (s, core) = seeded();
    let before = block_on(core.compose_catalog(None)).unwrap();
    let parsed = block_on(core.parse_draft_recipients("a@example.test")).unwrap();
    assert!(
        block_on(core.recipient_completions("missing", 1))
            .unwrap()
            .is_empty()
    );
    // Rust shutdown consumes Core; the SDK's retained closed-handle checks
    // belong to the following FFI artifact, not an impossible borrow here.
    block_on(core.shutdown());
    let reopened = open(&s);
    assert_eq!(block_on(reopened.compose_catalog(None)).unwrap(), before);
    assert_eq!(
        block_on(reopened.parse_draft_recipients("a@example.test")).unwrap(),
        parsed
    );
    block_on(reopened.shutdown());
}
