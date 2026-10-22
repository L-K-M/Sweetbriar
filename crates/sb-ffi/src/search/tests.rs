use super::*;
use crate::{CoreConfig, SearchParseCode, open_core};
use sb_test_support::{ScratchDir, block_on};
use std::future::Future;
use std::task::{Context, Poll, Waker};

fn context(tz: &str, clock: i64) -> ParseContext {
    ParseContext {
        tz: tz.into(),
        clock,
    }
}

fn opened(label: &str) -> (ScratchDir, Arc<Core>) {
    let scratch = ScratchDir::new(label);
    let core = block_on(open_core(
        CancelToken::new(),
        CoreConfig {
            data_root: scratch.path().join("data").to_string_lossy().into_owned(),
            cache_root: scratch.path().join("cache").to_string_lossy().into_owned(),
        },
    ))
    .unwrap();
    (scratch, core)
}

fn span(error: CoreError, expected: (u32, u32, SearchParseCode)) {
    match error {
        CoreError::SearchParse { start, end, code } => assert_eq!((start, end, code), expected),
        other => panic!("expected typed SearchParse, got {other:?}"),
    }
}

#[test]
fn exported_parser_roundtrips_explicit_context_and_original_values() {
    let (_scratch, core) = opened("ffi-search-roundtrip");
    for input in [
        "is:read",
        "é OR body:\"a b\"",
        "on:2026-10-17",
        "within:1",
        "from.is:\"\\\"Foo\\\"@EXAMPLE.test\"",
    ] {
        let json = block_on(core.parse_search(
            CancelToken::new(),
            input.into(),
            context("UTC", 172_800_000),
        ))
        .unwrap();
        let text = block_on(core.format_search(CancelToken::new(), json.clone())).unwrap();
        assert_eq!(
            block_on(core.parse_search(
                CancelToken::new(),
                text.clone(),
                context("invalid/unused", i64::MIN)
            ))
            .unwrap(),
            json
        );
        if input == "within:1" {
            assert!(text.contains("1@UTC@172800000"));
        }
    }
    let json = block_on(core.parse_search(
        CancelToken::new(),
        "on:2026-10-17".into(),
        context("America/New_York", 0),
    ))
    .unwrap();
    assert!(
        block_on(core.format_search(CancelToken::new(), json))
            .unwrap()
            .contains("America/New_York")
    );
    block_on(core.shutdown());
}

#[test]
fn exported_errors_keep_every_code_utf8_eof_and_input_conversion_limits() {
    let (_scratch, core) = opened("ffi-search-errors");
    for (input, start, end, code) in [
        ("é )", 3, 4, SearchParseCode::UnexpectedToken),
        ("é AND", 6, 6, SearchParseCode::MissingValue),
        ("é \"word", 3, 4, SearchParseCode::UnclosedQuote),
        ("é (word", 3, 4, SearchParseCode::UnclosedGroup),
        ("é unknown:x", 3, 10, SearchParseCode::UnknownKey),
        ("é body.is:x", 8, 10, SearchParseCode::InvalidOperator),
        (r#"é "\q""#, 4, 6, SearchParseCode::InvalidValue),
        ("é on:2026-10-17", 6, 16, SearchParseCode::InvalidValue),
    ] {
        span(
            block_on(core.parse_search(CancelToken::new(), input.into(), context("bad", 0)))
                .unwrap_err(),
            (start, end, code),
        );
    }
    let oversized = format!("{}😀", " ".repeat(16_384));
    for error in [
        block_on(core.parse_search(CancelToken::new(), oversized.clone(), context("UTC", 0)))
            .unwrap_err(),
        block_on(core.format_search(CancelToken::new(), oversized)).unwrap_err(),
    ] {
        span(error, (16_384, 16_388, SearchParseCode::Limit));
    }
    span(
        block_on(core.parse_search(CancelToken::new(), "a".repeat(16_383), context("UTC", 0)))
            .unwrap_err(),
        (0, 16_383, SearchParseCode::Limit),
    );
    for input in ["is:read", r#"{"v":1,"q":{"text":"é"}} !"#] {
        let start = input.find('!').unwrap_or(0) as u32;
        span(
            block_on(core.format_search(CancelToken::new(), input.into())).unwrap_err(),
            (start, start + 1, SearchParseCode::UnexpectedToken),
        );
    }
    block_on(core.shutdown());
}

#[derive(Clone, Copy)]
enum Query {
    Parse,
    Format,
}

async fn call(core: &Core, query: Query, token: Arc<CancelToken>) -> Result<String, CoreError> {
    match query {
        Query::Parse => {
            core.parse_search(token, "is:read".into(), context("UTC", 0))
                .await
        }
        Query::Format => {
            core.format_search(token, r#"{"v":1,"q":{"flag":"seen"}}"#.into())
                .await
        }
    }
}

#[test]
fn both_exported_queries_cancel_at_the_known_lifecycle_lock_and_reject_shutdown() {
    let (_scratch, core) = opened("ffi-search-cancel");
    for query in [Query::Parse, Query::Format] {
        let cancelled = CancelToken::new();
        cancelled.cancel();
        assert!(matches!(
            block_on(call(&core, query, cancelled)),
            Err(CoreError::Cancelled)
        ));
    }
    let writing = block_on(core.service.write());
    for query in [Query::Parse, Query::Format] {
        let token = CancelToken::new();
        let mut future = Box::pin(call(&core, query, token.clone()));
        let mut cx = Context::from_waker(Waker::noop());
        assert!(
            future.as_mut().poll(&mut cx).is_pending(),
            "the real lifecycle lock holds the query"
        );
        token.cancel();
        assert!(matches!(
            future.as_mut().poll(&mut cx),
            Poll::Ready(Err(CoreError::Cancelled))
        ));
    }
    drop(writing);
    for query in [Query::Parse, Query::Format] {
        assert!(
            !block_on(call(&core, query, CancelToken::new()))
                .unwrap()
                .is_empty()
        );
    }
    block_on(core.shutdown());
    for query in [Query::Parse, Query::Format] {
        assert!(matches!(
            block_on(call(&core, query, CancelToken::new())),
            Err(CoreError::Unavailable { .. })
        ));
    }
}
