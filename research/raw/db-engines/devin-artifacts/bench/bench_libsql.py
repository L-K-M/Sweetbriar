"""libSQL (libsql-experimental wheel, SQLite fork) - same ops as SQLite."""
import os, random, sys, threading, time
sys.path.insert(0, os.path.dirname(__file__))
import libsql_experimental as libsql
from common import *
from sql_ops import schema_sql, run_sql_bench

path = os.path.join(DATA, "libsql.db")
for suf in ("", "-wal", "-shm"):
    p = path + suf
    if os.path.exists(p):
        os.remove(p)

con = libsql.connect(path)
res = {"engine": "libsql-experimental",
       "sqlite_base_version": con.execute("select sqlite_version()").fetchone()[0]}
for p in ("journal_mode=WAL", "synchronous=FULL", "busy_timeout=5000"):
    con.execute(f"PRAGMA {p}")
for s in schema_sql():
    con.execute(s)


def execute(q):
    return con.execute(q).fetchall()


def executemany(q, rows):
    con.execute("BEGIN")
    con.executemany(q, rows)
    con.execute("COMMIT")


rows = gen_rows(ROWS, random.Random(42))
res.update(run_sql_bench("libsql", con, path, rows, executemany, execute))

# ---- concurrent readers ------------------------------------------------------
stop = False
read_counts = [0] * 4


def reader(k):
    c = libsql.connect(path)
    n, r = 0, random.Random(k)
    while not stop:
        try:
            c.execute(
                f"SELECT sender FROM messages WHERE id={r.randrange(0, ROWS)}"
            ).fetchall()
            n += 1
        except Exception:
            pass
    read_counts[k] = n
    c.close()


# drain any implicit transaction left open by earlier SELECTs (legacy mode)
while getattr(con, "in_transaction", False):
    try:
        con.execute("ROLLBACK")
    except Exception:
        break

threads = [threading.Thread(target=reader, args=(k,)) for k in range(4)]
for t in threads:
    t.start()
t0 = time.perf_counter()
wb = 0
try:
    for b in range(100):
        chunk = [(20_000_000 + b * 250 + i, 3, 0, 0, i, 0, 0, 0, "x", "y")
                 for i in range(250)]
        con.execute("BEGIN")
        con.executemany(
            "INSERT INTO messages VALUES (?,?,?,?,?,?,?,?,?,?)", chunk)
        con.execute("COMMIT")
        wb += 1
except Exception as e:
    res["concurrent_writer_error"] = str(e)
res["concurrent_writer_batches"] = wb
res["concurrent_writer_s"] = time.perf_counter() - t0
stop = True
for t in threads:
    t.join()
res["concurrent_reader_total_lookups"] = sum(read_counts)

res["db_file_bytes"] = dir_size(path) + dir_size(path + "-wal")
con.close()

t0 = time.perf_counter()
con2 = libsql.connect(path)
con2.execute("SELECT count(*) FROM messages").fetchall()
res["open_plus_count_s"] = time.perf_counter() - t0
t0 = time.perf_counter()
ic = con2.execute("PRAGMA integrity_check").fetchall()
res["integrity_check_s"] = time.perf_counter() - t0
res["integrity"] = ic[0][0]
con2.close()
report("libsql", res)
print(json.dumps(res, indent=1))
