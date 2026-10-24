//! Read-only, finite rediscovery after a transient corpus report is lost.

use rusqlite::{Transaction, params};

use super::{base_current, sql_error, supported, target};
use crate::{StoreError, corpus_tracking};

const MAX_SOURCES: usize = 200;
const SOURCE_SQL: &str = "SELECT a.acquisition_id,
    EXISTS(SELECT 1 FROM logical_messages m WHERE m.acquisition_id=a.acquisition_id)
    FROM acquisitions a WHERE a.acquisition_id>?1 AND a.acquisition_id<=?2
    ORDER BY a.acquisition_id LIMIT ?3";

#[derive(Debug)]
pub struct RepairSources {
    pub acquisition_ids: Vec<i64>,
    pub through: i64,
    pub cutoff: i64,
    pub inputs: u64,
}

pub fn sources(
    tx: &Transaction,
    after: i64,
    cutoff: Option<i64>,
    build: u32,
) -> Result<RepairSources, StoreError> {
    if after < 0 || cutoff.is_some_and(|end| end < after) {
        return Err(StoreError::InvalidInput {
            message: "repair source range is negative or reversed".into(),
        });
    }
    let schema: u32 = tx
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(sql_error)?;
    if schema < corpus_tracking::SCHEMA_VERSION {
        return Err(StoreError::unavailable(
            "repair discovery requires schema 12",
        ));
    }
    supported(target(tx)?, build)?;
    let inputs = corpus_tracking::counter(tx, "repair_inputs")?;
    let cutoff = match cutoff {
        Some(end) => end,
        None => tx
            .query_row(
                "SELECT coalesce(max(acquisition_id),0) FROM acquisitions",
                [],
                |row| row.get(0),
            )
            .map_err(sql_error)?,
    };
    if cutoff < after {
        return Err(StoreError::InvalidInput {
            message: "repair source range is reversed".into(),
        });
    }

    let raw = tx
        .prepare_cached(SOURCE_SQL)
        .map_err(sql_error)?
        .query_map(params![after, cutoff, MAX_SOURCES as i64], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, bool>(1)?))
        })
        .map_err(sql_error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sql_error)?;
    let through = raw.last().map_or(after, |(id, _)| *id);
    let acquisition_ids = raw
        .into_iter()
        .filter_map(|(id, referenced)| {
            // Unknown BASE stays a candidate; begin owns its typed validation.
            (referenced && !matches!(base_current(tx, id, build), Ok(true))).then_some(id)
        })
        .collect();
    Ok(RepairSources {
        acquisition_ids,
        through,
        cutoff,
        inputs,
    })
}

#[cfg(test)]
mod tests;
