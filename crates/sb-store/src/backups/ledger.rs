//! Bounded trusted-store scratch for later manifest reconciliation.
use super::{classify, manifest::Entry};
use crate::StoreError;
use crate::store::await_write;
use crate::writer::{WritePriority, Writer};
use rusqlite::{Connection, params};

pub(super) const BATCH: usize = 200;

pub(super) struct LedgerRow {
    pub entry: Entry,
    pub referenced: bool,
}

fn bounded(len: usize) -> Result<(), StoreError> {
    if len > BATCH {
        return Err(StoreError::internal(
            "verifier ledger batch exceeds 200 rows",
        ));
    }
    Ok(())
}

pub(super) fn insert(writer: &Writer, id: String, entries: Vec<Entry>) -> Result<(), StoreError> {
    bounded(entries.len())?;
    await_write(writer.submit(WritePriority::Index, move |tx| {
        for entry in entries {
            let count = tx.execute(
                "INSERT INTO verifier_objects(scratch_id,sha256,bytes) VALUES (?1,?2,?3) ON CONFLICT(scratch_id,sha256) DO NOTHING",
                params![id, entry.sha256.as_slice(), entry.bytes.to_be_bytes().as_slice()],
            ).map_err(|e| classify(e, "insert verifier object"))?;
            if count == 0 {
                return Err(StoreError::corrupt("duplicate manifest object"));
            }
        }
        Ok(())
    }))
}

pub(super) fn reference(
    writer: &Writer,
    id: String,
    hashes: Vec<[u8; 32]>,
) -> Result<(), StoreError> {
    reference_sized(
        writer,
        id,
        hashes.into_iter().map(|hash| (hash, None)).collect(),
    )
}

pub(super) fn reference_sized(
    writer: &Writer,
    id: String,
    hashes: Vec<([u8; 32], Option<u64>)>,
) -> Result<(), StoreError> {
    bounded(hashes.len())?;
    await_write(writer.submit(WritePriority::Index, move |tx| {
        for (hash, size) in hashes {
            let count = tx
                .execute(
                    "UPDATE verifier_objects SET referenced=1 WHERE scratch_id=?1 AND sha256=?2 AND (?3 IS NULL OR bytes=?3)",
                    params![id, hash.as_slice(), size.map(u64::to_be_bytes).as_ref().map(|n| n.as_slice())],
                )
                .map_err(|e| classify(e, "reference verifier object"))?;
            if count == 0 {
                return Err(StoreError::corrupt(
                    "snapshot reference absent from manifest or size differs",
                ));
            }
        }
        Ok(())
    }))
}

pub(super) fn page(
    conn: &Connection,
    id: &str,
    after: Option<[u8; 32]>,
) -> Result<Vec<LedgerRow>, StoreError> {
    let mut stmt = conn.prepare("SELECT sha256, bytes, referenced FROM verifier_objects WHERE scratch_id=?1 AND sha256>?2 ORDER BY sha256 LIMIT 200")
        .map_err(|e| classify(e, "prepare verifier page"))?;
    let rows = stmt
        .query_map(
            params![id, after.as_ref().map_or(&[][..], |h| h.as_slice())],
            |row| {
                let hash: Vec<u8> = row.get(0)?;
                let bytes: Vec<u8> = row.get(1)?;
                let sha256 = hash.try_into().map_err(|_| rusqlite::Error::InvalidQuery)?;
                let bytes = u64::from_be_bytes(
                    bytes
                        .try_into()
                        .map_err(|_| rusqlite::Error::InvalidQuery)?,
                );
                Ok(LedgerRow {
                    entry: Entry { sha256, bytes },
                    referenced: row.get(2)?,
                })
            },
        )
        .map_err(|e| classify(e, "read verifier page"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| classify(e, "read verifier page"))
}

pub(super) fn purge_batch(conn: &Connection, id: &str) -> Result<usize, StoreError> {
    conn.execute("DELETE FROM verifier_objects WHERE scratch_id=?1 AND sha256 IN (SELECT sha256 FROM verifier_objects WHERE scratch_id=?1 ORDER BY sha256 LIMIT 200)", [id])
        .map_err(|e| classify(e, "purge verifier ledger"))
}

pub(super) fn purge(writer: &Writer, id: &str) -> Result<(), StoreError> {
    loop {
        let id = id.to_owned();
        let count =
            await_write(writer.submit(WritePriority::Index, move |tx| purge_batch(tx, &id)))?;
        if count == 0 {
            return Ok(());
        }
    }
}
