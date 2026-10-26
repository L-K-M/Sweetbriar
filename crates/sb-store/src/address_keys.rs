//! Exact address keys per source (spec/02, M1.6r2d): what an exact address
//! leaf compares, stored so a driven read can find a key's messages (06).
//!
//! ```text
//!   summary (target 0) ─┐                    ┌─► d_address_keys (field, key, domain)
//!   completed assembly ─┴─► entries per field ┤
//!                                             └─► d_address_unidentified (field)
//! ```

use std::collections::BTreeSet;

use rusqlite::{Connection, Transaction, params};

use crate::StoreError;
use crate::message_source::FIELDS;
use crate::messages::{self, ReaderAddress};
use crate::query::AddressOperand;
use crate::repair;

/// A summary's `parser_target` in the key tables; assemblies use theirs.
pub(crate) const SUMMARY_TARGET: i64 = 0;

/// The three address fields, named as the key tables store them.
const ADDRESS_FIELDS: [&str; 3] = ["from", "to", "cc"];

/// One source's address columns, as stored: From's first mailbox and the
/// To and Cc JSON lists.
pub(crate) struct Columns<'a> {
    pub(crate) from_addr: Option<&'a str>,
    pub(crate) from_name: Option<&'a str>,
    pub(crate) to_json: Option<&'a str>,
    pub(crate) cc_json: Option<&'a str>,
}

/// One field's keys: distinct (address key, domain key) pairs, and whether
/// any entry has no identifiable address.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct FieldKeys {
    pub(crate) keys: BTreeSet<(String, String)>,
    pub(crate) unidentified: bool,
}

/// The keys an exact leaf compares for `entries`: an undecodable list or an
/// entry without a strictly received address is unidentified (spec/06).
pub(crate) fn derive(entries: Result<Vec<ReaderAddress>, StoreError>) -> FieldKeys {
    let Ok(entries) = entries else {
        return FieldKeys {
            keys: BTreeSet::new(),
            unidentified: true,
        };
    };
    let mut derived = FieldKeys::default();
    for entry in entries {
        let parsed = entry
            .address
            .as_deref()
            .and_then(|address| AddressOperand::parse_received(address).ok());
        match parsed {
            Some(address) => {
                derived
                    .keys
                    .insert((address.key().to_owned(), address.domain_key().to_owned()));
            }
            None => derived.unidentified = true,
        }
    }
    derived
}

/// Every field's keys for one source, From as its one mailbox the way the
/// leaf adapts it: both parts absent is an empty list.
pub(crate) fn derive_columns(columns: &Columns<'_>) -> [FieldKeys; 3] {
    let from = match (columns.from_addr, columns.from_name) {
        (None, None) => Ok(Vec::new()),
        (address, name) => Ok(vec![ReaderAddress {
            name: name.map(str::to_owned),
            address: address.map(str::to_owned),
        }]),
    };
    let list = |json: Option<&str>| messages::addresses(json.map(str::to_owned));
    [
        derive(from),
        derive(list(columns.to_json)),
        derive(list(columns.cc_json)),
    ]
}

/// Store one source's keys; a source is written once, so rows never
/// change afterwards.
pub(crate) fn write(
    tx: &Transaction,
    acquisition: i64,
    target: i64,
    columns: &Columns<'_>,
) -> Result<(), StoreError> {
    store(tx, acquisition, target, &derive_columns(columns))
}

/// Store a completed assembly's keys from its verified text fields.
pub(crate) fn write_assembly(
    tx: &Transaction,
    acquisition: i64,
    target: u32,
    fields: &[Option<String>],
) -> Result<(), StoreError> {
    write(
        tx,
        acquisition,
        i64::from(target),
        &assembly_columns(fields),
    )
}

/// An assembly's address columns among its text fields, indexed like the
/// summary columns its pieces repair (02).
fn assembly_columns(fields: &[Option<String>]) -> Columns<'_> {
    let field = |name: &str| {
        let index = FIELDS
            .iter()
            .position(|field| *field == name)
            .expect("an assembly text field");
        fields[index].as_deref()
    };
    Columns {
        from_addr: field("from_addr"),
        from_name: field("from_name"),
        to_json: field("to_json"),
        cc_json: field("cc_json"),
    }
}

fn store(
    conn: &Connection,
    acquisition: i64,
    target: i64,
    fields: &[FieldKeys; 3],
) -> Result<(), StoreError> {
    let mut key = conn
        .prepare_cached(
            "INSERT OR IGNORE INTO d_address_keys
             (field, address_key, acquisition_id, parser_target, domain_key)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .map_err(sql_error)?;
    let mut unidentified = conn
        .prepare_cached(
            "INSERT OR IGNORE INTO d_address_unidentified (field, acquisition_id, parser_target)
             VALUES (?1, ?2, ?3)",
        )
        .map_err(sql_error)?;
    for (name, derived) in ADDRESS_FIELDS.iter().zip(fields) {
        for (address, domain) in &derived.keys {
            key.execute(params![name, address, acquisition, target, domain])
                .map_err(sql_error)?;
        }
        if derived.unidentified {
            unidentified
                .execute(params![name, acquisition, target])
                .map_err(sql_error)?;
        }
    }
    Ok(())
}

/// Migration 14: store the keys of every summary and completed assembly,
/// one source at a time, so a large store is never held in memory.
pub(crate) fn backfill(tx: &Transaction) -> Result<(), StoreError> {
    each_source(tx, |acquisition, target, fields| {
        store(tx, acquisition, target, &fields)
    })
}

/// Visit every stored summary's and completed assembly's keys, derived
/// afresh. An assembly that no longer reads back is unidentified in every
/// field, so reads evaluate its messages and report the corruption.
fn each_source(
    conn: &Connection,
    mut visit: impl FnMut(i64, i64, [FieldKeys; 3]) -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    let mut summaries = conn
        .prepare(
            "SELECT acquisition_id, from_addr, from_name, to_json, cc_json FROM d_message_summary",
        )
        .map_err(sql_error)?;
    let mut rows = summaries.query([]).map_err(sql_error)?;
    while let Some(row) = rows.next().map_err(sql_error)? {
        // STRICT and CHECK keep ids readable; a damaged one names no source.
        let Ok(acquisition) = row.get::<_, i64>(0) else {
            continue;
        };
        // A column that is not text, or not UTF-8, cannot be decoded.
        let text = |index| {
            row.get_ref(index)
                .ok()
                .and_then(|value| value.as_str_or_null().ok())
        };
        let fields = match (text(1), text(2), text(3), text(4)) {
            (Some(from_addr), Some(from_name), Some(to_json), Some(cc_json)) => {
                derive_columns(&Columns {
                    from_addr,
                    from_name,
                    to_json,
                    cc_json,
                })
            }
            _ => unidentified(),
        };
        visit(acquisition, SUMMARY_TARGET, fields)?;
    }

    let mut assemblies = conn
        .prepare(
            "SELECT acquisition_id, parser_target, field_lengths FROM d_repair_assemblies
             WHERE completed_at IS NOT NULL",
        )
        .map_err(sql_error)?;
    let mut rows = assemblies.query([]).map_err(sql_error)?;
    while let Some(row) = rows.next().map_err(sql_error)? {
        // A damaged id names no assembly a binding can reach: a binding's
        // validity check fails on it instead (02).
        let (Ok(acquisition), Ok(target)) = (row.get::<_, i64>(0), row.get::<_, u32>(1)) else {
            continue;
        };
        let lengths = row.get_ref(2).ok().and_then(|value| value.as_str().ok());
        let read = match lengths {
            Some(lengths) => repair::read_fields(conn, acquisition, target, lengths),
            None => Err(StoreError::corrupt("unreadable MIME repair field lengths")),
        };
        let fields = match read {
            Ok(fields) => derive_columns(&assembly_columns(&fields)),
            Err(StoreError::Corrupt { .. }) => unidentified(),
            Err(error) => return Err(error),
        };
        visit(acquisition, i64::from(target), fields)?;
    }
    Ok(())
}

/// A source that cannot be read back: every field unidentified, so reads
/// evaluate its messages and report the corruption. A migration must never
/// fail on stored data.
fn unidentified() -> [FieldKeys; 3] {
    std::array::from_fn(|_| FieldKeys {
        keys: BTreeSet::new(),
        unidentified: true,
    })
}

/// Fail unless the key tables hold exactly what the sources derive afresh.
#[cfg(test)]
#[track_caller]
pub(crate) fn assert_current(conn: &Connection) {
    let mut keys = BTreeSet::new();
    let mut unidentified = BTreeSet::new();
    each_source(conn, |acquisition, target, fields| {
        for (name, derived) in ADDRESS_FIELDS.iter().zip(fields) {
            for (address, domain) in derived.keys {
                keys.insert((name.to_string(), address, acquisition, target, domain));
            }
            if derived.unidentified {
                unidentified.insert((name.to_string(), acquisition, target));
            }
        }
        Ok(())
    })
    .unwrap();
    let stored_keys: BTreeSet<(String, String, i64, i64, String)> = conn
        .prepare("SELECT field, address_key, acquisition_id, parser_target, domain_key FROM d_address_keys")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let stored_unidentified: BTreeSet<(String, i64, i64)> = conn
        .prepare("SELECT field, acquisition_id, parser_target FROM d_address_unidentified")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(stored_keys, keys, "address keys drifted from their sources");
    assert_eq!(
        stored_unidentified, unidentified,
        "unidentified sources drifted"
    );
}

fn sql_error(error: rusqlite::Error) -> StoreError {
    crate::schema::classify(error, "write address keys")
}

#[cfg(test)]
mod tests;
