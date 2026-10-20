//! Validated literal text (spec/06 "Remaining operands"). Only SQLite's
//! shipped tokenizer defines its token sequence; Rust Unicode categories
//! and a raw MATCH expression are not equivalent validation oracles.

use std::error::Error;
use std::fmt;

use rusqlite::Connection;

use super::MAX_QUERY_INPUT_BYTES;
use crate::StoreError;
use crate::schema::classify;

/// Invalid input and native failure remain distinct for later parser spans.
#[derive(Debug, Clone)]
pub enum TextError {
    /// Empty, tokenless or unrepresentable literal text.
    Invalid(&'static str),
    /// Input exceeds the query's UTF-8 byte ceiling.
    Limit,
    /// Native initialization or lookup failed; no parser verdict was made.
    Store(StoreError),
}

impl fmt::Display for TextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(reason) => write!(f, "invalid text operand: {reason}"),
            Self::Limit => write!(
                f,
                "text operand exceeds {MAX_QUERY_INPUT_BYTES} UTF-8 bytes"
            ),
            Self::Store(error) => fmt::Display::fmt(error, f),
        }
    }
}

impl Error for TextError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Store(error) => Some(error),
            _ => None,
        }
    }
}

impl From<StoreError> for TextError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

/// A checked literal term/phrase, preserving its original value and the
/// normalized native tokens in phrase order, including repetitions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextOperand {
    value: String,
    tokens: Vec<String>,
}

impl TextOperand {
    /// Validate input before native initialization; no raw MATCH is executed.
    pub fn parse(value: &str) -> Result<Self, TextError> {
        check_source(value)?;
        TextValidator::open()?.parse(value)
    }

    pub fn value(&self) -> &str {
        &self.value
    }

    pub fn tokens(&self) -> &[String] {
        &self.tokens
    }
}

fn check_source(value: &str) -> Result<(), TextError> {
    if value.len() > MAX_QUERY_INPUT_BYTES {
        return Err(TextError::Limit);
    }
    if value.is_empty() {
        return Err(TextError::Invalid("a value is required"));
    }
    if value.contains('\0') {
        return Err(TextError::Invalid(
            "NUL cannot be represented by a literal MATCH carrier",
        ));
    }
    Ok(())
}

/// One private memory oracle can validate successive values without
/// reopening it. No unchecked operand can escape from this path either.
struct TextValidator {
    conn: Connection,
}

impl TextValidator {
    fn open() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory()
            .map_err(|error| classify(error, "open the query token oracle"))?;
        conn.execute_batch(
            "PRAGMA temp_store=MEMORY;
             CREATE VIRTUAL TABLE sb_qt USING fts5(x, tokenize='unicode61 remove_diacritics 2');
             CREATE VIRTUAL TABLE sb_qv USING fts5vocab(sb_qt, 'instance');",
        )
        .map_err(|error| classify(error, "initialize the query token oracle"))?;
        Ok(Self { conn })
    }

    fn parse(&self, value: &str) -> Result<TextOperand, TextError> {
        check_source(value)?;
        self.conn
            .execute("DELETE FROM sb_qt", [])
            .map_err(|error| classify(error, "clear the query token oracle"))?;
        self.conn
            .execute("INSERT INTO sb_qt(rowid,x) VALUES (1,?1)", [value])
            .map_err(|error| classify(error, "feed the query token oracle"))?;
        let tokens = self
            .conn
            .prepare("SELECT term FROM sb_qv ORDER BY offset")
            .and_then(|mut stmt| {
                stmt.query_map([], |row| row.get(0))?
                    .collect::<Result<Vec<String>, _>>()
            })
            .map_err(|error| classify(error, "read the query token sequence"))?;
        if tokens.is_empty() {
            return Err(TextError::Invalid("a tokenizer token is required"));
        }
        Ok(TextOperand {
            value: value.to_owned(),
            tokens,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn original_values_and_exact_native_sequences_are_preserved() {
        for (value, expected) in [
            ("B A B", vec!["b", "a", "b"]),
            ("café CAFE cafe\u{301}", vec!["cafe", "cafe", "cafe"]),
            ("007 a-b_c it's", vec!["007", "a", "b", "c", "it", "s"]),
            ("中文", vec!["中文"]),
            ("\u{e000}\u{f8ff}", vec!["\u{e000}\u{f8ff}"]),
            ("\u{378} \u{20ba}", vec!["\u{378}", "\u{20ba}"]),
            ("x\u{20ba}y", vec!["x\u{20ba}y"]),
            ("OR NEAR(a b) a*b", vec!["or", "near", "a", "b", "a", "b"]),
            ("\"quoted\"", vec!["quoted"]),
        ] {
            let text = TextOperand::parse(value).unwrap();
            assert_eq!(text.value(), value);
            assert_eq!(text.tokens(), expected);
            assert_eq!(text, text.clone());
        }
    }

    #[test]
    fn one_validator_reuses_native_state_without_leaking_old_tokens() {
        let validator = TextValidator::open().unwrap();
        let temp_store: i64 = validator
            .conn
            .query_row("PRAGMA temp_store", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            temp_store, 2,
            "the private oracle must not spill temporary data to disk"
        );
        assert_eq!(
            validator.parse("first first").unwrap().tokens(),
            ["first", "first"]
        );
        assert!(matches!(validator.parse("!!!"), Err(TextError::Invalid(_))));
        assert_eq!(validator.parse("Second").unwrap().tokens(), ["second"]);
        assert!(matches!(
            validator.parse("\0second"),
            Err(TextError::Invalid(_))
        ));
        assert_eq!(validator.parse("third").unwrap().tokens(), ["third"]);
    }

    #[test]
    fn empty_tokenless_and_nul_values_are_explicit_input_errors() {
        for value in [
            "", "   ", "!!!", "©", "€", "∑", "😀", "\u{301}", "\u{7}", "-", "*",
        ] {
            assert!(
                matches!(TextOperand::parse(value), Err(TextError::Invalid(_))),
                "{value:?}"
            );
        }
        for value in ["\0needle", "needle\0tail", "needle\0"] {
            assert!(matches!(
                TextOperand::parse(value),
                Err(TextError::Invalid(_))
            ));
        }
    }

    #[test]
    fn exact_ascii_and_multibyte_byte_limits_precede_native_work() {
        let ascii = "a".repeat(MAX_QUERY_INPUT_BYTES);
        let unicode = "é".repeat(MAX_QUERY_INPUT_BYTES / 2);
        for value in [&ascii, &unicode] {
            assert_eq!(value.len(), MAX_QUERY_INPUT_BYTES);
            let operand = TextOperand::parse(value).unwrap();
            assert_eq!(operand.value(), *value);
            assert_eq!(operand.tokens().len(), 1);
        }
        for value in [format!("{ascii}a"), format!("{unicode}a")] {
            assert!(matches!(TextOperand::parse(&value), Err(TextError::Limit)));
        }
        // Even an unusable native schema cannot preempt source validation.
        let validator = TextValidator::open().unwrap();
        validator.conn.execute_batch("DROP TABLE sb_qv").unwrap();
        assert!(matches!(
            validator.parse(&format!("{ascii}a")),
            Err(TextError::Limit)
        ));
        assert!(matches!(validator.parse("\0"), Err(TextError::Invalid(_))));
        assert!(matches!(validator.parse(""), Err(TextError::Invalid(_))));
    }

    #[test]
    fn a_real_failed_private_lookup_is_native_not_token_absence() {
        let validator = TextValidator::open().unwrap();
        validator.conn.execute_batch("DROP TABLE sb_qv").unwrap();
        let error = validator.parse("needle").unwrap_err();
        assert!(matches!(
            error,
            TextError::Store(StoreError::Internal { .. })
        ));
        assert!(error.source().is_some());
    }
}
