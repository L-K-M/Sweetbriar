//! Exact declared catalog of the compiled migration chain. Only the trusted
//! in-memory reference executes migrations; the retained image is read-only.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::Connection;

use super::inspection_error;
use crate::{Seams, StoreError, schema};

#[derive(Debug, PartialEq, Eq)]
struct Entry {
    kind: String,
    table: String,
    sql: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Catalog(BTreeMap<String, Entry>);

impl Catalog {
    /// A catalog-only row/SQL bound derived from trusted declarations. Keep
    /// room for SQLite record headers and small internal statements, without
    /// allowing large arbitrary CHECK literals during eager schema parsing.
    pub(super) fn metadata_limit(&self) -> Result<i32, StoreError> {
        let maximum = self
            .0
            .iter()
            .map(|(name, entry)| {
                name.len()
                    + entry.kind.len()
                    + entry.table.len()
                    + entry.sql.as_ref().map_or(0, String::len)
                    + 128
            })
            .max()
            .unwrap_or(0)
            .max(4096);
        if maximum > super::MAX_SQLITE_VALUE as usize {
            return Err(StoreError::internal(
                "compiled catalog exceeds inspection metadata budget",
            ));
        }
        Ok(maximum as i32)
    }
}

const SCAN: &str = "SELECT type, name, tbl_name, sql FROM main.sqlite_schema NOT INDEXED";

pub(super) fn reference(seams: &Seams) -> Result<Catalog, StoreError> {
    let mut connection = Connection::open_in_memory()
        .map_err(|error| schema::classify(error, "open trusted schema reference"))?;
    schema::run_migrations(&mut connection, seams, schema::MIGRATIONS)?;
    let expected = (|| {
        let mut statement = connection
            .prepare(SCAN)
            .map_err(|error| schema::classify(error, "prepare trusted catalog"))?;
        let mut rows = statement
            .query([])
            .map_err(|error| schema::classify(error, "read trusted catalog"))?;
        let mut entries = BTreeMap::new();
        while let Some(row) = rows
            .next()
            .map_err(|error| schema::classify(error, "read trusted catalog"))?
        {
            let name = row
                .get(1)
                .map_err(|error| schema::classify(error, "read trusted catalog name"))?;
            let entry = Entry {
                kind: row
                    .get(0)
                    .map_err(|error| schema::classify(error, "read trusted catalog kind"))?,
                table: row
                    .get(2)
                    .map_err(|error| schema::classify(error, "read trusted catalog table"))?,
                sql: row
                    .get(3)
                    .map_err(|error| schema::classify(error, "read trusted catalog SQL"))?,
            };
            if entries.insert(name, entry).is_some() {
                return Err(StoreError::internal(
                    "duplicate name in compiled schema catalog",
                ));
            }
        }
        Ok(Catalog(entries))
    })();
    connection
        .close()
        .map_err(|(_, error)| schema::classify(error, "close trusted schema reference"))?;
    expected
}

/// The caller installs schema-input, SQLite-value/work and cancellation
/// bounds before the first schema load. A row limit here alone cannot do
/// that. Scan physically without sorting or trusting an inherited index;
/// retain only names borrowed from the fixed trusted reference.
pub(super) fn compare(connection: &Connection, expected: &Catalog) -> Result<(), StoreError> {
    let mut statement = connection
        .prepare(SCAN)
        .map_err(|error| inspection_error(error, "prepare retained catalog comparison"))?;
    let mut rows = statement
        .query([])
        .map_err(|error| inspection_error(error, "read retained catalog"))?;
    let mut seen = BTreeSet::new();
    while let Some(row) = rows
        .next()
        .map_err(|error| inspection_error(error, "read retained catalog"))?
    {
        if seen.len() == expected.0.len() {
            return Err(different());
        }
        let text = |column| -> Result<&str, StoreError> {
            row.get_ref(column)
                .and_then(|value| value.as_str().map_err(Into::into))
                .map_err(|error| inspection_error(error, "read catalog metadata"))
        };
        let name = text(1)?;
        let Some((canonical_name, canonical)) = expected.0.get_key_value(name) else {
            return Err(different());
        };
        if !seen.insert(canonical_name) || text(0)? != canonical.kind || text(2)? != canonical.table
        {
            return Err(different());
        }
        let sql = row
            .get_ref(3)
            .map_err(|error| inspection_error(error, "read catalog SQL"))?;
        let same_sql = match (&canonical.sql, sql) {
            (None, rusqlite::types::ValueRef::Null) => true,
            (Some(expected), rusqlite::types::ValueRef::Text(actual)) => {
                actual == expected.as_bytes()
            }
            _ => false,
        };
        if !same_sql {
            return Err(different());
        }
    }
    if seen.len() != expected.0.len() {
        return Err(different());
    }
    Ok(())
}

fn different() -> StoreError {
    // Never interpolate hostile names or SQL into diagnostics.
    StoreError::corrupt("retained catalog differs from the current compiled schema")
}
