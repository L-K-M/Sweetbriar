//! Inert HTML policy. Resource/CSS rewriting and rendition limits belong to M1.5d.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use ammonia::{Builder, UrlRelative};
use lol_html::html_content::{ContentType, TextType};
use lol_html::{RewriteStrSettings, doc_text, element, rewrite_str, text};
use url::{Host, Url};

// Text structure and table layout only. No active or foreign-namespace elements.
const TAGS: &[&str] = &[
    "a",
    "abbr",
    "address",
    "b",
    "bdi",
    "bdo",
    "blockquote",
    "br",
    "caption",
    "cite",
    "code",
    "col",
    "colgroup",
    "dd",
    "del",
    "dfn",
    "div",
    "dl",
    "dt",
    "em",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "hr",
    "i",
    "ins",
    "kbd",
    "li",
    "ol",
    "p",
    "pre",
    "q",
    "rp",
    "rt",
    "ruby",
    "s",
    "samp",
    "small",
    "span",
    "strong",
    "sub",
    "sup",
    "table",
    "tbody",
    "td",
    "tfoot",
    "th",
    "thead",
    "time",
    "tr",
    "u",
    "ul",
    "var",
    "wbr",
];
const DROP_CONTENT: &[&str] = &[
    "script",
    "style",
    "iframe",
    "frame",
    "frameset",
    "object",
    "embed",
    "form",
    "input",
    "button",
    "select",
    "option",
    "optgroup",
    "textarea",
    "base",
    "meta",
    "link",
    "svg",
    "math",
    "template",
    "noscript",
    "noembed",
    "noframes",
    "xmp",
    "plaintext",
    "title",
    "audio",
    "video",
    "source",
    "track",
    "canvas",
];
// Sender tooltips on descendants must not override a link's trusted title.
const GENERIC_ATTRIBUTES: &[&str] = &["dir", "lang"];
const TAG_ATTRIBUTES: &[(&str, &[&str])] = &[
    ("a", &["href", "title"]),
    ("ol", &["start", "reversed", "type"]),
    ("li", &["value"]),
    ("col", &["span", "width", "align", "valign"]),
    ("colgroup", &["span", "width", "align", "valign"]),
    (
        "table",
        &["width", "border", "cellpadding", "cellspacing", "align"],
    ),
    ("tr", &["align", "valign"]),
    (
        "td",
        &["colspan", "rowspan", "width", "height", "align", "valign"],
    ),
    (
        "th",
        &[
            "colspan", "rowspan", "width", "height", "align", "valign", "scope",
        ],
    ),
];
const LINK_SCHEMES: &[&str] = &["http", "https", "mailto"];
/// Longest link destination kept, and its title.
const MAX_HREF_BYTES: usize = 4096;
/// Longest value kept for any other attribute: layout numbers and codes.
const MAX_ATTRIBUTE_BYTES: usize = 256;
/// A text run's escaping cost, bytes times escaped characters, past which
/// `split_dense_text` splits it: about 1,000 bytes of `&`.
const SPLIT_COST: usize = 1 << 20;
/// Splits a text node in two; empty, it renders as nothing.
const TEXT_BREAK: &str = "<span></span>";

/// Per-destination link warnings in the spec/07 rendition manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LinkWarning {
    TextDomain,
    RawIp,
    UserInfo,
}

impl LinkWarning {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TextDomain => "text_domain",
            Self::RawIp => "raw_ip",
            Self::UserInfo => "user_info",
        }
    }
}

/// One canonical destination, with the union of all its anchors' warnings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkRecord {
    destination: String,
    warnings: Vec<LinkWarning>,
}

impl LinkRecord {
    pub fn destination(&self) -> &str {
        &self.destination
    }

    pub fn warnings(&self) -> &[LinkWarning] {
        &self.warnings
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SanitizedHtml {
    html: String,
    links: Vec<LinkRecord>,
}

impl SanitizedHtml {
    pub fn html(&self) -> &str {
        &self.html
    }

    pub fn links(&self) -> &[LinkRecord] {
        &self.links
    }
}

/// A sanitizer failure returns no partially sanitized content or parser payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HtmlSanitizeError {
    Rewrite,
    Panic,
}

impl std::fmt::Display for HtmlSanitizeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Rewrite => "the HTML rewriter failed",
            Self::Panic => "an HTML parser panicked",
        })
    }
}

impl std::error::Error for HtmlSanitizeError {}

fn policy() -> Builder<'static> {
    let mut policy = Builder::new();
    policy
        .tags(TAGS.iter().copied().collect::<HashSet<_>>())
        .clean_content_tags(DROP_CONTENT.iter().copied().collect())
        .generic_attributes(GENERIC_ATTRIBUTES.iter().copied().collect())
        .tag_attributes(
            TAG_ATTRIBUTES
                .iter()
                .map(|&(tag, attrs)| (tag, attrs.iter().copied().collect()))
                .collect::<HashMap<_, _>>(),
        )
        .url_schemes(LINK_SCHEMES.iter().copied().collect())
        .url_relative(UrlRelative::Deny)
        .link_rel(None)
        .attribute_filter(|tag, attr, value| {
            if tag == "a" && attr == "href" {
                return safe_destination(value)
                    .map(|url| url.to_string())
                    .filter(|href| href.len() <= MAX_HREF_BYTES)
                    .map(Into::into);
            }
            let limit = if attr == "title" {
                MAX_HREF_BYTES
            } else {
                MAX_ATTRIBUTE_BYTES
            };
            (value.len() <= limit).then(|| value.into())
        });
    // No style or resource-bearing attributes: M1.5d adds checked CSS and CID
    // rewriting/counting before this final allow-list boundary.
    policy
}

fn safe_destination(value: &str) -> Option<Url> {
    // No base URL is supplied: relative and protocol-relative URLs fail closed.
    let url = Url::parse(value).ok()?;
    if !LINK_SCHEMES.contains(&url.scheme()) {
        return None;
    }
    if matches!(url.scheme(), "http" | "https") && url.host().is_none() {
        return None;
    }
    Some(url)
}

/// Sanitize decoded HTML, e.g. [`crate::SelectedBody::text`], into inert markup.
/// This c2 policy strips all inline styles and resources pending M1.5d. It does
/// not count blocked resources or generate a complete rendition manifest.
/// Link records are sorted by canonical destination for deterministic output.
/// Unwinds are contained across both HTML parsers; panic payloads are discarded.
pub fn sanitize_html(decoded_html: &str) -> Result<SanitizedHtml, HtmlSanitizeError> {
    std::panic::catch_unwind(|| sanitize_run(decoded_html)).unwrap_or(Err(HtmlSanitizeError::Panic))
}

fn sanitize_run(decoded_html: &str) -> Result<SanitizedHtml, HtmlSanitizeError> {
    #[cfg(test)]
    if decoded_html == tests::PANIC_MARKER {
        panic!("sb-mime sanitizer test tripwire");
    }
    let policy = policy();
    let split = split_dense_text(decoded_html)?;
    // Normalize namespaces and malformed trees before collecting links, so
    // removed subtrees cannot contribute warning records.
    let normalized = policy.clean(&split).to_string();
    let anchors = std::cell::RefCell::new(Vec::<(Url, String)>::new());
    let rewritten = rewrite_str(
        &normalized,
        RewriteStrSettings::new()
            .append_element_content_handler(element!("a", |el| {
                let Some(href) = el.get_attribute("href") else {
                    el.remove_attribute("title");
                    return Ok(());
                };
                // lol_html exposes serialized attribute values. The initial
                // HTML5 pass has already decoded and canonicalized the URL.
                let decoded = decode_serialized(&href);
                let Some(url) = safe_destination(&decoded) else {
                    el.remove_attribute("href");
                    el.remove_attribute("title");
                    return Ok(());
                };
                el.set_attribute("title", &href)?;
                anchors.borrow_mut().push((url, String::new()));
                Ok(())
            }))
            .append_element_content_handler(text!("a[href]", |text| {
                if let Some((_, label)) = anchors.borrow_mut().last_mut() {
                    label.push_str(text.as_str());
                }
                Ok(())
            })),
    )
    .map_err(|_| HtmlSanitizeError::Rewrite)?;
    let mut destinations: BTreeMap<String, BTreeSet<LinkWarning>> = BTreeMap::new();
    for (url, label) in anchors.into_inner() {
        let warnings = destinations.entry(url.to_string()).or_default();
        if matches!(url.host(), Some(Host::Ipv4(_) | Host::Ipv6(_))) {
            warnings.insert(LinkWarning::RawIp);
        }
        if !url.username().is_empty() || url.password().is_some() {
            warnings.insert(LinkWarning::UserInfo);
        }
        let label = decode_serialized(&label);
        let targets = destination_hosts(&url);
        if label_hosts(&label).any(|host| !targets.contains(&host)) {
            warnings.insert(LinkWarning::TextDomain);
        }
    }
    // The first pass removed markup between short text nodes, so their
    // text now runs together: split again before the final serialization.
    let rewritten = split_dense_text(&rewritten)?;
    Ok(SanitizedHtml {
        html: policy.clean(&rewritten).to_string(),
        links: destinations
            .into_iter()
            .map(|(destination, warnings)| LinkRecord {
                destination,
                warnings: warnings.into_iter().collect(),
            })
            .collect(),
    })
}

// These are the five escapes emitted by the pinned HTML5 serializer, not a
// decoder for sender HTML. Decode ampersands last to avoid recursive decoding.
fn decode_serialized(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&nbsp;", "\u{a0}")
        .replace("&amp;", "&")
}

/// The domains a destination reaches: its host, or a `mailto:` link's
/// recipient domains, normalized as `label_hosts` normalizes.
fn destination_hosts(url: &Url) -> Vec<String> {
    if let Some(host) = url.host_str() {
        return vec![host.to_owned()];
    }
    url.path()
        .split(',')
        .filter_map(|recipient| {
            let domain = recipient.rsplit_once('@')?.1;
            let url = Url::parse(&format!("https://{domain}")).ok()?;
            url.host_str().map(str::to_owned)
        })
        .collect()
}

/// html5ever 0.40's serializer rescans for the next `<` or `>` at every
/// `&` and U+0080–U+00BF it escapes, so a text node costs about its length
/// times those characters: 100,000 `&` took 35 seconds. Once a run's cost
/// reaches `SPLIT_COST` it is split with an empty `<span>`, which renders
/// as nothing, outside any character reference. Only ordinary text is
/// split; raw text such as `<title>` would show the tag.
fn split_dense_text(html: &str) -> Result<String, HtmlSanitizeError> {
    #[derive(Default)]
    struct Run {
        bytes: usize,
        escapes: usize,
        in_reference: bool,
    }
    let run = std::cell::RefCell::new(Run::default());
    rewrite_str(
        html,
        RewriteStrSettings::new().append_document_content_handler(doc_text!(|chunk| {
            let mut run = run.borrow_mut();
            if chunk.text_type() != TextType::Data {
                *run = Run::default();
                return Ok(());
            }
            let mut split = String::new();
            let mut changed = false;
            for c in chunk.as_str().chars() {
                // A name or number character continues a reference, and its
                // `;` ends it: `&amp` + span + `;` would show the `;`.
                let continues =
                    run.in_reference && (c.is_ascii_alphanumeric() || c == '#' || c == ';');
                if !continues && run.bytes.saturating_mul(run.escapes) >= SPLIT_COST {
                    split.push_str(TEXT_BREAK);
                    *run = Run::default();
                    changed = true;
                }
                split.push(c);
                run.bytes += c.len_utf8();
                if c == '&' || ('\u{80}'..='\u{bf}').contains(&c) {
                    run.escapes += 1;
                }
                run.in_reference = c == '&' || (continues && c != ';');
            }
            if chunk.last_in_text_node() {
                *run = Run::default();
            }
            if changed {
                chunk.replace(&split, ContentType::Html);
            }
            Ok(())
        })),
    )
    .map_err(|_| HtmlSanitizeError::Rewrite)
}

fn label_hosts(label: &str) -> impl Iterator<Item = String> + '_ {
    label.split_whitespace().filter_map(|token| {
        let token =
            token.trim_matches(|c| matches!(c, '(' | ')' | '<' | '>' | ',' | ';' | '"' | '\''));
        let url = Url::parse(token)
            .ok()
            .filter(|url| url.host().is_some())
            .or_else(|| {
                // Recognize bare domains as well as URLs, without interpreting
                // ordinary prose words as domain names.
                let host = token.trim_end_matches('@').rsplit('@').next()?;
                if !host.contains('.') && !(host.starts_with('[') && host.ends_with(']')) {
                    return None;
                }
                Url::parse(&format!("https://{host}")).ok()
            })?;
        url.host_str().map(str::to_owned)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) const PANIC_MARKER: &str = "sb-mime-sanitizer-test-panic";

    #[test]
    fn contains_html_parser_panics() {
        assert_eq!(sanitize_html(PANIC_MARKER), Err(HtmlSanitizeError::Panic));
    }

    #[derive(Debug)]
    struct Element {
        tag: String,
        attrs: BTreeMap<String, String>,
    }

    // Reparse serialized output without sanitizing it: otherwise the assertion
    // could accidentally repair the very unsafe markup it is meant to detect.
    fn elements(html: &str) -> Vec<Element> {
        let mut elements = Vec::new();
        rewrite_str(
            html,
            RewriteStrSettings::new().append_element_content_handler(element!("*", |el| {
                assert_eq!(el.namespace_uri(), "http://www.w3.org/1999/xhtml");
                elements.push(Element {
                    tag: el.tag_name(),
                    attrs: el
                        .attributes()
                        .iter()
                        .map(|a| (a.name(), a.value()))
                        .collect(),
                });
                Ok(())
            })),
        )
        .unwrap();
        elements
    }

    fn assert_inert(html: &str) -> Vec<Element> {
        let elements = elements(html);
        for el in &elements {
            assert!(TAGS.contains(&el.tag.as_str()), "{el:?}");
            for (name, value) in &el.attrs {
                assert!(!name.starts_with("on"), "{el:?}");
                assert!(
                    ![
                        "style",
                        "src",
                        "srcset",
                        "background",
                        "poster",
                        "ping",
                        "download"
                    ]
                    .contains(&name.as_str()),
                    "{el:?}"
                );
                let allowed = GENERIC_ATTRIBUTES.contains(&name.as_str())
                    || TAG_ATTRIBUTES
                        .iter()
                        .any(|&(tag, attrs)| tag == el.tag && attrs.contains(&name.as_str()));
                assert!(allowed, "{el:?}");
                if name == "href" {
                    assert_eq!(el.tag, "a");
                    let value = mail_parser::decoders::html::html_to_text(value);
                    let url = Url::parse(&value).unwrap();
                    assert!(LINK_SCHEMES.contains(&url.scheme()));
                }
            }
        }
        elements
    }

    #[test]
    fn removes_active_subtrees_and_resource_attributes() {
        let html = r#"<p LANG=en DiR=ltr ONCLICK=bad()>outside</p>
            <script><a href="https://hidden.example.invalid">script</a></script>
            <style>body { background: url(https://example.invalid/x) }</style>
            <iframe srcdoc="<p>bad</p>">frame text</iframe><frame src=x>
            <object data=x>object text</object><embed src=x>
            <form action=x><input src=x><button formaction=x>submit</button>form text</form>
            <base href="https://example.invalid"><meta http-equiv=refresh content=0><link href=x>
            <template><a href="https://hidden.example.invalid">template text</a></template>
            <div style="color:red" background=x poster=x src=x srcset=x onmouseover=x>kept</div>
            <img src=x srcset="x 1x,y 2x"><video poster=x src=x>video text</video>"#;
        let result = sanitize_html(html).unwrap();
        let elements = assert_inert(result.html());
        assert!(result.links().is_empty());
        let text = mail_parser::decoders::html::html_to_text(result.html());
        assert!(text.contains("outside"));
        assert!(text.contains("kept"));
        for forbidden in [
            "script",
            "frame text",
            "object text",
            "submit",
            "form text",
            "template text",
            "video text",
        ] {
            assert!(!text.contains(forbidden), "{text}");
        }
        assert!(
            elements
                .iter()
                .any(|el| el.attrs.get("lang").is_some_and(|v| v == "en"))
        );
    }

    #[test]
    fn denies_relative_and_obfuscated_unsafe_links() {
        let hrefs = [
            "javascript:alert(1)",
            "jav&#x61;script:alert(1)",
            "java&#9;script:alert(1)",
            "java&#10;script:alert(1)",
            "&#1;javascript:alert(1)",
            "JaVaScRiPt:alert(1)",
            "data:text/html,bad",
            "file:///tmp/x",
            "vbscript:x",
            "blob:https://example.invalid/x",
            "sweetbriar-cid://sender/x",
            "cid:x",
            "//example.invalid/x",
            "/x",
            "../x",
            "#x",
            "?x",
            "x",
        ];
        for href in hrefs {
            let html = format!(
                r#"<a HrEf="{href}" title="sender" PiNg="https://example.invalid" DoWnLoAd>label</a>"#
            );
            let result = sanitize_html(&html).unwrap();
            let elements = assert_inert(result.html());
            assert_eq!(elements.len(), 1, "{href}");
            assert!(elements[0].attrs.is_empty(), "{href}: {elements:?}");
            assert!(result.links().is_empty(), "{href}");
            assert_eq!(
                mail_parser::decoders::html::html_to_text(result.html()),
                "label"
            );
        }
    }

    #[test]
    fn foreign_namespaces_and_malformed_markup_do_not_resurrect() {
        let inputs = [
            r#"<svg><foreignObject><a href="https://hidden.example.invalid">hidden</a><script>bad</script></foreignObject></svg><p>outside</p>"#,
            r#"<math><annotation-xml encoding="text/html"><svg><foreignObject><iframe src=x></iframe></foreignObject></svg></annotation-xml></math><p>outside</p>"#,
            r#"<math><mtext><table><mglyph><style><!--</style><img title="--><img src=x onerror=bad()>">"#,
            r#"<svg></p><style><g title="</style><img src=x onerror=bad()>">"#,
            r#"<table><form><input><tr><td><a href="javascript:x" href="https://example.invalid">text</a></table></form>"#,
            r#"<noscript><p title="</noscript><img src=x onerror=bad()>">x</p>"#,
            r#"<p>&lt;script&gt;literal&lt;/script&gt;</p><!-- unclosed <script>"#,
        ];
        for input in inputs {
            let result = sanitize_html(input).unwrap();
            assert_inert(result.html());
            // HTML5 reparsing/serialization must be stable, including raw-text
            // and foreign-content transitions handled by the final sanitizer.
            assert_eq!(policy().clean(result.html()).to_string(), result.html());
            assert!(result.links().is_empty());
            if input.contains("outside") {
                assert!(result.html().contains("outside"));
                assert!(!result.html().contains("hidden"));
            }
        }
    }

    #[test]
    fn safe_links_have_real_titles_and_destination_warning_unions() {
        let html = r#"<a HREF="HTTPS://EXAMPLE.INVALID/path?a=1&amp;b=2" title="lie" target=_blank ping=x download>example.invalid</a>
            <a href="https://example.invalid/path?a=1&amp;b=2"><b title="forged tooltip">other&#46;example.invalid</b></a>
            <a href="https://example.invalid/path?a=1&amp;b=2">example.invalid</a>
            <a href="https://user:pass@192.0.2.1/x">https://other.example.invalid</a>
            <a href="http://[2001:db8::1]/">open</a>
            <a href="mailto:reader@example.invalid?subject=Hello&amp;body=World">email</a>"#;
        let result = sanitize_html(html).unwrap();
        let elements = assert_inert(result.html());
        assert!(
            elements
                .iter()
                .filter(|el| el.tag != "a")
                .all(|el| !el.attrs.contains_key("title"))
        );
        let anchors: Vec<_> = elements.iter().filter(|el| el.tag == "a").collect();
        assert_eq!(anchors.len(), 6);
        for anchor in anchors {
            assert_eq!(anchor.attrs["title"], anchor.attrs["href"]);
        }
        let records: BTreeMap<_, _> = result
            .links()
            .iter()
            .map(|l| (l.destination(), l.warnings()))
            .collect();
        assert_eq!(records.len(), 4);
        assert_eq!(
            records["https://example.invalid/path?a=1&b=2"],
            &[LinkWarning::TextDomain]
        );
        assert_eq!(
            records["https://user:pass@192.0.2.1/x"],
            &[
                LinkWarning::TextDomain,
                LinkWarning::RawIp,
                LinkWarning::UserInfo
            ]
        );
        assert_eq!(records["http://[2001:db8::1]/"], &[LinkWarning::RawIp]);
        assert!(records["mailto:reader@example.invalid?subject=Hello&body=World"].is_empty());
        assert_eq!(LinkWarning::TextDomain.as_str(), "text_domain");
        assert_eq!(LinkWarning::RawIp.as_str(), "raw_ip");
        assert_eq!(LinkWarning::UserInfo.as_str(), "user_info");
    }

    #[test]
    fn destination_entities_are_decoded_once_without_text_conversion() {
        for destination in [
            "https://example.invalid/?a=1&b=2&literal=&amp;",
            "mailto:reader@example.invalid,<other@example.invalid>",
            "mailto:reader@example.invalid?subject=two words",
        ] {
            let escaped = destination
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;");
            let result = sanitize_html(&format!(r#"<a href="{escaped}">open</a>"#)).unwrap();
            let expected = Url::parse(destination).unwrap().to_string();
            assert_eq!(result.links()[0].destination(), expected);
            let elements = assert_inert(result.html());
            assert_eq!(decode_serialized(&elements[0].attrs["title"]), expected);
            assert_eq!(decode_serialized(&elements[0].attrs["href"]), expected);
        }
    }

    #[test]
    fn compares_parsed_hosts_instead_of_prefixes_or_case() {
        for label in [
            "EXAMPLE.INVALID",
            "https://EXAMPLE.INVALID:443/another",
            "EXAMPLE.INVALID:443/another",
            "<b>example.</b><i>invalid</i>",
        ] {
            let html = format!(r#"<a href="https://example.invalid/">{label}</a>"#);
            assert!(
                sanitize_html(&html).unwrap().links()[0]
                    .warnings()
                    .is_empty(),
                "{label}"
            );
        }
        for label in [
            "https://example.invalid.other.invalid/",
            "See other.example.invalid",
            "other.example.invalid:443/path",
            "https://example.invalid@other.invalid/",
        ] {
            let html = format!(r#"<a href="https://example.invalid/">{label}</a>"#);
            assert_eq!(
                sanitize_html(&html).unwrap().links()[0].warnings(),
                &[LinkWarning::TextDomain],
                "{label}"
            );
        }
    }

    #[test]
    fn email_and_domain_mention_labels_are_host_compared() {
        for label in [
            "reader@other.example.invalid",
            "@other.example.invalid",
            "other.example.invalid@",
            "[2001:db8::2]",
        ] {
            let html = format!(r#"<a href="https://example.invalid/">{label}</a>"#);
            assert_eq!(
                sanitize_html(&html).unwrap().links()[0].warnings(),
                &[LinkWarning::TextDomain],
                "{label}"
            );
        }
        let result =
            sanitize_html(r#"<a href="https://example.invalid/">first.last@example.invalid</a>"#)
                .unwrap();
        assert!(result.links()[0].warnings().is_empty());
    }

    #[test]
    fn mailto_labels_compare_with_the_recipient_domain() {
        for (destination, label) in [
            ("mailto:support@real.invalid", "support@phish.invalid"),
            ("mailto:support@real.invalid", "https://phish.invalid/"),
            ("mailto:?subject=hello", "phish.invalid"),
        ] {
            let html = format!(r#"<a href="{destination}">{label}</a>"#);
            assert_eq!(
                sanitize_html(&html).unwrap().links()[0].warnings(),
                &[LinkWarning::TextDomain],
                "{label}"
            );
        }
        for label in [
            "support@real.invalid",
            "SUPPORT@REAL.INVALID",
            "other.invalid",
            "Write to us",
        ] {
            let html =
                format!(r#"<a href="mailto:support@real.invalid,b@other.invalid">{label}</a>"#);
            assert!(
                sanitize_html(&html).unwrap().links()[0]
                    .warnings()
                    .is_empty(),
                "{label}"
            );
        }
    }

    /// The pinned serializer rescans for `<` or `>` at every `&` and
    /// U+0080–U+00BF it meets (html5ever 0.40), so a dense text node costs
    /// its length squared: 100,000 `&` took 35 seconds. Dense runs are split.
    #[test]
    fn dense_escapes_are_split_into_short_text_runs() {
        const UNITS: usize = 20_000;
        for (unit, decoded) in [
            ("&amp;", "&"),
            ("&", "&"),
            ("&nbsp;", "\u{a0}"),
            ("\u{a0}", "\u{a0}"),
            ("©", "©"),
        ] {
            let html = format!("<p>{}&lt;b&gt;</p>", unit.repeat(UNITS));
            let result = sanitize_html(&html).unwrap();
            assert_inert(result.html());
            let longest = result.html().split(['<', '>']).map(str::len).max().unwrap();
            assert!(longest < 8 * 1024, "{unit:?}: a {longest}-byte run");
            let text = mail_parser::decoders::html::html_to_text(result.html());
            assert_eq!(text.matches(decoded).count(), UNITS, "{unit:?}");
            assert!(text.trim_end().ends_with("<b>"), "{unit:?}");
        }
    }

    /// Markup the sanitizer removes (a disallowed element, a comment)
    /// leaves its neighbours' text contiguous for the final pass, so short
    /// source text nodes can still serialize as one dense run.
    #[test]
    fn removed_markup_does_not_rejoin_dense_text() {
        for unit in [
            "<font>&amp;</font>",
            "&amp;<!--x-->",
            "<blink>&nbsp;</blink>",
        ] {
            let html = format!("<p>{}</p>", unit.repeat(20_000));
            let result = sanitize_html(&html).unwrap();
            let longest = result.html().split(['<', '>']).map(str::len).max().unwrap();
            assert!(longest < 8 * 1024, "{unit:?}: a {longest}-byte run");
        }
    }

    /// A split never falls inside a character reference, whatever text
    /// precedes the run: `&amp` + span + `;` would show a semicolon.
    #[test]
    fn splits_keep_character_references_whole() {
        const UNITS: usize = 2_000;
        for prefix in ["", "x", "xy", "xyz", "é"] {
            for (unit, decoded) in [
                ("&amp;", "&"),
                ("&#38;", "&"),
                ("&#x26;", "&"),
                ("&nbsp;", "\u{a0}"),
                ("&lt;", "<"),
            ] {
                let html = format!("<p>{prefix}{}</p>", unit.repeat(UNITS));
                let result = sanitize_html(&html).unwrap();
                let text = mail_parser::decoders::html::html_to_text(result.html());
                assert!(
                    !text.contains(';'),
                    "{prefix:?} {unit:?}: a split reference"
                );
                assert_eq!(text.matches(decoded).count(), UNITS, "{prefix:?} {unit:?}");
            }
        }
    }

    /// Attribute values escape the same way, so long ones are dropped.
    #[test]
    fn long_attribute_values_are_dropped() {
        let long_query = "&".repeat(MAX_HREF_BYTES);
        let html = format!(
            r#"<ol start="{}" type="1"><li>x</li></ol><a href="https://example.invalid/?{long_query}">long</a><a href="https://example.invalid/?a&b">short</a>"#,
            "&".repeat(MAX_ATTRIBUTE_BYTES + 1)
        );
        let result = sanitize_html(&html).unwrap();
        let elements = assert_inert(result.html());
        let list = elements.iter().find(|el| el.tag == "ol").unwrap();
        assert!(!list.attrs.contains_key("start"));
        assert_eq!(list.attrs.get("type").map(String::as_str), Some("1"));
        let anchors: Vec<_> = elements.iter().filter(|el| el.tag == "a").collect();
        assert!(anchors[0].attrs.is_empty());
        assert!(anchors[1].attrs.contains_key("href"));
        assert_eq!(result.links().len(), 1);
    }

    #[test]
    fn preserves_benign_text_and_table_layout() {
        let html = r#"<h2>Hello &amp; welcome</h2><p lang=en>Text <em>emphasis</em> &lt;script&gt;</p>
            <table width=600 border=1 cellpadding=4 cellspacing=0><caption>Data</caption>
            <colgroup><col width=200 span=2></colgroup><thead><tr><th scope=col>Heading</th></tr></thead>
            <tbody><tr align=left><td colspan=2 rowspan=3 width=400 height=20 valign=top>Cell</td></tr></tbody></table>
            <ol start=2 reversed><li value=3>Item</li></ol><pre>  code\n</pre>"#;
        let result = sanitize_html(html).unwrap();
        let elements = assert_inert(result.html());
        for tag in [
            "h2", "p", "em", "table", "caption", "colgroup", "col", "thead", "tbody", "tr", "th",
            "td", "ol", "li", "pre",
        ] {
            assert!(elements.iter().any(|el| el.tag == tag), "{tag}");
        }
        let cell = elements.iter().find(|el| el.tag == "td").unwrap();
        assert_eq!(cell.attrs.get("colspan").unwrap(), "2");
        assert_eq!(cell.attrs.get("rowspan").unwrap(), "3");
        assert_eq!(cell.attrs.get("valign").unwrap(), "top");
        let table = elements.iter().find(|el| el.tag == "table").unwrap();
        assert_eq!(table.attrs.get("width").unwrap(), "600");
        assert_eq!(table.attrs.get("cellpadding").unwrap(), "4");
        let text = mail_parser::decoders::html::html_to_text(result.html());
        for expected in [
            "Hello & welcome",
            "emphasis",
            "<script>",
            "Heading",
            "Cell",
            "Item",
        ] {
            assert!(text.contains(expected), "{expected}: {text}");
        }
    }
}
