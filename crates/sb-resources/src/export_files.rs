//! File output for a sealed export capture (spec/09 "Export file
//! output"): original acquisition bytes only, an owned destination,
//! one checkpoint per committed file, then the version-1 manifest.
//! EML only; mboxrd output lands in M1.7c2.

use std::io::{self, Read, Write};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};

use sb_platform::{Lease, LockError};
use sb_store::exports::{
    ExportFormat, ExportJob, ExportJobState, MAX_EXPORT_CHUNKS, advance_export_job, export_chunks,
    export_job,
};
use sb_store::{
    Clock, FileKind, Fs, ObjectId, ObjectStore, Scoped, Store, StoreError, WritePriority,
    await_write, read_error_as_store_error,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// The job's ownership marker inside a claimed destination.
const CLAIM_NAME: &str = "sweetbriar-export.claim";
/// The lease's stable lock file: created once by `lock_exclusive` and
/// never renamed, so publication cannot strand the held lock on an
/// unlinked inode and admit a second writer.
const LOCK_NAME: &str = "sweetbriar-export.lock";
/// The version-1 sidecar manifest (spec/09).
const MANIFEST_NAME: &str = "sweetbriar-metadata.json";
/// Uncommitted output carries this suffix; only a rename publishes.
const PARTIAL_SUFFIX: &str = ".partial";
/// Acquisition reads and output writes stream in bounded blocks.
const STREAM_CHUNK: usize = 64 * 1024;
/// The output checkpoint family (`{"v":1,"output":1,...}`).
const OUTPUT_VERSION: u64 = 1;
/// A claim marker holds a job id, bounded by the ledger's own rule.
const CLAIM_BYTES: usize = 128;

/// One sealed member of a planned output file.
struct Member {
    object: ObjectId,
}

/// One deterministic output file inside the destination.
struct FilePlan {
    name: String,
    members: Vec<Member>,
    bytes: u64,
}

/// The deterministic plan a sealed capture produces, replayed
/// identically after a crash: the ledger never changes once `ready`
/// seals it. `links` maps each record to (file, member); `None` marks
/// an unavailable body.
struct Plan {
    format: ExportFormat,
    destination: PathBuf,
    scope: Value,
    generation: u64,
    entries: u64,
    ids_sha256: String,
    files: Vec<FilePlan>,
    links: Vec<Option<(usize, usize)>>,
}

/// Pages the sealed chunk ledger into NDJSON lines with bounded
/// memory: a single record's tail can outgrow one 65,536-byte chunk.
struct LedgerReader<'a> {
    store: &'a Store,
    id: String,
    captured: u64,
    next: u64,
    tail: Vec<u8>,
}

impl<'a> LedgerReader<'a> {
    fn new(store: &'a Store, id: &str, captured: u64) -> Self {
        Self {
            store,
            id: id.to_owned(),
            captured,
            next: 0,
            tail: Vec::new(),
        }
    }

    fn line(&mut self) -> Result<Option<Vec<u8>>, StoreError> {
        loop {
            if let Some(at) = self.tail.iter().position(|byte| *byte == b'\n') {
                let mut rest = self.tail.split_off(at + 1);
                std::mem::swap(&mut self.tail, &mut rest);
                rest.pop();
                return Ok(Some(rest));
            }
            if self.next >= self.captured {
                if self.tail.is_empty() {
                    return Ok(None);
                }
                return Err(corrupt("the export capture ends mid-record"));
            }
            let id = self.id.clone();
            let from = self.next;
            let page = self
                .store
                .read(move |tx| export_chunks(tx, &id, from, MAX_EXPORT_CHUNKS as u32))?;
            if page.is_empty() {
                return Err(corrupt("the export capture ended before its sealed length"));
            }
            for chunk in page {
                self.next += 1;
                self.tail.extend_from_slice(&chunk);
            }
        }
    }
}

/// Drive the sealed job's file output to completion (spec/09).
pub(crate) fn run(
    store: &Store,
    fs: &dyn Fs,
    clock: &dyn Clock,
    id: &str,
    mut progress: impl FnMut(u64) -> ControlFlow<()>,
) -> Result<Scoped<ExportJob>, StoreError> {
    let mut job = store.read(|tx| export_job(tx, id))?;
    match job.state() {
        // A completed replay touches no output.
        ExportJobState::Completed => return Ok(Scoped::Done(job)),
        ExportJobState::Ready | ExportJobState::Running => {}
        _ => {
            return Err(StoreError::Conflict {
                detail: "the export job is not ready to write files".into(),
            });
        }
    }

    let captured = job.captured_chunks();
    let mut plan = Plan::read(&mut LedgerReader::new(store, id, captured), &job)?;
    let resuming = job.state() == ExportJobState::Running;
    // The destination lease stays held until the write ends, so a later
    // claim always meets either the published marker or the held lease.
    let _claim = ensure_claim(fs, &plan.destination, job.id(), resuming)?;
    if !resuming {
        job = advance(
            store,
            clock,
            &job,
            ExportJobState::Running,
            checkpoint(0, None),
        )?;
    }

    let done = resume_files(&job)? as usize;
    if done > plan.files.len() {
        return Err(corrupt(
            "the export file checkpoint exceeds the sealed plan",
        ));
    }

    let mut files_done = done as u64;
    if progress(files_done).is_break() {
        return Ok(Scoped::Stopped);
    }

    for (index, file) in plan.files.iter_mut().enumerate() {
        let committed = index < done;
        // A committed file is re-read, not trusted: damaged or missing
        // means rewritten with identical bytes.
        if !committed || !recover_file(fs, &plan.destination, file)? {
            let name = file.name.clone();
            let published = publish(fs, &plan.destination, &name, |out| {
                encode_file(store, out, file, &mut || progress(files_done))
            })?;
            let Scoped::Done(total) = published else {
                return Ok(Scoped::Stopped);
            };
            file.bytes = total;
            if !committed {
                let next = checkpoint(index as u64 + 1, None);
                job = advance(store, clock, &job, ExportJobState::Running, next)?;
                files_done += 1;
            }
        }
        if progress(files_done).is_break() {
            return Ok(Scoped::Stopped);
        }
    }

    let manifest = publish(fs, &plan.destination, MANIFEST_NAME, |out| {
        write_manifest(store, id, captured, &plan, out).map(Scoped::Done)
    })?;
    if matches!(manifest, Scoped::Stopped) {
        return Ok(Scoped::Stopped);
    }
    let manifest_bytes = fs
        .metadata(&plan.destination.join(MANIFEST_NAME))
        .map_err(unavailable("cannot describe the export manifest"))?
        .map(|meta| meta.len())
        .ok_or_else(|| corrupt("the export manifest vanished after publication"))?;
    let done_cp = checkpoint(plan.files.len() as u64, Some(manifest_bytes));
    let job = advance(store, clock, &job, ExportJobState::Completed, done_cp)?;
    Ok(Scoped::Done(job))
}

impl Plan {
    fn read(reader: &mut LedgerReader<'_>, job: &ExportJob) -> Result<Self, StoreError> {
        let line = reader
            .line()?
            .ok_or_else(|| corrupt("the export capture has no header"))?;
        let header = parse(&line)?;
        let intent = &header["intent"];
        let versioned = header["v"].as_u64() == Some(1)
            && header["kind"].as_str() == Some("capture")
            && intent["v"].as_u64() == Some(1);
        if !versioned {
            return Err(corrupt("the export capture has no version-1 header"));
        }
        // The request binds the intent's bytes: an edited capture is
        // corrupt, never re-scoped.
        let sealed = request_sha(job)?;
        let sealed_intent = serde_json::to_vec(intent)
            .map_err(|_| corrupt("the export capture holds an unserializable intent"))?;
        if hex(&Sha256::digest(&sealed_intent)) != sealed {
            return Err(corrupt("the export capture changed since sealing"));
        }
        let format = match str_field(intent, "format")? {
            "eml" => ExportFormat::Eml,
            // A sealed mboxrd capture is valid; its writer lands in
            // M1.7c2, so the job stays `ready` for that artifact.
            "mboxrd" => {
                return Err(StoreError::InvalidInput {
                    message: "mboxrd export files land in M1.7c2".into(),
                });
            }
            _ => return Err(corrupt("the export capture holds an unknown format")),
        };
        let destination = str_field(intent, "destination")?;
        if !Path::new(destination).is_absolute() {
            return Err(corrupt("the export capture holds no absolute destination"));
        }
        let destination = PathBuf::from(destination);

        let mut files: Vec<FilePlan> = Vec::new();
        let mut links = Vec::new();
        let mut digest = Sha256::new();
        let mut count = 0u64;
        let new_file = |files: &mut Vec<FilePlan>, name: String| {
            files.push(FilePlan {
                name,
                members: Vec::new(),
                bytes: 0,
            });
            files.len() - 1
        };
        while let Some(line) = reader.line()? {
            let record = parse(&line)?;
            let occurrence = i64_field(&record, "occurrence_id")?;
            digest.update(occurrence.to_be_bytes());
            let acquired = match str_field(&record, "body_availability")? {
                "acquired" => true,
                "not_acquired" | "missing" | "quarantined" => false,
                _ => return Err(corrupt("an export record holds unknown availability")),
            };
            let file_index = if acquired {
                Some(new_file(&mut files, format!("{occurrence}.eml")))
            } else {
                None
            };
            if !acquired {
                links.push(None);
                count += 1;
                continue;
            }
            let file_index = file_index.expect("an acquired record has a file");
            let acquisition = &record["acquisition"];
            let sha256 = str_field(acquisition, "sha256")?;
            let bytes = acquisition["bytes"]
                .as_u64()
                .ok_or_else(|| corrupt("an acquired record has no acquisition length"))?;
            let member = files[file_index].members.len();
            files[file_index].members.push(Member {
                object: ObjectId::from_hex(sha256, bytes)?,
            });
            links.push(Some((file_index, member)));
            count += 1;
        }
        let generation = u64_field(&header, "generation")?;
        let entries = u64_field(&header, "entries")?;
        let ids_sha256 = str_field(&header, "ids_sha256")?.to_owned();
        if count != entries || hex(&digest.finalize()) != ids_sha256 {
            return Err(corrupt("the export capture's membership proof fails"));
        }
        Ok(Self {
            format,
            destination,
            scope: intent["scope"].clone(),
            generation,
            entries,
            ids_sha256,
            files,
            links,
        })
    }
}

/// What the claim marker holds for the asking job.
enum Claim {
    /// No marker, or an empty one.
    Free,
    /// The marker holds this job's exact id.
    Own,
    /// Any other content: another id, an oversized marker or a
    /// non-regular entry.
    Foreign,
}

/// Ownership: a first claim creates the destination, or an existing
/// directory must be empty or already claimed by this job. A foreign
/// claim, foreign contents or a non-directory refuse, never an
/// overwrite. Every ownership decision happens under the destination's
/// exclusive lease, a non-blocking lock on `LOCK_NAME`, so two exports
/// cannot both observe an empty destination and publish; the returned
/// lease stays held by the caller until the write ends.
fn ensure_claim(
    fs: &dyn Fs,
    dir: &Path,
    job: &str,
    resuming: bool,
) -> Result<Box<dyn Lease>, StoreError> {
    fn refuse(detail: &str) -> StoreError {
        StoreError::InvalidInput {
            message: format!("{detail}: refuse to write into it"),
        }
    }
    /// Read the marker: `Free` for absent or empty content, `Own` for
    /// this job's exact id, `Foreign` for anything else.
    fn read_claim(fs: &dyn Fs, marker: &Path, job: &str) -> Result<Claim, StoreError> {
        let (mut file, meta) = match fs.open_regular(marker) {
            Ok(open) => open,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Claim::Free);
            }
            Err(error) if error.kind() == io::ErrorKind::InvalidInput => {
                return Ok(Claim::Foreign);
            }
            Err(error) => return Err(unavailable("cannot read the export claim")(error)),
        };
        if meta.len() > CLAIM_BYTES as u64 {
            return Ok(Claim::Foreign);
        }
        let mut content = Vec::with_capacity(meta.len() as usize);
        file.read_to_end(&mut content)
            .map_err(unavailable("cannot read the export claim"))?;
        Ok(match content.as_slice() {
            [] => Claim::Free,
            bytes if bytes == job.as_bytes() => Claim::Own,
            _ => Claim::Foreign,
        })
    }

    let marker = dir.join(CLAIM_NAME);
    let lock = dir.join(LOCK_NAME);
    let meta = fs
        .metadata(dir)
        .map_err(unavailable("cannot read the export destination"))?;
    if !resuming {
        match meta {
            // Create the destination for a first claim; a racer may
            // have created it first.
            None => match fs.create_dir(dir) {
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                result => result.map_err(unavailable("cannot create the export destination"))?,
            },
            Some(meta) if meta.kind() != FileKind::Directory => {
                return Err(refuse("the export destination is not a directory"));
            }
            // Refuse a confidently foreign destination before the
            // lease's placeholder can be created inside it.
            Some(_) => match read_claim(fs, &marker, job)? {
                Claim::Foreign => {
                    return Err(refuse("the destination is claimed by another export"));
                }
                Claim::Own => {}
                Claim::Free if destination_occupied(fs, dir)? => {
                    return Err(refuse("the destination holds unowned files"));
                }
                Claim::Free => {}
            },
        }
    }
    // Ownership decisions serialize on the destination's exclusive
    // lease: a second claim either meets the held lease or the
    // published marker, never an apparently empty directory. The lease
    // locks `LOCK_NAME`, not the marker: publishing the claim renames
    // over the marker path, which would strand the lock on an unlinked
    // inode and admit a second call for the same job.
    let lease = match fs.lock_exclusive(&lock) {
        Ok(lease) => lease,
        Err(LockError::Held) if resuming => {
            return Err(StoreError::Unavailable {
                reason: "the export destination is held by another export".into(),
                io_kind: None,
            });
        }
        Err(LockError::Held) => {
            return Err(refuse("the destination is claimed by another export"));
        }
        Err(LockError::Io(error)) => {
            return Err(unavailable("cannot lock the export destination")(error));
        }
    };
    // Under the lease every earlier observation is made again; only
    // this pass is authoritative.
    let meta = fs
        .metadata(dir)
        .map_err(unavailable("cannot read the export destination"))?;
    let claim = match &meta {
        Some(meta) if meta.kind() == FileKind::Directory => read_claim(fs, &marker, job)?,
        _ => Claim::Free,
    };
    if matches!(claim, Claim::Own) {
        // An accepted claim still owes its durability barriers: a retry
        // after a failed parent sync must not complete without them.
        let lease = hold_marker(fs, &marker, lease)?;
        sync_destination(fs, dir)?;
        return Ok(lease);
    }
    if resuming {
        return Err(StoreError::Unavailable {
            reason: "the export destination no longer holds this job's claim".into(),
            io_kind: None,
        });
    }
    match meta {
        None => {
            return Err(StoreError::Unavailable {
                reason: "the export destination vanished before it could be claimed".into(),
                io_kind: None,
            });
        }
        Some(meta) if meta.kind() != FileKind::Directory => {
            return Err(refuse("the export destination is not a directory"));
        }
        Some(_) => {
            if matches!(claim, Claim::Foreign) {
                return Err(refuse("the destination is claimed by another export"));
            }
            if destination_occupied(fs, dir)? {
                return Err(refuse("the destination holds unowned files"));
            }
        }
    }
    // Claim before any output via the publication protocol: a killed
    // write leaves a staging file, never a half-written claim.
    publish(fs, dir, CLAIM_NAME, |out| {
        out.write_all(job.as_bytes())
            .map_err(unavailable("cannot write the export claim"))?;
        Ok(Scoped::Done(()))
    })?;
    let lease = hold_marker(fs, &marker, lease)?;
    sync_parent(fs, dir)?;
    Ok(lease)
}

/// Fold the published claim marker into the held lease: `LOCK_NAME`
/// serializes entry while the marker carries the job id, and locking
/// the marker too keeps the claim itself reading held for the write's
/// whole duration. Called only after the marker's final rename, so the
/// lock rides the published inode; both leases drop with the write.
fn hold_marker(
    fs: &dyn Fs,
    marker: &Path,
    lease: Box<dyn Lease>,
) -> Result<Box<dyn Lease>, StoreError> {
    let marker_lease = match fs.lock_exclusive(marker) {
        Ok(held) => held,
        Err(LockError::Held) => {
            return Err(StoreError::Unavailable {
                reason: "the export claim is held by another export".into(),
                io_kind: None,
            });
        }
        Err(LockError::Io(error)) => {
            return Err(unavailable("cannot lock the export claim")(error));
        }
    };
    Ok(Box::new((lease, marker_lease)))
}

/// The only entries a claimable destination may hold: the marker, a
/// killed claim write's staging file and the lease's own lock file.
fn destination_occupied(fs: &dyn Fs, dir: &Path) -> Result<bool, StoreError> {
    let marker = dir.join(CLAIM_NAME);
    let claim_partial = dir.join(format!("{CLAIM_NAME}{PARTIAL_SUFFIX}"));
    let lock = dir.join(LOCK_NAME);
    let entries = fs
        .read_dir(dir)
        .map_err(unavailable("cannot list the export destination"))?;
    Ok(entries
        .iter()
        .any(|entry| entry != &marker && entry != &claim_partial && entry != &lock))
}

/// The durability barriers an accepted claim owes: the destination's
/// own entries, then the parent's record of the destination itself.
fn sync_destination(fs: &dyn Fs, dir: &Path) -> Result<(), StoreError> {
    fs.sync_dir(dir)
        .map_err(unavailable("cannot sync the export destination"))?;
    sync_parent(fs, dir)
}

/// The destination's entry in its parent: a missed parent flush is the
/// hole a power cut removes the whole claimed directory through.
fn sync_parent(fs: &dyn Fs, dir: &Path) -> Result<(), StoreError> {
    if let Some(parent) = dir.parent() {
        fs.sync_dir(parent)
            .map_err(unavailable("cannot sync the export destination's parent"))?;
    }
    Ok(())
}

/// Publication per spec/03: bounded `<name>.partial` write, file sync,
/// rename, directory sync. A stop or failure removes the partial; a crash leaves it for resume.
fn publish<T>(
    fs: &dyn Fs,
    dir: &Path,
    name: &str,
    write: impl FnOnce(&mut dyn Write) -> Result<Scoped<T>, StoreError>,
) -> Result<Scoped<T>, StoreError> {
    let tmp = dir.join(format!("{name}{PARTIAL_SUFFIX}"));
    match fs.remove(&tmp) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        result => result.map_err(unavailable("cannot remove a stale export partial"))?,
    }
    let mut file = fs
        .create(&tmp)
        .map_err(unavailable("cannot create an export file"))?;
    let outcome = write(&mut *file).and_then(|scoped| match scoped {
        Scoped::Done(done) => {
            file.sync_all()
                .map_err(unavailable("cannot flush an export file"))?;
            Ok(Scoped::Done(done))
        }
        Scoped::Stopped => Ok(Scoped::Stopped),
    });
    drop(file);
    let Scoped::Done(done) = outcome.inspect_err(|_| {
        let _ = fs.remove(&tmp); // best effort; the error stands
    })?
    else {
        let _ = fs.remove(&tmp);
        return Ok(Scoped::Stopped);
    };
    fs.rename(&tmp, &dir.join(name))
        .map_err(unavailable("cannot publish an export file"))?;
    fs.sync_dir(dir)
        .map_err(unavailable("cannot sync the export destination"))?;
    Ok(Scoped::Done(done))
}

/// Stream each member's verified object bytes into `out`: EML copies
/// raw acquisition bytes. `report` runs after each record; a stop
/// leaves the file uncommitted.
fn encode_file(
    store: &Store,
    out: &mut dyn Write,
    file: &mut FilePlan,
    report: &mut dyn FnMut() -> ControlFlow<()>,
) -> Result<Scoped<u64>, StoreError> {
    let mut total = 0u64;
    for member in &mut file.members {
        let mut object = store.objects().open(&member.object)?;
        let written = io::copy(&mut object, &mut *out).map_err(stream_error(
            "cannot copy an acquisition into an export file",
        ))?;
        if written != member.object.byte_length() {
            return Err(corrupt(
                "an acquisition's length differs from its frozen metadata",
            ));
        }
        total += written;
        if report().is_break() {
            return Ok(Scoped::Stopped);
        }
    }
    Ok(Scoped::Done(total))
}

/// A committed file must still be exactly what the plan produces:
/// matching bytes. `false` means republish; only real I/O failures
/// are errors.
fn recover_file(fs: &dyn Fs, dir: &Path, file: &mut FilePlan) -> Result<bool, StoreError> {
    let (mut handle, _meta) = match fs.open_regular(&dir.join(&file.name)) {
        Ok(opened) => opened,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(unavailable("cannot open a committed export file")(error));
        }
    };
    let [member] = file.members.as_slice() else {
        return Ok(false);
    };
    let mut hasher = Sha256::new();
    let mut buf = [0; STREAM_CHUNK];
    let mut seen = 0u64;
    loop {
        let read = handle
            .read(&mut buf)
            .map_err(unavailable("cannot read a committed export file"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
        seen += read as u64;
    }
    let digest: [u8; 32] = hasher.finalize().into();
    if seen != member.object.byte_length() || digest[..] != member.object.sha256() {
        return Ok(false);
    }
    file.bytes = seen;
    Ok(true)
}

/// The version-1 sidecar, streamed one record at a time: the frozen
/// metadata records pass through verbatim, plus each acquired record's
/// output placement. Unavailable entries carry `file: null`.
fn write_manifest(
    store: &Store,
    id: &str,
    captured: u64,
    plan: &Plan,
    out: &mut dyn Write,
) -> Result<(), StoreError> {
    let format = match plan.format {
        ExportFormat::Eml => "eml",
        ExportFormat::Mboxrd => "mboxrd",
    };
    let files: Vec<Value> = plan
        .files
        .iter()
        .map(|file| json!({"name": file.name, "bytes": file.bytes, "records": file.members.len()}))
        .collect();
    let head = json!({
        "v": 1,
        "kind": "export-manifest",
        "job": id,
        "format": format,
        "destination": plan.destination.to_string_lossy(),
        "scope": plan.scope,
        "generation": plan.generation,
        "entries": plan.entries,
        "ids_sha256": plan.ids_sha256,
        "files": files,
    })
    .to_string();
    out.write_all(head.strip_suffix('}').unwrap_or(&head).as_bytes())
        .and_then(|()| out.write_all(b",\"messages\":["))
        .map_err(unavailable("cannot write the export manifest"))?;

    let mut reader = LedgerReader::new(store, id, captured);
    let _ = reader.line()?; // the already-validated header
    for (ordinal, link) in plan.links.iter().enumerate() {
        let line = reader
            .line()?
            .ok_or_else(|| corrupt("the export capture ended inside the manifest pass"))?;
        let mut record = parse(&line)?;
        match link {
            None => record["file"] = Value::Null,
            &Some((file_index, _member_index)) => {
                let file = &plan.files[file_index];
                record["file"] = Value::String(file.name.clone());
            }
        }
        if ordinal > 0 {
            out.write_all(b",")
                .map_err(unavailable("cannot write the export manifest"))?;
        }
        serde_json::to_writer(&mut *out, &record).map_err(|error| StoreError::Unavailable {
            reason: format!("cannot write the export manifest: {error}"),
            // serde_json keeps the sink's native I/O kind (spec/09).
            io_kind: error.io_error_kind(),
        })?;
    }
    out.write_all(b"]}")
        .map_err(unavailable("cannot write the export manifest"))
}

/// The running checkpoint is a file count; `completed` adds the
/// published manifest's size.
fn checkpoint(files: u64, manifest_bytes: Option<u64>) -> String {
    match manifest_bytes {
        Some(bytes) => {
            json!({"v": 1, "output": OUTPUT_VERSION, "files": files, "manifest_bytes": bytes})
                .to_string()
        }
        None => json!({"v": 1, "output": OUTPUT_VERSION, "files": files}).to_string(),
    }
}

/// A job-held JSON document that must be a version-1 object.
fn job_json(raw: &str, what: &str) -> Result<Value, StoreError> {
    let value: Value = serde_json::from_str(raw)
        .map_err(|_| corrupt(format!("the export {what} is unreadable")))?;
    if value["v"].as_u64() != Some(1) {
        return Err(corrupt(format!("the export job holds an unknown {what}")));
    }
    Ok(value)
}

/// A resuming job's committed file count from its output checkpoint.
fn resume_files(job: &ExportJob) -> Result<u64, StoreError> {
    let checkpoint = job_json(job.checkpoint_json(), "output checkpoint")?;
    if checkpoint["output"].as_u64() != Some(OUTPUT_VERSION) {
        return Err(corrupt("the export job holds a foreign checkpoint"));
    }
    checkpoint["files"]
        .as_u64()
        .ok_or_else(|| corrupt("the export output checkpoint has no file count"))
}

/// The sealed request's intent digest: the capture plan binds to it.
fn request_sha(job: &ExportJob) -> Result<String, StoreError> {
    let request = job_json(job.request_json(), "request")?;
    if request["capture"].as_u64() != Some(1) {
        return Err(corrupt("the export job holds an unknown request"));
    }
    str_field(&request, "sha256").map(str::to_owned)
}

/// One fenced transition on the writer, awaited on the calling thread.
fn advance(
    store: &Store,
    clock: &dyn Clock,
    job: &ExportJob,
    next: ExportJobState,
    checkpoint: String,
) -> Result<ExportJob, StoreError> {
    let id = job.id().to_owned();
    let revision = job.revision();
    let now = clock.now();
    await_write(store.write(WritePriority::Index, move |tx| {
        advance_export_job(tx, &id, revision, next, &checkpoint, now)
    }))
}

fn parse(line: &[u8]) -> Result<Value, StoreError> {
    serde_json::from_slice(line).map_err(|_| corrupt("the export capture holds bad JSON"))
}

fn field<T>(value: &Value, key: &str, get: fn(&Value) -> Option<T>) -> Result<T, StoreError> {
    get(&value[key]).ok_or_else(|| corrupt(format!("the export capture holds no {key}")))
}

fn str_field<'a>(value: &'a Value, key: &str) -> Result<&'a str, StoreError> {
    value[key]
        .as_str()
        .ok_or_else(|| corrupt(format!("the export capture holds no {key}")))
}

fn u64_field(value: &Value, key: &str) -> Result<u64, StoreError> {
    field(value, key, Value::as_u64)
}

fn i64_field(value: &Value, key: &str) -> Result<i64, StoreError> {
    field(value, key, Value::as_i64)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn corrupt(detail: impl Into<String>) -> StoreError {
    StoreError::Corrupt {
        detail: detail.into(),
    }
}

/// `Unavailable` carrying the native kind; a message fixed at the call
/// site so the formatted error is never parsed back.
fn unavailable(message: &'static str) -> impl Fn(io::Error) -> StoreError {
    move |error| StoreError::Unavailable {
        reason: format!("{message}: {error}"),
        io_kind: Some(error.kind()),
    }
}

/// Reads through `ObjectReader` report `Corrupt`/`Unavailable` inside
/// `io::Error`; recover the typed cause before mapping the rest.
fn stream_error(message: &'static str) -> impl Fn(io::Error) -> StoreError {
    move |error| {
        read_error_as_store_error(&error)
            .cloned()
            .unwrap_or_else(|| unavailable(message)(error))
    }
}
