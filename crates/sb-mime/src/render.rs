//! Inert HTML policy. Resource/CSS rewriting and rendition limits belong to M1.5d.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use ammonia::{Builder, UrlRelative};
use lol_html::html_content::ContentType;
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
/// Begins a character spelled `PLACEHOLDER` + hex + `;` in text between
/// the first parse and the last serialization (see `neutralize_text`).
/// Private use, and a sender's own is spelled that way too.
const PLACEHOLDER: char = '\u{e000}';

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
    let neutral = neutralize_text(decoded_html)?;
    // Normalize namespaces and malformed trees before collecting links, so
    // removed subtrees cannot contribute warning records.
    let normalized = policy.clean(&neutral).to_string();
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
        let label = decode_label(&label);
        let targets = destination_hosts(&url);
        if label_hosts(&label).any(|host| !targets.contains(&host)) {
            warnings.insert(LinkWarning::TextDomain);
        }
    }
    Ok(SanitizedHtml {
        html: restore_text(&policy.clean(&rewritten).to_string())?,
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
/// times those characters: 100,000 `&` took 35 seconds. Which text nodes
/// form only the tree builder knows (it joins text around ignored tags,
/// fosters text out of tables, drops comments), so instead no such
/// character reaches it. Before the first parse each text node's
/// references are decoded as HTML5's tokenizer decodes them, at its
/// boundaries, and every `&`, `<`, `>` and U+0080–U+00BF in the result is
/// spelled `PLACEHOLDER` + hex + `;`. Every serialization is then linear;
/// `restore_text` spells them back. Raw text is neutralized too, since
/// lol_html and html5ever can disagree about it; html5ever's drops anyway.
fn neutralize_text(html: &str) -> Result<String, HtmlSanitizeError> {
    per_text_node(html, |text| {
        // The parser turns source line breaks into line feeds before it
        // decodes anything; only a referenced carriage return survives.
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let mut neutral = String::with_capacity(text.len());
        for c in decode_references(&text).chars() {
            // A decoded carriage return too: as itself the next parse
            // would normalize it to a line feed.
            if matches!(
                c,
                '&' | '<' | '>' | '\r' | PLACEHOLDER | '\u{80}'..='\u{bf}'
            ) {
                neutral.push(PLACEHOLDER);
                neutral.push_str(&format!("{:X};", u32::from(c)));
            } else {
                neutral.push(c);
            }
        }
        neutral
    })
}

/// Undo `neutralize_text` in the final, serialized text: each placeholder
/// spells its character again, `&`, `<` and `>` as references. Attributes
/// are never touched.
fn restore_text(html: &str) -> Result<String, HtmlSanitizeError> {
    per_text_node(html, |text| {
        let mut restored = String::with_capacity(text.len());
        for c in restored_chars(text) {
            match c {
                '&' => restored.push_str("&amp;"),
                '<' => restored.push_str("&lt;"),
                '>' => restored.push_str("&gt;"),
                '\r' => restored.push_str("&#13;"),
                c => restored.push(c),
            }
        }
        restored
    })
}

/// A link's label as the reader shows it: the neutral text's characters.
fn decode_label(label: &str) -> String {
    restored_chars(label).collect()
}

/// The characters of neutral text, placeholders spelled out.
fn restored_chars(text: &str) -> impl Iterator<Item = char> + '_ {
    let mut rest = text;
    std::iter::from_fn(move || {
        let mut chars = rest.chars();
        let c = chars.next()?;
        if c == PLACEHOLDER
            && let Some((hex, tail)) = chars.as_str().split_once(';')
            && let Some(decoded) = u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
        {
            rest = tail;
            return Some(decoded);
        }
        rest = chars.as_str();
        Some(c)
    })
}

/// Rewrite each ordinary text node of `html` whole (lol_html may hand it
/// over in chunks) with `rewrite`, whose result is markup.
fn per_text_node(
    html: &str,
    mut rewrite: impl FnMut(&str) -> String,
) -> Result<String, HtmlSanitizeError> {
    let mut node = String::new();
    rewrite_str(
        html,
        RewriteStrSettings::new().append_document_content_handler(doc_text!(|chunk| {
            // Every kind of text: lol_html and html5ever can disagree about
            // raw text in malformed foreign content, and what html5ever
            // treats as raw text the allow-list drops anyway.
            node.push_str(chunk.as_str());
            if chunk.last_in_text_node() {
                chunk.replace(&rewrite(&node), ContentType::Html);
                node.clear();
            } else {
                chunk.remove();
            }
            Ok(())
        })),
    )
    .map_err(|_| HtmlSanitizeError::Rewrite)
}

/// Decode `text`'s character references as HTML5's tokenizer does in
/// ordinary text: html5ever 0.40's algorithm (tokenizer/char_ref), with
/// the same entity table and C1 remapping, so the sanitized text reads
/// exactly as the sender's would.
fn decode_references(text: &str) -> String {
    use markup5ever::data::{C1_REPLACEMENTS, NAMED_ENTITIES};
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        if let Some(numeric) = after.strip_prefix('#') {
            let (hex, digits) = match numeric.strip_prefix(['x', 'X']) {
                Some(hex) => (true, hex),
                None => (false, numeric),
            };
            let base = if hex { 16 } else { 10 };
            let length = digits
                .find(|c: char| !c.is_digit(base))
                .unwrap_or(digits.len());
            if length == 0 {
                // No digits: the `&` is literal, `#` and `x` plain text.
                out.push('&');
                rest = after;
                continue;
            }
            let value = digits[..length]
                .chars()
                .try_fold(0u32, |n, d| {
                    n.checked_mul(base)?.checked_add(d.to_digit(base)?)
                })
                .filter(|&n| n <= 0x10FFFF);
            out.push(match value {
                None | Some(0 | 0xD800..=0xDFFF) => '\u{fffd}',
                Some(n @ 0x80..=0x9F) => C1_REPLACEMENTS[(n - 0x80) as usize]
                    .unwrap_or_else(|| char::from_u32(n).expect("a C1 scalar")),
                Some(n) => char::from_u32(n).expect("a checked scalar"),
            });
            let tail = &digits[length..];
            rest = tail.strip_prefix(';').unwrap_or(tail);
            continue;
        }
        // Named: extend while the table has the prefix, keeping the
        // longest name with a value.
        let mut best = None;
        for (end, _) in after.char_indices().skip(1).chain([(after.len(), ' ')]) {
            match NAMED_ENTITIES.get(&after[..end]) {
                Some(&(first, second)) => {
                    if first != 0 {
                        best = Some((end, first, second));
                    }
                }
                None => break,
            }
        }
        match best {
            Some((end, first, second)) => {
                out.extend(
                    [first, second]
                        .into_iter()
                        .filter(|&c| c != 0)
                        .filter_map(char::from_u32),
                );
                rest = &after[end..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
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
            assert_eq!(sanitize_html(result.html()).unwrap().html(), result.html());
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

    /// Whether a serialization reached the serializer with a character it
    /// rescans for: html5ever escapes `&` and U+00A0 and writes the rest of
    /// U+0080–U+00BF as themselves.
    fn rescanned(serialized: &str) -> bool {
        serialized.contains("&amp;")
            || serialized.contains("&nbsp;")
            || serialized.contains(|c: char| ('\u{80}'..='\u{bf}').contains(&c))
    }

    /// The pinned serializer rescans for `<` or `>` at every `&` and
    /// U+0080–U+00BF it meets (html5ever 0.40), so a dense text node costs
    /// its length squared: 100,000 `&` took 35 seconds. None reaches it, in
    /// any pass, whatever the markup around the text, and the text a reader
    /// sees is unchanged.
    #[test]
    fn no_rescanned_character_reaches_the_serializer() {
        const UNITS: usize = 2_000;
        let table_repair = format!(
            "<table>{}</table>",
            format!(
                "{}<tr><td>{}x</td></tr>",
                "&amp;".repeat(450),
                "&amp;".repeat(8)
            )
            .repeat(40)
        );
        let mut inputs = vec![table_repair];
        for (unit, decoded) in [
            ("&amp;", "&"),
            ("&", "&"),
            ("&#38;", "&"),
            ("&#x26;", "&"),
            ("&nbsp;", "\u{a0}"),
            ("\u{a0}", "\u{a0}"),
            ("©", "©"),
            ("&lt;", "<"),
            ("\u{e000}", "\u{e000}"),
        ] {
            for shape in [
                "{}",
                "<font>{}</font>",
                "{}<!--x-->",
                "{}</bogus>",
                "{}<!DOCTYPE html>",
                "{}<title>x</title>",
            ] {
                let html = format!(
                    "<p>x{}&lt;b&gt;</p>",
                    shape.replace("{}", unit).repeat(UNITS)
                );
                let result = sanitize_html(&html).unwrap();
                assert_inert(result.html());
                let text = mail_parser::decoders::html::html_to_text(result.html());
                // The closing `&lt;b&gt;` adds one `<`.
                let expected = UNITS + usize::from(decoded == "<");
                assert_eq!(text.matches(decoded).count(), expected, "{shape} {unit:?}");
                assert!(text.trim_end().ends_with("<b>"), "{shape} {unit:?}");
                assert!(
                    !text.contains('\u{e000}') || unit == "\u{e000}",
                    "{shape} {unit:?}"
                );
                inputs.push(html);
            }
        }
        for html in inputs {
            let first = policy().clean(&neutralize_text(&html).unwrap()).to_string();
            assert!(!rescanned(&first), "the first pass");
            let last = policy().clean(&first).to_string();
            assert!(!rescanned(&last), "the final pass");
        }
    }

    /// Text reads as HTML5 reads it, references decoded at the boundaries
    /// the tokenizer sees: a multi-character entity names its domain, a
    /// comment still ends a reference, and a literal C1 character stays.
    #[test]
    fn text_decodes_as_html5_reads_it() {
        let result =
            sanitize_html(r#"<a href="https://fordbank.example/">&fjlig;ordbank.example</a>"#)
                .unwrap();
        assert_eq!(result.links()[0].warnings(), &[LinkWarning::TextDomain]);
        for (html, shown) in [
            ("<p>&#x<!--x-->41;</p>", "&#x41;"),
            ("<p>&not<!--x-->in;</p>", "\u{ac}in;"),
            ("<p>&#65x &notit; &amp</p>", "Ax \u{ac}it; &"),
            ("<p>\u{80}&#x80;</p>", "\u{80}\u{20ac}"),
        ] {
            let text =
                mail_parser::decoders::html::html_to_text(sanitize_html(html).unwrap().html());
            assert_eq!(text.trim(), shown, "{html}");
        }
    }

    /// lol_html and html5ever disagree about raw text in malformed foreign
    /// content; text html5ever keeps must be neutral whatever lol_html
    /// called it. A decoded carriage return survives the reparse.
    #[test]
    fn text_is_neutral_whatever_its_context() {
        let dense = format!(
            "<svg><td><foreignObject></td><xmp><pre>{}\u{e000}3C;</pre>",
            "&amp;".repeat(2_000)
        );
        let first = policy()
            .clean(&neutralize_text(&dense).unwrap())
            .to_string();
        assert!(!rescanned(&first), "the first pass");
        let result = sanitize_html(&dense).unwrap();
        assert!(
            !result.html().contains("&amp;amp;"),
            "{}",
            &result.html()[..200]
        );
        assert!(
            !result.html().contains("&lt;</pre>"),
            "a sender's placeholder"
        );
        for (html, sanitized) in [
            ("<pre>&#13;X</pre>", "<pre>&#13;X</pre>"),
            ("<pre>&#13;&#10;X</pre>", "<pre>&#13;\nX</pre>"),
        ] {
            let result = sanitize_html(html).unwrap();
            assert!(
                result.html().contains(sanitized),
                "{html}: {:?}",
                result.html()
            );
        }
    }

    /// Restoring the text adds nothing: a <pre> keeps its leading newline
    /// rule, and attributes keep a sender's own placeholder character.
    #[test]
    fn neutral_text_leaves_markup_alone() {
        let result = sanitize_html("<p>&amp;</p><pre>\ncode</pre>").unwrap();
        assert!(
            result.html().contains("<pre>code</pre>"),
            "{}",
            result.html()
        );
        let result = sanitize_html("<p lang=\"x\u{e000}quot;\">a</p>").unwrap();
        assert!(
            result.html().contains("lang=\"x\u{e000}quot;\""),
            "{}",
            result.html()
        );
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
