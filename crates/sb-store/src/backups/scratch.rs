//! Durable verifier scratch ownership. The caller records its row first and
//! removes it only after cleanup succeeds.
use sb_types::fs::{ConfinedDir, DirectoryIdentity};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

const IMAGE: &str = "state.sqlite";

fn names(id: &str, token: &str) -> io::Result<(PathBuf, PathBuf)> {
    if id.parse::<sb_types::id::Uuid>().is_err() || token.parse::<sb_types::id::Uuid>().is_err() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid verifier ownership",
        ));
    }
    Ok((
        format!("verifier-{id}").into(),
        format!(".verifier-{id}.owner").into(),
    ))
}
fn ownership(token: &str, identity: DirectoryIdentity) -> String {
    format!("{token}\n{}:{}", identity.device, identity.inode)
}
fn read_marker(dir: &dyn ConfinedDir, name: &Path) -> io::Result<Vec<u8>> {
    let (file, _) = dir.open_regular(name)?;
    let mut bytes = Vec::new();
    file.take(129).read_to_end(&mut bytes)?;
    Ok(bytes)
}
fn partial_marker(token: &str, bytes: &[u8]) -> bool {
    if token.as_bytes() == bytes {
        return true;
    }
    let Some(rest) = bytes.strip_prefix(format!("{token}\n").as_bytes()) else {
        return false;
    };
    if rest.len() > 41 {
        return false;
    }
    let mut fields = rest.split(|byte| *byte == b':');
    let device = fields.next().unwrap_or_default();
    let inode = fields.next();
    device.len() <= 20
        && device.iter().all(u8::is_ascii_digit)
        && inode
            .is_none_or(|n| !device.is_empty() && n.len() <= 20 && n.iter().all(u8::is_ascii_digit))
        && fields.next().is_none()
}
fn mismatch() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "verifier ownership cannot be proved",
    )
}
fn absent_ok(result: io::Result<()>) -> io::Result<()> {
    match result {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Create after the durable row exists; partial creation is deliberately
/// left for recovery, which refuses to guess an unproved directory's owner.
pub(super) fn prepare(
    root: &dyn ConfinedDir,
    id: &str,
    token: &str,
) -> io::Result<Box<dyn ConfinedDir>> {
    let (dir, marker) = names(id, token)?;
    let mut sibling = root.create(&marker)?;
    sibling.write_all(token.as_bytes())?;
    sibling.sync_all()?;
    root.sync()?;
    root.create_private_dir(&dir)?;
    root.sync()?;
    let scratch = root.open_dir(&dir)?;
    let identity = scratch.identity()?;
    write!(sibling, "\n{}:{}", identity.device, identity.inode)?;
    sibling.sync_all()?;
    drop(sibling);
    scratch.set_ownership(token.as_bytes())?;
    scratch.sync()?;
    Ok(scratch)
}

/// Recover using durable markers, optionally checking the still-retained
/// directory from the operation which created it.
pub(super) fn cleanup(
    root: &dyn ConfinedDir,
    id: &str,
    token: &str,
    retained: Option<&dyn ConfinedDir>,
) -> io::Result<()> {
    let (dir, marker) = names(id, token)?;
    let scratch = match root.open_dir(&dir) {
        Ok(dir) => Some(dir),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    if scratch.is_none() && retained.is_some() {
        return Err(mismatch());
    }
    let outer = match read_marker(root, &marker) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    if let Some(scratch) = scratch {
        let identity = scratch.identity()?;
        let inner = match scratch.ownership() {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(mismatch()),
            Err(e) => return Err(e),
        };
        if let Some(held) = retained
            && held.identity()? != identity
        {
            return Err(mismatch());
        }
        if outer.as_deref() != Some(ownership(token, identity).as_bytes())
            || inner != token.as_bytes()
        {
            return Err(mismatch());
        }
        absent_ok(scratch.remove_file(Path::new(IMAGE)))?;
        scratch.sync()?;
        // POSIX cannot combine this identity check with unlinkat. The caller
        // excludes concurrent same-UID directory renames during this interval.
        if root.open_dir(&dir)?.identity()? != identity {
            return Err(mismatch());
        }
        root.remove_empty_dir(&dir)?;
    } else if let Some(bytes) = &outer
        && !partial_marker(token, bytes)
    {
        return Err(mismatch());
    }
    root.sync()?;
    absent_ok(root.remove_file(&marker))?;
    root.sync()
}

pub(super) fn recover(
    conn: &rusqlite::Connection,
    fs: &dyn sb_types::fs::Fs,
    path: &Path,
) -> Result<(), crate::StoreError> {
    use rusqlite::OptionalExtension;
    let io_error = |e: io::Error| {
        crate::StoreError::unavailable_io(format!("verifier scratch I/O failed: {e}"), e.kind())
    };
    let current: String = conn
        .query_row(
            "SELECT value FROM store_meta WHERE key='incarnation'",
            [],
            |row| row.get(0),
        )
        .map_err(|e| crate::schema::classify(e, "read verifier incarnation"))?;
    let mut root = None;
    loop {
        let row: Option<(String, String)> = conn
            .query_row(
                "SELECT id, token FROM verifier_scratch WHERE incarnation=?1 ORDER BY id LIMIT 1",
                [&current],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|e| crate::schema::classify(e, "read verifier cleanup"))?;
        let Some((id, token)) = row else {
            return Ok(());
        };
        let root = match &mut root {
            Some(root) => root,
            slot @ None => slot.insert(fs.open_confined_dir(path).map_err(io_error)?),
        };
        cleanup(root.as_ref(), &id, &token, None).map_err(io_error)?;
        conn.execute("DELETE FROM verifier_scratch WHERE id=?1", [&id])
            .map_err(|e| crate::schema::classify(e, "finish verifier cleanup"))?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sb_test_support::{ScratchDir, real_fs};

    const ID: &str = "00000000-0000-4000-8000-000000000041";
    const TOKEN: &str = "00000000-0000-4000-8000-000000000042";

    #[test]
    fn rejects_empty_partial_and_copied_marker_substitutions() {
        for inner in [None, Some(&TOKEN[..8]), Some(TOKEN)] {
            let temp = ScratchDir::new("scratch-substitution");
            let root = real_fs().open_confined_dir(temp.path()).unwrap();
            let held = prepare(root.as_ref(), ID, TOKEN).unwrap();
            let (name, _) = names(ID, TOKEN).unwrap();
            let path = temp.path().join(&name);
            let moved = temp.path().join("owned-moved");
            std::fs::rename(&path, &moved).unwrap();
            std::fs::create_dir(&path).unwrap();
            if let Some(inner) = inner {
                root.open_dir(&name)
                    .unwrap()
                    .set_ownership(inner.as_bytes())
                    .unwrap();
            }
            assert!(cleanup(root.as_ref(), ID, TOKEN, Some(held.as_ref())).is_err());
            assert!(path.is_dir());
            if let Some(inner) = inner {
                assert_eq!(
                    root.open_dir(&name).unwrap().ownership().unwrap(),
                    inner.as_bytes()
                );
            }
            std::fs::remove_dir(&path).unwrap();
            std::fs::rename(&moved, &path).unwrap();
            cleanup(root.as_ref(), ID, TOKEN, None).unwrap();
            assert!(!path.exists());
        }
    }

    #[test]
    fn recovery_keeps_unproved_directories_and_removes_proved_ones() {
        let temp = ScratchDir::new("scratch-recovery");
        let root = real_fs().open_confined_dir(temp.path()).unwrap();
        let (name, marker) = names(ID, TOKEN).unwrap();
        std::fs::write(temp.path().join(&marker), &TOKEN[..8]).unwrap();
        std::fs::create_dir(temp.path().join(&name)).unwrap();
        assert!(cleanup(root.as_ref(), ID, TOKEN, None).is_err());
        assert!(temp.path().join(&name).is_dir());
        std::fs::remove_dir(temp.path().join(&name)).unwrap();
        assert!(cleanup(root.as_ref(), ID, TOKEN, None).is_err());
        std::fs::remove_file(temp.path().join(&marker)).unwrap();
        let held = prepare(root.as_ref(), ID, TOKEN).unwrap();
        let mut image = held.create(Path::new(IMAGE)).unwrap();
        image.write_all(b"owned").unwrap();
        image.sync_all().unwrap();
        drop(image);
        held.sync().unwrap();
        drop(held);
        cleanup(root.as_ref(), ID, TOKEN, None).unwrap();
        assert!(!temp.path().join(name).exists());
        assert!(!temp.path().join(marker).exists());
    }
    #[test]
    fn directory_removal_failure_is_retryable_after_power_cut() {
        use sb_test_support::{FaultFs, FaultKind, FsOp, SimFs};
        use sb_types::fs::Fs;
        use std::sync::Arc;
        let fs = Arc::new(SimFs::new());
        fs.create_dir_all(Path::new("/root")).unwrap();
        let root = fs.open_confined_dir(Path::new("/root")).unwrap();
        let held = prepare(root.as_ref(), ID, TOKEN).unwrap();
        let mut image = held.create(Path::new(IMAGE)).unwrap();
        image.write_all(b"owned").unwrap();
        image.sync_all().unwrap();
        drop(image);
        held.sync().unwrap();
        drop(held);
        fs.power_cut();
        let fault = FaultFs::new(fs.clone(), FsOp::RemoveDirAll, 1, FaultKind::Eio);
        let faulty_root = fault.open_confined_dir(Path::new("/root")).unwrap();
        assert!(cleanup(faulty_root.as_ref(), ID, TOKEN, None).is_err());
        fs.power_cut();
        cleanup(root.as_ref(), ID, TOKEN, None).unwrap();
        assert!(root.open_dir(&names(ID, TOKEN).unwrap().0).is_err());
    }

    #[test]
    fn missing_inner_marker_on_original_directory_fails_closed() {
        let temp = ScratchDir::new("scratch-missing-owner");
        let root = real_fs().open_confined_dir(temp.path()).unwrap();
        let (dir, marker) = names(ID, TOKEN).unwrap();
        root.create_private_dir(&dir).unwrap();
        let child = root.open_dir(&dir).unwrap();
        let mut sibling = root.create(&marker).unwrap();
        sibling
            .write_all(ownership(TOKEN, child.identity().unwrap()).as_bytes())
            .unwrap();
        sibling.sync_all().unwrap();
        assert!(cleanup(root.as_ref(), ID, TOKEN, None).is_err());
        assert!(temp.path().join(dir).is_dir());
    }
    #[test]
    fn recovery_keeps_unproved_and_inherited_rows() {
        use sb_test_support::SimFs;
        use sb_types::fs::Fs;
        let fs = SimFs::new();
        fs.create_dir_all(Path::new("/root")).unwrap();
        let root = fs.open_confined_dir(Path::new("/root")).unwrap();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE store_meta(key TEXT, value TEXT); INSERT INTO store_meta VALUES('incarnation', 'current'); CREATE TABLE verifier_scratch(id TEXT PRIMARY KEY, token TEXT, incarnation TEXT);").unwrap();
        conn.execute(
            "INSERT INTO verifier_scratch VALUES (?1,?2,'current')",
            [ID, TOKEN],
        )
        .unwrap();
        let (dir, _) = names(ID, TOKEN).unwrap();
        root.create_private_dir(&dir).unwrap();
        assert!(recover(&conn, &fs, Path::new("/root")).is_err());
        assert!(root.open_dir(&dir).is_ok());
        let count: i64 = conn
            .query_row("SELECT count(*) FROM verifier_scratch", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
        conn.execute("UPDATE verifier_scratch SET incarnation='inherited'", [])
            .unwrap();
        recover(&conn, &fs, Path::new("/root")).unwrap();
        assert!(root.open_dir(&dir).is_ok());
        let count: i64 = conn
            .query_row("SELECT count(*) FROM verifier_scratch", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn collision_recovery_preserves_unproved_markers_and_rows() {
        use sb_test_support::SimFs;
        use sb_types::fs::Fs;
        for bytes in [&b""[..], &TOKEN.as_bytes()[..8]] {
            let fs = SimFs::new();
            fs.create_dir_all(Path::new("/root")).unwrap();
            let root = fs.open_confined_dir(Path::new("/root")).unwrap();
            let (_, marker) = names(ID, TOKEN).unwrap();
            let mut file = root.create(&marker).unwrap();
            file.write_all(bytes).unwrap();
            drop(file);
            assert_eq!(
                prepare(root.as_ref(), ID, TOKEN).err().unwrap().kind(),
                io::ErrorKind::AlreadyExists
            );
            let conn = rusqlite::Connection::open_in_memory().unwrap();
            conn.execute_batch("CREATE TABLE store_meta(key TEXT, value TEXT); INSERT INTO store_meta VALUES('incarnation', 'current'); CREATE TABLE verifier_scratch(id TEXT PRIMARY KEY, token TEXT, incarnation TEXT);").unwrap();
            conn.execute(
                "INSERT INTO verifier_scratch VALUES (?1,?2,'current')",
                [ID, TOKEN],
            )
            .unwrap();
            assert!(recover(&conn, &fs, Path::new("/root")).is_err());
            assert_eq!(read_marker(root.as_ref(), &marker).unwrap(), bytes);
            assert_eq!(
                conn.query_row("SELECT count(*) FROM verifier_scratch", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                1
            );
        }
    }

    #[test]
    fn recovery_preserves_regular_file_at_directory_name() {
        use sb_test_support::SimFs;
        use sb_types::fs::Fs;
        let fs = SimFs::new();
        fs.create_dir_all(Path::new("/root")).unwrap();
        let root = fs.open_confined_dir(Path::new("/root")).unwrap();
        let (dir, marker) = names(ID, TOKEN).unwrap();
        root.create(&dir).unwrap().write_all(b"foreign").unwrap();
        root.create(&marker)
            .unwrap()
            .write_all(TOKEN.as_bytes())
            .unwrap();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE store_meta(key TEXT, value TEXT); INSERT INTO store_meta VALUES('incarnation', 'current'); CREATE TABLE verifier_scratch(id TEXT PRIMARY KEY, token TEXT, incarnation TEXT);").unwrap();
        conn.execute(
            "INSERT INTO verifier_scratch VALUES (?1,?2,'current')",
            [ID, TOKEN],
        )
        .unwrap();
        assert_eq!(
            root.open_dir(&dir).err().unwrap().kind(),
            io::ErrorKind::NotADirectory
        );
        assert!(recover(&conn, &fs, Path::new("/root")).is_err());
        assert_eq!(read_marker(root.as_ref(), &dir).unwrap(), b"foreign");
        assert_eq!(
            read_marker(root.as_ref(), &marker).unwrap(),
            TOKEN.as_bytes()
        );
        assert_eq!(
            conn.query_row("SELECT count(*) FROM verifier_scratch", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn power_cuts_keep_scratch_proof_or_fail_closed() {
        use sb_test_support::{FaultFs, FaultKind, FsOp, SimFs};
        use sb_types::fs::Fs;
        use std::sync::Arc;
        for op in [
            FsOp::Create,
            FsOp::CreatePrivateDir,
            FsOp::Open,
            FsOp::Write,
            FsOp::SyncFile,
            FsOp::SyncDir,
        ] {
            for nth in 1..=12 {
                let fs = Arc::new(SimFs::new());
                fs.create_dir_all(Path::new("/root")).unwrap();
                let fault = FaultFs::new(fs.clone(), op, nth, FaultKind::Eio);
                let Ok(root) = fault.open_confined_dir(Path::new("/root")) else {
                    continue;
                };
                let _ = prepare(root.as_ref(), ID, TOKEN);
                fs.power_cut();
                let root = fs.open_confined_dir(Path::new("/root")).unwrap();
                let (dir, _) = names(ID, TOKEN).unwrap();
                let before = root.open_dir(&dir).ok().map(|d| d.identity().unwrap());
                if cleanup(root.as_ref(), ID, TOKEN, None).is_err() {
                    assert_eq!(
                        root.open_dir(&dir).ok().map(|d| d.identity().unwrap()),
                        before,
                        "{op:?} {nth}"
                    );
                } else {
                    assert!(root.open_dir(&dir).is_err(), "{op:?} {nth}");
                }
            }
        }
    }
    #[test]
    #[ignore = "spawned by scratch_crash_cuts"]
    fn scratch_cut_child() {
        use sb_test_support::CutFs;
        use sb_types::fs::Fs;
        let nth = std::env::var("SB_SCRATCH_CUT_N").unwrap().parse().unwrap();
        let path = std::env::var("SB_SCRATCH_CUT_ROOT").unwrap();
        let fs = CutFs::new(real_fs(), nth);
        let root = fs.open_confined_dir(Path::new(&path)).unwrap();
        let scratch = prepare(root.as_ref(), ID, TOKEN).unwrap();
        let mut image = scratch.create(Path::new(IMAGE)).unwrap();
        image.write_all(b"image").unwrap();
        image.sync_all().unwrap();
        drop(image);
        scratch.sync().unwrap();
        cleanup(root.as_ref(), ID, TOKEN, Some(scratch.as_ref())).unwrap();
    }

    #[test]
    fn scratch_crash_cuts() {
        use sb_test_support::run_crash_cuts;
        let temp = ScratchDir::new("scratch-crash");
        let count = run_crash_cuts(
            "SB_SCRATCH_CUT_N",
            "SB_SCRATCH_CUT_ROOT",
            "backups::scratch::tests::scratch_cut_child",
            &mut |n| {
                let path = temp.path().join(format!("cut-{n}"));
                std::fs::create_dir(&path).unwrap();
                std::fs::write(path.join("sentinel"), b"untouched").unwrap();
                path
            },
            &mut |path| {
                let root = real_fs().open_confined_dir(path).unwrap();
                let (dir, _) = names(ID, TOKEN).unwrap();
                let before = root.open_dir(&dir).ok().map(|d| d.identity().unwrap());
                if cleanup(root.as_ref(), ID, TOKEN, None).is_err() {
                    assert_eq!(
                        root.open_dir(&dir).ok().map(|d| d.identity().unwrap()),
                        before
                    );
                } else {
                    assert!(root.open_dir(&dir).is_err());
                }
                assert_eq!(std::fs::read(path.join("sentinel")).unwrap(), b"untouched");
            },
        );
        assert!(count > 0);
    }
}
