"""SQLite via apsw: WAL, synchronous=FULL."""
import os, random, sys, threading, time
sys.path.insert(0, os.path.dirname(__file__))
import apsw
from common import *
from sql_ops import schema_sql, run_sql_bench

path = os.path.join(DATA, "sqlite.db")
for suf in ("", "-wal", "-shm"):
    p = path + suf
    if os.path.exists(p):
        os.remove(p)

con = apsw.Connection(path)
cur = con.cursor()

t0 = time.perf_counter()
for p in ("journal_mode=WAL", "synchronous=FULL", "busy_timeout=5000"):
    cur.execute(f"PRAGMA {p}")
for s in schema_sql():
    cur.execute(s)
res = {"engine": "sqlite-apsw", "sqlite_version": apsw.sqlitelibversion()}


def execute(q):
    return cur.execute(q)


def executemany(q, rows):
    cur.execute("BEGIN")
    cur.executemany(q, rows)
    cur.execute("COMMIT")


rows = gen_rows(ROWS, random.Random(42))
res.update(run_sql_bench("sqlite", con, path, rows, executemany, execute))

# ---- concurrent readers while writer writes -------------------------------
stop = False
read_counts = [0] * 4


def reader(k):
    c = apsw.Connection(path, flags=apsw.SQLITE_OPEN_READONLY)
    cu = c.cursor()
    n = 0
    r = random.Random(k)
    while not stop:
        try:
            list(cu.execute(
                f"SELECT sender FROM messages WHERE id={r.randrange(0, ROWS)}"))
            n += 1
        except Exception:
            pass
    read_counts[k] = n


threads = [threading.Thread(target=reader, args=(k,)) for k in range(4)]
for t in threads:
    t.start()
t0 = time.perf_counter()
wb = 0
for b in range(100):
    chunk = [(20_000_000 + b * 250 + i, 3, 0, 0, i, 0, 0, 0, "x", "y")
             for i in range(250)]
    cur.execute("BEGIN")
    cur.executemany(
        "INSERT INTO messages VALUES (?,?,?,?,?,?,?,?,?,?)", chunk)
    cur.execute("COMMIT")
    wb += 1
res["concurrent_writer_batches"] = wb
res["concurrent_writer_s"] = time.perf_counter() - t0
stop = True
for t in threads:
    t.join()
res["concurrent_reader_total_lookups"] = sum(read_counts)

res["db_file_bytes"] = dir_size(path) + dir_size(path + "-wal")

# reopen time
con.close()
t0 = time.perf_counter()
con2 = apsw.Connection(path)
list(con2.cursor().execute("SELECT count(*) FROM messages"))
res["open_plus_count_s"] = time.perf_counter() - t0
t0 = time.perf_counter()
ic = list(con2.cursor().execute("PRAGMA integrity_check"))
res["integrity_check_s"] = time.perf_counter() - t0
res["integrity"] = ic[0][0]
con2.close()
report("sqlite", res)
print(json.dumps(res, indent=1))
