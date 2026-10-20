use super::*;
use ParseCode as P;
use sb_types::time::Timestamp;

fn ctx() -> ParseContext<'static> {
    ParseContext::new("UTC", Timestamp::from_millis(0))
}
fn parse(input: &str) -> Query {
    Query::parse_text(input, &ctx()).unwrap()
}

fn same(input: &str, node: &str) {
    let expected = Query::parse_json(&format!("{{\"v\":1,\"q\":{node}}}")).unwrap();
    let query = parse(input);
    assert_eq!(query, expected, "{input}");
    let text = query.canonical_text().unwrap();
    let unused = ParseContext::new("invalid/unconsumed", Timestamp::from_millis(i64::MIN));
    assert_eq!(Query::parse_text(&text, &unused).unwrap(), query, "{text}");
}

fn failure(input: &str) -> SearchParseError {
    let QueryError::Parse(error) = Query::parse_text(input, &ctx()).unwrap_err() else {
        panic!("input error, not native failure");
    };
    assert!(input.is_char_boundary(error.start()) && input.is_char_boundary(error.end()));
    error
}

fn bad(input: &str, marker: &str, code: P) {
    // Empty marker is EOF; other fixtures deliberately name their first match.
    let start = if marker.is_empty() {
        input.len()
    } else {
        input.find(marker).unwrap()
    };
    let error = failure(input);
    assert_eq!(
        (error.start(), error.end(), error.code()),
        (start, start + marker.len(), code),
        "{input}"
    );
}

#[test]
fn precedence_unary_implicit_and_and_explicit_groups_roundtrip() {
    for (input, node) in [
        ("", r#"{"and":[]}"#),
        (" \t\n", r#"{"and":[]}"#),
        ("(())", r#"{"and":[]}"#),
        (
            "a b OR c",
            r#"{"or":[{"and":[{"text":"a"},{"text":"b"}]},{"text":"c"}]}"#,
        ),
        (
            "a OR b AND c",
            r#"{"or":[{"text":"a"},{"and":[{"text":"b"},{"text":"c"}]}]}"#,
        ),
        ("NOT a b", r#"{"and":[{"not":{"text":"a"}},{"text":"b"}]}"#),
        ("--a", r#"{"not":{"not":{"text":"a"}}}"#),
        ("-(a OR b)", r#"{"not":{"or":[{"text":"a"},{"text":"b"}]}}"#),
        (
            "a AND (b AND c)",
            r#"{"and":[{"text":"a"},{"text":"b"},{"text":"c"}]}"#,
        ),
        (
            "a OR (b OR c)",
            r#"{"or":[{"text":"a"},{"text":"b"},{"text":"c"}]}"#,
        ),
        ("a ()", r#"{"and":[{"text":"a"},{"and":[]}]}"#),
        ("() ()", r#"{"and":[{"and":[]},{"and":[]}]}"#),
        ("a OR ()", r#"{"or":[{"text":"a"},{"and":[]}]}"#),
        (
            "ANDromeda ORANGE NOTable",
            r#"{"and":[{"text":"ANDromeda"},{"text":"ORANGE"},{"text":"NOTable"}]}"#,
        ),
        (r#""AND""#, r#"{"text":"AND"}"#),
    ] {
        same(input, node);
    }
}

#[test]
fn every_leaf_and_field_operator_has_a_lossless_text_spelling() {
    for name in ["from", "to", "cc", "subject", "body", "attachment"] {
        same(
            &format!("{name}:\"B A B\""),
            &format!(r#"{{"field":"{name}","op":"contains","value":"B A B"}}"#),
        );
    }
    for name in ["from", "to", "cc"] {
        same(
            &format!("{name}.is:\"\\\"Foo\\\"@EXAMPLE.com\""),
            &format!(r#"{{"field":"{name}","op":"is","value":"\"Foo\"@EXAMPLE.com"}}"#),
        );
        same(
            &format!("{name}.domain:Ｅxample.COM."),
            &format!(r#"{{"field":"{name}","op":"domain","value":"Ｅxample.COM."}}"#),
        );
    }
    for (input, node) in [
        ("is:read", r#"{"flag":"seen"}"#),
        ("is:unread", r#"{"not":{"flag":"seen"}}"#),
        ("is:flagged", r#"{"flag":"flagged"}"#),
        ("is:answered", r#"{"flag":"answered"}"#),
        ("is:draft", r#"{"flag":"draft"}"#),
        ("has:attachment", r#"{"has":"attachment"}"#),
        ("has:note", r#"{"has":"note"}"#),
        ("in:00042", r#"{"mailbox":42}"#),
        ("size.lt:10", r#"{"size":{"op":"lt","bytes":10}}"#),
        ("size.eq:-0", r#"{"size":{"op":"eq","bytes":0}}"#),
        (
            "size.gt:9223372036854775807",
            r#"{"size":{"op":"gt","bytes":9223372036854775807}}"#,
        ),
        (
            "subject:-word",
            r#"{"field":"subject","op":"contains","value":"-word"}"#,
        ),
    ] {
        same(input, node);
    }
    for key in ["tag", "account"] {
        for id in [
            "00000000-0000-0000-0000-000000000000",
            "ffffffff-ffff-ffff-ffff-ffffffffffff",
        ] {
            same(&format!("{key}:{id}"), &format!(r#"{{"{key}":"{id}"}}"#));
        }
    }
}

#[test]
fn dates_use_only_consumed_defaults_and_format_explicit_context() {
    for key in ["before", "after", "on"] {
        same(
            &format!("{key}:2026-01-01"),
            &format!(r#"{{"date":{{"{key}":"2026-01-01","tz":"UTC"}}}}"#),
        );
        same(
            &format!("{key}:2026-01-01@US/Eastern"),
            &format!(r#"{{"date":{{"{key}":"2026-01-01","tz":"US/Eastern"}}}}"#),
        );
    }
    same(
        "within:2",
        r#"{"date":{"within_days":2,"tz":"UTC","clock":0}}"#,
    );
    same(
        "within:3@Europe/Zurich@-100",
        r#"{"date":{"within_days":3,"tz":"Europe/Zurich","clock":-100}}"#,
    );
    let context = ParseContext::new("US/Eastern", Timestamp::from_millis(123));
    let query = Query::parse_text("within:5", &context).unwrap();
    assert_eq!(
        query.canonical_text().unwrap(),
        "within:\"5@US/Eastern@123\""
    );
    let invalid = ParseContext::new("invalid", Timestamp::from_millis(i64::MIN));
    assert!(Query::parse_text("is:read", &invalid).is_ok());
    assert!(Query::parse_text("before:2026-01-01@UTC", &invalid).is_ok());
    let QueryError::Parse(error) = Query::parse_text("before:2026-01-01", &invalid).unwrap_err()
    else {
        panic!("consumed context");
    };
    assert_eq!(
        (error.start(), error.end(), error.code()),
        (7, 17, P::InvalidValue)
    );
}

#[test]
fn unicode_whitespace_dsl_escapes_and_operator_like_text_are_literal() {
    same("a\u{a0}b", r#"{"and":[{"text":"a"},{"text":"b"}]}"#);
    let query = parse(r#""quote\" slash\\ café́ 😀""#);
    let Node::Text(text) = query.root() else {
        panic!("literal");
    };
    assert_eq!(text.value(), "quote\" slash\\ café\u{301} 😀");
    assert_eq!(text.tokens(), ["quote", "slash", "cafe"]);
    assert_eq!(
        query.canonical_text().unwrap(),
        "\"quote\\\" slash\\\\ café\u{301} 😀\""
    );
    for value in [
        "OR NEAR(a b) a*b",
        "a-b_c it's",
        "\u{378} \u{20ba}",
        "中文",
        "\u{e000}",
        "word\nline\u{1}",
    ] {
        let json = serde_json::json!({"v":1,"q":{"text":value}}).to_string();
        let query = Query::parse_json(&json).unwrap();
        assert_eq!(
            Query::parse_text(&query.canonical_text().unwrap(), &ctx()).unwrap(),
            query
        );
    }
}

#[test]
fn json_groups_normalize_without_dropping_empty_children_or_order() {
    for (original, normalized) in [
        (
            r#"{"and":[{"and":[{"text":"a"},{"text":"b"}]},{"and":[]},{"text":"c"}]}"#,
            r#"{"and":[{"text":"a"},{"text":"b"},{"and":[]},{"text":"c"}]}"#,
        ),
        (
            r#"{"or":[{"or":[{"text":"a"},{"text":"b"}]},{"not":{"and":[]}}]}"#,
            r#"{"or":[{"text":"a"},{"text":"b"},{"not":{"and":[]}}]}"#,
        ),
        (
            r#"{"or":[{"and":[{"flag":"seen"}]}]}"#,
            r#"{"flag":"seen"}"#,
        ),
    ] {
        let query = Query::parse_json(&format!("{{\"v\":1,\"q\":{original}}}")).unwrap();
        same(&query.canonical_text().unwrap(), normalized);
    }
}

#[test]
fn multiple_text_values_keep_their_own_native_sequences() {
    let query = parse(r#""B A B" body:"C C" "d""#);
    let Node::And(children) = query.root() else {
        panic!("AND");
    };
    let Node::Text(first) = &children[0] else {
        panic!("text");
    };
    let Node::Field(FieldPredicate::Contains { value: second, .. }) = &children[1] else {
        panic!("field");
    };
    let Node::Text(third) = &children[2] else {
        panic!("text");
    };
    assert_eq!(first.tokens(), ["b", "a", "b"]);
    assert_eq!(second.tokens(), ["c", "c"]);
    assert_eq!(third.tokens(), ["d"]);
}

#[test]
fn malformed_keys_suffixes_and_colon_spacing_have_exact_spans() {
    for (input, marker, code) in [
        ("unknown:x", "unknown", P::UnknownKey),
        ("from.unknown:x", "unknown", P::InvalidOperator),
        ("size:5", "size", P::InvalidOperator),
        ("size.lte:5", "lte", P::InvalidOperator),
        ("body.is:x", "is", P::InvalidOperator),
        ("subject.domain:x", "domain", P::InvalidOperator),
        ("attachment.is:x", "is", P::InvalidOperator),
        ("body.contains:x", "contains", P::InvalidOperator),
        ("has.is:x", "has.is", P::UnknownKey),
        ("12:30", "12", P::UnexpectedToken),
        ("From:x", "From", P::UnexpectedToken),
        ("from..is:x", "from..is", P::UnexpectedToken),
        ("NOT:x", "NOT", P::UnexpectedToken),
        ("from :x", ":", P::UnexpectedToken),
        ("from: x", "x", P::MissingValue),
        ("from:\u{a0}x", "x", P::MissingValue),
        ("from:", "", P::MissingValue),
        ("from.is:", "", P::MissingValue),
        ("subject:OR", "OR", P::MissingValue),
        ("from::x", ":x", P::UnexpectedToken),
    ] {
        if input == "from::x" {
            let e = failure(input);
            assert_eq!((e.start(), e.end(), e.code()), (5, 6, code));
        } else {
            bad(input, marker, code);
        }
    }
    let error = failure("from:a:b");
    assert_eq!(
        (error.start(), error.end(), error.code()),
        (6, 7, P::UnexpectedToken)
    );
}

#[test]
fn syntax_escape_unicode_value_and_eof_errors_remain_distinct() {
    for (input, marker, code) in [
        ("OR a", "OR", P::UnexpectedToken),
        ("a AND OR b", "OR", P::MissingValue),
        ("a AND", "", P::MissingValue),
        ("NOT", "", P::MissingValue),
        ("(a OR", "", P::MissingValue),
        ("a)", ")", P::UnexpectedToken),
        ("NOT )", ")", P::UnexpectedToken),
        ("(a AND)", ")", P::MissingValue),
        ("a (b", "(", P::UnclosedGroup),
        ("(", "(", P::UnclosedGroup),
        ("é \"word", "\"", P::UnclosedQuote),
        (r#""word\"#, "\"", P::UnclosedQuote),
        (r#""\q""#, r#"\q"#, P::InvalidValue),
        (r#""\é""#, r#"\é"#, P::InvalidValue),
        (r#""\u0061""#, r#"\u"#, P::InvalidValue),
        (r#"a\b"#, "\\", P::UnexpectedToken),
        (r#"subject:"""#, "\"\"", P::InvalidValue),
        (r#""😀 €""#, r#""😀 €""#, P::InvalidValue),
        ("a\0b", "a\0b", P::InvalidValue),
    ] {
        bad(input, marker, code);
    }
    let error = failure("((a");
    assert_eq!(
        (error.start(), error.end(), error.code()),
        (1, 2, P::UnclosedGroup)
    );
}

#[test]
fn scalar_refusals_include_sign_range_uuid_address_and_date_context_edges() {
    for (input, value) in [
        ("in:0", "0"),
        ("in:-1", "-1"),
        ("in:+1", "+1"),
        ("in:9223372036854775808", "9223372036854775808"),
        ("size.eq:1.0", "1.0"),
        ("size.gt:1e3", "1e3"),
        ("size.lt:-5", "-5"),
        ("size.eq:+5", "+5"),
        ("within:+3", "+3"),
        ("within:0", "0"),
        ("within:4294967296", "4294967296"),
        ("within:3@UTC", "3@UTC"),
        ("within:1@UTC@+5", "1@UTC@+5"),
        (
            "within:1@UTC@-9223372036854775808",
            "1@UTC@-9223372036854775808",
        ),
        ("on:2026-02-29", "2026-02-29"),
        ("before:2026-01-01@utc", "2026-01-01@utc"),
        ("on:2011-12-30@Pacific/Apia", "2011-12-30@Pacific/Apia"),
        ("is:seen", "seen"),
        ("has:file", "file"),
        ("from.domain:a..test", "a..test"),
        (r#"from.is:"Name <a@b.test>""#, r#""Name <a@b.test>""#),
        (
            "tag:12345678-1234-4234-8234-123456789ABC",
            "12345678-1234-4234-8234-123456789ABC",
        ),
        ("account:not-a-uuid", "not-a-uuid"),
    ] {
        bad(input, value, P::InvalidValue);
    }
}

#[test]
fn normalization_budget_and_redundant_grouping_do_not_overreject_or_recurse() {
    let grouped = format!("{}is:read{}", "(".repeat(8000), ")".repeat(8000));
    assert_eq!(parse(&grouped), parse("is:read"));
    for join in ["AND", "OR"] {
        let child = vec!["a"; 254].join(&format!(" {join} "));
        let input = format!("a {join} ({child})");
        let query = parse(&input);
        let children = match query.root() {
            Node::And(children) | Node::Or(children) => children,
            _ => panic!("flat"),
        };
        assert_eq!(children.len(), 255);
        assert_eq!(parse(&query.canonical_text().unwrap()), query);
        let bad_input = format!("{input} {join} a");
        let error = failure(&bad_input);
        assert_eq!(error.code(), P::Limit);
    }
    let deep = format!("{}a{}", "a AND (".repeat(254), ")".repeat(254));
    assert!(matches!(parse(&deep).root(), Node::And(children) if children.len() == 255));
    let accepted = format!("{}is:read", "NOT ".repeat(31));
    assert_eq!(
        parse(&accepted),
        parse(&parse(&accepted).canonical_text().unwrap())
    );
    bad(&format!("{}is:read", "NOT ".repeat(32)), "NOT", P::Limit);
    let mut mixed = "a".to_owned();
    for n in 0..32 {
        if n == 31 {
            let query = parse(&mixed);
            assert_eq!(parse(&query.canonical_text().unwrap()), query);
        }
        mixed = format!("a {} ({mixed})", if n % 2 == 0 { "AND" } else { "OR" });
    }
    assert_eq!(failure(&mixed).code(), P::Limit);
}

#[test]
fn raw_and_conversion_byte_edges_are_checked_without_inventing_format_spans() {
    let input = format!("is:read{}", " ".repeat(MAX_QUERY_INPUT_BYTES - 7));
    assert_eq!(input.len(), MAX_QUERY_INPUT_BYTES);
    assert_eq!(parse(&input), parse("is:read"));
    let error = failure(&format!("{input} "));
    assert_eq!(
        (error.start(), error.end(), error.code()),
        (MAX_QUERY_INPUT_BYTES, MAX_QUERY_INPUT_BYTES + 1, P::Limit)
    );
    let input = format!("{}é", " ".repeat(MAX_QUERY_INPUT_BYTES - 1));
    let error = failure(&input);
    assert_eq!(
        (error.start(), error.end()),
        (MAX_QUERY_INPUT_BYTES - 1, MAX_QUERY_INPUT_BYTES + 1)
    );
    let input = "a".repeat(MAX_QUERY_INPUT_BYTES - 1);
    let error = failure(&input);
    assert_eq!(
        (error.start(), error.end(), error.code()),
        (0, input.len(), P::Limit)
    );
    for value in [
        "a".repeat(MAX_QUERY_INPUT_BYTES),
        "é".repeat(MAX_QUERY_INPUT_BYTES / 2),
    ] {
        let value = TextOperand::parse(&value).unwrap();
        assert_eq!(
            super::format::render(&Node::Text(value)),
            Err(TextFormatError::Limit)
        );
    }
    let value = TextOperand::parse(&"é".repeat((MAX_QUERY_INPUT_BYTES - 2) / 2)).unwrap();
    assert_eq!(
        super::format::render(&Node::Text(value)).unwrap().len(),
        MAX_QUERY_INPUT_BYTES
    );
}
