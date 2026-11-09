//! Bounded, read-only shape inspection for the restore quarantine transaction.
//! This is not a database-integrity, quarantine or swap-eligibility verdict.

use rusqlite::{Connection, OptionalExtension, limits::Limit};
use sb_types::fs::ReadFile;
use std::sync::atomic::{AtomicBool, Ordering};

use super::image_inspect::{MAX_PROGRESS_CALLS, MAX_SQLITE_VALUE, guarded, inspection_error};
use crate::{StoreError, schema};

const MAX_COLUMNS: i32 = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Inspection {
    pub schema_version: i64,
}

/// Inspect only the retained file. The VFS cannot resolve its pathname or
/// open sidecars. Limits and cancellation apply before schema materialization;
/// unrelated archive values are never read or subjected to these limits.
pub(super) fn inspect(
    file: Box<dyn ReadFile>,
    cancel: &AtomicBool,
) -> Result<Option<Inspection>, StoreError> {
    let conn = crate::image_vfs::open(file)
        .map_err(|e| schema::classify(e, "open restore schema inspection"))?;
    let outcome = inspect_connection(&conn, cancel, MAX_PROGRESS_CALLS);
    conn.close()
        .map_err(|(_, e)| schema::classify(e, "close restore schema inspection"))?;
    outcome
}

fn inspect_connection(
    conn: &Connection,
    cancel: &AtomicBool,
    budget: u32,
) -> Result<Option<Inspection>, StoreError> {
    guarded(conn, cancel, budget, |conn| {
        for limit in [Limit::SQLITE_LIMIT_LENGTH, Limit::SQLITE_LIMIT_SQL_LENGTH] {
            conn.set_limit(limit, MAX_SQLITE_VALUE)
                .map_err(|e| schema::classify(e, "bound restore schema metadata"))?;
        }
        conn.set_limit(Limit::SQLITE_LIMIT_COLUMN, MAX_COLUMNS)
            .map_err(|e| schema::classify(e, "bound restore schema columns"))?;
        conn.set_limit(
            Limit::SQLITE_LIMIT_EXPR_DEPTH,
            super::image_inspect::INSPECTION_EXPR_DEPTH,
        )
        .map_err(|e| schema::classify(e, "bound restore schema depth"))?;
        conn.execute_batch(
            "PRAGMA trusted_schema=OFF; PRAGMA query_only=ON; PRAGMA temp_store=MEMORY;",
        )
        .map_err(|e| inspection_error(e, "harden restore schema inspection"))?;
        let version = conn
            .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .map_err(|e| inspection_error(e, "read restore schema version"))?;
        if version != i64::from(schema::LATEST_SCHEMA_VERSION) {
            return Err(StoreError::corrupt(format!(
                "restore schema version {version} is incompatible with current version {}",
                schema::LATEST_SCHEMA_VERSION
            )));
        }
        for table in REQUIRED {
            if cancel.load(Ordering::Acquire) {
                // guarded translates cancellation, including a final race, to None.
                return Err(StoreError::unavailable(
                    "restore schema inspection cancelled",
                ));
            }
            inspect_table(conn, table)?;
        }
        Ok(Inspection {
            schema_version: version,
        })
    })
}

#[derive(Clone, Copy)]
enum RowidShape {
    Rowid,
    WithoutRowid,
}

struct RequiredTable {
    name: &'static str,
    shape: RowidShape,
    columns: &'static [(&'static str, &'static str)],
}

fn inspect_table(conn: &Connection, table: &RequiredTable) -> Result<(), StoreError> {
    let shape = conn
        .query_row(
            "SELECT type, wr, strict FROM pragma_table_list
             WHERE schema='main' AND name=?1 COLLATE NOCASE",
            [table.name],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()
        .map_err(|e| inspection_error(e, "read restore table shape"))?;
    let expected_wr = match table.shape {
        RowidShape::Rowid => 0,
        RowidShape::WithoutRowid => 1,
    };
    if shape
        .as_ref()
        .is_none_or(|(kind, wr, strict)| kind != "table" || *wr != expected_wr || *strict != 1)
    {
        return Err(StoreError::corrupt(format!(
            "restore table {} is missing or has an incompatible table shape",
            table.name
        )));
    }

    let mut statement = conn
        .prepare("SELECT name, type, hidden FROM pragma_table_xinfo(?1, 'main')")
        .map_err(|e| inspection_error(e, "inspect restore table columns"))?;
    let mut rows = statement
        .query([table.name])
        .map_err(|e| inspection_error(e, "inspect restore table columns"))?;
    let mut found = 0u32;
    while let Some(row) = rows
        .next()
        .map_err(|e| inspection_error(e, "read restore column"))?
    {
        let name = row
            .get_ref(0)
            .map_err(|e| inspection_error(e, "read restore column name"))?
            .as_str()
            .map_err(|e| inspection_error(e.into(), "read restore column name"))?;
        let declared = row
            .get_ref(1)
            .map_err(|e| inspection_error(e, "read restore column type"))?
            .as_str()
            .map_err(|e| inspection_error(e.into(), "read restore column type"))?;
        let hidden: i64 = row
            .get(2)
            .map_err(|e| inspection_error(e, "read restore column visibility"))?;
        // A generated column can shadow rowid too. Check before skipping
        // hidden columns, or a physical quarantine cursor could become an expression.
        if matches!(table.shape, RowidShape::Rowid)
            && ["rowid", "_rowid_", "oid"]
                .iter()
                .any(|alias| name.eq_ignore_ascii_case(alias))
        {
            return Err(StoreError::corrupt(format!(
                "restore table {} shadows rowid",
                table.name
            )));
        }
        if hidden != 0 {
            continue;
        }
        for (index, (column, expected_type)) in table.columns.iter().enumerate() {
            if name.eq_ignore_ascii_case(column) && declared.eq_ignore_ascii_case(expected_type) {
                found |= 1u32 << index;
            }
        }
    }
    if found != (1u32 << table.columns.len()) - 1 {
        return Err(StoreError::corrupt(format!(
            "restore table {} lacks an ordinary column with its required type",
            table.name
        )));
    }
    Ok(())
}

// Only the quarantine transaction's named columns and physical-cursor shapes.
// In particular, verifier_objects really is WITHOUT ROWID; backup_generations
// must have an unshadowed rowid for its NOT INDEXED metadata preflight.
const REQUIRED: &[RequiredTable] = &[
    RequiredTable {
        name: "store_meta",
        shape: RowidShape::Rowid,
        columns: &[("key", "TEXT"), ("value", "TEXT")],
    },
    RequiredTable {
        name: "accounts",
        shape: RowidShape::Rowid,
        columns: &[("account_id", "TEXT"), ("state", "TEXT")],
    },
    RequiredTable {
        name: "import_jobs",
        shape: RowidShape::Rowid,
        columns: &[("job_id", "TEXT"), ("state", "TEXT")],
    },
    RequiredTable {
        name: "intents",
        shape: RowidShape::Rowid,
        columns: &[
            ("intent_id", "TEXT"),
            ("state", "TEXT"),
            ("quarantine_reason", "TEXT"),
        ],
    },
    RequiredTable {
        name: "send_attempts",
        shape: RowidShape::Rowid,
        columns: &[("attempt_id", "TEXT"), ("state", "TEXT")],
    },
    RequiredTable {
        name: "backup_generations",
        shape: RowidShape::Rowid,
        columns: &[
            ("generation_id", "TEXT"),
            ("started_at", "INTEGER"),
            ("completed_at", "INTEGER"),
            ("state", "TEXT"),
            ("location", "TEXT"),
            ("manifest_sha256", "TEXT"),
            ("object_count", "INTEGER"),
            ("total_bytes", "INTEGER"),
        ],
    },
    RequiredTable {
        name: "object_pins",
        shape: RowidShape::Rowid,
        columns: &[
            ("object_sha256", "TEXT"),
            ("pin_kind", "TEXT"),
            ("pin_ref", "TEXT"),
            ("created_at", "INTEGER"),
        ],
    },
    RequiredTable {
        name: "backup_ownership",
        shape: RowidShape::Rowid,
        columns: &[("generation_id", "TEXT"), ("token", "TEXT")],
    },
    RequiredTable {
        name: "backup_cleanup_pending",
        shape: RowidShape::Rowid,
        columns: &[("generation_id", "TEXT")],
    },
    RequiredTable {
        name: "verifier_scratch",
        shape: RowidShape::Rowid,
        columns: &[("id", "TEXT"), ("token", "TEXT"), ("incarnation", "TEXT")],
    },
    RequiredTable {
        name: "verifier_objects",
        shape: RowidShape::WithoutRowid,
        columns: &[
            ("scratch_id", "TEXT"),
            ("sha256", "BLOB"),
            ("bytes", "BLOB"),
            ("referenced", "INTEGER"),
        ],
    },
];

#[cfg(test)]
mod tests;
