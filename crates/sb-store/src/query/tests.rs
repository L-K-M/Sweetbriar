use super::*;
use ParseCode as P;

fn wire(node: &str) -> String {
    format!("{{\"v\":1,\"q\":{node}}}")
}

fn checked(node: &str) -> Query {
    Query::parse_json(&wire(node)).unwrap()
}

fn rejected(input: &str, value: &str, code: ParseCode) {
    let QueryError::Parse(error) = Query::parse_json(input).unwrap_err() else {
        panic!("native failure is not an input verdict");
    };
    let start = input.rfind(value).unwrap();
    assert_eq!(
        (error.start(), error.end(), error.code()),
        (start, start + value.len(), code),
        "{input}"
    );
    assert!(input.is_char_boundary(error.start()) && input.is_char_boundary(error.end()));
}

fn bad_node(node: &str, value: &str, code: ParseCode) {
    rejected(&wire(node), value, code);
}

#[test]
fn explicit_groups_and_original_addresses_have_exact_canonical_roundtrip() {
    let input = r#" { "v":1, "q":{"or":[{"value":"\"Foo\"@EXAMPLE.com","op":"is","field":"from"},{"and":[]},{"not":{"has":"note"}}]}} "#;
    let expected = r#"{"q":{"or":[{"field":"from","op":"is","value":"\"Foo\"@EXAMPLE.com"},{"and":[]},{"not":{"has":"note"}}]},"v":1}"#;
    let query = Query::parse_json(input).unwrap();
    assert_eq!(query.canonical_json(), expected);
    assert_eq!(Query::parse_json(expected).unwrap(), query);
    let Node::Or(children) = query.root() else {
        panic!("explicit OR retained");
    };
    let Node::Field(FieldPredicate::Is { field, address }) = &children[0] else {
        panic!("typed is operand");
    };
    assert_eq!(*field, AddressField::From);
    assert_eq!(address.key(), "\"Foo\"@example.com");
    assert!(matches!(&children[1], Node::And(group) if group.is_empty()));
    assert!(matches!(&children[2], Node::Not(child) if **child == Node::Has(Has::Note)));
    assert_eq!(query.clone(), query);
}

#[test]
fn all_field_pairs_use_checked_scalars_and_keep_native_phrase_order() {
    for (name, expected) in [
        ("from", Field::From),
        ("to", Field::To),
        ("cc", Field::Cc),
        ("subject", Field::Subject),
        ("body", Field::Body),
        ("attachment", Field::Attachment),
    ] {
        let query = checked(&format!(
            r#"{{"field":"{name}","op":"contains","value":"B A B"}}"#
        ));
        let Node::Field(FieldPredicate::Contains { field, value }) = query.root() else {
            panic!("contains");
        };
        assert_eq!(*field, expected);
        assert_eq!(value.value(), "B A B");
        assert_eq!(value.tokens(), ["b", "a", "b"]);
    }
    for (name, expected) in [
        ("from", AddressField::From),
        ("to", AddressField::To),
        ("cc", AddressField::Cc),
    ] {
        let is = checked(&format!(
            r#"{{"field":"{name}","op":"is","value":"é@İ.test"}}"#
        ));
        assert!(
            matches!(is.root(), Node::Field(FieldPredicate::Is { field, address })
            if *field == expected && address.local_key() == "é" && address.domain_key() == "i\u{307}.test")
        );
        let domain = checked(&format!(
            r#"{{"field":"{name}","op":"domain","value":"Ｅxample.COM."}}"#
        ));
        assert!(
            matches!(domain.root(), Node::Field(FieldPredicate::Domain { field, domain })
            if *field == expected && domain.key() == "ｅxample.com." && domain.value() == "Ｅxample.COM.")
        );
    }
}

#[test]
fn all_leaf_enums_ids_and_integer_edges_are_typed() {
    for (name, flag) in [
        ("seen", Flag::Seen),
        ("flagged", Flag::Flagged),
        ("answered", Flag::Answered),
        ("draft", Flag::Draft),
    ] {
        assert_eq!(
            checked(&format!(r#"{{"flag":"{name}"}}"#)).root(),
            &Node::Flag(flag)
        );
    }
    for (name, kind) in [("attachment", Has::Attachment), ("note", Has::Note)] {
        assert_eq!(
            checked(&format!(r#"{{"has":"{name}"}}"#)).root(),
            &Node::Has(kind)
        );
    }
    for id in [
        "00000000-0000-0000-0000-000000000000",
        "ffffffff-ffff-ffff-ffff-ffffffffffff",
    ] {
        let uuid = id.parse().unwrap();
        assert_eq!(
            checked(&format!(r#"{{"tag":"{id}"}}"#)).root(),
            &Node::Tag(uuid)
        );
        assert_eq!(
            checked(&format!(r#"{{"account":"{id}"}}"#)).root(),
            &Node::Account(uuid)
        );
    }
    for id in [1, i64::MAX] {
        assert_eq!(
            checked(&format!(r#"{{"mailbox":{id}}}"#)).root(),
            &Node::Mailbox(id)
        );
    }
    for (name, op) in [("lt", SizeOp::Lt), ("eq", SizeOp::Eq), ("gt", SizeOp::Gt)] {
        for bytes in [0, i64::MAX] {
            assert_eq!(
                checked(&format!(r#"{{"size":{{"op":"{name}","bytes":{bytes}}}}}"#)).root(),
                &Node::Size { op, bytes }
            );
        }
    }
    assert_eq!(
        checked(r#"{"size":{"op":"eq","bytes":-0}}"#).canonical_json(),
        r#"{"q":{"size":{"bytes":0,"op":"eq"}},"v":1}"#
    );
}

#[test]
fn explicit_date_context_roundtrips_without_clock_or_zone_re_evaluation() {
    for op in ["before", "after", "on"] {
        let query = checked(&format!(
            r#"{{"date":{{"tz":"US/Eastern","{op}":"2026-11-01"}}}}"#
        ));
        let Node::Date(date) = query.root() else {
            panic!("checked date");
        };
        assert_eq!(date.zone(), "US/Eastern");
        assert_eq!(Query::parse_json(query.canonical_json()).unwrap(), query);
    }
    for (days, clock) in [(1, i64::MIN + 86_400_000), (u32::MAX, i64::MAX), (1, 0)] {
        let query = checked(&format!(
            r#"{{"date":{{"within_days":{days},"tz":"UTC","clock":{clock}}}}}"#
        ));
        let Node::Date(date) = query.root() else {
            panic!("checked relative date");
        };
        assert_eq!(
            date.value(),
            &DateValue::WithinDays {
                days,
                clock: sb_types::time::Timestamp::from_millis(clock)
            }
        );
        assert_eq!(
            query.canonical_json(),
            format!(
                r#"{{"q":{{"date":{{"clock":{clock},"tz":"UTC","within_days":{days}}}}},"v":1}}"#
            )
        );
    }
}

#[test]
fn serde_escapes_and_literal_operators_preserve_values_and_exact_native_tokens() {
    let query = checked(
        r#"{"and":[{"text":"OR NEAR(a b) a*b"},{"text":"quote\" slash\\ café\u0301\uD83D\uDE00"}]}"#,
    );
    let Node::And(children) = query.root() else {
        panic!("AND");
    };
    let Node::Text(first) = &children[0] else {
        panic!("text");
    };
    let Node::Text(second) = &children[1] else {
        panic!("text");
    };
    assert_eq!(first.value(), "OR NEAR(a b) a*b");
    assert_eq!(first.tokens(), ["or", "near", "a", "b", "a", "b"]);
    assert_eq!(second.value(), "quote\" slash\\ café\u{301}😀");
    assert_eq!(second.tokens(), ["quote", "slash", "cafe"]);
    assert_eq!(
        query.canonical_json(),
        "{\"q\":{\"and\":[{\"text\":\"OR NEAR(a b) a*b\"},{\"text\":\"quote\\\" slash\\\\ café\u{301}😀\"}]},\"v\":1}"
    );
    assert_eq!(Query::parse_json(query.canonical_json()).unwrap(), query);
    for value in ["\u{378}", "\u{20ba}", "\u{e000}", "中文"] {
        assert!(
            matches!(checked(&format!(r#"{{"text":"{value}"}}"#)).root(), Node::Text(text) if !text.tokens().is_empty())
        );
    }
}

#[test]
fn envelope_unknown_duplicate_escaped_and_wrong_version_values_have_exact_spans() {
    for (input, value, code) in [
        (
            r#"{"v":1,"q":{"and":[]},"other":"é"}"#,
            r#""é""#,
            P::UnknownKey,
        ),
        (r#"{"v":1,"\u0076":2,"q":{"and":[]}}"#, "2", P::InvalidValue),
        (
            r#"{"v":1,"q":{"and":[]},"q":{"has":"note"}}"#,
            r#"{"has":"note"}"#,
            P::InvalidValue,
        ),
        (r#"{"v":1}"#, r#"{"v":1}"#, P::MissingValue),
        (
            r#"{"q":{"and":[]}}"#,
            r#"{"q":{"and":[]}}"#,
            P::MissingValue,
        ),
        ("[]", "[]", P::InvalidValue),
        ("null", "null", P::InvalidValue),
    ] {
        rejected(input, value, code);
    }
    for version in [
        "0",
        "2",
        "-1",
        "1.0",
        "1e0",
        "18446744073709551616",
        "true",
        r#""1""#,
    ] {
        rejected(
            &format!(r#"{{"v":{version},"q":{{"and":[]}}}}"#),
            version,
            P::InvalidValue,
        );
    }
}

#[test]
fn strict_node_shapes_and_nested_duplicates_do_not_overwrite_or_reinterpret() {
    for (node, value, code) in [
        (r#"{"text":"a","other":"é"}"#, r#""é""#, P::UnknownKey),
        (r#"{"text":"a","\u0074ext":"b"}"#, r#""b""#, P::InvalidValue),
        (
            r#"{"field":"from","field":"to","op":"is","value":"a@x.test"}"#,
            r#""to""#,
            P::InvalidValue,
        ),
        (
            r#"{"date":{"on":"2026-01-01","tz":"UTC","\u0074z":"Europe/Zurich"}}"#,
            r#""Europe/Zurich""#,
            P::InvalidValue,
        ),
        (
            r#"{"size":{"op":"eq","bytes":1,"bytes":2}}"#,
            "2",
            P::InvalidValue,
        ),
        (
            r#"{"text":"x","flag":"seen"}"#,
            r#"{"text":"x","flag":"seen"}"#,
            P::InvalidValue,
        ),
        (
            r#"{"field":"from","op":"is","value":"a@x.test","has":"note"}"#,
            r#"{"field":"from","op":"is","value":"a@x.test","has":"note"}"#,
            P::InvalidValue,
        ),
        (
            r#"{"op":"contains","value":"x"}"#,
            r#"{"op":"contains","value":"x"}"#,
            P::MissingValue,
        ),
        (
            r#"{"field":"from","op":"is"}"#,
            r#"{"field":"from","op":"is"}"#,
            P::MissingValue,
        ),
        (
            r#"{"field":"from","value":"x"}"#,
            r#"{"field":"from","value":"x"}"#,
            P::MissingValue,
        ),
        ("{}", "{}", P::MissingValue),
        (r#""word""#, r#""word""#, P::InvalidValue),
        (r#"{"or":[]}"#, "[]", P::InvalidValue),
        (r#"{"and":false}"#, "false", P::InvalidValue),
        (
            r#"{"not":[{"flag":"seen"}]}"#,
            r#"[{"flag":"seen"}]"#,
            P::InvalidValue,
        ),
        (
            r#"{"not":{"and":[],"has":"note"}}"#,
            r#"{"and":[],"has":"note"}"#,
            P::InvalidValue,
        ),
    ] {
        bad_node(node, value, code);
    }
}

#[test]
fn wrong_enums_field_pairs_scalars_and_native_token_absence_keep_value_spans() {
    for (node, value, code) in [
        (r#"{"flag":"unread"}"#, r#""unread""#, P::InvalidValue),
        (r#"{"has":"file"}"#, r#""file""#, P::InvalidValue),
        (
            r#"{"field":"sender","op":"contains","value":"x"}"#,
            r#""sender""#,
            P::InvalidValue,
        ),
        (
            r#"{"field":"body","op":"is","value":"x"}"#,
            r#""is""#,
            P::InvalidOperator,
        ),
        (
            r#"{"field":"subject","op":"domain","value":"x"}"#,
            r#""domain""#,
            P::InvalidOperator,
        ),
        (
            r#"{"field":"attachment","op":"is","value":"x"}"#,
            r#""is""#,
            P::InvalidOperator,
        ),
        (
            r#"{"field":"from","op":"prefix","value":"x"}"#,
            r#""prefix""#,
            P::InvalidValue,
        ),
        (
            r#"{"field":"to","op":"is","value":"Name <a@b.test>"}"#,
            r#""Name <a@b.test>""#,
            P::InvalidValue,
        ),
        (
            r#"{"field":"cc","op":"domain","value":"a..test"}"#,
            r#""a..test""#,
            P::InvalidValue,
        ),
        (r#"{"text":null}"#, "null", P::InvalidValue),
        (r#"{"text":""}"#, r#""""#, P::InvalidValue),
        (r#"{"text":"😀 € ∑"}"#, r#""😀 € ∑""#, P::InvalidValue),
        (
            r#"{"text":"needle\u0000tail"}"#,
            r#""needle\u0000tail""#,
            P::InvalidValue,
        ),
        (
            r#"{"field":"body","op":"contains","value":"!!!"}"#,
            r#""!!!""#,
            P::InvalidValue,
        ),
        (
            r#"{"size":{"op":"lte","bytes":1}}"#,
            r#""lte""#,
            P::InvalidValue,
        ),
        (r#"{"size":{"bytes":1}}"#, r#"{"bytes":1}"#, P::MissingValue),
    ] {
        bad_node(node, value, code);
    }
    for id in [
        "",
        "12345678-1234-4234-8234-123456789ABC",
        "12345678123442348234123456789abc",
        "12345678-1234-4234-8234-12345678gabc",
    ] {
        for key in ["tag", "account"] {
            let value = format!("\"{id}\"");
            bad_node(&format!(r#"{{"{key}":{value}}}"#), &value, P::InvalidValue);
        }
    }
}

#[test]
fn integer_slots_reject_noninteger_spellings_types_bounds_and_invalid_signs() {
    for value in [
        "0",
        "-1",
        "9223372036854775808",
        "1.0",
        "1e3",
        "true",
        "null",
        r#""1""#,
    ] {
        bad_node(&format!(r#"{{"mailbox":{value}}}"#), value, P::InvalidValue);
    }
    for value in ["-1", "9223372036854775808", "1.0", "1e3", "true", r#""0""#] {
        bad_node(
            &format!(r#"{{"size":{{"op":"eq","bytes":{value}}}}}"#),
            value,
            P::InvalidValue,
        );
    }
    for value in ["0", "-1", "4294967296", "1.0", "1e3", r#""1""#] {
        bad_node(
            &format!(r#"{{"date":{{"within_days":{value},"clock":1,"tz":"UTC"}}}}"#),
            value,
            P::InvalidValue,
        );
    }
    for value in [
        "-9223372036854775808",
        "9223372036854775808",
        "1.0",
        "1e3",
        r#""0""#,
    ] {
        bad_node(
            &format!(r#"{{"date":{{"within_days":1,"clock":{value},"tz":"UTC"}}}}"#),
            value,
            P::InvalidValue,
        );
    }
}

#[test]
fn date_faults_select_the_actual_zone_date_days_or_clock_value() {
    for (node, value, code) in [
        (
            r#"{"date":{"on":"2026-02-29","tz":"UTC"}}"#,
            r#""2026-02-29""#,
            P::InvalidValue,
        ),
        (
            r#"{"date":{"on":"2011-12-30","tz":"Pacific/Apia"}}"#,
            r#""2011-12-30""#,
            P::InvalidValue,
        ),
        (
            r#"{"date":{"before":"2026-01-01","tz":"utc"}}"#,
            r#""utc""#,
            P::InvalidValue,
        ),
        (
            r#"{"date":{"before":"9999-12-31","tz":"UTC"}}"#,
            r#""9999-12-31""#,
            P::InvalidValue,
        ),
        (
            r#"{"date":{"on":"2026-01-01"}}"#,
            r#"{"on":"2026-01-01"}"#,
            P::MissingValue,
        ),
        (
            r#"{"date":{"within_days":1,"tz":"UTC"}}"#,
            r#"{"within_days":1,"tz":"UTC"}"#,
            P::MissingValue,
        ),
        (
            r#"{"date":{"on":"2026-01-01","tz":"UTC","clock":0}}"#,
            "0",
            P::InvalidValue,
        ),
        (
            r#"{"date":{"tz":"UTC","on":"2026-01-01","after":"2026-01-02"}}"#,
            r#"{"tz":"UTC","on":"2026-01-01","after":"2026-01-02"}"#,
            P::InvalidValue,
        ),
        (
            r#"{"date":{"between":"2026-01-01","tz":"UTC"}}"#,
            r#""2026-01-01""#,
            P::UnknownKey,
        ),
    ] {
        bad_node(node, value, code);
    }
}

#[test]
fn byte_limits_are_exact_for_ascii_and_multibyte_input_and_checked_output() {
    let prefix = r#"{"q":{"text":""#;
    let suffix = r#""},"v":1}"#;
    let room = MAX_QUERY_INPUT_BYTES - prefix.len() - suffix.len();
    for value in [
        "a".repeat(room),
        format!("{}{}", "é".repeat(room / 2), "a".repeat(room % 2)),
    ] {
        let input = format!("{prefix}{value}{suffix}");
        assert_eq!(input.len(), MAX_QUERY_INPUT_BYTES);
        assert_eq!(Query::parse_json(&input).unwrap().canonical_json(), input);
    }
    for (input, start, end) in [
        (
            " ".repeat(MAX_QUERY_INPUT_BYTES + 1),
            MAX_QUERY_INPUT_BYTES,
            MAX_QUERY_INPUT_BYTES + 1,
        ),
        (
            format!("{}é", " ".repeat(MAX_QUERY_INPUT_BYTES - 1)),
            MAX_QUERY_INPUT_BYTES - 1,
            MAX_QUERY_INPUT_BYTES + 1,
        ),
    ] {
        let QueryError::Parse(error) = Query::parse_json(&input).unwrap_err() else {
            panic!("limit");
        };
        assert_eq!(
            (error.start(), error.end(), error.code()),
            (start, end, P::Limit)
        );
    }
    let value = TextOperand::parse(&"a".repeat(MAX_QUERY_INPUT_BYTES)).unwrap();
    let QueryError::Parse(error) = Query::finish(Node::Text(value), (4, 8)).unwrap_err() else {
        panic!("output cap");
    };
    assert_eq!((error.start(), error.end(), error.code()), (4, 8, P::Limit));
}

fn nested(count: usize, leaf: &str) -> String {
    format!("{}{}{}", "{\"not\":".repeat(count), leaf, "}".repeat(count))
}

#[test]
fn depth_and_node_edges_count_only_ast_nodes_before_recursing() {
    let leaf = r#"{"date":{"on":"2026-01-01","tz":"UTC"}}"#;
    assert!(matches!(checked(&nested(31, leaf)).root(), Node::Not(_)));
    bad_node(&nested(32, leaf), leaf, P::Limit);
    let deep = wire(&nested(700, r#"{"mailbox":1}"#));
    let QueryError::Parse(error) = Query::parse_json(&deep).unwrap_err() else {
        panic!("bounded raw walk");
    };
    assert_eq!(error.code(), P::Limit);
    assert!(deep.is_char_boundary(error.start()) && deep.is_char_boundary(error.end()));
    let leaf = r#"{"size":{"op":"eq","bytes":0}}"#;
    let group = |count| format!("{{\"and\":[{}]}}", vec![leaf; count].join(","));
    assert!(matches!(checked(&group(255)).root(), Node::And(children) if children.len() == 255));
    bad_node(&group(256), leaf, P::Limit);
}

#[test]
fn malformed_json_never_falls_back_and_syntax_spans_are_utf8_boundaries() {
    for (input, value, code) in [
        ("", "", P::MissingValue),
        (" \t", "", P::MissingValue),
        ("word", "w", P::UnexpectedToken),
        (r#"{"v":1,"q":"#, "", P::MissingValue),
        (r#"{"v":1,"q":{"and":[{"mailbox":1},"#, "", P::MissingValue),
        (r#"{"v":1,"q":{"text":"abc"#, "\"abc", P::UnclosedQuote),
        (r#"{"v":1,"q":{"and":[]}"#, "{", P::UnclosedGroup),
        (r#"{"v":1,"q":{"mailbox":1e"#, "e", P::UnexpectedToken),
        (r#"{"v":1,"q":tru"#, "", P::UnexpectedToken),
        (r#"{"v":1,"q":{"text":"\q"}}"#, "q", P::UnexpectedToken),
        (r#"{"v":1,"q":{"text":"x"}} !"#, "!", P::UnexpectedToken),
        (
            "{\n\"v\":1,\n\"q\":{\"text\":\"é\"},\n\"z\":]}",
            "]",
            P::UnexpectedToken,
        ),
    ] {
        if code == P::UnclosedQuote || code == P::UnclosedGroup {
            let QueryError::Parse(error) = Query::parse_json(input).unwrap_err() else {
                panic!("opening");
            };
            let start = if code == P::UnclosedQuote {
                input.find(value).unwrap()
            } else {
                input.find('{').unwrap()
            };
            assert_eq!(
                (error.start(), error.end(), error.code()),
                (start, start + 1, code)
            );
        } else {
            rejected(input, value, code);
        }
    }
    for node in [
        r#"{"text":"\uD800"}"#,
        r#"{"text":"\uDC00"}"#,
        r#"{"text":"a
b"}"#,
    ] {
        let QueryError::Parse(error) = Query::parse_json(&wire(node)).unwrap_err() else {
            panic!("malformed string");
        };
        assert_eq!(error.code(), P::UnexpectedToken);
    }
}

#[test]
fn delayed_surrogate_errors_translate_slice_offsets_after_multibyte_input() {
    for node in [r#"{"text":"\uD800"}"#, r#"{"\uD800":true}"#] {
        let input = wire(&format!(r#"{{"and":[{{"text":"é"}},{node}]}}"#));
        let QueryError::Parse(error) = Query::parse_json(&input).unwrap_err() else {
            panic!("malformed surrogate is a syntax error");
        };
        let closing_quote = input.find("\\uD800").unwrap() + 6;
        assert_eq!(
            (error.start(), error.end(), error.code()),
            (closing_quote, closing_quote + 1, P::UnexpectedToken)
        );
        assert!(input.is_char_boundary(error.start()) && input.is_char_boundary(error.end()));
    }
}

#[test]
fn parse_error_codes_remain_the_eight_stable_wire_spellings() {
    for (code, spelling) in [
        (P::UnexpectedToken, "unexpected_token"),
        (P::MissingValue, "missing_value"),
        (P::UnclosedQuote, "unclosed_quote"),
        (P::UnclosedGroup, "unclosed_group"),
        (P::UnknownKey, "unknown_key"),
        (P::InvalidOperator, "invalid_operator"),
        (P::InvalidValue, "invalid_value"),
        (P::Limit, "limit"),
    ] {
        assert_eq!(code.as_str(), spelling);
        let error = SearchParseError::new((2, 4), code);
        assert_eq!(error.to_string(), format!("{spelling} at bytes [2,4)"));
    }
}
