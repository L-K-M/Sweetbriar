use super::*;

#[test]
fn a_retained_connection_cannot_reuse_a_checkpoint_after_later_writes() {
    let source = ScratchDir::new("wal-stale-checkpoint");
    let path = source.path().join("source.db");
    let db = Connection::open(&path).unwrap();
    db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE t(v);")
        .unwrap();
    db.close().unwrap();
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(&bytes[18..20], &[2, 2]);
    let fs = SimFs::new();
    let (main, journal, statement) = caps(&fs);
    main.write_at(&bytes, 0).unwrap();
    let root = fs.open_confined_dir(Path::new("/stage")).unwrap();
    let wal = Arc::from(root.create_read_write(Path::new("wal")).unwrap());
    let writer = ConfinedSqlite::open_wal(main, journal, statement, wal).unwrap();
    let borrowed = writer.connection();
    borrowed
        .execute_batch("INSERT INTO t VALUES('first');")
        .unwrap();
    writer.checkpoint_wal().unwrap();
    // This borrow predates the wrapper's checkpoint, so no new getter
    // can invalidate its marker. Even a raw later checkpoint must not
    // make that old marker certify a different set of writes.
    borrowed
        .execute_batch("INSERT INTO t VALUES('later');")
        .unwrap();
    let result: (i64, i64, i64) = borrowed
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .unwrap();
    assert_eq!(result, (0, 0, 0));
    let result = writer.close();
    assert!(
        matches!(&result, Err(CloseError::WalNotCheckpointed)),
        "close: {result:?}"
    );
}
