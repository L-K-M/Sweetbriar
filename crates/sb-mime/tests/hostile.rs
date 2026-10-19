//! Synthetic spec/10 hostile corpus. No sanitizing is done by the assertions.
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::{Duration, Instant};

use cssparser::{Parser, Token};
use lol_html::{RewriteStrSettings, element, rewrite_str};
use sb_mime::{Limits, RenderKind, RenderWarningCode, Rendition, render, render_input};

const TOKEN: &str = "hostile-token";
// Independent copy of spec/10's property list; do not import renderer policy.
const PROPERTIES: &[&str] = &[
    "color",
    "background-color",
    "font-family",
    "font-size",
    "font-weight",
    "font-style",
    "text-align",
    "text-decoration",
    "text-indent",
    "line-height",
    "vertical-align",
    "white-space",
    "list-style-type",
    "margin",
    "margin-top",
    "margin-right",
    "margin-bottom",
    "margin-left",
    "padding",
    "padding-top",
    "padding-right",
    "padding-bottom",
    "padding-left",
    "border",
    "border-top",
    "border-right",
    "border-bottom",
    "border-left",
    "border-width",
    "border-style",
    "border-color",
    "border-top-width",
    "border-top-style",
    "border-top-color",
    "border-right-width",
    "border-right-style",
    "border-right-color",
    "border-bottom-width",
    "border-bottom-style",
    "border-bottom-color",
    "border-left-width",
    "border-left-style",
    "border-left-color",
    "border-collapse",
    "border-spacing",
    "width",
    "min-width",
    "max-width",
    "height",
    "background-image",
];

fn decoded(value: &str) -> String {
    // lol_html returns serialized attribute values. These are the HTML5
    // serializer's escapes, decoded once, ampersands last.
    value
        .replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", "\u{a0}")
        .replace("&amp;", "&")
}

fn assert_css(css: &str, minted: &BTreeSet<String>, used: &mut BTreeSet<String>) {
    let mut parser = Parser::new(css);
    while !parser.is_exhausted() {
        let property = parser.expect_ident_cloned().unwrap().to_string();
        assert!(
            PROPERTIES.contains(&property.as_str()),
            "property {property}"
        );
        parser.expect_colon().unwrap();
        let mut values = 0;
        while !parser.is_exhausted() {
            let token = parser.next().unwrap().clone();
            match token {
                Token::Semicolon => break,
                Token::UnquotedUrl(url) => {
                    assert_eq!(property, "background-image");
                    assert!(minted.contains(url.as_ref()), "unminted CSS {url}");
                    used.insert(url.to_string());
                }
                Token::Function(name) if name.eq_ignore_ascii_case("url") => {
                    assert_eq!(property, "background-image");
                    let url = parser
                        .parse_nested_block(|p| {
                            let url = p.expect_string_cloned()?.to_string();
                            p.expect_exhausted()?;
                            Ok::<_, cssparser::ParseError<()>>(url)
                        })
                        .unwrap();
                    assert!(minted.contains(&url), "unminted CSS {url}");
                    used.insert(url);
                }
                Token::Function(name) => {
                    assert!(
                        ["rgb", "rgba", "hsl", "hsla"].contains(&name.as_ref()),
                        "function {name}"
                    );
                    parser
                        .parse_nested_block(|p| {
                            while !p.is_exhausted() {
                                assert!(matches!(
                                    p.next()?,
                                    Token::Number { .. }
                                        | Token::Percentage { .. }
                                        | Token::Dimension { .. }
                                        | Token::Comma
                                        | Token::Delim('/')
                                ));
                            }
                            Ok::<_, cssparser::ParseError<()>>(())
                        })
                        .unwrap();
                }
                Token::Ident(_)
                | Token::Number { .. }
                | Token::Percentage { .. }
                | Token::Dimension { .. }
                | Token::Hash(_)
                | Token::IDHash(_) => (),
                Token::QuotedString(_) | Token::Comma if property == "font-family" => (),
                other => panic!("unsafe CSS token {other:?}"),
            }
            values += 1;
        }
        assert!(values > 0);
    }
}

fn assert_invariants(result: &Rendition) {
    assert_eq!(result.kind(), RenderKind::Html);
    let manifest = result.manifest();
    let mut minted = BTreeSet::new();
    let mut files = BTreeSet::new();
    for r in manifest.resources() {
        assert!(r.id().strip_prefix('r').unwrap().parse::<u32>().unwrap() > 0);
        assert!(minted.insert(format!("sweetbriar-cid://{TOKEN}/{}", r.id())));
        assert!(!r.file().contains(['/', '\\']) && !r.file().contains(".."));
        assert!(files.insert(r.file()));
    }
    let mut used = BTreeSet::new();
    let mut hrefs = BTreeSet::new();
    rewrite_str(
        result.content(),
        RewriteStrSettings::new().append_element_content_handler(element!("*", |el| {
            let tag = el.tag_name();
            assert_eq!(el.namespace_uri(), "http://www.w3.org/1999/xhtml");
            assert!(
                ![
                    "script", "style", "form", "input", "button", "iframe", "frame", "frameset",
                    "object", "embed", "applet", "portal", "base", "meta", "link", "svg", "math",
                    "video", "audio", "source", "track", "picture", "template", "noscript"
                ]
                .contains(&tag.as_str()),
                "forbidden tag {tag}"
            );
            for attr in el.attributes() {
                let name = attr.name().to_ascii_lowercase();
                let value = decoded(&attr.value());
                assert!(!name.starts_with("on"), "handler {name}");
                match name.as_str() {
                    "href" => {
                        assert_eq!(tag, "a");
                        let url = url::Url::parse(&value).unwrap();
                        assert!(["http", "https", "mailto"].contains(&url.scheme()));
                        assert_eq!(decoded(&el.get_attribute("title").unwrap()), value);
                        hrefs.insert(value);
                    }
                    "title" => {
                        assert_eq!(tag, "a");
                        assert!(el.get_attribute("href").is_some());
                    }
                    "src" => {
                        assert_eq!(tag, "img");
                        assert!(minted.contains(&value), "unminted src {value}");
                        used.insert(value);
                    }
                    "style" => assert_css(&value, &minted, &mut used),
                    "srcset" | "imagesrcset" | "poster" | "background" | "action"
                    | "formaction" | "data" | "xlink:href" | "ping" | "srcdoc" => {
                        panic!("resource attribute {name}")
                    }
                    _ => (),
                }
            }
            Ok(())
        })),
    )
    .unwrap();
    assert_eq!(used, minted, "manifest resources must be referenced");
    assert_eq!(
        hrefs,
        manifest
            .links()
            .iter()
            .map(|l| l.destination().to_owned())
            .collect()
    );
    assert_eq!(
        hrefs.len(),
        manifest.links().len(),
        "duplicate link records"
    );
    let Json::Object(fields) = parse_json(&manifest.to_json()) else {
        panic!("manifest object")
    };
    assert_eq!(fields.len(), 4);
    assert_eq!(fields["v"], Json::Number(1));
    let resources = manifest
        .resources()
        .iter()
        .map(|r| {
            object([
                ("id", Json::String(r.id().into())),
                ("file", Json::String(r.file().into())),
                ("media_type", Json::String(r.media_type().into())),
                ("bytes", Json::Number(r.bytes().len() as u64)),
            ])
        })
        .collect();
    assert_eq!(fields["resources"], Json::Array(resources));
    let links = manifest
        .links()
        .iter()
        .map(|l| {
            object([
                ("href", Json::String(l.destination().into())),
                (
                    "warnings",
                    Json::Array(
                        l.warnings()
                            .iter()
                            .map(|w| Json::String(w.as_str().into()))
                            .collect(),
                    ),
                ),
            ])
        })
        .collect();
    assert_eq!(fields["links"], Json::Array(links));
    let attachments = manifest
        .attachments()
        .iter()
        .map(|a| {
            object([
                ("part_id", Json::String(a.part_id().as_str().into())),
                ("display_name", Json::String(a.display_name().into())),
                ("media_type", Json::String(a.media_type().into())),
                ("bytes", a.decoded_length().map_or(Json::Null, Json::Number)),
            ])
        })
        .collect();
    assert_eq!(fields["attachments"], Json::Array(attachments));
}

fn object<const N: usize>(fields: [(&str, Json); N]) -> Json {
    Json::Object(
        fields
            .into_iter()
            .map(|(key, value)| (key.into(), value))
            .collect(),
    )
}

fn exercise(raw: &str) {
    let (headers, _) = raw.split_once("\n\n").expect("fixture headers");
    assert!(
        headers
            .lines()
            .any(|l| l.starts_with("X-Sweetbriar-Fixture: "))
    );
    let expect = headers
        .lines()
        .find_map(|l| l.strip_prefix("X-Sweetbriar-Expect: "))
        .unwrap_or("");
    let dense;
    let raw = if expect.split_whitespace().any(|e| e == "dense") {
        // One MiB on the wire, with dense entities and bare ampersands. Keep
        // this generated so corpus changes stay small enough to review.
        let pattern = "&amp;&&#160;";
        dense = raw.replace(
            "DENSE_BODY",
            &pattern.repeat((1_usize << 20).div_ceil(pattern.len())),
        );
        &dense
    } else {
        raw
    };
    let started = Instant::now();
    let input = render_input(raw.as_bytes(), &Limits::default()).expect("read fixture");
    assert!(input.html().is_some(), "fixture must select HTML");
    let result = render(&input, TOKEN, RenderKind::Html);
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "render exceeded 30 seconds"
    );
    assert_eq!(
        result.content() == "<p>Content unavailable.</p>",
        expect.split_whitespace().any(|e| e == "fallback"),
        "unexpected fallback: {:?}",
        result.warnings()
    );
    assert_invariants(&result);
    for expected in expect.split_whitespace() {
        match expected {
            "remote" => assert!(result.remote_content_blocked() > 0, "resource not counted"),
            "blocked=0" => assert_eq!(result.remote_content_blocked(), 0),
            "missing_cid" => assert!(
                result
                    .warnings()
                    .iter()
                    .any(|w| w.code() == RenderWarningCode::MissingCid)
            ),
            "dense" => {
                let text = mail_parser::decoders::html::html_to_text(result.content());
                assert!(text.starts_with("&&\u{a0}"));
                assert!(text.len() > 300_000, "dense content lost");
            }
            "fallback" => (),
            _ if expected.starts_with("resources=") => {
                assert_eq!(
                    result.manifest().resources().len(),
                    expected[10..].parse::<usize>().unwrap()
                );
            }
            _ if expected.starts_with("link=") => {
                let (href, warning) = expected[5..].split_once('|').unwrap();
                let links = result.manifest().links();
                assert_eq!(links.len(), 1, "expected link must survive");
                assert_eq!(links[0].destination(), href);
                let warnings: Vec<_> = links[0].warnings().iter().map(|w| w.as_str()).collect();
                assert_eq!(
                    warnings,
                    if warning == "none" {
                        vec![]
                    } else {
                        vec![warning]
                    }
                );
            }
            _ => panic!("unknown fixture expectation {expected}"),
        }
    }
}

#[test]
fn hostile_corpus() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/hostile");
    let mut paths: Vec<_> = std::fs::read_dir(root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "eml"))
        .collect();
    paths.sort();
    assert!(paths.len() >= 60, "hostile corpus missing");
    for path in paths {
        eprintln!("hostile fixture: {}", path.display());
        let raw = std::fs::read_to_string(&path).unwrap();
        exercise(&raw);
    }
}

// A dependency-free JSON reader for the exact schema fixture: strings,
// unsigned integers, null, arrays and objects. Parsing must consume it all.
#[derive(Debug, PartialEq, Eq)]
enum Json {
    String(String),
    Number(u64),
    Null,
    Array(Vec<Json>),
    Object(BTreeMap<String, Json>),
}
fn read_json(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Json {
    match chars.next().unwrap() {
        '"' => {
            let mut value = String::new();
            loop {
                match chars.next().unwrap() {
                    '"' => break,
                    '\\' => match chars.next().unwrap() {
                        '"' => value.push('"'),
                        '\\' => value.push('\\'),
                        'u' => {
                            let hex: String = chars.take(4).collect();
                            value.push(
                                char::from_u32(u32::from_str_radix(&hex, 16).unwrap()).unwrap(),
                            );
                        }
                        other => panic!("unexpected escape {other}"),
                    },
                    c => {
                        assert!(!c.is_control());
                        value.push(c);
                    }
                }
            }
            Json::String(value)
        }
        '[' => {
            let mut values = Vec::new();
            while chars.peek() != Some(&']') {
                if !values.is_empty() {
                    assert_eq!(chars.next(), Some(','));
                }
                values.push(read_json(chars));
            }
            chars.next();
            Json::Array(values)
        }
        '{' => {
            let mut fields = BTreeMap::new();
            while chars.peek() != Some(&'}') {
                if !fields.is_empty() {
                    assert_eq!(chars.next(), Some(','));
                }
                let Json::String(key) = read_json(chars) else {
                    panic!("object key");
                };
                assert_eq!(chars.next(), Some(':'));
                assert!(fields.insert(key, read_json(chars)).is_none());
            }
            chars.next();
            Json::Object(fields)
        }
        'n' => {
            assert_eq!(chars.take(3).collect::<String>(), "ull");
            Json::Null
        }
        c if c.is_ascii_digit() => {
            let mut n = c.to_string();
            while chars.peek().is_some_and(char::is_ascii_digit) {
                n.push(chars.next().unwrap());
            }
            Json::Number(n.parse().unwrap())
        }
        other => panic!("unexpected JSON token {other}"),
    }
}
fn parse_json(value: &str) -> Json {
    let mut chars = value.chars().peekable();
    let parsed = read_json(&mut chars);
    assert_eq!(chars.next(), None);
    parsed
}
