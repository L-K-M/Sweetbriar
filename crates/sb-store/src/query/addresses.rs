//! Validated modern bare address operands (spec/06 "Address operands").
//!
//! The owner's v1 policy is a modern bare grammar with local-as-written
//! matching: one dot-atom or quoted local part, `@`, then a domain name
//! or bracketed IP literal. Display names, comments, folding whitespace,
//! obsolete routes and permissive header-parser recovery are all rejected.
//! Comparison keys deliberately differ from the original operand: the
//! local key is the exact written spelling (quotes and escapes retained,
//! never unquoted/case-folded/normalized) while the domain key is the
//! original domain Unicode-lowercased. `idna`'s UTS 46 mapping is a
//! validity check only — its output is discarded and never becomes a key.

use std::error::Error;
use std::fmt;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::str::FromStr;

use idna::uts46::{AsciiDenyList, DnsLength, Hyphens, Uts46};

use super::MAX_QUERY_INPUT_BYTES;

/// The rejected operand class plus a static reason (spec/06). The tags
/// let the later parser attach the error to an exact input span; no
/// rejected input text is stored here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressError {
    /// The address text or its local part is invalid.
    Address(&'static str),
    /// The domain name or bracketed literal is invalid.
    Domain(&'static str),
    /// The operand exceeds [`MAX_QUERY_INPUT_BYTES`].
    Limit(&'static str),
}

impl fmt::Display for AddressError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (operand, reason) = match self {
            Self::Address(reason) => ("address", reason),
            Self::Domain(reason) => ("domain", reason),
            Self::Limit(reason) => ("length", reason),
        };
        write!(f, "invalid {operand} operand: {reason}")
    }
}

impl Error for AddressError {}

/// One validated mailbox operand (`local@domain`), immutable and keeping
/// the original text for lossless later AST/text roundtrips (spec/06).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressOperand {
    value: String,
    /// Byte offset of the unquoted `@` separating local and domain.
    /// The local key occupies `key[..separator]` byte-for-byte.
    separator: usize,
    /// The written local part, `@`, and the original domain lowercased.
    key: String,
}

impl AddressOperand {
    /// Validates `value` against the modern bare grammar.
    pub fn parse(value: &str) -> Result<Self, AddressError> {
        if value.len() > MAX_QUERY_INPUT_BYTES {
            return Err(AddressError::Limit("input exceeds 16384 UTF-8 bytes"));
        }
        Self::parse_received(value)
    }

    // Received locals share the grammar, not the query-input byte ceiling.
    pub(crate) fn parse_received(value: &str) -> Result<Self, AddressError> {
        let separator = split_local_domain(value)?;
        let domain = &value[separator + 1..];
        validate_domain(domain)?;
        let mut key = String::with_capacity(value.len() + 1);
        key.push_str(&value[..separator]);
        key.push('@');
        key.push_str(&domain.to_lowercase());
        Ok(Self {
            value: value.to_owned(),
            separator,
            key,
        })
    }

    /// The original operand text, byte-for-byte.
    pub fn value(&self) -> &str {
        &self.value
    }

    /// The comparison key: written local + `@` + lowercased domain.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The local part exactly as written — quotes, escapes, case and
    /// Unicode all preserved. Always a UTF-8 boundary slice of `key()`.
    pub fn local_key(&self) -> &str {
        &self.key[..self.separator]
    }

    /// The original domain Unicode-lowercased — never the IDNA-mapped
    /// form, normalized form or canonicalized literal spelling.
    pub fn domain_key(&self) -> &str {
        &self.key[self.separator + 1..]
    }
}

/// One validated `domain:` operand — a domain name or bracketed IP
/// literal — immutable and keeping the original text (spec/06).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainOperand {
    value: String,
    /// The original domain Unicode-lowercased.
    key: String,
}

impl DomainOperand {
    /// Validates `value` against the domain grammar.
    pub fn parse(value: &str) -> Result<Self, AddressError> {
        if value.len() > MAX_QUERY_INPUT_BYTES {
            return Err(AddressError::Limit("input exceeds 16384 UTF-8 bytes"));
        }
        validate_domain(value)?;
        Ok(Self {
            value: value.to_owned(),
            key: value.to_lowercase(),
        })
    }

    /// The original operand text, byte-for-byte.
    pub fn value(&self) -> &str {
        &self.value
    }

    /// The comparison key: the original domain Unicode-lowercased.
    pub fn key(&self) -> &str {
        &self.key
    }
}

/// Validates the local part and returns the byte offset of the `@`
/// separating it from the domain (spec/06).
fn split_local_domain(value: &str) -> Result<usize, AddressError> {
    let separator = if value.starts_with('"') {
        let close = quoted_local_close(value)?;
        if value.as_bytes().get(close + 1) != Some(&b'@') {
            return Err(AddressError::Address("quoted local not followed by '@'"));
        }
        close + 1
    } else {
        let at = value
            .find('@')
            .ok_or(AddressError::Address("missing '@'"))?;
        validate_dot_atom(&value[..at])?;
        at
    };
    if separator + 1 == value.len() {
        return Err(AddressError::Domain("missing domain"));
    }
    Ok(separator)
}

/// Validates a dot-atom local part: nonempty atoms of ASCII atext or
/// non-control/non-whitespace Unicode scalars joined by single dots
/// (spec/06).
fn validate_dot_atom(local: &str) -> Result<(), AddressError> {
    if local.is_empty() {
        return Err(AddressError::Address("empty local part"));
    }
    for atom in local.split('.') {
        if atom.is_empty() {
            return Err(AddressError::Address("empty atom in local part"));
        }
        for c in atom.chars() {
            if c.is_ascii() {
                if !is_atext(c as u8) {
                    return Err(AddressError::Address(
                        "character not allowed in dot-atom local",
                    ));
                }
            } else if c.is_control() || c.is_whitespace() {
                return Err(AddressError::Address(
                    "control or whitespace scalar in local part",
                ));
            }
        }
    }
    Ok(())
}

/// Scans a `"`-opened local part for its unescaped closing quote and
/// returns that quote's byte offset (spec/06). Quoted content allows
/// printable ASCII (space included) and non-control Unicode; a backslash
/// escapes exactly one printable ASCII character, retained verbatim.
fn quoted_local_close(value: &str) -> Result<usize, AddressError> {
    let mut chars = value.char_indices();
    chars.next(); // the opening quote itself
    while let Some((offset, c)) = chars.next() {
        match c {
            '"' => return Ok(offset),
            '\\' => match chars.next() {
                Some((_, e)) if e.is_ascii() && (0x20..=0x7e).contains(&(e as u8)) => {}
                Some(_) => {
                    return Err(AddressError::Address(
                        "quoted pair escapes a non-printable character",
                    ));
                }
                None => return Err(AddressError::Address("dangling escape")),
            },
            _ if c.is_ascii() && !(0x20..=0x7e).contains(&(c as u8)) => {
                return Err(AddressError::Address("control in quoted local"));
            }
            _ if c.is_control() => return Err(AddressError::Address("control in quoted local")),
            _ => {}
        }
    }
    Err(AddressError::Address("unterminated quoted local"))
}

/// RFC 5322 `atext`: ASCII letters, digits and `!#$%&'*+-/=?^_`{|}~`.
fn is_atext(b: u8) -> bool {
    b.is_ascii_alphanumeric()
        || matches!(
            b,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'/'
                | b'='
                | b'?'
                | b'^'
                | b'_'
                | b'`'
                | b'{'
                | b'|'
                | b'}'
                | b'~'
        )
}

/// Validates a domain name or bracketed IP literal (spec/06).
fn validate_domain(domain: &str) -> Result<(), AddressError> {
    if let Some(inner) = domain.strip_prefix('[') {
        let inner = inner
            .strip_suffix(']')
            .ok_or(AddressError::Domain("unclosed address literal"))?;
        return validate_literal(inner);
    }
    if domain.contains(['[', ']']) {
        return Err(AddressError::Domain("stray bracket in domain"));
    }
    validate_domain_name(domain)
}

/// Validates a bracketed literal: bare IPv4 text, or `IPv6:` (tag
/// case-insensitive) followed by a valid IPv6 address (spec/06). Only
/// syntax is checked — the original spelling is kept for the key.
fn validate_literal(inner: &str) -> Result<(), AddressError> {
    if inner
        .get(..5)
        .is_some_and(|tag| tag.eq_ignore_ascii_case("ipv6:"))
    {
        return Ipv6Addr::from_str(&inner[5..])
            .map(|_| ())
            .map_err(|_| AddressError::Domain("invalid IPv6 literal"));
    }
    Ipv4Addr::from_str(inner)
        .map(|_| ())
        .map_err(|_| AddressError::Domain("invalid address literal"))
}

/// Validates a domain name through `idna`'s UTS 46 ToASCII with the
/// STD3 deny list, hyphen checks and DNS length verification, allowing
/// one trailing root dot (spec/06). The mapped output proves validity
/// only; it is dropped here and never reaches a comparison key.
fn validate_domain_name(domain: &str) -> Result<(), AddressError> {
    if domain.is_empty() {
        return Err(AddressError::Domain("empty domain"));
    }
    let mapped = Uts46::new()
        .to_ascii(
            domain.as_bytes(),
            AsciiDenyList::STD3,
            Hyphens::Check,
            DnsLength::VerifyAllowRootDot,
        )
        .map_err(|_| AddressError::Domain("invalid domain name"))?;
    if mapped.trim_end_matches('.').is_empty() {
        return Err(AddressError::Domain("domain name maps to the root"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address(value: &str) -> AddressOperand {
        AddressOperand::parse(value).expect("operand should validate")
    }

    /// Independent expectations: every key below is written out by hand
    /// from the spec/06 key rules, never derived from the code under test.
    #[test]
    fn valid_operands_get_expected_keys() {
        let cases: [(&str, &str, &str, &str); 33] = [
            // input, expected key(), expected local_key(), domain_key()
            (
                "user@example.com",
                "user@example.com",
                "user",
                "example.com",
            ),
            (
                "first.last@example.com",
                "first.last@example.com",
                "first.last",
                "example.com",
            ),
            (
                "a!#$%&'*+-/=?^_`{|}~z@x.co",
                "a!#$%&'*+-/=?^_`{|}~z@x.co",
                "a!#$%&'*+-/=?^_`{|}~z",
                "x.co",
            ),
            // Local case is preserved verbatim; only the domain lowercases.
            ("FOO@Example.COM", "FOO@example.com", "FOO", "example.com"),
            (
                "é用户@example.com",
                "é用户@example.com",
                "é用户",
                "example.com",
            ),
            // Quoted locals keep quotes, spaces, inner @ and escapes.
            (
                "\"Foo Bar\"@example.com",
                "\"Foo Bar\"@example.com",
                "\"Foo Bar\"",
                "example.com",
            ),
            ("\"\"@x", "\"\"@x", "\"\"", "x"),
            ("\"a@b\"@x", "\"a@b\"@x", "\"a@b\"", "x"),
            ("\"a\\\"b\"@x", "\"a\\\"b\"@x", "\"a\\\"b\"", "x"),
            ("\"a\\\\b\"@x", "\"a\\\\b\"@x", "\"a\\\\b\"", "x"),
            ("\"a\\@b\"@x", "\"a\\@b\"@x", "\"a\\@b\"", "x"),
            ("\"a\\Fb\"@x", "\"a\\Fb\"@x", "\"a\\Fb\"", "x"),
            (
                "\"a(comment)\"@x",
                "\"a(comment)\"@x",
                "\"a(comment)\"",
                "x",
            ),
            ("\"a\u{a0}b\"@x", "\"a\u{a0}b\"@x", "\"a\u{a0}b\"", "x"),
            ("\"x\"@Bücher.DE", "\"x\"@bücher.de", "\"x\"", "bücher.de"),
            // Quoted @ and multibyte local: slicing stays on UTF-8 bounds.
            ("\"é@ü\"@x.de", "\"é@ü\"@x.de", "\"é@ü\"", "x.de"),
            // Unicode domains lowercase only.
            ("u@bücher.de", "u@bücher.de", "u", "bücher.de"),
            ("u@BÜCHER.de", "u@bücher.de", "u", "bücher.de"),
            (
                "u@xn--bcher-kva.de",
                "u@xn--bcher-kva.de",
                "u",
                "xn--bcher-kva.de",
            ),
            // ß lowercases to ß (capital ẞ too), never to "ss".
            ("u@faß.de", "u@faß.de", "u", "faß.de"),
            ("u@FAẞ.DE", "u@faß.de", "u", "faß.de"),
            // Lowercasing can expand the domain without moving the local slice.
            ("é@İ.test", "é@i\u{307}.test", "é", "i\u{307}.test"),
            // A single trailing root dot is allowed and retained.
            ("u@example.com.", "u@example.com.", "u", "example.com."),
            ("u@x.", "u@x.", "u", "x."),
            // Fullwidth input is valid but keeps its own lowercased
            // spelling — the mapped ASCII output never becomes the key.
            (
                "u@ＥＸＡＭＰＬＥ.com",
                "u@ｅｘａｍｐｌｅ.com",
                "u",
                "ｅｘａｍｐｌｅ.com",
            ),
            // Bracketed literals: IPv4 or tagged IPv6, spelling kept.
            ("u@[127.0.0.1]", "u@[127.0.0.1]", "u", "[127.0.0.1]"),
            (
                "u@[IPv6:FE80::1]",
                "u@[ipv6:fe80::1]",
                "u",
                "[ipv6:fe80::1]",
            ),
            (
                "u@[ipv6:fe80::1]",
                "u@[ipv6:fe80::1]",
                "u",
                "[ipv6:fe80::1]",
            ),
            (
                "u@[IPv6:0:0:0:0:0:0:0:1]",
                "u@[ipv6:0:0:0:0:0:0:0:1]",
                "u",
                "[ipv6:0:0:0:0:0:0:0:1]",
            ),
            (
                "u@[IPv6:::FFFF:1.2.3.4]",
                "u@[ipv6:::ffff:1.2.3.4]",
                "u",
                "[ipv6:::ffff:1.2.3.4]",
            ),
            ("u@[IPV6:::1]", "u@[ipv6:::1]", "u", "[ipv6:::1]"),
            (
                "u@[255.255.255.255]",
                "u@[255.255.255.255]",
                "u",
                "[255.255.255.255]",
            ),
            ("u@[0.0.0.0]", "u@[0.0.0.0]", "u", "[0.0.0.0]"),
        ];
        for (input, key, local_key, domain_key) in cases {
            let operand = address(input);
            assert_eq!(operand.value(), input, "value of {input}");
            assert_eq!(operand.key(), key, "key of {input}");
            assert_eq!(operand.local_key(), local_key, "local key of {input}");
            assert_eq!(operand.domain_key(), domain_key, "domain key of {input}");
            assert_eq!(
                operand.key(),
                format!("{}@{}", operand.local_key(), operand.domain_key()),
                "key composition of {input}"
            );
        }
    }

    /// Different written spellings stay distinct keys even when some
    /// legacy rule would consider them the same mailbox (spec/06).
    #[test]
    fn written_spellings_stay_distinct() {
        let pairs: [(&str, &str); 7] = [
            ("\"Foo\"@x", "Foo@x"),                       // quoted vs bare local
            ("\"a\\Fb\"@x", "aFb@x"),                     // escaped vs unescaped
            ("FOO@x", "foo@x"),                           // local case
            ("u@e\u{301}x.de", "u@éx.de"),                // NFD vs NFC
            ("u@bücher.de", "u@xn--bcher-kva.de"),        // Unicode vs A-label
            ("u@example.com", "u@example.com."),          // trailing root dot
            ("u@[IPv6:0:0:0:0:0:0:0:1]", "u@[IPv6:::1]"), // literal spellings
        ];
        for (left, right) in pairs {
            assert_ne!(
                address(left).key(),
                address(right).key(),
                "{left} vs {right}"
            );
        }
    }

    /// The same domain in different case yields equal keys while the
    /// original operand values are still retained — there is no
    /// case-insensitive whole-operand comparison (spec/06).
    #[test]
    fn case_only_domain_differences_share_a_key() {
        let pairs: [(&str, &str); 3] = [
            ("u@EXAMPLE.com", "u@example.com"),
            ("u@[IPv6:FE80::1]", "u@[ipv6:fe80::1]"),
            ("u@Ｅxample.com", "u@ｅxample.com"),
        ];
        for (upper, lower) in pairs {
            let a = address(upper);
            let b = address(lower);
            assert_eq!(a.key(), b.key(), "{upper} vs {lower}");
            assert_eq!(a.domain_key(), b.domain_key());
            assert_ne!(a.value(), b.value());
            assert_ne!(a, b, "operands keep their raw values");
        }
    }

    /// Invalid local parts, quoting, structure and header syntax refuse
    /// with the address tag (spec/06).
    #[test]
    fn invalid_locals_and_structure_refuse() {
        let cases: [(&str, &str); 31] = [
            ("", "missing '@'"),
            ("user", "missing '@'"),
            ("@x", "empty local part"),
            ("a b@x", "character not allowed in dot-atom local"),
            ("a\u{a0}b@x", "control or whitespace scalar in local part"),
            (".a@x", "empty atom in local part"),
            ("a.@x", "empty atom in local part"),
            ("a..b@x", "empty atom in local part"),
            ("a\tb@x", "character not allowed in dot-atom local"),
            ("a\rb@x", "character not allowed in dot-atom local"),
            ("a\nb@x", "character not allowed in dot-atom local"),
            ("a\u{0}b@x", "character not allowed in dot-atom local"),
            ("a\u{7f}b@x", "character not allowed in dot-atom local"),
            ("a\u{85}b@x", "control or whitespace scalar in local part"),
            ("a\"b@x", "character not allowed in dot-atom local"),
            ("\"abc@x", "unterminated quoted local"),
            ("\"ab\"cd@x", "quoted local not followed by '@'"),
            ("\"ab\" @x", "quoted local not followed by '@'"),
            ("\"a\0b\"@x", "control in quoted local"),
            ("\"a\tb\"@x", "control in quoted local"),
            ("\"a\rb\"@x", "control in quoted local"),
            ("\"a\nb\"@x", "control in quoted local"),
            ("\"a\u{7f}b\"@x", "control in quoted local"),
            ("\"a\u{85}b\"@x", "control in quoted local"),
            ("\"a\\", "dangling escape"),
            (
                "\"a\\\u{9}\"@x",
                "quoted pair escapes a non-printable character",
            ),
            (
                "\"a\\é\"@x",
                "quoted pair escapes a non-printable character",
            ),
            ("User <u@x>", "character not allowed in dot-atom local"),
            ("@domain:u@x", "empty local part"),
            ("mailto:u@x", "character not allowed in dot-atom local"),
            (" u@x", "character not allowed in dot-atom local"),
        ];
        for (input, reason) in cases {
            assert_eq!(
                AddressOperand::parse(input),
                Err(AddressError::Address(reason)),
                "rejection of {input:?}"
            );
        }
    }

    /// Invalid domain names refuse with the domain tag (spec/06).
    #[test]
    fn invalid_domains_refuse() {
        // A 64-byte label and a 255-byte overall name both exceed DNS
        // length (labels cap at 63, names at 253 sans the root dot).
        let label_64 = format!("u@{}.com", "a".repeat(64));
        let name_255 = format!(
            "u@{}.{}.{}.{}",
            "a".repeat(63),
            "b".repeat(63),
            "c".repeat(63),
            "d".repeat(63)
        );
        assert_eq!(name_255.len() - "u@".len(), 255);
        let cases = vec![
            "u@".to_owned(),
            "u@@x".to_owned(),
            "u@ex_ample.com".to_owned(),
            "u@-bad.com".to_owned(),
            "u@bad-.com".to_owned(),
            "u@a..com".to_owned(),
            "u@.com".to_owned(),
            "u@x com".to_owned(),
            "u@x, v@y".to_owned(),
            "u@x ".to_owned(),
            "u@x(comment)".to_owned(),
            "u@x\ny".to_owned(),
            "u@.".to_owned(),
            "u@\u{ad}".to_owned(),
            "u@https://x".to_owned(),
            label_64,
            name_255,
        ];
        for input in &cases {
            assert!(
                matches!(AddressOperand::parse(input), Err(AddressError::Domain(_))),
                "rejection of {input:?}"
            );
        }
    }

    /// Invalid bracketed literals refuse with the domain tag (spec/06).
    #[test]
    fn invalid_literals_refuse() {
        let cases: [&str; 10] = [
            "u@[999.0.0.1]",
            "u@[1.2.3]",
            "u@[010.1.2.3]",
            "u@[IPv6:gg::1]",
            "u@[IPv6:]",
            "u@[::1]",
            "u@[IPv6:fe80::1%eth0]",
            "u@[hello]",
            "u@[1.2.3.4",
            "u@[127.0.0.1]x",
        ];
        for input in cases {
            assert!(
                matches!(AddressOperand::parse(input), Err(AddressError::Domain(_))),
                "rejection of {input:?}"
            );
        }
    }

    /// The shared 16,384-byte query ceiling applies to the whole raw
    /// input — bytes, not chars — before any parse work (spec/06). No
    /// SMTP envelope or 64-byte local limit applies to query operands.
    #[test]
    fn byte_limit_applies_before_parsing() {
        // Syntactically valid locals far beyond the SMTP 64-byte limit
        // are fine for archive queries.
        let max_ascii = format!("{}@x", "a".repeat(MAX_QUERY_INPUT_BYTES - 2));
        assert_eq!(max_ascii.len(), MAX_QUERY_INPUT_BYTES);
        let operand = address(&max_ascii);
        assert_eq!(operand.domain_key(), "x");
        assert_eq!(operand.local_key().len(), MAX_QUERY_INPUT_BYTES - 2);

        // Multibyte locals are counted in bytes: 8191 'é' (2 bytes)
        // plus "@x" lands exactly on the cap.
        let max_multi = format!("{}@x", "é".repeat(8191));
        assert_eq!(max_multi.len(), MAX_QUERY_INPUT_BYTES);
        assert_eq!(address(&max_multi).local_key().chars().count(), 8191);

        // One byte over, from either side, is the typed limit error —
        // a huge domain hits it before any IDNA work runs.
        for input in [
            format!("{}@x", "a".repeat(MAX_QUERY_INPUT_BYTES - 1)),
            format!("{}@xy", "é".repeat(8191)),
            format!("u@{}", "a".repeat(MAX_QUERY_INPUT_BYTES)),
            format!("u@{}", "é".repeat(MAX_QUERY_INPUT_BYTES)),
        ] {
            assert!(input.len() > MAX_QUERY_INPUT_BYTES);
            assert_eq!(
                AddressOperand::parse(&input),
                Err(AddressError::Limit("input exceeds 16384 UTF-8 bytes")),
                "limit for input of {} bytes",
                input.len()
            );
        }
    }

    /// The domain operand validates names and literals the same way,
    /// keeps the original value and lowercases only for the key.
    #[test]
    fn domain_operand_validates_and_keys() {
        let cases: [(&str, &str); 5] = [
            ("example.com", "example.com"),
            ("BÜCHER.de", "bücher.de"),
            ("example.com.", "example.com."),
            ("xn--bcher-kva.de", "xn--bcher-kva.de"),
            ("[IPv6:FE80::1]", "[ipv6:fe80::1]"),
        ];
        for (input, key) in cases {
            let operand = DomainOperand::parse(input).expect("domain should validate");
            assert_eq!(operand.value(), input);
            assert_eq!(operand.key(), key);
        }

        let invalid: [&str; 6] = ["", "ex_ample.com", "u@x", ".", "-x.com", "[::1]"];
        for input in invalid {
            assert!(
                matches!(DomainOperand::parse(input), Err(AddressError::Domain(_))),
                "rejection of {input:?}"
            );
        }
        assert_eq!(
            DomainOperand::parse(&"a".repeat(MAX_QUERY_INPUT_BYTES + 1)),
            Err(AddressError::Limit("input exceeds 16384 UTF-8 bytes"))
        );
    }

    /// Immutability, clone, value roundtrip and error plumbing (spec/06).
    #[test]
    fn operands_are_stable_and_errors_typed() {
        let operand = address("\"A\\@B\"@Bücher.DE");
        assert_eq!(operand, operand.clone());
        assert_eq!(
            AddressOperand::parse(operand.value()),
            Ok(operand.clone()),
            "re-parsing the retained value reproduces the operand"
        );

        // Static, data-free tags the later parser can pin to spans.
        assert_eq!(
            AddressError::Address("reason").to_string(),
            "invalid address operand: reason"
        );
        assert_eq!(
            AddressError::Domain("reason").to_string(),
            "invalid domain operand: reason"
        );
        assert_eq!(
            AddressError::Limit("reason").to_string(),
            "invalid length operand: reason"
        );
        fn is_error(_: &dyn Error) {}
        is_error(&AddressError::Address("x"));
    }
}
