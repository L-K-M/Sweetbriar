#!/usr/bin/env bash
set -euo pipefail
cd -- "$(dirname -- "$0")"
export UV_CACHE_DIR="$PWD/.cache/uv"
export TMPDIR="$PWD/.cache/tmp"
export SQLITE_TMPDIR="$TMPDIR"
mkdir -p "$TMPDIR"
if [[ ! -x .venv/bin/python ]]; then
  uv venv .venv
fi
uv pip install --python .venv/bin/python -r requirements.txt
.venv/bin/python setup_fts.py
tag="repro-$(date -u +%Y%m%dT%H%M%SZ)"
for engine in sqlite duckdb libsql lmdb rocksdb; do
  .venv/bin/python bench.py --engine "$engine" --rows 500000 --out "artifacts/$tag-500k"
  .venv/bin/python crash_test.py --engine "$engine" --cycles 30 --out "artifacts/$tag-crash"
done
for engine in sqlite duckdb; do
  .venv/bin/python bench.py --engine "$engine" --rows 1000000 --out "artifacts/$tag-1m"
done
.venv/bin/python duckdb_arrow_batches.py --run "artifacts/$tag-500k" --output "artifacts/$tag-arrow.json"
for engine in sqlite duckdb; do
  .venv/bin/python large_crash_test.py --engine "$engine" --cycles 30 --run "artifacts/$tag-500k" --out "artifacts/$tag-crash-large"
done
.venv/bin/python sustained_readers.py --run "artifacts/$tag-500k" --out "artifacts/$tag-sustained"
.venv/bin/python process_probe.py --out "artifacts/$tag-process"
.venv/bin/python collect_environment.py --out "artifacts/$tag-environment.json"
