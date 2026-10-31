//! The streaming capture writer (spec/09): a blocking `std::io::Write`
//! sink that turns an export payload byte stream into bounded,
//! Index-priority appends on the durable chunk ledger.

use std::io;

use sb_types::time::Clock;

use crate::StoreError;
use crate::writer::{WritePriority, Writer};

use super::{
    ExportJob, ExportJobState, MAX_EXPORT_CHUNK_BYTES, MAX_EXPORT_CHUNKS, append_export_chunks,
    export_job,
};

/// A blocking [`io::Write`] sink over an export job's capture ledger
/// (spec/09 "Streaming capture writer"). It splits the payload at
/// 65,536-byte boundaries — inside UTF-8 too — retains at most 30
/// chunks, and commits a full pending batch before taking more bytes,
/// so no whole encoded record is ever buffered. Borrowing the store's
/// writer and clock, the sink cannot outlive the store or its root
/// lease.
///
/// Every sink starts at ordinal zero: on a job that already captured
/// chunks the caller reproduces the same bytes and flush boundaries,
/// which each commit verifies; changed bytes or a shorter replay are
/// `Conflict`. [`finish`](Self::finish) commits the partial tail and
/// returns the job snapshot; neither it nor [`flush`](io::Write::flush)
/// seals the job. Call `finish` to complete the stream; Drop never flushes.
pub struct ExportChunkWriter<'a> {
    writer: &'a Writer,
    clock: &'a dyn Clock,
    /// The latest returned snapshot: the append's revision fence and
    /// the captured end a straddling batch splits at.
    job: ExportJob,
    /// The ordinal the buffered stream continues at; replayed slices
    /// advance it even though the ledger's end sits farther out.
    cursor: u64,
    /// Full chunks awaiting commit; `partial` never joins as a 31st.
    pending: Vec<Vec<u8>>,
    partial: Vec<u8>,
    /// The first failure, replayed by every later operation.
    failure: Option<StoreError>,
}

/// The refusal every entry point makes the writer's own thread: it
/// could only wait on its own queue.
const OWN_THREAD: &str = "the writer thread cannot append through its own queue";

impl<'a> ExportChunkWriter<'a> {
    /// Start a sink on a `capturing` job snapshot, cursor at ordinal
    /// zero even when the job already holds chunks.
    pub(crate) fn new(
        writer: &'a Writer,
        clock: &'a dyn Clock,
        job: &ExportJob,
    ) -> Result<Self, StoreError> {
        if writer.is_writer_thread() {
            return Err(StoreError::internal(OWN_THREAD));
        }
        if job.state() != ExportJobState::Capturing {
            return Err(StoreError::conflict("the export job is not capturing"));
        }
        Ok(Self {
            writer,
            clock,
            job: job.clone(),
            cursor: 0,
            pending: Vec::new(),
            partial: Vec::with_capacity(MAX_EXPORT_CHUNK_BYTES),
            failure: None,
        })
    }

    /// Commit the buffered tail and return the job snapshot once the
    /// stream covers the captured payload; a stream ending short of it
    /// is `Conflict`. Does not seal the job.
    pub fn finish(mut self) -> Result<ExportJob, StoreError> {
        if let Some(error) = self.refused() {
            return Err(self.remember(error));
        }
        self.commit().map_err(|error| self.remember(error))?;
        if self.cursor != self.job.captured_chunks() {
            return Err(self.remember(StoreError::conflict(
                "the export stream ended before the captured payload",
            )));
        }
        // Even an empty replay must fence cancellation or sealing since
        // construction. This queue turn reads but changes no rows.
        let id = self.job.id().to_owned();
        let revision = self.job.revision();
        let cursor = self.cursor;
        crate::store::await_write(self.writer.submit(WritePriority::Index, move |tx| {
            let job = export_job(tx, &id)?;
            if job.revision() != revision
                || job.state() != ExportJobState::Capturing
                || job.captured_chunks() != cursor
            {
                return Err(StoreError::conflict(
                    "the export stream changed before finish",
                ));
            }
            Ok(job)
        }))
        .map_err(|error| self.remember(error))
    }

    /// The remembered first failure, or the writer-thread refusal every
    /// entry point checks before touching the queue.
    fn refused(&self) -> Option<StoreError> {
        if let Some(failure) = &self.failure {
            return Some(failure.clone());
        }
        self.writer
            .is_writer_thread()
            .then(|| StoreError::internal(OWN_THREAD))
    }

    /// Keep the first failure: later operations replay it, not new ones.
    fn remember(&mut self, error: StoreError) -> StoreError {
        self.failure.get_or_insert(error).clone()
    }

    // Keep the typed cause through serde_json's I/O conversion.
    fn io_failure(&mut self, error: StoreError) -> io::Error {
        let error = self.remember(error);
        io::Error::new(error.io_kind().unwrap_or(io::ErrorKind::Other), error)
    }

    /// Commit every buffered chunk: a nonempty partial tail joins the
    /// batch, and a batch straddling the captured end splits so each
    /// submit is a pure replay or a pure append — the ledger accepts no
    /// mixed overlap.
    fn commit(&mut self) -> Result<(), StoreError> {
        if !self.partial.is_empty() {
            self.seal_partial();
        }
        while !self.pending.is_empty() {
            let captured = self.job.captured_chunks();
            let take = if self.cursor < captured {
                ((captured - self.cursor).min(self.pending.len() as u64)) as usize
            } else {
                self.pending.len()
            };
            let batch = self.pending.drain(..take).collect();
            self.append(batch)?;
        }
        Ok(())
    }

    fn seal_partial(&mut self) {
        let full = std::mem::replace(
            &mut self.partial,
            Vec::with_capacity(MAX_EXPORT_CHUNK_BYTES),
        );
        self.pending.push(full);
    }

    /// Queue one pure replay/append slice at Index priority and wait
    /// for its commit. The cursor counts the submitted slice, not the
    /// returned captured end — during a replay that end is farther out.
    fn append(&mut self, batch: Vec<Vec<u8>>) -> Result<(), StoreError> {
        let submitted = batch.len() as u64;
        let id = self.job.id().to_owned();
        let revision = self.job.revision();
        let from = self.cursor;
        let now = self.clock.now();
        let job = crate::store::await_write(self.writer.submit(WritePriority::Index, move |tx| {
            append_export_chunks(tx, &id, revision, from, &batch, now)
        }))?;
        self.cursor += submitted;
        self.job = job;
        Ok(())
    }
}

impl io::Write for ExportChunkWriter<'_> {
    /// Accept at most the current chunk's remaining room: `write_all`
    /// and serde_json loop over the short writes.
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Some(error) = self.refused() {
            return Err(self.io_failure(error));
        }
        if self.pending.len() >= MAX_EXPORT_CHUNKS
            && let Err(error) = self.commit()
        {
            return Err(self.io_failure(error));
        }
        let take = (MAX_EXPORT_CHUNK_BYTES - self.partial.len()).min(buf.len());
        self.partial.extend_from_slice(&buf[..take]);
        if self.partial.len() == MAX_EXPORT_CHUNK_BYTES {
            self.seal_partial();
        }
        Ok(take)
    }

    /// Commit buffered bytes, including a nonempty partial tail.
    fn flush(&mut self) -> io::Result<()> {
        if let Some(error) = self.refused() {
            return Err(self.io_failure(error));
        }
        match self.commit() {
            Ok(()) => Ok(()),
            Err(error) => Err(self.io_failure(error)),
        }
    }
}
