use super::*;

fn corrupt_after_catalog(image: &Image) -> StoreError {
    assert!(
        stage_canonical::inspect(
            Box::new(std::fs::File::open(&image.path).unwrap()),
            &test_seams(real_fs()),
            &AtomicBool::new(false),
        )
        .unwrap()
        .is_some(),
        "physical fixture must pass declared catalog equality"
    );
    let before = std::fs::read(&image.path).unwrap();
    let closed = crate::image_vfs::closes_on_this_thread();
    let error = image.inspect().unwrap_err();
    assert!(matches!(error, StoreError::Corrupt { .. }), "{error:?}");
    assert_eq!(crate::image_vfs::closes_on_this_thread(), closed + 1);
    assert_eq!(std::fs::read(&image.path).unwrap(), before);
    error
}

fn page_location(image: &Image, name: &str) -> (usize, usize) {
    let conn = Connection::open(&image.path).unwrap();
    let root: u32 = conn
        .query_row(
            "SELECT rootpage FROM sqlite_schema WHERE name=?1",
            [name],
            |row| row.get(0),
        )
        .unwrap();
    let size: u32 = conn
        .query_row("PRAGMA page_size", [], |row| row.get(0))
        .unwrap();
    assert!(root > 1, "never damage the catalog page in this fixture");
    conn.close().unwrap();
    let size = usize::try_from(size).unwrap();
    ((usize::try_from(root).unwrap() - 1) * size, size)
}

fn change_page(image: &Image, name: &str, change: impl FnOnce(&mut [u8])) {
    let (start, size) = page_location(image, name);
    let mut bytes = std::fs::read(&image.path).unwrap();
    let before = bytes.clone();
    change(&mut bytes[start..start + size]);
    assert_ne!(bytes, before, "fixture must change actual stored bytes");
    std::fs::write(&image.path, bytes).unwrap();
}

fn varint(bytes: &[u8], offset: &mut usize) -> u64 {
    let mut value = 0;
    for index in 0..9 {
        let byte = bytes[*offset];
        *offset += 1;
        if index == 8 {
            return (value << 8) | u64::from(byte);
        }
        value = (value << 7) | u64::from(byte & 0x7f);
        if byte & 0x80 == 0 {
            return value;
        }
    }
    unreachable!()
}

#[test]
fn duplicate_and_out_of_order_physical_rowids_fail_with_exact_catalogs() {
    for duplicate in [true, false] {
        let image = Image::new();
        assert!(image.inspect().unwrap().is_some());
        change_page(&image, "store_meta", |page| {
            assert_eq!(page[0], 0x0d, "table leaf");
            let cells = usize::from(u16::from_be_bytes([page[3], page[4]]));
            assert!(cells >= 2);
            let rowid = |cell: usize| {
                let pointer = 8 + cell * 2;
                let mut offset =
                    usize::from(u16::from_be_bytes([page[pointer], page[pointer + 1]]));
                varint(page, &mut offset); // payload size precedes rowid
                let start = offset;
                let value = varint(page, &mut offset);
                assert_eq!(offset, start + 1, "small fixture rowids use one byte");
                (start, value)
            };
            let (_, first) = rowid(0);
            let (second_offset, second) = rowid(1);
            assert!(first > 0 && first < second && second < 128);
            page[second_offset] = if duplicate {
                first as u8
            } else {
                (first - 1) as u8
            };
        });
        corrupt_after_catalog(&image);
    }
}

#[test]
fn an_inherited_index_cannot_hide_its_disagreement_with_table_records() {
    let image = Image::new();
    image.sql("INSERT INTO store_meta(key,value) VALUES('other_keyx','fixture')");
    assert!(image.inspect().unwrap().is_some());
    change_page(&image, "sqlite_autoindex_store_meta_1", |page| {
        assert_eq!(page[0], 0x0a, "index leaf");
        let matches: Vec<_> = page
            .windows(10)
            .enumerate()
            .filter_map(|(offset, bytes)| (bytes == b"other_keyx").then_some(offset))
            .collect();
        assert_eq!(matches.len(), 1);
        page[matches[0]..matches[0] + 10].copy_from_slice(b"store_uuid");
    });
    corrupt_after_catalog(&image);
}

#[test]
fn an_invalid_data_btree_page_is_not_certified_by_its_declared_schema() {
    let image = Image::new();
    assert!(image.inspect().unwrap().is_some());
    change_page(&image, "d_index_queue", |page| {
        assert_eq!(page[0], 0x0d);
        page[0] = 0xff;
    });
    corrupt_after_catalog(&image);
}

#[test]
fn foreign_key_violations_need_a_separate_check_after_integrity_ok() {
    let image = Image::new();
    assert!(image.inspect().unwrap().is_some());
    let conn = Connection::open(&image.path).unwrap();
    conn.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
    assert_eq!(conn.execute("INSERT INTO verifier_objects(scratch_id,sha256,bytes) VALUES('absent-scratch',zeroblob(32),zeroblob(8))", []).unwrap(), 1);
    assert_eq!(
        conn.query_row("PRAGMA main.integrity_check(8)", [], |row| row
            .get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    {
        let mut statement = conn.prepare("PRAGMA main.foreign_key_check").unwrap();
        let mut rows = statement.query([]).unwrap();
        assert!(
            rows.next().unwrap().is_some(),
            "native FK violation must actually exist"
        );
    }
    conn.close().unwrap();
    let error = corrupt_after_catalog(&image);
    assert!(
        matches!(error, StoreError::Corrupt { ref detail } if detail == "restore image has foreign-key violations")
    );
}

#[test]
fn fts_structure_damage_is_checked_beyond_ordinary_shadow_btrees() {
    let image = Image::new();
    image.sql("INSERT INTO d_search_docs(doc_id,extractor_version,state,subject) VALUES(1,1,'indexed','fixture token'); INSERT INTO d_search_fts(rowid,subject) VALUES(1,'fixture token');");
    assert!(image.inspect().unwrap().is_some());
    let conn = Connection::open(&image.path).unwrap();
    let segment: i64 = conn
        .query_row("SELECT max(id) FROM d_search_fts_data", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(segment > 10, "fixture must have an active FTS segment");
    let before: Vec<u8> = conn
        .query_row(
            "SELECT block FROM d_search_fts_data WHERE id=?1",
            [segment],
            |row| row.get(0),
        )
        .unwrap();
    assert_ne!(before, [0]);
    assert_eq!(
        conn.execute(
            "UPDATE d_search_fts_data SET block=x'00' WHERE id=?1",
            [segment]
        )
        .unwrap(),
        1
    );
    assert_eq!(
        conn.query_row(
            "PRAGMA main.integrity_check('d_search_fts_data')",
            [],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        "ok",
        "ordinary shadow-table btree stays intact"
    );
    conn.close().unwrap();
    let control = Connection::open(&image.path).unwrap();
    assert_eq!(
        control
            .query_row(
                "SELECT block FROM d_search_fts_data WHERE id=?1",
                [segment],
                |row| row.get::<_, Vec<u8>>(0)
            )
            .unwrap(),
        [0]
    );
    control.close().unwrap();
    corrupt_after_catalog(&image);
}
