# Sweetbriar export format (M1.7, spec/09 "Export file output")

An export job writes one directory the job claims as its own: the
message files first, then `sweetbriar-metadata.json` as the last
published entry. A directory without the manifest is not a finished
export; a receipt or job state never claims a manifest that has not
been published and size-verified.

## Output by format

| `format` | Files | One file holds |
| --- | --- | --- |
| `eml` | `<occurrence_id>.eml` | one occurrence's exact acquired bytes |
| `mboxrd` | `<mailbox_id>-<cleaned_name>.mbox` | one mailbox's records, mboxrd transport-encoded (`From `/`>From ` lines escaped) |

Two identical messages stay two records: the capture ledger keys them
by occurrence id, never by content hash.

## `sweetbriar-metadata.json`

```json
{
  "v": 1,
  "kind": "export-manifest",
  "job": "<job_id>",
  "format": "eml|mboxrd",
  "destination": "<absolute path>",
  "scope": {"kind": "mailbox|account|search|selection", "...": "..."},
  "generation": <list generation the membership was fenced at>,
  "entries": <records captured>,
  "ids_sha256": "<sha256 of the ordered occurrence ids>",
  "files": [{"name": "1-Inbox.mbox", "bytes": 1234, "records": 2}],
  "messages": [ "...one metadata record per entry..." ]
}
```

Each `messages` entry is the export-metadata record verbatim — subject,
addresses, dates, flags, acquisition evidence — plus placement:

- `file`: the output file's name, or `null` when the bytes were
  unavailable (`not_acquired`, `missing` or `quarantined`).
- mboxrd records also carry `record_offset`, `encoded_bytes` (the
  record's size inside the `.mbox`), `original_bytes` and
  `framing_lf_added`.

## Durability

- The job's durable checkpoint counts published files; `Completed`
  adds the verified manifest size. A worker or process that stops mid-
  run leaves the last committed checkpoint; the next open resumes
  `ready`/`running` jobs and fails `capturing` ones (their request is
  not durable).
- Resumed output rewrites missing or corrupted committed files with
  identical bytes — the sealed plan names them, so a repair is
  deterministic.
- The destination claim (`sweetbriar-export.claim`/`.lock`) rejects a
  second export into a claimed directory; those marker files are the
  job's bookkeeping, not output.
