//! Owned renditions. The caller owns lease creation and all filesystem work.
use std::cell::RefCell;
use std::collections::HashSet;
use std::fmt::Write;

use crate::{AttachmentDescriptor, LinkRecord, RenderInput, RenderWarning, RenderWarningCode};
use lol_html::{HtmlRewriter, MemorySettings, Settings, element, text};

pub const MAX_RENDITION_BYTES: usize = 50_000_000;
const REWRITER_MEMORY: usize = 32_000_000;
const FALLBACK: &str = "<p>Content unavailable.</p>";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderKind {
    Html,
    PlainText,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resource {
    id: String,
    file: String,
    media_type: String,
    bytes: Vec<u8>,
}
impl Resource {
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn file(&self) -> &str {
        &self.file
    }
    pub fn media_type(&self) -> &str {
        &self.media_type
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Manifest {
    resources: Vec<Resource>,
    links: Vec<LinkRecord>,
    attachments: Vec<AttachmentDescriptor>,
}
impl Manifest {
    pub fn resources(&self) -> &[Resource] {
        &self.resources
    }
    pub fn links(&self) -> &[LinkRecord] {
        &self.links
    }
    pub fn attachments(&self) -> &[AttachmentDescriptor] {
        &self.attachments
    }

    /// Exact spec/07 version-1 schema. Resource bytes are descriptors in JSON,
    /// while the owned buffers above are for the caller to materialize.
    pub fn to_json(&self) -> String {
        let mut out = String::from("{\"v\":1,\"resources\":[");
        for (i, r) in self.resources.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            write!(
                out,
                "{{\"id\":{},\"file\":{},\"media_type\":{},\"bytes\":{}}}",
                json(&r.id),
                json(&r.file),
                json(&r.media_type),
                r.bytes.len()
            )
            .unwrap();
        }
        out.push_str("],\"links\":[");
        for (i, link) in self.links.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            write!(
                out,
                "{{\"href\":{},\"warnings\":[",
                json(link.destination())
            )
            .unwrap();
            for (j, warning) in link.warnings().iter().enumerate() {
                if j > 0 {
                    out.push(',');
                }
                out.push_str(&json(warning.as_str()));
            }
            out.push_str("]}");
        }
        out.push_str("],\"attachments\":[");
        for (i, a) in self.attachments.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            write!(
                out,
                "{{\"part_id\":{},\"display_name\":{},\"media_type\":{},\"bytes\":{}}}",
                json(a.part_id().as_str()),
                json(a.display_name()),
                json(a.media_type()),
                a.decoded_length()
                    .map_or_else(|| "null".into(), |n| n.to_string())
            )
            .unwrap();
        }
        out.push_str("]}");
        out
    }
}
fn json(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{0}'..='\u{1f}' => {
                write!(out, "\\u{:04x}", c as u32).unwrap();
            }
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendition {
    kind: RenderKind,
    content: String,
    remote_content_blocked: u32,
    warnings: Vec<RenderWarning>,
    manifest: Manifest,
}
impl Rendition {
    pub fn kind(&self) -> RenderKind {
        self.kind
    }
    /// Sanitized HTML for Html, literal UTF-8 for PlainText.
    pub fn content(&self) -> &str {
        &self.content
    }
    pub fn remote_content_blocked(&self) -> u32 {
        self.remote_content_blocked
    }
    pub fn warnings(&self) -> &[RenderWarning] {
        &self.warnings
    }
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }
}

/// Render with an opaque caller-supplied token (ASCII letters, digits, '-' or
/// '_'). Invalid tokens fail closed. No files or lease authority are created.
pub fn render(input: &RenderInput, token: &str, preferred: RenderKind) -> Rendition {
    let mut result = Rendition {
        kind: preferred,
        content: String::new(),
        remote_content_blocked: 0,
        warnings: input.warnings().to_vec(),
        manifest: Manifest {
            attachments: input.attachments().to_vec(),
            ..Manifest::default()
        },
    };
    if preferred == RenderKind::PlainText
        && let Some(plain) = input.plain()
    {
        result.content = plain.text().to_owned();
        enforce_limit(&mut result, MAX_RENDITION_BYTES);
        return result;
    }
    let Some(body) = input.html() else {
        result.kind = RenderKind::PlainText;
        result.content = input.plain().map_or("", |p| p.text()).to_owned();
        enforce_limit(&mut result, MAX_RENDITION_BYTES);
        return result;
    };
    let mut state = Inventory {
        input,
        token,
        result: &mut result,
        used: Vec::new(),
    };
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if token.is_empty()
            || !token
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c))
        {
            state.warn(RenderWarningCode::UnsafeUrl);
            return None;
        }
        let rewritten = first_pass(body.text(), &mut state)?;
        let minted = state
            .used
            .iter()
            .map(|&i| state.url(i))
            .collect::<HashSet<_>>();
        let sanitized = crate::render::sanitize_run(
            &rewritten,
            &minted,
            crate::render::ResourcePolicy::Rewritten,
        )
        .ok()?;
        if sanitized.html().len() > MAX_RENDITION_BYTES {
            state.warn(RenderWarningCode::RenditionLimit);
            return None;
        }
        state.result.manifest.links = sanitized.links().to_vec();
        retain_referenced_resources(sanitized.html(), &mut state)?;
        Some(if preferred == RenderKind::PlainText {
            mail_parser::decoders::html::html_to_text(sanitized.html())
        } else {
            sanitized.html().to_owned()
        })
    }));
    match outcome {
        Ok(Some(html)) => {
            result.content = html;
            if preferred == RenderKind::PlainText {
                result.manifest.resources.clear();
                result.manifest.links.clear();
            }
        }
        _ => {
            result.content = if preferred == RenderKind::Html {
                FALLBACK.into()
            } else {
                "Content unavailable.".into()
            };
            result.manifest.resources.clear();
            result.manifest.links.clear();
            push_warning(&mut result.warnings, RenderWarningCode::UnsupportedContent);
        }
    }
    enforce_limit(&mut result, MAX_RENDITION_BYTES);
    result
}
// Final HTML parsing can remove entire subtrees; do not materialize their bytes.
fn retain_referenced_resources(html: &str, state: &mut Inventory<'_>) -> Option<()> {
    let mut referenced = HashSet::new();
    lol_html::rewrite_str(
        html,
        lol_html::RewriteStrSettings::new().append_element_content_handler(element!("*", |el| {
            if let Some(src) = el.get_attribute("src") {
                referenced.insert(attribute(&src));
            }
            if let Some(css) = el.get_attribute("style") {
                let css = attribute(&css);
                let mut parser = cssparser::Parser::new(&css);
                while let Ok(token) = parser.next().cloned() {
                    if let cssparser::Token::Function(name) = token
                        && name.eq_ignore_ascii_case("url")
                    {
                        let _: Result<(), cssparser::ParseError<()>> =
                            parser.parse_nested_block(|p| {
                                if let Ok(url) = p.expect_string_cloned() {
                                    referenced.insert(url.to_string());
                                }
                                Ok(())
                            });
                    }
                }
            }
            Ok(())
        })),
    )
    .ok()?;
    let token = state.token;
    state
        .result
        .manifest
        .resources
        .retain(|r| referenced.contains(&format!("sweetbriar-cid://{token}/{}", r.id)));
    Some(())
}

fn enforce_limit(result: &mut Rendition, limit: usize) {
    if result.content.len() <= limit {
        return;
    }
    result.content = if result.kind == RenderKind::Html {
        FALLBACK.into()
    } else {
        "Content unavailable.".into()
    };
    result.manifest.resources.clear();
    result.manifest.links.clear();
    push_warning(&mut result.warnings, RenderWarningCode::RenditionLimit);
}
fn push_warning(warnings: &mut Vec<RenderWarning>, code: RenderWarningCode) {
    if !warnings
        .iter()
        .any(|w| w.code() == code && w.part_id().is_none())
    {
        warnings.push(RenderWarning::new(code, None));
    }
}

pub(crate) struct Inventory<'a> {
    input: &'a RenderInput,
    token: &'a str,
    result: &'a mut Rendition,
    used: Vec<usize>,
}
impl Inventory<'_> {
    pub(crate) fn warn(&mut self, code: RenderWarningCode) {
        push_warning(&mut self.result.warnings, code);
    }
    fn url(&self, i: usize) -> String {
        format!("sweetbriar-cid://{}/r{}", self.token, i + 1)
    }
    pub(crate) fn resource_url(&mut self, value: &str) -> Option<String> {
        let value = value.trim_matches(|c: char| c.is_ascii_whitespace() || c.is_ascii_control());
        // URL parsing recognizes browser-style tabs/newlines in schemes too.
        let parsed = url::Url::parse(value).ok();
        let scheme = parsed.as_ref().map(url::Url::scheme);
        if scheme == Some("cid") {
            let id = value.split_once(':').and_then(|(_, id)| decode_cid(id));
            if id
                .as_ref()
                .is_some_and(|id| self.input.ambiguous_cids().contains(id))
            {
                self.warn(RenderWarningCode::AmbiguousCid);
                return None;
            }
            let Some(i) = id.and_then(|id| {
                self.input
                    .resources()
                    .iter()
                    .position(|r| r.content_id() == id)
            }) else {
                self.warn(RenderWarningCode::MissingCid);
                return None;
            };
            let r = &self.input.resources()[i];
            let Some((media_type, ext)) =
                raster(r.bytes()).filter(|(mime, _)| *mime == r.media_type())
            else {
                let warning = RenderWarning::new(
                    RenderWarningCode::UnsupportedContent,
                    Some(r.part_id().clone()),
                );
                if !self.result.warnings.contains(&warning) {
                    self.result.warnings.push(warning);
                }
                return None;
            };
            let id = format!("r{}", i + 1);
            if !self.used.contains(&i) {
                self.used.push(i);
                self.result.manifest.resources.push(Resource {
                    file: format!("{id}.{ext}"),
                    id,
                    media_type: media_type.into(),
                    bytes: r.bytes().to_vec(),
                });
            }
            return Some(self.url(i));
        }
        match scheme {
            Some("http" | "https" | "ftp" | "ftps" | "ws" | "wss") | None => self.blocked(),
            _ => self.warn(RenderWarningCode::UnsafeUrl),
        }
        None
    }
    pub(crate) fn observe_url(&mut self, value: &str) {
        if url::Url::parse(value)
            .ok()
            .is_some_and(|u| u.scheme() == "cid")
        {
            return;
        }
        self.resource_url(value);
    }
    fn blocked(&mut self) {
        match self.result.remote_content_blocked.checked_add(1) {
            Some(n) => self.result.remote_content_blocked = n,
            None => self.warn(RenderWarningCode::CountSaturated),
        }
    }
}
fn decode_cid(value: &str) -> Option<String> {
    let mut bytes = Vec::new();
    let mut chars = value.bytes();
    while let Some(c) = chars.next() {
        if c == b'%' {
            let a = (chars.next()? as char).to_digit(16)?;
            let b = (chars.next()? as char).to_digit(16)?;
            bytes.push((a * 16 + b) as u8);
        } else {
            bytes.push(c);
        }
    }
    let id = String::from_utf8(bytes).ok()?;
    (!id.is_empty() && !id.chars().any(|c| c.is_control() || c.is_whitespace())).then_some(id)
}
fn raster(bytes: &[u8]) -> Option<(&'static str, &'static str)> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some(("image/png", "png"));
    }
    if bytes.starts_with(b"\xff\xd8\xff") {
        return Some(("image/jpeg", "jpg"));
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some(("image/gif", "gif"));
    }
    if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        return Some(("image/webp", "webp"));
    }
    None
}

// Decode in the HTML attribute context, including numeric and semicolonless
// references. Do not use text extraction, which would collapse whitespace.
fn attribute(value: &str) -> String {
    if !value.contains('&') {
        return value.into();
    }
    let decoded = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let captured = decoded.clone();
    ammonia::Builder::new()
        .add_generic_attributes(["title"])
        .attribute_filter(move |_, _, value| {
            *captured.lock().unwrap() = value.into();
            None
        })
        .clean(&format!("<p title=\"{}\">", value.replace('"', "&quot;")));
    decoded.lock().unwrap().clone()
}
fn first_pass(html: &str, state: &mut Inventory<'_>) -> Option<String> {
    let state = RefCell::new(state);
    let style = RefCell::new(String::new());
    let mut out = Vec::new();
    let mut exceeded = false;
    {
        let mut rewriter = HtmlRewriter::new(
            Settings::new()
                .with_memory_settings(
                    MemorySettings::new().with_max_allowed_memory_usage(REWRITER_MEMORY),
                )
                .append_element_content_handler(element!("*", |el| {
                    let mut state = state.borrow_mut();
                    let tag = el.tag_name();
                    for name in ["src", "poster", "background", "data", "href", "xlink:href"] {
                        if name == "href"
                            && !["link", "image", "use", "feimage"]
                                .contains(&tag.to_ascii_lowercase().as_str())
                        {
                            continue;
                        }
                        if name == "data" && tag != "object" {
                            continue;
                        }
                        if let Some(value) = el.get_attribute(name) {
                            let value = attribute(&value);
                            el.remove_attribute(name);
                            if tag == "img" && name == "src" {
                                if let Some(url) = state.resource_url(&value) {
                                    el.set_attribute(name, &url)?;
                                }
                            } else {
                                state.observe_url(&value);
                            }
                        }
                    }
                    for name in ["srcset", "imagesrcset"] {
                        if let Some(value) = el.get_attribute(name) {
                            for url in srcset(&attribute(&value)) {
                                state.observe_url(url);
                            }
                            el.remove_attribute(name);
                        }
                    }
                    if let Some(value) = el.get_attribute("style") {
                        let css = attribute(&value);
                        crate::render_css::inventory(&css, &mut state);
                        let safe = crate::render_css::sanitize(&css, &mut state);
                        el.remove_attribute("style");
                        if !safe.is_empty() {
                            el.set_attribute("style", &safe)?;
                        }
                    }
                    Ok(())
                }))
                .append_element_content_handler(text!("style", |chunk| {
                    style.borrow_mut().push_str(chunk.as_str());
                    if chunk.last_in_text_node() {
                        crate::render_css::inventory(&style.take(), &mut state.borrow_mut());
                        state
                            .borrow_mut()
                            .warn(RenderWarningCode::UnsupportedContent);
                    }
                    Ok(())
                })),
            |bytes: &[u8]| {
                if out.len().saturating_add(bytes.len()) > MAX_RENDITION_BYTES {
                    exceeded = true;
                }
                if !exceeded {
                    out.extend_from_slice(bytes);
                }
            },
        );
        if rewriter.write(html.as_bytes()).is_err() || rewriter.end().is_err() {
            return None;
        }
    }
    if exceeded {
        state.borrow_mut().warn(RenderWarningCode::RenditionLimit);
        return None;
    }
    String::from_utf8(out).ok()
}

// HTML's candidate tokenizer: URL runs end at ASCII whitespace, with trailing
// commas as separators. Commas inside a data URL are not separate candidates.
fn srcset(mut value: &str) -> Vec<&str> {
    let mut urls = Vec::new();
    loop {
        value = value.trim_start_matches(|c: char| c.is_ascii_whitespace() || c == ',');
        if value.is_empty() {
            return urls;
        }
        let end = value
            .find(|c: char| c.is_ascii_whitespace())
            .unwrap_or(value.len());
        let raw = &value[..end];
        let url = raw.trim_end_matches(',');
        if !url.is_empty() {
            urls.push(url);
        }
        value = &value[end..];
        if raw.ends_with(',') {
            continue;
        }
        let mut depth = 0usize;
        let end = value
            .char_indices()
            .find_map(|(i, c)| match c {
                '(' => {
                    depth += 1;
                    None
                }
                ')' => {
                    depth = depth.saturating_sub(1);
                    None
                }
                ',' if depth == 0 => Some(i + 1),
                _ => None,
            })
            .unwrap_or(value.len());
        value = &value[end..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Limits, render_input};
    use std::collections::BTreeMap;

    fn input(html: &str) -> RenderInput {
        render_input(
            format!("Content-Type: text/html; charset=utf-8\r\n\r\n{html}").as_bytes(),
            &Limits::default(),
        )
        .unwrap()
    }
    fn run(html: &str) -> Rendition {
        render(&input(html), "lease_1", RenderKind::Html)
    }
    fn related(html: &str, parts: &[(&str, &str, &str)]) -> RenderInput {
        let mut raw = format!(
            "Content-Type: multipart/related; boundary=b\r\n\r\n--b\r\nContent-Type: text/html\r\n\r\n{html}\r\n"
        );
        for (cid, mime, body) in parts {
            write!(raw, "--b\r\nContent-Type: {mime}\r\nContent-ID: <{cid}>\r\nContent-Transfer-Encoding: base64\r\n\r\n{body}\r\n").unwrap();
        }
        raw.push_str("--b--\r\n");
        render_input(raw.as_bytes(), &Limits::default()).unwrap()
    }
    // Parse the emitted markup without cleaning it again.
    fn elements(html: &str) -> Vec<(String, BTreeMap<String, String>)> {
        let mut out = Vec::new();
        lol_html::rewrite_str(
            html,
            lol_html::RewriteStrSettings::new().append_element_content_handler(element!(
                "*",
                |el| {
                    assert_eq!(el.namespace_uri(), "http://www.w3.org/1999/xhtml");
                    let tag = el.tag_name();
                    assert!(
                        !["style", "script", "link", "iframe", "object", "svg", "math"]
                            .contains(&tag.as_str())
                    );
                    let attrs: BTreeMap<_, _> = el
                        .attributes()
                        .iter()
                        .map(|a| (a.name(), attribute(&a.value())))
                        .collect();
                    for (name, value) in &attrs {
                        assert!(!name.starts_with("on"));
                        assert!(
                            !["srcset", "poster", "background", "ping"].contains(&name.as_str())
                        );
                        if name == "src" {
                            assert!(value.starts_with("sweetbriar-cid://lease_1/r"));
                        }
                    }
                    out.push((tag, attrs));
                    Ok(())
                }
            )),
        )
        .unwrap();
        out
    }
    fn has_warning(result: &Rendition, code: RenderWarningCode) -> bool {
        result.warnings().iter().any(|w| w.code() == code)
    }

    #[test]
    fn inventories_each_resource_occurrence_and_not_hyperlinks() {
        let html = r#"<img src="https://example.invalid/a" srcset="https://example.invalid/b 1x, /c 2x, //example.invalid/d 3x">
            <video src=/movie poster=/poster><source src=/source></video><table background=/bg></table>
            <link rel=preconnect href="https://example.invalid"><link rel=preload imagesrcset="/a 1x, /b 2x">
            <object data=/object></object><iframe src=/frame></iframe>
            <a href="https://example.invalid/" ping="https://example.invalid/ping">open</a>
            <p style="color:red;cursor:url(/cursor);--bad:uRl('/custom');background-image:image-set('/set' 1x, url(/set2) 2x)">text</p>
            <style>@import '/import'; @font-face{src:URL('/font')} @namespace svg url(https://example.invalid/ns);</style>"#;
        let result = run(html);
        assert_eq!(result.remote_content_blocked(), 20);
        assert_eq!(result, run(html));
        let els = elements(result.content());
        assert_eq!(
            els.iter().find(|e| e.0 == "p").unwrap().1["style"],
            "color:red;"
        );
        assert_eq!(result.manifest().links().len(), 1);
    }

    #[test]
    fn css_function_escapes_and_case_are_inventoried_even_when_disallowed() {
        for function in ["url", "uRl", r"u\72l", "URL"] {
            for argument in ["/pixel", "'/pixel'", "\"https://example.invalid/pixel\""] {
                let css = format!("cursor:{function}({argument}); color:blue");
                let html = format!("<p style='{}'>text</p>", css.replace('\'', "&#39;"));
                let result = run(&html);
                assert_eq!(result.remote_content_blocked(), 1, "{css}");
                assert_eq!(elements(result.content())[0].1["style"], "color:blue;");
            }
        }
        let result = run(
            r#"<style>@IMPORT url('/a'); @namespace svg '/ns'; @font-face{src:local('name'),u\72l('/font')} @media all{x{background:image-set('/b' 1x, '/c' 2x)}}</style><p>ok</p>"#,
        );
        assert_eq!(result.remote_content_blocked(), 5);
        assert_eq!(elements(result.content())[0].0, "p");
    }

    #[test]
    fn css_keeps_only_the_spec_properties_and_value_categories() {
        let kept = "color:#aabbcc;background-color:RGBA(1,2,3,.5);font-family:'Example Font',serif;font-size:12px;font-weight:700;font-style:italic;text-align:center;text-decoration:underline;text-indent:2em;line-height:120%;vertical-align:top;white-space:pre-wrap;list-style-type:disc;margin:1px 2px;padding-left:3px;border:1px solid red;border-top-color:blue;border-width:2px;border-collapse:collapse;border-spacing:2px;width:80%;min-width:1px;max-width:40rem;height:20px;";
        let dropped = "position:fixed;display:none;opacity:0;--x:red;color:var(--x);width:calc(1px + 2px);height:1px!important;background-image:url(/remote);font-family:expression(foo);color:red!bad;";
        let html = format!("<p style=\"{}{}\">text</p>", kept, dropped);
        let result = run(&html);
        let els = elements(result.content());
        let mut parser = cssparser::Parser::new(&els[0].1["style"]);
        let mut names = Vec::new();
        while !parser.is_exhausted() {
            names.push(parser.expect_ident_cloned().unwrap().to_string());
            parser.expect_colon().unwrap();
            parser
                .parse_until_before(cssparser::Delimiter::Semicolon, |p| {
                    while p.next().is_ok() {}
                    Ok::<_, cssparser::ParseError<()>>(())
                })
                .unwrap();
            parser.expect_semicolon().unwrap();
        }
        assert_eq!(
            names,
            kept.split(';')
                .filter(|s| !s.is_empty())
                .map(|s| s.split_once(':').unwrap().0)
                .collect::<Vec<_>>()
        );
        assert_eq!(result.remote_content_blocked(), 1);
        assert!(has_warning(&result, RenderWarningCode::UnsupportedContent));
    }

    #[test]
    fn cids_are_scoped_signature_checked_and_never_sender_authorized() {
        // Signature-only synthetic bytes, never owner data.
        let source = related(
            r#"<img src="cid:pic%40example.invalid"><p style="background-image:URL('cid:pic@example.invalid')">ok</p>
            <img src="sweetbriar-cid://lease_1/r1"><img src="cid:missing"><img src="cid:bad"><img src="cid:dup">"#,
            &[
                ("pic@example.invalid", "image/png", "iVBORw0KGgo="),
                ("bad", "image/png", "PGh0bWw+"),
                ("dup", "image/gif", "R0lGODlh"),
                ("dup", "image/gif", "R0lGODlh"),
            ],
        );
        let result = render(&source, "lease_1", RenderKind::Html);
        assert_eq!(result.remote_content_blocked(), 0);
        let els = elements(result.content());
        assert_eq!(els[0].1["src"], "sweetbriar-cid://lease_1/r2");
        assert_eq!(
            els[1].1["style"],
            "background-image:url(\"sweetbriar-cid://lease_1/r2\");"
        );
        assert!(els[2..].iter().all(|e| !e.1.contains_key("src")));
        assert_eq!(result.manifest.resources.len(), 1);
        assert_eq!(result.manifest.resources[0].bytes(), b"\x89PNG\r\n\x1a\n");
        for code in [
            RenderWarningCode::MissingCid,
            RenderWarningCode::AmbiguousCid,
            RenderWarningCode::UnsafeUrl,
            RenderWarningCode::UnsupportedContent,
        ] {
            assert!(has_warning(&result, code));
        }
        assert_eq!(result.manifest.attachments(), source.attachments());
    }

    #[test]
    fn raster_formats_cid_decoding_and_bad_tokens_fail_closed() {
        for (mime, bytes) in [
            ("image/png", b"\x89PNG\r\n\x1a\n".as_slice()),
            ("image/jpeg", b"\xff\xd8\xff"),
            ("image/gif", b"GIF89a"),
            ("image/webp", b"RIFF1234WEBP"),
        ] {
            assert_eq!(raster(bytes).unwrap().0, mime);
        }
        for bytes in [b"<html>".as_slice(), b"GIF88a", b"RIFF1234HTML"] {
            assert_eq!(raster(bytes), None);
        }
        assert_eq!(decode_cid("a%40b"), Some("a@b".into()));
        assert_eq!(decode_cid("%2540"), Some("%40".into()));
        for cid in ["", "%", "%gg", "%00", "%20", "%ff"] {
            assert_eq!(decode_cid(cid), None);
        }
        for token in ["", "../x", "x/y", "x?query", "x@host", "x:80", "é"] {
            let result = render(&input("<p>text</p>"), token, RenderKind::Html);
            assert_eq!(elements(result.content())[0].0, "p");
            assert!(has_warning(&result, RenderWarningCode::UnsafeUrl));
            assert!(result.manifest.resources.is_empty());
        }
    }

    #[test]
    fn local_unknown_relative_and_entity_urls_have_distinct_counts() {
        let result = run(
            r#"<img src="data:image/png,x"><img src="javascript:alert(1)"><img src="custom:thing">
            <img src="sweetbriar-cid://other/r1"><img src="cid:missing"><img src="../relative"><img src="//example.invalid/p">
            <img src="h&#116;tps://example.invalid/p"><img src="https&colon;//example.invalid/q">
            <p style="cursor:URL(&quot;/css&quot;)">ok</p>"#,
        );
        assert_eq!(result.remote_content_blocked(), 5);
        assert!(
            elements(result.content())
                .iter()
                .all(|e| !e.1.contains_key("src"))
        );
        assert!(has_warning(&result, RenderWarningCode::UnsafeUrl));
        assert!(has_warning(&result, RenderWarningCode::MissingCid));
        assert_eq!(
            srcset("data:image/png,AAAA 1x, /a 2x, /b, /c 3x"),
            ["data:image/png,AAAA", "/a", "/b", "/c"]
        );
    }

    #[test]
    fn plain_renditions_have_attachments_only_and_html_fallback_is_sanitized() {
        let source = related(
            "<script>bad</script><p>Hello <b>reader</b></p><img src=cid:a>",
            &[("a", "image/gif", "R0lGODlh")],
        );
        let result = render(&source, "lease_1", RenderKind::PlainText);
        assert_eq!(result.kind(), RenderKind::PlainText);
        assert!(result.content().contains("Hello reader"));
        assert!(!result.content().contains("bad"));
        assert!(result.manifest.links.is_empty());
        assert!(result.manifest.resources.is_empty());
        assert_eq!(result.manifest.attachments(), source.attachments());
        let plain = render_input(
            b"Content-Type: text/plain\r\n\r\n<b>literal</b>",
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(
            render(&plain, "lease_1", RenderKind::Html).content(),
            "<b>literal</b>"
        );
        assert_eq!(
            render(&plain, "lease_1", RenderKind::Html).kind(),
            RenderKind::PlainText
        );
    }

    #[test]
    fn count_saturates_and_rendition_limit_has_a_safe_fallback() {
        let source = input("<p>ok</p>");
        let mut result = render(&source, "lease_1", RenderKind::Html);
        result.remote_content_blocked = u32::MAX - 1;
        let mut state = Inventory {
            input: &source,
            token: "lease_1",
            result: &mut result,
            used: Vec::new(),
        };
        state.observe_url("/a");
        state.observe_url("/b");
        assert_eq!(result.remote_content_blocked(), u32::MAX);
        assert!(has_warning(&result, RenderWarningCode::CountSaturated));
        enforce_limit(&mut result, 9);
        assert!(!has_warning(&result, RenderWarningCode::RenditionLimit));
        enforce_limit(&mut result, 8);
        assert_eq!(elements(result.content())[0].0, "p");
        assert!(has_warning(&result, RenderWarningCode::RenditionLimit));
        assert!(result.manifest.resources.is_empty());
        assert_eq!(MAX_RENDITION_BYTES, 50_000_000);
    }
    #[test]
    fn removed_subtrees_do_not_leave_resource_bytes_in_manifest() {
        let source = related(
            "<form><img src=cid:a></form><p>ok</p>",
            &[("a", "image/gif", "R0lGODlh")],
        );
        let result = render(&source, "lease_1", RenderKind::Html);
        assert_eq!(elements(result.content()).len(), 1);
        assert!(result.manifest.resources.is_empty());
    }

    #[test]
    fn css_deeply_nested_resources_are_still_counted() {
        let css = format!("{}url(/pixel){}", "f(".repeat(100), ")".repeat(100));
        let result = run(&format!("<p style='cursor:{css}'>ok</p>"));
        assert_eq!(result.remote_content_blocked(), 1);
        assert!(!elements(result.content())[0].1.contains_key("style"));
    }

    #[test]
    fn html_serialization_expansion_obeys_the_real_byte_cap() {
        // Minting can expand a small admitted body beyond the file budget.
        let source = related(
            &"<img src=cid:a>".repeat(51),
            &[("a", "image/gif", "R0lGODlh")],
        );
        let result = render(&source, &"a".repeat(1_000_000), RenderKind::Html);
        assert!(has_warning(&result, RenderWarningCode::RenditionLimit));
        assert_eq!(result.content(), FALLBACK);
        assert_eq!(elements(result.content())[0].0, "p");
    }

    #[test]
    fn foreign_related_cids_are_never_served() {
        let raw = b"Content-Type: multipart/mixed; boundary=m\r\n\r\n--m\r\nContent-Type: multipart/related; boundary=a\r\n\r\n--a\r\nContent-Type: text/html\r\n\r\n<img src=cid:foreign>\r\n--a--\r\n--m\r\nContent-Type: image/gif\r\nContent-ID: <foreign>\r\nContent-Transfer-Encoding: base64\r\n\r\nR0lGODlh\r\n--m--\r\n";
        let source = render_input(raw, &Limits::default()).unwrap();
        let result = render(&source, "lease_1", RenderKind::Html);
        assert!(result.manifest.resources.is_empty());
        assert!(!elements(result.content())[0].1.contains_key("src"));
        assert!(has_warning(&result, RenderWarningCode::MissingCid));
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

    #[test]
    fn manifest_v1_shape_and_owned_values_round_trip() {
        let source = related(
            r#"<img src=cid:a><a href="https://u@192.0.2.1/">open</a><a href="https://u@192.0.2.1/">other.example.invalid</a>"#,
            &[("a", "image/png", "iVBORw0KGgo=")],
        );
        let result = render(&source, "lease_1", RenderKind::Html);
        let serialized = result.manifest.to_json();
        let expected = r#"{"v":1,"resources":[{"id":"r1","file":"r1.png","media_type":"image/png","bytes":8}],"links":[{"href":"https://u@192.0.2.1/","warnings":["text_domain","raw_ip","user_info"]}],"attachments":[{"part_id":"1.2","display_name":"attachment","media_type":"image/png","bytes":8}]}"#;
        assert_eq!(parse_json(&serialized), parse_json(expected));
        let Json::Object(fields) = parse_json(&serialized) else {
            panic!("manifest object");
        };
        assert_eq!(fields.len(), 4);
        let Json::Array(resources) = &fields["resources"] else {
            panic!("resources array");
        };
        let Json::Object(resource) = &resources[0] else {
            panic!("resource object");
        };
        assert_eq!(
            resource["id"],
            Json::String(result.manifest.resources[0].id().into())
        );
        assert_eq!(
            resource["file"],
            Json::String(result.manifest.resources[0].file().into())
        );
        assert_eq!(
            resource["media_type"],
            Json::String(result.manifest.resources[0].media_type().into())
        );
        assert_eq!(
            resource["bytes"],
            Json::Number(result.manifest.resources[0].bytes().len() as u64)
        );
        let special = "quote\" slash\\ \n\r\t\0 café & <>";
        assert_eq!(parse_json(&json(special)), Json::String(special.into()));
        assert_eq!(
            parse_json(&Manifest::default().to_json()),
            parse_json(r#"{"v":1,"resources":[],"links":[],"attachments":[]}"#)
        );
    }
    #[test]
    fn css_only_cids_survive_and_unsupported_syntax_warns() {
        let source = related(
            "<p style=\"background-image:url(cid:a);width:1px!important;color:var(--x);padding:2px\">ok</p>",
            &[("a", "image/gif", "R0lGODlh")],
        );
        let result = render(&source, "lease_1", RenderKind::Html);
        assert_eq!(
            elements(result.content())[0].1["style"],
            "background-image:url(\"sweetbriar-cid://lease_1/r1\");padding:2px;"
        );
        assert_eq!(result.manifest.resources.len(), 1);
        assert_eq!(result.remote_content_blocked(), 0);
        assert!(has_warning(&result, RenderWarningCode::UnsupportedContent));
        let result = run("<p style='@import url(/a);color:red'>ok</p>");
        assert_eq!(result.remote_content_blocked(), 1);
        assert_eq!(elements(result.content())[0].1["style"], "color:red;");
        assert!(has_warning(&result, RenderWarningCode::UnsupportedContent));
    }

    #[test]
    fn svg_resources_and_mixed_srcset_are_counted_before_removal() {
        let result = run(
            "<svg><image href='/image'/><use href='/use'/></svg><img srcset='data:image/png,AAAA 1x, /a 2x, //example.invalid/b 3x'>",
        );
        assert_eq!(result.remote_content_blocked(), 4);
        let els = elements(result.content());
        assert_eq!(els.len(), 1);
        assert_eq!(els[0].0, "img");
        assert!(!els[0].1.contains_key("srcset"));
    }

    #[test]
    fn damaged_attachment_length_serializes_as_null() {
        let source = related("<p>ok</p>", &[("bad", "application/octet-stream", "%%%")]);
        let result = render(&source, "lease_1", RenderKind::PlainText);
        let Json::Object(fields) = parse_json(&result.manifest.to_json()) else {
            panic!("object");
        };
        let Json::Array(attachments) = &fields["attachments"] else {
            panic!("array");
        };
        let Json::Object(attachment) = &attachments[0] else {
            panic!("object");
        };
        assert_eq!(attachment["bytes"], Json::Null);
    }
    #[test]
    fn renderer_contains_sanitizer_unwinds() {
        let result = run("sb-mime-sanitizer-test-panic");
        assert_eq!(result.content(), FALLBACK);
        assert!(has_warning(&result, RenderWarningCode::UnsupportedContent));
        assert_eq!(elements(result.content())[0].0, "p");
    }
}
