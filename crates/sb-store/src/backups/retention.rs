//! Interrupted-generation cleanup and retention (spec/03).

use std::collections::HashSet;
use std::io::Read;
use std::path::{Component, Path};

use super::*;

const HOUR: i64 = 3_600_000;
const DAY: i64 = 24 * HOUR;

#[derive(Debug)]
struct Generation {
    id: String,
    location: String,
    started: i64,
}

#[cfg(test)]
pub(super) fn cut(point: &str) {
    if std::env::var("SB_BACKUP_RECOVERY_CUT").as_deref() == Ok(point) {
        let _ = std::process::Command::new("/bin/kill")
            .args(["-9", &std::process::id().to_string()])
            .status();
        panic!("recovery cut failed");
    }
}

impl Backups {
    /// Failed rows inherited by restore are deliberately not cleanup authority.
    /// A marker records exactly which failed rows this process must finish.
    pub(super) fn recover(&self) -> Result<(), StoreError> {
        loop {
            let conn = open_connection(&self.state_db, Access::ReadOnly, READER_PRAGMAS)?;
            let next = conn.query_row(
                "SELECT g.generation_id, g.location, g.started_at, g.state FROM backup_generations g LEFT JOIN backup_cleanup_pending p ON p.generation_id=g.generation_id WHERE g.state='in_progress' OR p.generation_id IS NOT NULL ORDER BY g.started_at, g.generation_id LIMIT 1", [],
                |r| Ok((Generation { id: r.get(0)?, location: r.get(1)?, started: r.get(2)? }, r.get::<_, String>(3)?)),
            );
            let (generation, state) = match next {
                Ok(g) => g,
                Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(()),
                Err(e) => return Err(classify(e, "read interrupted backup")),
            };
            drop(conn);
            if state != "in_progress" && state != "failed" {
                return Err(StoreError::corrupt(
                    "backup cleanup marker belongs to a complete generation",
                ));
            }
            // Refuse bad metadata before even changing its state.
            checked_location(&generation)?;
            self.verify_ownership(&generation, true)?;
            #[cfg(test)]
            cut("before-mark");
            let id = generation.id.clone();
            await_write(self.writer.submit(WritePriority::Index, move |tx| {
                tx.execute("UPDATE backup_generations SET state='failed' WHERE generation_id=?1 AND state='in_progress'", [&id])
                    .map_err(|e| classify(e, "mark interrupted backup failed"))?;
                tx.execute("INSERT OR IGNORE INTO backup_cleanup_pending VALUES (?1)", [&id])
                    .map_err(|e| classify(e, "record backup cleanup"))?;
                #[cfg(test)]
                cut("inside-mark");
                Ok(())
            }))?;
            #[cfg(test)]
            cut("marked");
            self.remove_generation(&generation, true)?;
            #[cfg(test)]
            cut("removed");
            loop {
                let id = generation.id.clone();
                let removed = await_write(self.writer.submit(WritePriority::Index, move |tx| {
                    tx.execute("DELETE FROM object_pins WHERE rowid IN (SELECT rowid FROM object_pins WHERE pin_kind=?1 AND pin_ref=?2 LIMIT ?3)", params![PIN_KIND, id, PIN_BATCH as i64])
                        .map_err(|e| classify(e, "remove interrupted backup pins"))
                }))?;
                #[cfg(test)]
                cut("unpin");
                if removed == 0 {
                    break;
                }
            }
            #[cfg(test)]
            cut("before-finish");
            let id = generation.id;
            await_write(self.writer.submit(WritePriority::Index, move |tx| {
                tx.execute(
                    "DELETE FROM backup_cleanup_pending WHERE generation_id=?1",
                    [&id],
                )
                .map_err(|e| classify(e, "finish backup cleanup"))?;
                Ok(())
            }))?;
            #[cfg(test)]
            cut("finished");
        }
    }

    /// Prune one row at a time, after its directory entry is durable.
    pub(super) fn retain(&self) -> Result<(), StoreError> {
        let conn = open_connection(&self.state_db, Access::ReadOnly, READER_PRAGMAS)?;
        let mut stmt = conn.prepare("SELECT generation_id, location, started_at FROM backup_generations WHERE state='complete' ORDER BY started_at DESC, generation_id DESC")
            .map_err(|e| classify(e, "list backup generations for retention"))?;
        let generations = stmt
            .query_map([], |r| {
                Ok(Generation {
                    id: r.get(0)?,
                    location: r.get(1)?,
                    started: r.get(2)?,
                })
            })
            .map_err(|e| classify(e, "list backup generations for retention"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| classify(e, "read backup generation"))?;
        drop(stmt);
        drop(conn);
        let mut hours = HashSet::new();
        let mut days = HashSet::new();
        let mut weeks = HashSet::new();
        let mut expired = Vec::new();
        for generation in generations {
            let hour = generation.started.div_euclid(HOUR);
            let day = generation.started.div_euclid(DAY);
            // 1970-01-01 was Thursday; Monday is three days earlier.
            let week = (day + 3).div_euclid(7);
            let keep = bucket(&mut hours, hour, 24)
                | bucket(&mut days, day, 7)
                | bucket(&mut weeks, week, 4);
            if !keep {
                expired.push(generation);
            }
        }
        for generation in expired.into_iter().rev() {
            self.remove_generation(&generation, false)?;
            #[cfg(test)]
            cut("retention-removed");
            let id = generation.id;
            await_write(self.writer.submit(WritePriority::Index, move |tx| {
                tx.execute("DELETE FROM backup_ownership WHERE generation_id=?1", [&id])
                    .map_err(|e| classify(e, "remove backup ownership evidence"))?;
                tx.execute(
                    "DELETE FROM backup_generations WHERE generation_id=?1 AND state='complete'",
                    [&id],
                )
                .map_err(|e| classify(e, "remove expired backup row"))?;
                Ok(())
            }))?;
            #[cfg(test)]
            cut("retention-finished");
        }
        Ok(())
    }

    // The token is durable before directory creation. A partially created
    // directory may contain no entries or only a prefix of that token.
    // Anything else requires the complete marker, including old backups.
    fn verify_ownership(
        &self,
        generation: &Generation,
        incomplete: bool,
    ) -> Result<(), StoreError> {
        let path = checked_location(generation)?;
        let parent = path.parent().expect("checked parent");
        let failed = |e: io::Error| {
            StoreError::unavailable_io(
                format!("cannot inspect backup {}: {e}", path.display()),
                e.kind(),
            )
        };
        if self.fs.canonicalize(parent).map_err(failed)? != parent {
            return Err(StoreError::corrupt("backup destination parent changed"));
        }
        let Some(metadata) = self.fs.metadata(path).map_err(failed)? else {
            return Ok(());
        };
        if metadata.kind() != FileKind::Directory
            || self.fs.canonicalize(path).map_err(failed)? != path
        {
            return Err(StoreError::corrupt("backup generation directory changed"));
        }
        let entries = self.fs.read_dir(path).map_err(failed)?;
        if entries.is_empty() && incomplete {
            return Ok(());
        }
        let marker = path.join(OWNER_FILE);
        if !entries.contains(&marker) {
            return Err(StoreError::corrupt(
                "backup directory has no ownership marker",
            ));
        }
        let conn = open_connection(&self.state_db, Access::ReadOnly, READER_PRAGMAS)?;
        let token: Option<String> = match conn.query_row(
            "SELECT token FROM backup_ownership WHERE generation_id=?1",
            [&generation.id],
            |r| r.get(0),
        ) {
            Ok(token) => Some(token),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => return Err(classify(e, "read backup ownership")),
        };
        let token = token
            .filter(|t| t.parse::<Uuid>().is_ok())
            .ok_or_else(|| StoreError::corrupt("backup ownership token is absent or invalid"))?;
        if self.fs.canonicalize(&marker).map_err(failed)? != marker {
            return Err(StoreError::corrupt("backup ownership marker changed"));
        }
        let (mut file, _) = self.fs.open_regular(&marker).map_err(failed)?;
        let mut bytes = Vec::new();
        file.by_ref()
            .take(37)
            .read_to_end(&mut bytes)
            .map_err(failed)?;
        let exact = bytes == token.as_bytes();
        let partial = incomplete && entries.len() == 1 && token.as_bytes().starts_with(&bytes);
        if !exact && !partial {
            return Err(StoreError::corrupt(
                "backup ownership marker does not match",
            ));
        }
        Ok(())
    }

    fn remove_generation(
        &self,
        generation: &Generation,
        incomplete: bool,
    ) -> Result<(), StoreError> {
        self.verify_ownership(generation, incomplete)?;
        let path = checked_location(generation)?;
        let io_error = |e: io::Error| {
            StoreError::unavailable_io(
                format!("cannot remove backup generation {}: {e}", path.display()),
                e.kind(),
            )
        };
        self.fs.remove_dir_all(path).map_err(io_error)?;
        #[cfg(test)]
        cut("unlinked");
        self.fs
            .sync_dir(path.parent().expect("validated generation has parent"))
            .map_err(io_error)
    }
}

fn bucket(seen: &mut HashSet<i64>, value: i64, limit: usize) -> bool {
    if seen.contains(&value) {
        return false;
    }
    if seen.len() == limit {
        return false;
    }
    seen.insert(value);
    true
}

fn checked_location(generation: &Generation) -> Result<&Path, StoreError> {
    let path = Path::new(&generation.location);
    let valid_id = generation.id.parse::<Uuid>().is_ok();
    let valid_components = path
        .components()
        .all(|c| matches!(c, Component::RootDir | Component::Normal(_)));
    if !path.is_absolute()
        || !valid_components
        || !valid_id
        || path.file_name().and_then(|v| v.to_str()) != Some(&generation.id)
        || path.parent().is_none()
    {
        return Err(StoreError::corrupt("invalid backup generation location"));
    }
    Ok(path)
}
