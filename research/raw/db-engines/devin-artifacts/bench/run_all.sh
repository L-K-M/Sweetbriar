#!/bin/bash
# Sequential benchmark run - results go to results/*.json
cd "$(dirname "$0")/.."
PY=.venv/bin/python
set -x
$PY bench/bench_sqlite.py 2>&1 | tee results/log_sqlite.txt
$PY bench/bench_libsql.py 2>&1 | tee results/log_libsql.txt
$PY bench/bench_duckdb.py 2>&1 | tee results/log_duckdb.txt
$PY bench/bench_lmdb.py   2>&1 | tee results/log_lmdb.txt
$PY bench/bench_rocks.py  2>&1 | tee results/log_rocks.txt
$PY bench/bench_fts.py    2>&1 | tee results/log_fts.txt
$PY bench/crash_test.py sqlite 15   2>&1 | tee results/log_crash_sqlite.txt
$PY bench/crash_test.py libsql 15   2>&1 | tee results/log_crash_libsql.txt
$PY bench/crash_test.py duckdb 15   2>&1 | tee results/log_crash_duckdb.txt
$PY bench/crash_test.py lmdb 15     2>&1 | tee results/log_crash_lmdb.txt
$PY bench/crash_test.py rocks 15    2>&1 | tee results/log_crash_rocks.txt
echo "ALL DONE"
