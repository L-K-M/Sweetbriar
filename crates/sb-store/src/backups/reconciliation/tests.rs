use super::*;

#[test]
fn corrupt_history_cannot_hide_a_duplicate_generation_across_batches() {
    let scratch = sb_test_support::ScratchDir::new("reconcile-generation-order");
    let path = scratch.path().join("image.sqlite");
    let conn = Connection::open(&path).unwrap();
    const PAGE_SIZE: usize = 65_536;
    conn.execute_batch("PRAGMA page_size=65536; CREATE TABLE backup_generations(generation_id TEXT, started_at INTEGER, state TEXT); BEGIN;").unwrap();
    let mut generation_id = [0; 16];
    generation_id[6] = 0x40;
    generation_id[8] = 0x80;
    generation_id[15] = 0x41;
    let target = uuid(generation_id);
    for row in 1..=300 {
        let name = if row == 1 || row == 200 {
            target.clone()
        } else {
            format!("11111111-1111-4111-8111-{row:012x}")
        };
        conn.execute(
            "INSERT INTO backup_generations VALUES (?1,42,'in_progress')",
            [name],
        )
        .unwrap();
    }
    conn.execute_batch("COMMIT").unwrap();
    let root: u32 = conn
        .query_row(
            "SELECT rootpage FROM sqlite_schema WHERE name='backup_generations'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    drop(conn);
    let mut bytes = std::fs::read(path).unwrap();
    let page = (usize::try_from(root).unwrap() - 1) * PAGE_SIZE;
    assert_eq!(bytes[page], 0x0d, "table leaf page");
    assert_eq!(u16::from_be_bytes([bytes[page + 3], bytes[page + 4]]), 300);
    // Put row 201 at the batch boundary. Restarting at rowid > 201
    // skips row 200, which carries the second copy of the target UUID.
    let first = page + 8 + 199 * 2;
    let second = page + 8 + 200 * 2;
    bytes.swap(first, second);
    bytes.swap(first + 1, second + 1);
    let conn = crate::image_vfs::open(Box::new(std::io::Cursor::new(bytes))).unwrap();
    let physical: i64 = conn
        .query_row(
            "SELECT count(*) FROM backup_generations NOT INDEXED WHERE generation_id=?1",
            [&target],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(physical, 2);
    let expected = manifest::Manifest {
        store_uuid: generation_id,
        generation_id,
        schema_version: 1,
        created_at: 42,
        database: manifest::Entry {
            sha256: [0; 32],
            bytes: PAGE_SIZE as u64,
        },
    };
    let result = generation_matches(&conn, &expected, &AtomicBool::new(false));
    assert!(
        matches!(result, Err(StoreError::Corrupt { .. })),
        "{result:?}"
    );
}
