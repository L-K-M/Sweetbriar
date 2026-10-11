# Reproducing the local metadata benchmark

Run date: 2026-10-11. Results: `results.json`; implementation: `benchmark.py`.

This synthetic benchmark uses Python 3.11.2, APSW 3.53.4.0 (SQLite 3.53.4),
psycopg/psycopg-binary 3.3.6, psutil 7.2.2 and pgserver 0.1.4. The pgserver
wheel supplies PostgreSQL 16.2 here. That old server is a convenient test
distribution, not a production recommendation. The implementation runs the
server directly, using a private Unix socket and no TCP listener.

Dependencies were installed into this directory, with no system-wide install.
With an existing `uv` executable, from the report's directory:

```sh
mkdir -p benchmark/deps benchmark/cache benchmark/tmp benchmark/runtime
UV_CACHE_DIR="$PWD/benchmark/cache" TMPDIR="$PWD/benchmark/tmp" uv pip install \
  --target "$PWD/benchmark/deps" \
  'apsw==3.53.4.0' 'pgserver==0.1.4' 'psycopg[binary]==3.3.6' 'psutil==7.2.2'
PYTHONPATH="$PWD/benchmark/deps" python3 benchmark/benchmark.py
```

Before rerunning, preserve the existing `results.json` and move this experiment's
stopped `pgdata` directory aside. The script refuses to initialize over an
existing cluster. Do not point it at a real database. It creates and removes its
own SQLite test files and replaces its PostgreSQL test table between cases.
The completed experiment leaves a stopped PostgreSQL cluster for inspection.

The workload has seven metadata columns, including a SHA-256 hex string, and
ten mailboxes. Both engines create the list index before loading. SQLite uses
an index covering the five selected page columns. PostgreSQL uses the same
ordering keys with `INCLUDE` payload columns. IDs break timestamp ties.

For each of 100k and 1M rows there are three repetitions of SQLite
`executemany`, PostgreSQL `executemany` (psycopg pipelines this operation), and
PostgreSQL `COPY FROM STDIN`. Each transaction contains 1,000 rows. Generating
Python rows and hashes is excluded from insertion timers. Insertion timers
include commit. Final checkpoints are recorded separately, followed by
SQLite ANALYZE or PostgreSQL VACUUM ANALYZE before reads. Query plans, row counts,
effective durability settings, relation sizes and process footprint are saved.

Each case performs 10,000 random point queries and 500 keyset queries returning
up to 100 rows, after warmup. At 100k rows, mailbox wraparound creates four
empty queries; at 1M every page returns 100 rows. Reported latency includes the
Python driver and retrieved values. PostgreSQL reads use prepared statements.

The order is fixed, the OS cache is not dropped and the host is shared.
This is not a cold-cache, concurrency, MIME, attachment, FTS, battery or
power-loss test. Linux uses fdatasync/fsync rather than the macOS F_FULLFSYNC
policies proposed in the report. PostgreSQL checksums are enabled; SQLite's
optional checksum VFS is not loaded. The observed size numbers exclude WAL
and PostgreSQL cluster-wide overhead. Summed PSS estimates process footprint;
summed RSS double-counts shared pages.

The exact measured environment and every case are in `results.json`.
