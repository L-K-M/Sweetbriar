//! Synthetic source-identity regressions. Run with the product's exact lock
//! and parser containment profile, not a separately resolved research graph.

use sb_mime::{Limits, MessageSummary, ParseOutcome, parse};

fn summary(headers: &str) -> MessageSummary {
    let raw = format!("{headers}\r\nSubject: Synthetic address identity\r\n\r\nbody\r\n");
    match parse(raw.as_bytes(), &Limits::default()) {
        ParseOutcome::Parsed(summary) => summary,
        other => panic!("valid modern headers must remain complete: {other:?}"),
    }
}

#[test]
fn bare_quoted_locals_remain_source_addr_specs() {
    for address in [
        r#""Foo"@EXAMPLE.com"#,
        r#""a b"@EXAMPLE.com"#,
        r#""é"@EXAMPLE.com"#,
        r#""a\"b"@EXAMPLE.com"#,
        r#"""@EXAMPLE.com"#,
    ] {
        let parsed = summary(&format!("From: {address}"));
        assert_eq!(parsed.from().unwrap().address(), Some(address), "{address}");
        assert_eq!(
            parsed.from().unwrap().name(),
            None,
            "local is not a display name"
        );
    }
}

#[test]
fn angle_quoted_pairs_and_delimiters_remain_source_addr_specs() {
    for address in [
        r#""a\ b"@EXAMPLE.com"#,
        r#""a\"b"@EXAMPLE.com"#,
        r#""a\\b"@EXAMPLE.com"#,
        r#""a>b,c:d;(e)"@EXAMPLE.com"#,
        r#""é @ é"@bücher.example"#,
    ] {
        let parsed = summary(&format!("From: <{address}>"));
        assert_eq!(parsed.from().unwrap().address(), Some(address), "{address}");
    }
}

#[test]
fn display_decoding_groups_and_first_from_policy_are_preserved() {
    let parsed = summary(concat!(
        "From: \"A Display\" <\"Foo\"@EXAMPLE.com>, later@example.test\r\n",
        "To: Team: One <\"a\\ b\"@EXAMPLE.com>, \"b\"@EXAMPLE.com;\r\n",
        "Cc: =?UTF-8?B?w4k=?= <bare@EXAMPLE.com>"
    ));
    assert_eq!(parsed.from().unwrap().name(), Some("A Display"));
    assert_eq!(
        parsed.from().unwrap().address(),
        Some(r#""Foo"@EXAMPLE.com"#)
    );
    assert_eq!(
        parsed
            .to()
            .iter()
            .map(|address| address.address())
            .collect::<Vec<_>>(),
        [Some(r#""a\ b"@EXAMPLE.com"#), Some(r#""b"@EXAMPLE.com"#)]
    );
    assert_eq!(parsed.cc()[0].name(), Some("É"));
    assert_eq!(parsed.cc()[0].address(), Some("bare@EXAMPLE.com"));
}
