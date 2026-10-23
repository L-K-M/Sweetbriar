//! Whole-source selection; consumers retain their own header and absence rules.

use rusqlite::{Connection, Row, functions::FunctionFlags};

pub(crate) const MESSAGES: &str = "logical_messages lm";
pub(crate) const HEADERS: &str = "message_headers h ON h.message_id = lm.message_id";
pub(crate) const MIN_USABLE_VERSION: u32 = 2;

pub(crate) const FIELDS: [&str; 16] = [
    "header_message_id",
    "subject",
    "from_addr",
    "from_name",
    "reply_to_json",
    "to_json",
    "cc_json",
    "in_reply_to",
    "references_json",
    "list_id",
    "delivered_to_json",
    "snippet",
    "date_header",
    "has_attachments",
    "parser_version",
    "parse_state",
];

#[derive(Clone, Copy)]
pub(crate) enum Source {
    Stored,
    Candidate(u32),
}

impl Source {
    pub(crate) fn joins(self) -> String {
        let key = match self {
            Self::Stored => {
                "v.acquisition_id=b.acquisition_id AND v.parser_target=b.parser_target".into()
            }
            Self::Candidate(target) => {
                format!("v.acquisition_id=lm.acquisition_id AND v.parser_target={target}")
            }
        };
        format!(
            "LEFT JOIN d_message_summary s ON s.acquisition_id=lm.acquisition_id
            LEFT JOIN d_message_bindings b ON b.message_id=lm.message_id
            LEFT JOIN d_repair_assemblies v ON {key}
            LEFT JOIN acquisitions ms_acq ON ms_acq.acquisition_id=lm.acquisition_id"
        )
    }

    fn bound(self) -> &'static str {
        match self {
            Self::Stored => "b.message_id IS NOT NULL",
            Self::Candidate(_) => "1",
        }
    }

    /// Zero is absent, one valid, minus one invalid; SQL NULL is never a state.
    pub(crate) fn validity(self) -> String {
        let bound = self.bound();
        let maximum = crate::repair::MAX_TEXT_BYTES;
        format!("CASE WHEN NOT ({bound}) THEN 0
          WHEN v.completed_at IS NULL OR v.acquisition_id IS NOT lm.acquisition_id
            OR v.source_sha256 IS NOT ms_acq.content_sha256 OR v.source_bytes IS NOT ms_acq.byte_length THEN -1
          WHEN NOT json_valid(v.field_lengths) THEN -1
          WHEN json_type(v.field_lengths)<>'array' OR json_array_length(v.field_lengths)<>12
            OR EXISTS(SELECT 1 FROM json_each(v.field_lengths) WHERE type NOT IN('null','integer') OR value<0 OR value>{maximum}) THEN -1
          WHEN (SELECT coalesce(sum(value),0) FROM json_each(v.field_lengths))>{maximum} THEN -1 ELSE 1 END")
    }

    pub(crate) fn present(self) -> String {
        format!("({}) OR s.acquisition_id IS NOT NULL", self.bound())
    }

    pub(crate) fn field(self, name: &str) -> String {
        let repaired = match name {
            "parser_version" => "v.parser_target".into(),
            "parse_state" => "'ok'".into(),
            "date_header" | "has_attachments" => format!("v.{name}"),
            _ => {
                let index = FIELDS
                    .iter()
                    .position(|field| *field == name)
                    .expect("a summary field");
                format!("CASE WHEN NOT json_valid(v.field_lengths) THEN NULL WHEN json_extract(v.field_lengths,'$[{index}]') IS NOT NULL
                    THEN coalesce((SELECT group_concat(text,'' ORDER BY byte_offset) FROM d_repair_pieces
                      WHERE acquisition_id=v.acquisition_id AND parser_target=v.parser_target AND field_index={index}),'') END")
            }
        };
        format!(
            "CASE WHEN {} THEN {repaired} ELSE s.{name} END",
            self.bound()
        )
    }

    pub(crate) fn fields<const N: usize>(self, names: [&str; N]) -> [String; N] {
        names.map(|name| self.field(name))
    }

    pub(crate) fn usable(self) -> String {
        format!(
            "({})='ok' AND ({})>={MIN_USABLE_VERSION}",
            self.field("parse_state"),
            self.field("parser_version")
        )
    }
}

pub(crate) fn invalid() -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
        None,
    )
}

pub(crate) fn check(row: &Row<'_>, column: usize) -> rusqlite::Result<()> {
    if row.get::<_, i64>(column)? < 0 {
        return Err(invalid());
    }
    Ok(())
}

pub(crate) fn register(conn: &Connection) -> rusqlite::Result<()> {
    conn.create_scalar_function(
        "sb_mime_valid",
        1,
        FunctionFlags::SQLITE_UTF8
            | FunctionFlags::SQLITE_DETERMINISTIC
            | FunctionFlags::SQLITE_INNOCUOUS,
        |ctx| {
            if ctx.get::<i64>(0)? < 0 {
                return Err(invalid());
            }
            Ok(1)
        },
    )
}
