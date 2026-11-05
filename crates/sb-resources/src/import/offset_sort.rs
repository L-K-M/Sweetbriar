//! Bounded external sort of offsets into a separately owned source.
use std::cmp::Ordering;
use std::io::{self, SeekFrom};

use sb_store::{ValidationScratch, ValidationTape};

const RUN: usize = 256;

fn scratch_error(error: sb_store::StoreError) -> io::Error {
    io::Error::new(error.io_kind().unwrap_or(io::ErrorKind::Other), error)
}

fn put(writer: &mut dyn sb_store::WriteFile, value: u64) -> io::Result<()> {
    writer.write_all(&value.to_le_bytes())
}

fn get(reader: &mut dyn sb_store::ReadFile, index: u64) -> io::Result<u64> {
    let position = index
        .checked_mul(8)
        .ok_or_else(|| io::Error::other("offset tape too large"))?;
    reader.seek(SeekFrom::Start(position))?;
    let mut bytes = [0; 8];
    reader.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

/// The caller supplies exact, fallible comparison and phase cancellation.
pub(super) struct OffsetSort<'a, 's> {
    scratch: &'a ValidationScratch<'s>,
    writer: Box<dyn sb_store::WriteFile>,
    run: Vec<u64>,
    count: u64,
}

impl<'a, 's> OffsetSort<'a, 's> {
    pub(super) fn new(scratch: &'a ValidationScratch<'s>) -> io::Result<Self> {
        Ok(Self {
            scratch,
            writer: scratch.create(ValidationTape::A).map_err(scratch_error)?,
            run: Vec::with_capacity(RUN),
            count: 0,
        })
    }

    pub(super) fn push<C>(
        &mut self,
        value: u64,
        context: &mut C,
        compare: &mut impl FnMut(&mut C, u64, u64) -> io::Result<Ordering>,
    ) -> io::Result<()> {
        self.run.push(value);
        self.count = self
            .count
            .checked_add(1)
            .ok_or_else(|| io::Error::other("too many offsets"))?;
        if self.run.len() == RUN {
            self.flush(context, compare)?;
        }
        Ok(())
    }

    fn flush<C>(
        &mut self,
        context: &mut C,
        compare: &mut impl FnMut(&mut C, u64, u64) -> io::Result<Ordering>,
    ) -> io::Result<()> {
        // Insertion sort permits fallible comparisons without storing keys.
        for i in 1..self.run.len() {
            let mut j = i;
            while j > 0 && compare(context, self.run[j - 1], self.run[j])? == Ordering::Greater {
                self.run.swap(j - 1, j);
                j -= 1;
            }
        }
        for &value in &self.run {
            put(&mut *self.writer, value)?;
        }
        self.run.clear();
        Ok(())
    }

    pub(super) fn finish<C>(
        mut self,
        context: &mut C,
        compare: &mut impl FnMut(&mut C, u64, u64) -> io::Result<Ordering>,
        check: &mut impl FnMut(&mut C) -> io::Result<()>,
        mut visit: impl FnMut(&mut C, u64) -> io::Result<()>,
    ) -> io::Result<()> {
        self.flush(context, compare)?;
        self.writer.sync_all()?;
        drop(self.writer);
        let mut source = ValidationTape::A;
        let mut destination = ValidationTape::B;
        let mut width = RUN as u64;
        while width < self.count {
            check(context)?;
            let mut reader = self.scratch.open(source).map_err(scratch_error)?;
            let mut writer = self.scratch.create(destination).map_err(scratch_error)?;
            let mut start = 0;
            while start < self.count {
                check(context)?;
                let mid = start.saturating_add(width).min(self.count);
                let end = mid.saturating_add(width).min(self.count);
                let (mut left, mut right) = (start, mid);
                let (mut a, mut b) = (None, None);
                while left < mid || right < end {
                    if (left - start + right - mid) % RUN as u64 == 0 {
                        check(context)?;
                    }
                    if a.is_none() && left < mid {
                        a = Some(get(&mut *reader, left)?);
                    }
                    if b.is_none() && right < end {
                        b = Some(get(&mut *reader, right)?);
                    }
                    let take_left = match (a, b) {
                        (Some(x), Some(y)) => compare(context, x, y)? != Ordering::Greater,
                        (Some(_), None) => true,
                        _ => false,
                    };
                    if take_left {
                        put(&mut *writer, a.take().unwrap())?;
                        left += 1;
                    } else {
                        put(&mut *writer, b.take().unwrap())?;
                        right += 1;
                    }
                }
                start = end;
            }
            writer.sync_all()?;
            drop(writer);
            drop(reader);
            self.scratch.remove(source).map_err(scratch_error)?;
            std::mem::swap(&mut source, &mut destination);
            width = width.saturating_mul(2);
        }
        check(context)?;
        let mut reader = self.scratch.open(source).map_err(scratch_error)?;
        for index in 0..self.count {
            if index % RUN as u64 == 0 {
                check(context)?;
            }
            visit(context, get(&mut *reader, index)?)?;
        }
        drop(reader);
        self.scratch.remove(source).map_err(scratch_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sb_test_support::{FaultFs, FaultKind, FsOp, ScratchDir, real_fs};
    use std::sync::Arc;

    #[test]
    fn tape_read_write_and_sync_faults_are_reported_and_cleaned() {
        for op in [FsOp::Read, FsOp::Write, FsOp::SyncFile] {
            let tmp = ScratchDir::new("offset-fault");
            let store = tmp.path().join("store");
            drop(crate::Resources::open(&store).unwrap());
            let probe = Arc::new(FaultFs::new(real_fs(), op, u64::MAX, FaultKind::Eio));
            let resources =
                crate::Resources::open_on(&store, probe.clone(), Arc::new(crate::SystemClock))
                    .unwrap();
            let opening_calls = probe.calls(op);
            drop(resources);
            let fs = Arc::new(FaultFs::new(
                real_fs(),
                op,
                opening_calls + 1,
                FaultKind::Eio,
            ));
            let resources =
                crate::Resources::open_on(&store, fs.clone(), Arc::new(crate::SystemClock))
                    .unwrap();
            assert_eq!(fs.calls(op), opening_calls);
            let scratch = resources.store.import_validation_scratch().unwrap();
            let result = (|| {
                let mut sorter = OffsetSort::new(&scratch)?;
                let mut compare = |_: &mut (), a: u64, b: u64| Ok(a.cmp(&b));
                for i in 0..257 {
                    sorter.push(i, &mut (), &mut compare)?;
                }
                sorter.finish(&mut (), &mut compare, &mut |_| Ok(()), |_, _| Ok(()))
            })();
            assert!(result.is_err(), "{op:?}");
            assert!(fs.calls(op) > opening_calls);
            drop(scratch);
            let root = store.join("objects/tmp/import-validation");
            assert_eq!(std::fs::read_dir(root).unwrap().count(), 0);
        }
    }

    #[test]
    fn sorts_boundary_runs_and_cleans_up() {
        let tmp = ScratchDir::new("offset-sort");
        let resources = crate::Resources::open(&tmp.path().join("store")).unwrap();
        let root = tmp.path().join("store/objects/tmp/import-validation");
        for count in [0, 1, 255, 256, 257, 777] {
            let scratch = resources.store.import_validation_scratch().unwrap();
            let mut sorter = OffsetSort::new(&scratch).unwrap();
            let mut compare = |_: &mut (), a: u64, b: u64| Ok(a.cmp(&b));
            for i in (0..count).rev() {
                sorter.push(i % 301, &mut (), &mut compare).unwrap();
            }
            let mut output = Vec::new();
            sorter
                .finish(&mut (), &mut compare, &mut |_| Ok(()), |_, value| {
                    output.push(value);
                    Ok(())
                })
                .unwrap();
            let mut expected = (0..count).map(|i| i % 301).collect::<Vec<_>>();
            expected.sort_unstable();
            assert_eq!(output, expected);
            scratch.finish().unwrap();
            assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        }
    }

    #[test]
    fn cancellation_and_comparison_failure_remove_only_their_lease() {
        let tmp = ScratchDir::new("offset-stop");
        let resources = crate::Resources::open(&tmp.path().join("store")).unwrap();
        let root = tmp.path().join("store/objects/tmp/import-validation");
        let held = resources.store.import_validation_scratch().unwrap();
        held.create(ValidationTape::A).unwrap();
        for cancel in [false, true] {
            let scratch = resources.store.import_validation_scratch().unwrap();
            let mut sorter = OffsetSort::new(&scratch).unwrap();
            let mut compare = |_: &mut (), a: u64, b: u64| Ok(a.cmp(&b));
            for i in 0..257 {
                sorter.push(i, &mut (), &mut compare).unwrap();
            }
            let result = sorter.finish(
                &mut (),
                &mut |_, a, b| {
                    if cancel {
                        Ok(a.cmp(&b))
                    } else {
                        Err(io::Error::other("comparison failed"))
                    }
                },
                &mut |_| {
                    if cancel {
                        Err(io::Error::other("cancelled"))
                    } else {
                        Ok(())
                    }
                },
                |_, _| Ok(()),
            );
            assert!(result.is_err());
            drop(scratch);
            assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
            assert!(held.open(ValidationTape::A).is_ok());
        }
        held.finish().unwrap();
        assert_eq!(std::fs::read_dir(root).unwrap().count(), 0);
    }
}
