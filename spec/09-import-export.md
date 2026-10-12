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
2. **Ingest.** For each message, in source order:
   - extract the exact bytes, undoing the variant's `From ` quoting:
     mboxrd removes one `>` from every `>+From ` line; mboxo only turns
     `>From ` into `From `. A `>>From ` line decodes differently in each,
     so when the variant is unknown and such lines exist, the job asks
     the user, records the choice with its checkpoint, and the report
     names it;
   - publish the bytes (03);
   - insert or reuse the acquisition, add an `acquisition_sources` row
     (`import_mbox` or `import_eml`, with source path and offset), create
     a logical message and an occurrence in the local archive, in a
     mailbox named after the source file or folder, and enqueue it for
     search.
   - Batches are at most 250 messages per transaction. The job records
     its progress (file and offset), so it can resume after a crash or
     quit without duplicating.
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

   It is shown in the Import report window and saved in `recovery/`.
7. **Sources are never modified or removed.**

**Acceptance:** importing a fixture set (09 fixtures in 11-testing)
reproduces exact byte hashes for every message, survives `kill -9` at any
point and resumes with identical final counts.

## Export

- **Formats:** EML (one file per message) and mboxrd (one file per
  mailbox), from any scope: account, mailbox, search or selection.
- **Bytes:** exported bytes are the original acquisition bytes, never a
  re-serialization.
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
