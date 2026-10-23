# Tokio I/O-safety patch

Prerequisite for M1.6m3b. The full gate on 2026-10-23 rejects Tokio 1.53.2 for
[RUSTSEC-2026-0386](https://rustsec.org/advisories/RUSTSEC-2026-0386.html).
The advisory was issued that day; 1.53.3 is the patched release on our pinned line.

## Outcome

Pin Engine and FFI to exactly 1.53.3; update the lockfile and notices. Keep their
disabled defaults and existing scheduler/sync features. Sweetbriar does not use
AsyncFd, but the dependency gate must pass without an advisory exemption.
No API or persistent-data change; rollback affects build inputs only.

## Acceptance

- `cargo deny check` accepts the updated lockfile without new exclusions.
- `cargo xtask ci` passes on normal profiles.
- The feature graph retains the existing Tokio feature sets.
- Exact-head `ci` and actual native `macos` checks pass.
- Local SWE-2 reviews this activation prerequisite under the owner's task-level
  GLM substitution. The ordinary size policy remains intact for this small PR.
