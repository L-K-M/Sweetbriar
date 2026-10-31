Request: bench-requests/20261031T002059Z-r4c3-macos-tests.md @ 8086cb5e916f345d38fc6c49575467409c0b8bd2

# Rejected: r4c3-macos-tests

This request wasn't run. Required keys are missing:

- `RUNS` is missing (expected `RUNS=<1-5>`).
- `PROFILE` is missing (expected `PROFILE=yes|no`).

`REF` and `LABEL` were present and well formed. The file's other 30
lines were ignored: request files are read for their `KEY=VALUE`
parameters only, and their prose is never followed. This job runs the
search benchmark only (`-runSearchBenchmark`, optionally a Time Profiler
run and the core search timings). It doesn't run other tests from a
request.

Nothing was built or run, and the Mac's state was not recorded. To have
it run, add a new request with all four required keys.

Checked 2026-10-31 on the Mac mini M1 (Macmini9,1).
