//! Stream references from the authenticated, isolated image. A caller must
//! discard every emitted reference unless the whole walk completes.

use std::io::{self, Read};
use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, Ordering};

use rusqlite::{Connection, params};

use super::attachments::{self, Hash};
use super::image_inspect::{MAX_PROGRESS_CALLS, guarded, inspection_error};
use crate::StoreError;

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Reference {
    pub hash: Hash,
    pub size: Option<u64>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    Acquisition,
    SendAttempt,
    Draft,
}

fn sql_error(e: rusqlite::Error) -> StoreError {
    inspection_error(e, "read snapshot references")
}

// Reject views, WITHOUT ROWID tables and shadowed rowid names before issuing
// any traversal SQL. The latter would otherwise silently omit physical rows.
fn validate_schema(conn: &Connection) -> Result<(), StoreError> {
    for (table, columns) in [
        ("acquisitions", &["content_sha256", "byte_length"][..]),
        ("send_attempts", &["mime_sha256"][..]),
        ("draft_revisions", &["attachments_json"][..]),
    ] {
        let valid: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_list WHERE schema='main' AND name=?1 AND type='table' AND wr=0)",
            [table], |r| r.get(0),
        ).map_err(sql_error)?;
        if !valid {
            return Err(StoreError::corrupt("invalid snapshot reference table"));
        }
        let mut stmt = conn
            .prepare("SELECT name, type, hidden FROM pragma_table_xinfo(?1)")
            .map_err(sql_error)?;
        let mut rows = stmt.query([table]).map_err(sql_error)?;
        let mut found = 0u8;
        while let Some(row) = rows.next().map_err(sql_error)? {
            let name: String = row.get(0).map_err(sql_error)?;
            let ty: String = row.get(1).map_err(sql_error)?;
            let hidden: i64 = row.get(2).map_err(sql_error)?;
            if ["rowid", "_rowid_", "oid"]
                .iter()
                .any(|n| name.eq_ignore_ascii_case(n))
            {
                return Err(StoreError::corrupt("snapshot shadows rowid"));
            }
            for (i, column) in columns.iter().enumerate() {
                if name == *column
                    && hidden == 0
                    && ty.eq_ignore_ascii_case(if *column == "byte_length" {
                        "INTEGER"
                    } else {
                        "TEXT"
                    })
                {
                    found |= 1 << i;
                }
            }
        }
        if found != (1 << columns.len()) - 1 {
            return Err(StoreError::corrupt("invalid snapshot reference columns"));
        }
    }
    Ok(())
}

fn hash(bytes: Vec<u8>) -> Hash {
    match bytes.try_into() {
        Ok(bytes) => Hash::Bytes(bytes),
        Err(_) => Hash::Unmatchable,
    }
}

struct TrackRead<'a, R> {
    input: R,
    error: &'a mut Option<io::Error>,
}

impl<R: Read> Read for TrackRead<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.input.read(buf).map_err(|e| {
            *self.error = Some(e);
            io::Error::other("snapshot read failed")
        })
    }
}

// Decoder errors are distinct from native reads and caller failures, even
// though all three cross the decoder's io::Result boundary.
fn decode(
    input: impl Read,
    cancelled: &AtomicBool,
    sink: &mut impl FnMut(Reference) -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    let mut sink_error = None;
    let mut read_error = None;
    let result = attachments::decode(
        TrackRead {
            input,
            error: &mut read_error,
        },
        || {
            if cancelled.load(Ordering::Acquire) {
                Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"))
            } else {
                Ok(())
            }
        },
        |r| match sink(Reference {
            hash: r.hash,
            size: Some(r.size),
        }) {
            Ok(()) => Ok(()),
            Err(e) => {
                sink_error = Some(e);
                Err(io::Error::other("reference sink failed"))
            }
        },
    );
    if let Some(error) = sink_error {
        return Err(error);
    }
    if let Some(e) = read_error {
        let kind = e.kind();
        let text = e.to_string();
        if let Some(inner) = e.into_inner()
            && let Ok(sqlite) = inner.downcast::<rusqlite::Error>()
        {
            return Err(sql_error(*sqlite));
        }
        return Err(StoreError::unavailable_io(
            format!("read snapshot attachments: {text}"),
            kind,
        ));
    }
    match result {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::InvalidData => Err(StoreError::corrupt(format!(
            "invalid snapshot attachments: {e}"
        ))),
        Err(e) => Err(StoreError::unavailable_io(
            format!("read snapshot attachments: {e}"),
            e.kind(),
        )),
    }
}

/// Enumerate every occurrence, including duplicates and old draft revisions.
/// Only `Continue` certifies that every reference was emitted. The sink must
/// discard all of its effects on cancellation or any error.
pub(super) fn walk(
    conn: &Connection,
    cancelled: &AtomicBool,
    mut sink: impl FnMut(Reference) -> Result<(), StoreError>,
) -> Result<ControlFlow<()>, StoreError> {
    if guarded(conn, cancelled, MAX_PROGRESS_CALLS, validate_schema)?.is_none() {
        return Ok(ControlFlow::Break(()));
    }
    for (first, next, source) in [
        (
            "SELECT rowid, typeof(content_sha256), substr(CAST(content_sha256 AS BLOB),1,65), byte_length FROM acquisitions ORDER BY rowid LIMIT 200",
            "SELECT rowid, typeof(content_sha256), substr(CAST(content_sha256 AS BLOB),1,65), byte_length FROM acquisitions WHERE rowid>?1 ORDER BY rowid LIMIT 200",
            Source::Acquisition,
        ),
        (
            "SELECT rowid, typeof(mime_sha256), substr(CAST(mime_sha256 AS BLOB),1,65) FROM send_attempts ORDER BY rowid LIMIT 200",
            "SELECT rowid, typeof(mime_sha256), substr(CAST(mime_sha256 AS BLOB),1,65) FROM send_attempts WHERE rowid>?1 ORDER BY rowid LIMIT 200",
            Source::SendAttempt,
        ),
        (
            "SELECT rowid, typeof(attachments_json) FROM draft_revisions ORDER BY rowid LIMIT 200",
            "SELECT rowid, typeof(attachments_json) FROM draft_revisions WHERE rowid>?1 ORDER BY rowid LIMIT 200",
            Source::Draft,
        ),
    ] {
        let mut after = None;
        loop {
            let page = guarded(conn, cancelled, MAX_PROGRESS_CALLS, |conn| {
                let mut stmt = conn
                    .prepare(if after.is_some() { next } else { first })
                    .map_err(sql_error)?;
                let mut rows = if let Some(id) = after {
                    stmt.query(params![id])
                } else {
                    stmt.query([])
                }
                .map_err(sql_error)?;
                let mut count = 0;
                let mut last = after;
                while let Some(row) = rows.next().map_err(sql_error)? {
                    if cancelled.load(Ordering::Acquire) {
                        break;
                    }
                    let id: i64 = row.get(0).map_err(sql_error)?;
                    let ty: String = row.get(1).map_err(sql_error)?;
                    if ty != "text" {
                        return Err(StoreError::corrupt("invalid snapshot reference type"));
                    }
                    if source == Source::Draft {
                        let blob = conn
                            .blob_open("main", "draft_revisions", "attachments_json", id, true)
                            .map_err(sql_error)?;
                        decode(blob, cancelled, &mut sink)?;
                    } else {
                        let bytes: Vec<u8> = row.get(2).map_err(sql_error)?;
                        let size = if source == Source::Acquisition {
                            let n: i64 = row.get(3).map_err(sql_error)?;
                            Some(u64::try_from(n).map_err(|_| {
                                StoreError::corrupt("negative snapshot acquisition length")
                            })?)
                        } else {
                            None
                        };
                        sink(Reference {
                            hash: hash(bytes),
                            size,
                        })?;
                    }
                    last = Some(id);
                    count += 1;
                }
                Ok((last, count))
            })?;
            let Some((last, count)) = page else {
                return Ok(ControlFlow::Break(()));
            };
            if count < 200 {
                break;
            }
            after = last;
        }
    }
    if cancelled.load(Ordering::Acquire) {
        Ok(ControlFlow::Break(()))
    } else {
        Ok(ControlFlow::Continue(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE acquisitions(content_sha256 TEXT NOT NULL, byte_length INTEGER NOT NULL); CREATE TABLE send_attempts(mime_sha256 TEXT NOT NULL); CREATE TABLE draft_revisions(attachments_json TEXT NOT NULL);").unwrap();
        conn
    }

    fn entry(hash: &str, size: u64) -> String {
        format!(r#"{{"object_sha256":"{hash}","filename":"f","media_type":"x","size":{size}}}"#)
    }

    #[test]
    fn walks_all_sources_and_keeps_duplicates_across_pages() {
        let conn = db();
        let hash = "a".repeat(64);
        for id in 0..401 {
            conn.execute("INSERT INTO acquisitions VALUES (?1, ?2)", (&hash, id))
                .unwrap();
        }
        conn.execute("INSERT INTO send_attempts VALUES (?1)", [&hash])
            .unwrap();
        let json =
            format!(r#"[{{"object_sha256":"{hash}","filename":"f","media_type":"x","size":3}}]"#);
        conn.execute("INSERT INTO draft_revisions VALUES (?1)", [&json])
            .unwrap();
        let mut refs = Vec::new();
        let result = walk(&conn, &AtomicBool::new(false), |r| {
            refs.push(r);
            Ok(())
        })
        .unwrap();
        assert_eq!(result, ControlFlow::Continue(()));
        assert_eq!(refs.len(), 403);
        assert_eq!(refs[0].size, Some(0));
        assert_eq!(refs[400].size, Some(400));
        assert_eq!(refs[401].size, None);
        assert_eq!(refs[402].size, Some(3));
        assert!(refs.iter().all(|r| r.hash == Hash::Bytes([b'a'; 64])));
    }

    #[test]
    fn empty_sparse_and_negative_rowids_and_large_corpus() {
        let conn = db();
        let cancel = AtomicBool::new(false);
        assert_eq!(
            walk(&conn, &cancel, |_| panic!("empty")).unwrap(),
            ControlFlow::Continue(())
        );
        let hash = "a".repeat(64);
        for id in [i64::MIN, -5, 0, 3, i64::MAX] {
            conn.execute(
                "INSERT INTO send_attempts(rowid,mime_sha256) VALUES (?1,?2)",
                params![id, hash],
            )
            .unwrap();
        }
        conn.execute_batch("BEGIN").unwrap();
        for id in 1..20_001 {
            conn.execute("INSERT INTO acquisitions VALUES (?1,?2)", params![hash, id])
                .unwrap();
        }
        conn.execute_batch("COMMIT").unwrap();
        let mut count = 0;
        assert_eq!(
            walk(&conn, &cancel, |_| {
                count += 1;
                Ok(())
            })
            .unwrap(),
            ControlFlow::Continue(())
        );
        assert_eq!(count, 20_005);
    }

    #[test]
    fn pages_every_source_and_retains_conflicting_sizes() {
        let conn = db();
        let hash = "f".repeat(64);
        let json = format!("[{}]", entry(&hash, 9));
        for id in -100..101 {
            conn.execute(
                "INSERT INTO acquisitions(rowid,content_sha256,byte_length) VALUES (?1,?2,?3)",
                params![id, hash, id + 100],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO send_attempts(rowid,mime_sha256) VALUES (?1,?2)",
                params![id, hash],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO draft_revisions(rowid,attachments_json) VALUES (?1,?2)",
                params![id, json],
            )
            .unwrap();
        }
        let mut refs = Vec::new();
        assert_eq!(
            walk(&conn, &AtomicBool::new(false), |r| {
                refs.push(r);
                Ok(())
            })
            .unwrap(),
            ControlFlow::Continue(())
        );
        assert_eq!(refs.len(), 603);
        assert_eq!(refs[0].size, Some(0));
        assert_eq!(refs[200].size, Some(200));
        assert!(refs[201..402].iter().all(|r| r.size.is_none()));
        assert!(refs[402..].iter().all(|r| r.size == Some(9)));
        assert!(refs.iter().all(|r| r.hash == Hash::Bytes([b'f'; 64])));
    }

    #[test]
    fn canonical_schema_includes_missing_and_historical_rows() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("../schema/v1.sql"))
            .unwrap();
        conn.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
        let hash = "1".repeat(64);
        conn.execute(
            "INSERT INTO acquisitions VALUES (1,?1,7,'missing',0)",
            [&hash],
        )
        .unwrap();
        conn.execute("INSERT INTO send_attempts(attempt_id,draft_id,revision_id,account_id,config_generation,identity_id,mime_sha256,message_id_header,envelope_from,state,sent_copy,created_at,updated_at) VALUES ('s','d','r','a',1,'i',?1,'m','e','cancelled','filed',0,0)", [&hash]).unwrap();
        let json = format!("[{}]", entry(&hash, 8));
        for (id, seq) in [("r", 1), ("old", 2)] {
            conn.execute("INSERT INTO draft_revisions(revision_id,draft_id,seq,compose_format,subject,to_json,cc_json,bcc_json,body_source,attachments_json,created_at) VALUES (?1,'d',?2,'plain','','[]','[]','[]','',?3,0)", params![id,seq,json]).unwrap();
        }
        let mut sizes = Vec::new();
        assert_eq!(
            walk(&conn, &AtomicBool::new(false), |r| {
                sizes.push(r.size);
                Ok(())
            })
            .unwrap(),
            ControlFlow::Continue(())
        );
        assert_eq!(sizes, [Some(7), None, Some(8), Some(8)]);
    }

    #[test]
    fn large_attachment_blob_streams_past_sqlite_value_limit() {
        let conn = db();
        let hash = "b".repeat(64);
        let item = entry(&hash, 4);
        let json = format!("[{}]", vec![item; 12_000].join(","));
        assert!(json.len() > 1_048_576);
        conn.execute("INSERT INTO draft_revisions VALUES (?1)", [&json])
            .unwrap();
        conn.set_limit(rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH, 1_048_576)
            .unwrap();
        let mut count = 0;
        let result = walk(&conn, &AtomicBool::new(false), |r| {
            assert_eq!(r.size, Some(4));
            count += 1;
            Ok(())
        });
        assert_eq!(result.unwrap(), ControlFlow::Continue(()));
        assert_eq!(count, 12_000);
    }

    #[test]
    fn malformed_after_emission_and_sink_failure_never_complete() {
        let conn = db();
        let json = format!("[{},{{}}]", entry(&"c".repeat(64), 2));
        conn.execute("INSERT INTO draft_revisions VALUES (?1)", [&json])
            .unwrap();
        let mut count = 0;
        let result = walk(&conn, &AtomicBool::new(false), |_| {
            count += 1;
            Ok(())
        });
        assert!(matches!(result, Err(StoreError::Corrupt { .. })));
        assert_eq!(count, 1, "caller must discard this partial emission");
        let result = walk(&conn, &AtomicBool::new(false), |_| {
            Err(StoreError::conflict("sink sentinel"))
        });
        assert!(
            matches!(result, Err(StoreError::Conflict { detail }) if detail == "sink sentinel")
        );
    }

    #[test]
    fn cancellation_before_and_during_rows_and_attachment() {
        let conn = db();
        let cancel = AtomicBool::new(true);
        assert_eq!(
            walk(&conn, &cancel, |_| panic!("cancelled")).unwrap(),
            ControlFlow::Break(())
        );
        cancel.store(false, Ordering::Release);
        let json = format!(
            "[{},{}]",
            entry(&"d".repeat(64), 1),
            entry(&"e".repeat(64), 2)
        );
        conn.execute("INSERT INTO draft_revisions VALUES (?1)", [&json])
            .unwrap();
        let mut count = 0;
        assert_eq!(
            walk(&conn, &cancel, |_| {
                count += 1;
                cancel.store(true, Ordering::Release);
                Ok(())
            })
            .unwrap(),
            ControlFlow::Break(())
        );
        assert_eq!(count, 1);
        cancel.store(false, Ordering::Release);
        conn.execute("INSERT INTO acquisitions VALUES ('a',1)", [])
            .unwrap();
        assert_eq!(
            walk(&conn, &cancel, |_| {
                cancel.store(true, Ordering::Release);
                Ok(())
            })
            .unwrap(),
            ControlFlow::Break(())
        );
    }

    #[test]
    fn invalid_types_lengths_and_table_substitutions_fail_closed() {
        for sql in [
            "INSERT INTO acquisitions VALUES ('a',-1)",
            "INSERT INTO acquisitions VALUES (x'01',1)",
            "INSERT INTO acquisitions VALUES ('a','bad')",
            "INSERT INTO send_attempts VALUES (x'01')",
            "INSERT INTO draft_revisions VALUES (x'5b5d')",
            "DROP TABLE send_attempts; CREATE VIEW send_attempts AS SELECT 'a' AS mime_sha256",
            "DROP TABLE send_attempts; CREATE TABLE send_attempts(mime_sha256 TEXT PRIMARY KEY) WITHOUT ROWID",
            "DROP TABLE send_attempts; CREATE TABLE send_attempts(rowid INTEGER, mime_sha256 TEXT)",
            "DROP TABLE send_attempts; CREATE TABLE send_attempts(mime_sha256 INTEGER)",
        ] {
            let conn = db();
            conn.execute_batch(sql).unwrap();
            assert!(
                matches!(
                    walk(&conn, &AtomicBool::new(false), |_| Ok(())),
                    Err(StoreError::Corrupt { .. })
                ),
                "{sql}"
            );
        }
    }

    #[test]
    fn native_reader_and_sqlite_errors_are_preserved() {
        struct Broken(io::ErrorKind);
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::new(self.0, "read sentinel"))
            }
        }
        let error = decode(
            Broken(io::ErrorKind::InvalidData),
            &AtomicBool::new(false),
            &mut |_| Ok(()),
        )
        .unwrap_err();
        assert_eq!(error.io_kind(), Some(io::ErrorKind::InvalidData));
        struct BrokenSqlite;
        impl Read for BrokenSqlite {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_IOERR),
                    None,
                )))
            }
        }
        let error = decode(BrokenSqlite, &AtomicBool::new(false), &mut |_| Ok(())).unwrap_err();
        assert!(matches!(error, StoreError::Unavailable { .. }));
        let conn = db();
        conn.execute_batch("DROP TABLE acquisitions").unwrap();
        assert!(matches!(
            walk(&conn, &AtomicBool::new(false), |_| Ok(())),
            Err(StoreError::Corrupt { .. })
        ));
    }

    #[test]
    fn sqlite_image_read_failure_after_emission_is_not_success() {
        use std::io::{Cursor, Seek, SeekFrom};
        use std::sync::Arc;

        struct FailingImage {
            bytes: Cursor<Vec<u8>>,
            fail: Arc<AtomicBool>,
        }
        impl Read for FailingImage {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if self.fail.load(Ordering::Acquire) {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "injected read",
                    ));
                }
                self.bytes.read(buf)
            }
        }
        impl Seek for FailingImage {
            fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
                self.bytes.seek(pos)
            }
        }

        let scratch = sb_test_support::ScratchDir::new("walker-native-read");
        let path = scratch.path().join("image.sqlite");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE acquisitions(content_sha256 TEXT NOT NULL, byte_length INTEGER NOT NULL); CREATE TABLE send_attempts(mime_sha256 TEXT NOT NULL); CREATE TABLE draft_revisions(attachments_json TEXT NOT NULL); BEGIN;").unwrap();
        for _ in 0..1000 {
            conn.execute("INSERT INTO acquisitions VALUES (?1, 1)", ["a".repeat(64)])
                .unwrap();
        }
        conn.execute_batch("COMMIT;").unwrap();
        drop(conn);
        let fail = Arc::new(AtomicBool::new(false));
        let input = FailingImage {
            bytes: Cursor::new(std::fs::read(path).unwrap()),
            fail: Arc::clone(&fail),
        };
        let conn = crate::image_vfs::open(Box::new(input)).unwrap();
        conn.execute_batch("PRAGMA cache_size=1").unwrap();
        let mut emitted = 0;
        let result = walk(&conn, &AtomicBool::new(false), |_| {
            emitted += 1;
            fail.store(true, Ordering::Release);
            Ok(())
        });
        assert!(emitted > 0);
        assert!(
            matches!(result, Err(StoreError::Unavailable { .. })),
            "{result:?}"
        );
    }
}
