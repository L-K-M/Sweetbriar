# 09 — Import and export

Rests on research [04-data-safety](../research/04-data-safety.md) and
[14-system-accounts](../research/14-system-accounts.md) (macOS 27
default-denies direct access to Apple Mail's data).

## Import

**Sources in the pilot:**

| Source | Format |
|---|---|
| Apple Mail export | A `.mbox` package: a directory containing an `mbox` file, plus `Info.plist` and table-of-contents files that are ignored |
| Plain mbox files | mboxrd and mboxo (the `From ` escaping variants) |
| EML files | Single `.eml` files or folders of them |

Direct reading of `~/Library/Mail` is *later*, and only through a
user-granted permission path.

**Procedure** (one import job):

1. **Inventory.** Walk the chosen files and count candidate messages
   (mbox `From ` separators, `.eml` files) and bytes. The UI shows the
   inventory before anything is imported.
   - Visit chosen roots in request order and directory entries in sorted
     order. A `.mbox` directory contributes only its `mbox` file; ignore
     its sidecars. Other directories are walked recursively for `.eml`
     files, `.mbox` files and packages. Extensions are case-insensitive.
     An explicitly chosen regular file without `.eml` is an mbox candidate.
   - Resolve symlinks and visit each canonical directory and source file
     once, including overlapping roots. Report inaccessible paths,
     unsupported explicitly chosen entries and non-UTF-8 paths visibly,
     without treating them as empty. One path's failure does not discard
     other discovered sources.
   - File/package mailbox names use the stem; EML files discovered in a
     folder share that folder's name. Nested folders use their own names.
     Empty mbox files are valid inventories with zero messages.
   - Capture each regular source into a private durable snapshot (03)
     before counting. Its token, SHA-256 and byte length identify the copy;
     offsets, inventory and ingest all refer to that same captured stream.
     Later changes to the original cannot change a running job's bytes.
   - Resume verifies and reopens the private copy. A missing or corrupt
     snapshot stops the job visibly; it never substitutes the original
     path's current contents.
   - Source inventory metadata is a version-1 JSON object containing the
     canonical path, mailbox name, source kind, snapshot reference, message
     count and mbox quoting requirement. Reconstructing it validates the
     version and required fields. Variant choices belong to checkpoints,
     not the immutable inventory. Cancellation is checked during discovery,
     copying and scan reads, including within a long mbox line.
2. **Ingest.** For each message, in source order:
   - extract the exact bytes, undoing the variant's `From ` quoting:
     mboxrd removes one `>` from every `>+From ` line; mboxo only turns
     `>From ` into `From `. A `>>From ` line decodes differently in each,
     so when the variant is unknown and such lines exist, the job asks
     the user, records the choice with its checkpoint, and the report
     names it;
   - publish the bytes (03), streaming them. Parsing and flag extraction
     see at most the MIME byte budget plus one byte (10), so a larger
     message keeps its exact bytes and imports as `partial`. A read
     failure names the source offset and stops the job; a truncated
     message is never stored;
   - insert or reuse the acquisition, add an `acquisition_sources` row
     (`import_mbox` or `import_eml`, with source path and offset), create
     a logical message and an occurrence in the local archive, in a
     mailbox named after the source file or folder, and enqueue it for
     search.
   - Batches are at most 250 messages per transaction. The job records
     its progress (file and offset), so it can resume after a crash or
     quit without duplicating.
   - A source's progress is its next unread offset plus the messages
     consumed. An mbox resumes at a recorded separator; an EML file is
     one message at offset 0. Ending with a count other than the
     inventory's stops the job as corrupt.
3. **Duplicates.**
   - Bytes are deduplicated: identical bytes share one acquisition and
     one object.
   - **Messages are not.** Every source message gets its own occurrence,
     even when two identical messages sit in the same mailbox, so
     multiplicity survives export.
   - **Re-import and resume** are idempotent by **source identity**:
     the source file's path and SHA-256, the target mailbox and the
     message's offset (for EML, the file's path and hash), recorded in
     `acquisition_sources.source_ref`. A source message already imported
     isn't imported again. Two byte-identical mbox files under different
     names are two sources, and both are imported.
   - Byte duplicates are counted in the report.
4. **Flags.** Read state comes from `Status` and `X-Status` headers where
   present. Otherwise imported messages are marked read: an archive isn't
   new mail. The report counts each case.
   - Header names are case-insensitive; flag letters are the ASCII codes:
     `R` means read, and `X-Status` uses `F` (flagged), `A` (answered),
     `D` (deleted) and `T` (draft). A present `Status` or `X-Status` header
     without `R` anywhere means unread. Repeated/folded values combine
     their flag letters; unrecognized letters are ignored.
   - Extraction obeys the MIME header byte/field limits. Known admitted
     flags survive a limit breach and still apply. With no admitted status
     header, apply the archive's default read state. Report incomplete
     extraction separately, not as proof that no status headers existed.
5. **Rejects.** Unparseable messages are still stored as acquisitions,
   since bytes are kept even when parsing fails, and flagged
   `parse_state='failed'`. Unreadable regions of a source file are
   logged with offsets.
6. **Report.** At the end, `import_report.json` lists:
   - files;
   - messages found, imported, duplicates, rejected;
   - bytes in and bytes stored;
   - the SHA-256 of each source file;
   - warnings (metadata the format can't carry: Apple Mail flags, colors,
     mailbox structure beyond the file name).

   It is shown in the Import report window and saved in `recovery/` as
   `import-report-<job id>.json` (03), a version-1 JSON object. It also
   names each file's mailbox, kind, byte count, message count, variant
   and final position, the read-state counts (from headers, archive
   default, incomplete header extraction), the problems and any failure.
   - A job that finishes first loses its completed or cancelled
     snapshots, then gets its report; the report marks that cleanup done.
     At open, each finished job without a report gets both steps again.
7. **Sources are never modified or removed.**

**Job lifecycle** (02 `import_jobs` states):

- `start_import(paths, target)` records the request and inventories in
  the background, in `inventory`. The pilot's only target is the local
  archive.
- A finished inventory records its sources in order and waits in
  `awaiting_confirmation`; nothing is ingested before `confirm_import`.
  Its problems stay with the job: all counted, the first 50 listed.
- `confirm_import(job_id, variants)` chooses `mboxo` or `mboxrd` for
  exactly the `Ambiguous` sources, by inventory ordinal. A missing, extra
  or repeated choice is `InvalidInput`. The job becomes `running`.
- Ingest takes sources in ordinal order. It ends `completed` once every
  source's position equals its inventory, or `failed`, checkpoints
  intact, when a source fails: a missing or corrupt snapshot, an
  unreadable region or a count mismatch.
- `cancel_job(id)` makes the job `cancelled` before it returns, so the
  cancellation survives a crash. An inventory stops at its next check
  and ingest at its next message; the uncommitted batch in flight is
  discarded, while committed batches stay imported. A finished job is
  `Conflict`.
- Shutdown pauses ingest at its next message, committing the batch in
  flight; the job stays `running`. At open, an interrupted inventory becomes `failed` (the user
  starts a new one), a waiting job keeps waiting and a running job
  resumes from its checkpoints.
- Shells poll `import_job(id)` (07) for the state, inventory, problems
  and counts, and, once the report is written, its path; a terminal
  state alone doesn't mean the report exists. `import_report(id)`
  returns the report itself once written.
- **In the app** (08), File → Import… picks supported files, packages
  and folders. The inventory and its problems are shown before
  confirmation, and an ambiguous source needs its own variant choice.
  Progress is polled with at most one request in flight. Closing the
  progress sheet leaves the job running; Cancel waits for `cancel_job`'s
  receipt. The shell keeps its active job's id and reattaches after a
  restart. A failed or cancelled job says so, and its committed messages
  stay visible. A report that isn't written yet shows as pending, with
  Retry; it is never shown as written.
- `sb-cli import [--variant mboxo|mboxrd] <paths>…` prints the
  inventory, then confirms with that variant for every ambiguous source.
  When one needs a choice and none was given, it cancels the job and
  says so. It waits, prints the totals and the report's path, and exits
  0 only for a completed job.
- A source checkpoint is `{"v":1,"offset":…,"messages":…}`. The job
  checkpoint holds the problems, the confirmed variant choices, the next
  unfinished source and the running totals.

**Acceptance:** importing a fixture set (09 fixtures in 11-testing)
reproduces exact byte hashes for every message, survives `kill -9` at any
point and resumes with identical final counts.

## Export

### Frozen metadata projection (M1.7b2b1)

`export_occurrence_metadata` reads one retained occurrence in one short
transaction. It returns an immutable, owned snapshot of its message,
account/mailbox names, all five flags, keywords, tags, note and selected
acquisition identity/availability. It reads no bodies, headers, rendition
files or other occurrences. Unknown IDs are `NotFound`; malformed stored
flags, states, keywords or acquisition identities are `Corrupt`.

The snapshot serializes deterministically through a streaming JSON writer,
retaining at most one target's metadata, without constructing a serialized
whole-corpus buffer. Notes and tag collections are never clipped. Field
order is fixed for this capture version; tags sort by name then ID.
Keywords remain the stored sorted, duplicate-free string array. Values
remain frozen if source rows change after the snapshot.

Capture format 1 has this shape (synthetic values):

```json
{
  "v": 1,
  "occurrence_id": 1,
  "message_id": 1,
  "account": {"id": "00000000-0000-4000-8000-000000000001", "name": "Archive"},
  "mailbox": {"id": 1, "name": "Inbox"},
  "occurrence_state": "present",
  "flags": {"seen": true, "flagged": false, "answered": false, "draft": false, "deleted": false},
  "keywords": [],
  "acquisition": {
    "id": 1,
    "sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    "bytes": 0
  },
  "body_availability": "acquired",
  "tags": [],
  "note": null
}
```

Tags contain `id`, `name` and nullable `color_srgb`. Acquisition is null
with `not_acquired` availability when no body is recorded; other recorded
availability values are `acquired`, `missing` and `quarantined`. These are
database facts, not a claim that object bytes have been verified. File
output verifies the frozen hash and length in M1.7c. Retained vanished,
epoch-stale and superseded occurrences can be projected without expanding
them into other mailboxes.

This query introduces no scope, command, receipt or durable job. Complete
four-scope capture, including real selection handles and their proof,
remains M1.7b2b2; its ledger must be sealed before a scope is acknowledged.
### Durable capture ledger (M1.7b2a)

Scope capture and file output are separate. Before output, an export keeps
its request and immutable metadata in the database. The capture payload is
an ordered byte stream, split into chunks of at most 65,536 bytes, so a
large note or selection need not fit one writer transaction. The complete
scope adapter defines the payload grammar; the ledger exposes no scope API.

- `export_jobs` keeps a version-1 request object, state, revision,
  captured chunk count, version-1 checkpoint object and timestamps.
  Requests and checkpoints are each at most 4,096 UTF-8 bytes; large
  membership and metadata belong in chunks, not these objects.
- States are `capturing`, `ready`, `running`, `completed`, `cancelled`
  and `failed`. Only `capturing` admits payload writes. Sealing moves to
  `ready`; execution moves to `running`, then `completed`. Cancellation
  or failure may end any unfinished state. Terminal jobs are immutable.
  Sealing does not claim any output file or manifest exists.
- Chunk ordinals start at zero and are contiguous. A new append requires
  the job's current revision and next ordinal. Replaying an already
  committed range succeeds only when every byte matches, without changing
  its revision, checkpoint or metadata. A changed or partial-overlap replay
  is `Conflict`. An empty append or an ordinal gap is `InvalidInput`.
- Each append has at most 30 chunks and conservatively charges at most
  2,097,152 changed row/index/framing bytes, including old/new job images.
  A rejected append writes nothing. Checkpoint advancement uses the same
  revision fence and stores no more than its admitted JSON byte bound.
- Reads page at most 30 chunks, only through the captured end. Job IDs
  are nonempty and at most 128 UTF-8 bytes. The same ID cannot name an
  import, Search rebuild and export, including SQL key updates.
- Repeating creation with an identical canonical request returns the kept
  job; another request or kind is `Conflict`. Payloads survive reopen.
  The later scope adapter must finish capture before acknowledging a
  durable export scope; an interrupted partial capture is not output-ready.

### Streaming capture writer (M1.7b2a2)

`export_chunk_writer` is a blocking native `std::io::Write` sink, through
Resources. Construct it from a capturing job snapshot; it borrows the
open store, so it cannot outlive the store's ownership lease. It retains
at most 30 chunks (1,966,080 payload bytes), never a whole encoded record
or scope. It splits bytes at 65,536 bytes, including inside UTF-8, and queues
bounded Index-priority appends. Callers release read transactions before
writing; the sink holds none. Construction, writes, flush and finish
refuse the writer's own thread.

Each new sink replays from ordinal zero at the snapshot's revision fence.
Callers reproduce the same bytes and flush boundaries; changed bytes or
concurrent mutations are `Conflict`. A batch crossing the kept end verifies
its prefix before appending its suffix. A short replay cannot finish.

`flush` commits buffered bytes, including a partial final chunk. `finish`
consumes the sink, flushes and verifies the complete stream, then returns
its job snapshot. Neither operation seals the scope or marks it ready.
An empty new stream writes nothing. Drop never flushes or claims success.

An I/O wrapper retains the original `StoreError`, including environmental
kinds; JSON serialization must not flatten it into `Internal`. The sink
remembers its first failure and refuses reuse with that failure. Partial
commits remain immutable and pre-acknowledgement until complete capture
seals them. This artifact introduces no scope API, scheduler or file I/O.

### Export formats and scopes

- **Formats:** EML (one file per message) and mboxrd (one file per
  mailbox), from any scope: account, mailbox, search or selection.
- **Bytes:** exported bytes are the original acquisition bytes, never a
  re-serialization.
  - mboxrd quotes every line beginning with zero or more `>` followed by
    `From ` by adding one `>`. Preserve all other bytes, including line
    endings. Each record starts with the conventional fixed envelope
    `From MAILER-DAEMON Thu Jan  1 00:00:00 1970\n`. Its timestamp is
    transport framing, not message metadata.
  - A nonempty acquisition without a final LF needs one framing LF so
    the next envelope starts on a new line. Append exactly one, even
    for the last record, and record the original byte length and added
    framing byte in the manifest. Empty acquisitions need no padding.
  - A manifest-aware import must validate record boundaries, framing,
    original byte length and SHA-256 before publishing the original
    acquisition bytes. Only the declared framing LF may be removed.
    A plain mbox import without the manifest retains that physical LF;
    its original presence cannot be inferred. EML needs no framing.
  - The transport encoder streams with bounded buffers, including long
    lines and runs of `>`. It reports original and encoded byte counts
    and framing only after successful output. File durability and
    publication belong to the export job.
- **Metadata:** a `sweetbriar-metadata.json` manifest is written
  alongside, holding per message the acquisition SHA-256, account,
  mailbox, flags, tags and notes, so private metadata survives leaving
  Sweetbriar.
- **Incomplete scopes:** messages whose bodies aren't acquired are
  listed in the manifest as `not_acquired`, never silently skipped.
- **Offboarding:** the export manifest format is versioned and documented
  in the repository. Together with the store layout (03) and schema (02),
  it lets an independent tool read a Sweetbriar store without the app
  (research debate/04, resolution change 18). The tool itself is *later*.
