//! CSS syntax is tokenized before policy decisions, including rejected values.
use crate::RenderWarningCode;
use crate::rendition::Inventory;
use cssparser::{
    AtRuleParser, CowRcStr, DeclarationParser, ParseError, Parser, ParserState,
    QualifiedRuleParser, RuleBodyItemParser, RuleBodyParser, ToCss, Token,
};

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
    "padding",
    "border",
    "border-collapse",
    "border-spacing",
    "width",
    "min-width",
    "max-width",
    "height",
    "background-image",
];
fn property(name: &str) -> bool {
    if PROPERTIES.contains(&name) {
        return true;
    }
    for base in ["margin", "padding", "border"] {
        if let Some(suffix) = name.strip_prefix(base).and_then(|n| n.strip_prefix('-')) {
            if ["top", "right", "bottom", "left"].contains(&suffix) {
                return true;
            }
            if base == "border" {
                if ["width", "style", "color"].contains(&suffix) {
                    return true;
                }
                if let Some((side, part)) = suffix.split_once('-') {
                    return ["top", "right", "bottom", "left"].contains(&side)
                        && ["width", "style", "color"].contains(&part);
                }
            }
        }
    }
    false
}
const LENGTH_UNITS: &[&str] = &[
    "px", "em", "rem", "ex", "ch", "cap", "ic", "lh", "rlh", "cm", "mm", "q", "in", "pt", "pc",
    "vw", "vh", "vi", "vb", "vmin", "vmax", "svw", "svh", "lvw", "lvh", "dvw", "dvh",
];
// Start a parser at each token boundary to avoid its automatic nested-block
// skipping. An explicit context stack inventories deep CSS without recursion.
pub(crate) fn inventory(mut css: &str, state: &mut Inventory<'_>) {
    let mut stack = vec![(Context::Normal, '\0')];
    while !css.is_empty() {
        let mut parser = Parser::new(css);
        let Ok(token) = parser.next().cloned() else {
            break;
        };
        css = &css[parser.position().byte_index()..];
        let context = &mut stack.last_mut().unwrap().0;
        match token {
            Token::UnquotedUrl(url) => {
                state.observe_url(&url);
                if *context == Context::Target {
                    *context = Context::Normal;
                }
            }
            Token::QuotedString(url) if *context != Context::Normal => {
                state.observe_url(&url);
                if *context != Context::ImageSet {
                    *context = Context::Normal;
                }
            }
            Token::AtKeyword(name) if name.eq_ignore_ascii_case("import") => {
                *context = Context::Target
            }
            Token::AtKeyword(name) if name.eq_ignore_ascii_case("namespace") => {
                *context = Context::Target
            }
            Token::Semicolon => *context = Context::Normal,
            Token::Function(name) => {
                if *context == Context::Target {
                    *context = Context::Normal;
                }
                let nested = if name.eq_ignore_ascii_case("url") {
                    Context::Target
                } else if name.eq_ignore_ascii_case("image-set")
                    || name.eq_ignore_ascii_case("-webkit-image-set")
                {
                    Context::ImageSet
                } else {
                    Context::Normal
                };
                stack.push((nested, ')'));
            }
            Token::CurlyBracketBlock => stack.push((Context::Normal, '}')),
            Token::SquareBracketBlock => stack.push((Context::Normal, ']')),
            Token::ParenthesisBlock => stack.push((Context::Normal, ')')),
            Token::CloseParenthesis | Token::CloseSquareBracket | Token::CloseCurlyBracket => {
                let end = match token {
                    Token::CloseParenthesis => ')',
                    Token::CloseSquareBracket => ']',
                    _ => '}',
                };
                if stack.len() > 1 && stack.last().unwrap().1 == end {
                    stack.pop();
                }
            }
            _ => (),
        }
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Context {
    Normal,
    ImageSet,
    Target,
}

pub(crate) fn sanitize(css: &str, state: &mut Inventory<'_>) -> String {
    let mut parser = Parser::new(css);
    let mut declarations = Declarations { state };
    let mut output = String::new();
    let mut rejected = false;
    for declaration in RuleBodyParser::new(&mut parser, &mut declarations) {
        match declaration {
            Ok(css) => output.push_str(&css),
            Err(_) => rejected = true,
        }
    }
    if rejected {
        declarations
            .state
            .warn(RenderWarningCode::UnsupportedContent);
    }
    output
}
struct Declarations<'a, 'b> {
    state: &'a mut Inventory<'b>,
}
impl<'i> DeclarationParser<'i> for Declarations<'_, '_> {
    type Declaration = String;
    type Error = ();
    fn parse_value(
        &mut self,
        name: CowRcStr<'i>,
        input: &mut Parser<'i>,
        _: &ParserState,
    ) -> Result<String, ParseError<()>> {
        let name = name.to_ascii_lowercase();
        let value = if property(&name) {
            if name == "background-image" {
                let url = input.expect_url()?.to_string();
                input.expect_exhausted()?;
                // The independent inventory already counted non-CID URLs.
                if !url.get(..4).is_some_and(|s| s.eq_ignore_ascii_case("cid:")) {
                    None
                } else {
                    self.state
                        .resource_url(&url)
                        .map(|url| format!("url(\"{url}\")"))
                }
            } else {
                safe_value(input, &name)
            }
        } else {
            None
        };
        match value {
            Some(value) if !value.is_empty() => Ok(format!("{name}:{value};")),
            _ => {
                self.state.warn(RenderWarningCode::UnsupportedContent);
                Err(ParseError::custom(()))
            }
        }
    }
}
impl<'i> AtRuleParser<'i> for Declarations<'_, '_> {
    type Prelude = ();
    type AtRule = String;
    type Error = ();
}
impl<'i> QualifiedRuleParser<'i> for Declarations<'_, '_> {
    type Prelude = ();
    type QualifiedRule = String;
    type Error = ();
}
impl<'i> RuleBodyItemParser<'i, String, ()> for Declarations<'_, '_> {
    fn parse_declarations(&self) -> bool {
        true
    }
    fn parse_qualified(&self) -> bool {
        false
    }
}
fn safe_value(input: &mut Parser<'_>, name: &str) -> Option<String> {
    let mut value = String::new();
    while !input.is_exhausted() {
        let token = input.next().ok()?.clone();
        let token_css = match token {
            Token::Ident(_) | Token::Number { .. } | Token::Percentage { .. } => {
                token.to_css_string()
            }
            Token::Hash(ref hex) | Token::IDHash(ref hex)
                if [3, 4, 6, 8].contains(&hex.len())
                    && hex.bytes().all(|c| c.is_ascii_hexdigit()) =>
            {
                token.to_css_string()
            }
            Token::Dimension { ref unit, .. }
                if LENGTH_UNITS.contains(&unit.to_ascii_lowercase().as_str()) =>
            {
                token.to_css_string()
            }
            Token::QuotedString(_) if name == "font-family" => token.to_css_string(),
            Token::Comma if name == "font-family" => ",".into(),
            Token::Function(ref function)
                if ["rgb", "rgba", "hsl", "hsla"]
                    .contains(&function.to_ascii_lowercase().as_str()) =>
            {
                let body = input
                    .parse_nested_block(|p| {
                        let mut body = String::new();
                        while !p.is_exhausted() {
                            let t = p.next()?.clone();
                            match t {
                                Token::Number { .. }
                                | Token::Percentage { .. }
                                | Token::Comma
                                | Token::Delim('/') => (),
                                Token::Dimension { ref unit, .. }
                                    if ["deg", "grad", "rad", "turn"]
                                        .contains(&unit.to_ascii_lowercase().as_str()) => {}
                                _ => return Err(ParseError::<()>::custom(())),
                            }
                            body.push_str(&t.to_css_string());
                            body.push(' ');
                        }
                        Ok(body)
                    })
                    .ok()?;
                format!("{}({body})", function.to_ascii_lowercase())
            }
            _ => return None,
        };
        if !value.is_empty() {
            value.push(' ');
        }
        value.push_str(&token_css);
    }
    Some(value)
}
