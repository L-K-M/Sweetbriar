"""DuckDB 1.5.x benchmark - default config plus two bulk-load paths."""
import os, random, sys, threading, time
sys.path.insert(0, os.path.dirname(__file__))
import duckdb
from common import *
from sql_ops import run_sql_bench

path = os.path.join(DATA, "duckdb.db")
for suf in ("", ".wal"):
    p = path + suf
    if os.path.exists(p):
        os.remove(p)

res = {"engine": "duckdb", "duckdb_version": duckdb.__version__}

# open on a fresh file
t0 = time.perf_counter()
con = duckdb.connect(path)
res["cold_open_s"] = time.perf_counter() - t0
try:
    mem = con.execute(
        "SELECT value FROM duckdb_settings() WHERE name='memory_limit'"
    ).fetchone()
    thr = con.execute(
        "SELECT value FROM duckdb_settings() WHERE name='threads'"
    ).fetchone()
    res["memory_limit_default"] = mem[0] if mem else "?"
    res["threads_default"] = thr[0] if thr else "?"
except Exception as e:
    res["settings_err"] = str(e)

con.execute("""CREATE TABLE messages(id BIGINT PRIMARY KEY,
 mailbox INT NOT NULL, uid INT NOT NULL, uidvalidity INT NOT NULL,
 date BIGINT NOT NULL, flags INT NOT NULL, size INT NOT NULL,
 thread BIGINT NOT NULL, sender VARCHAR NOT NULL, subject VARCHAR NOT NULL)""")
con.execute("CREATE INDEX idx_mbox_date ON messages(mailbox, date DESC, id DESC)")
con.execute("CREATE TABLE meta(k VARCHAR PRIMARY KEY, v BIGINT)")


def execute(q):
    return con.execute(q)


import pandas as pd
rows = gen_rows(ROWS, random.Random(42))

# --- bulk load path A: executemany on first 30k (documents slow path) ------
CAP = min(30_000, len(rows))
t0 = time.perf_counter()
con.execute("BEGIN")
con.executemany(
    "INSERT INTO messages VALUES (?,?,?,?,?,?,?,?,?,?)", rows[:CAP])
con.execute("COMMIT")
res["bulk_load_executemany_s_30k"] = time.perf_counter() - t0
res["bulk_load_executemany_rows"] = CAP

# --- bulk load path B: pandas DataFrame INSERT SELECT (DuckDB fast path) ----
cols = ["id", "mailbox", "uid", "uidvalidity", "date", "flags", "size",
        "thread", "sender", "subject"]
df = pd.DataFrame(rows[CAP:], columns=cols)
con.register("staging", df)
t0 = time.perf_counter()
con.execute("INSERT INTO messages SELECT * FROM staging")
res["bulk_load_s"] = time.perf_counter() - t0
res["bulk_load_rows"] = len(rows) - CAP
res["bulk_load_method"] = "pandas df INSERT SELECT (+30k executemany first)"
con.unregister("staging")

rng = random.Random(1234)
# --- sync batches ----------------------------------------------------------
extra = [(10_000_000 + i, 3, 200_000 + i, 26_003, 1_700_500_000 - i,
          rng.randrange(0, 8), rng.randrange(500, 50_000), i,
          f"sync{i}@ex.com", word_stream(6, rng))
         for i in range(SYNC_BATCHES * SYNC_BATCH_ROWS)]
# path A: executemany row-inserts (first 8 batches, documents per-row cost)
t0 = time.perf_counter()
for b in range(min(8, SYNC_BATCHES)):
    chunk = extra[b * SYNC_BATCH_ROWS:(b + 1) * SYNC_BATCH_ROWS]
    con.execute("BEGIN")
    con.executemany(
        "INSERT INTO messages VALUES (?,?,?,?,?,?,?,?,?,?)", chunk)
    con.execute("COMMIT")
res["sync_batch_executemany_s_8"] = time.perf_counter() - t0

# path B: df INSERT SELECT per 250-row txn (the realistic DuckDB ingest path)
t0 = time.perf_counter()
for b in range(8, SYNC_BATCHES):
    chunk = extra[b * SYNC_BATCH_ROWS:(b + 1) * SYNC_BATCH_ROWS]
    con.register("b", pd.DataFrame(chunk, columns=cols))
    con.execute("BEGIN")
    con.execute("INSERT INTO messages SELECT * FROM b")
    con.execute("COMMIT")
    con.unregister("b")
res["sync_batch_s"] = time.perf_counter() - t0
res["sync_batches"] = SYNC_BATCHES - 8
res["sync_batch_rows"] = SYNC_BATCH_ROWS
res["sync_batch_method"] = "df INSERT SELECT per txn (8 via executemany)"

# --- single-row commits -----------------------------------------------------
t0 = time.perf_counter()
for i in range(SINGLE_COMMITS):
    con.execute("BEGIN")
    con.execute(f"UPDATE messages SET flags=flags | 8 WHERE id={i % 900_000}")
    con.execute("COMMIT")
res["single_commit_s"] = time.perf_counter() - t0
res["single_commits"] = SINGLE_COMMITS
res["single_commit_us"] = (res["single_commit_s"] / SINGLE_COMMITS) * 1e6

# --- point lookups ------------------------------------------------------------
ids = [rng.randrange(0, ROWS) for _ in range(POINT_LOOKUPS)]
t0 = time.perf_counter()
for i in ids:
    con.execute(
        f"SELECT sender,subject FROM messages WHERE id={i}").fetchall()
res["point_lookup_s"] = time.perf_counter() - t0
res["point_lookups"] = POINT_LOOKUPS
res["point_lookup_us"] = (res["point_lookup_s"] / POINT_LOOKUPS) * 1e6

# --- keyset pagination ---------------------------------------------------------
(d, i) = con.execute(
    f"SELECT date,id FROM messages WHERE mailbox={BIG_MBOX} "
    f"ORDER BY date DESC,id DESC LIMIT 1").fetchone()
t0 = time.perf_counter()
for _ in range(KEYSET_PAGES):
    page = con.execute(
        f"SELECT id,date FROM messages WHERE mailbox={BIG_MBOX} AND "
        f"(date < {d} OR (date = {d} AND id < {i})) "
        f"ORDER BY date DESC,id DESC LIMIT {KEYSET_PAGE_SIZE}").fetchall()
    if not page:
        break
    d, i = page[-1]
res["keyset_pages"] = KEYSET_PAGES
res["keyset_total_s"] = time.perf_counter() - t0
res["keyset_ms_per_page"] = res["keyset_total_s"] / KEYSET_PAGES * 1000

# --- mark 10k read --------------------------------------------------------------
t0 = time.perf_counter()
con.execute(
    f"UPDATE messages SET flags=flags | 1 WHERE id IN "
    f"(SELECT id FROM messages WHERE mailbox={BIG_MBOX} "
    f" AND (flags & 1)=0 ORDER BY date DESC LIMIT {MARK_READ})")
res["mark_read_s"] = time.perf_counter() - t0
res["mark_read_rows"] = MARK_READ

# --- concurrent readers -----------------------------------------------------------
stop = False
read_counts = [0] * 4


def reader(k):
    # duckdb.connect(path, read_only=True) fails while a RW connection is
    # open; a duplicated cursor is a new connection on the same MVCC state.
    c = con.cursor()
    n = 0
    r = random.Random(k)
    while not stop:
        try:
            c.execute(
                f"SELECT sender FROM messages WHERE id={r.randrange(0, ROWS)}"
            ).fetchall()
            n += 1
        except Exception:
            pass
    read_counts[k] = n


threads = [threading.Thread(target=reader, args=(k,)) for k in range(4)]
for t in threads:
    t.start()
t0 = time.perf_counter()
wb = 0
try:
    for b in range(100):
        chunk = [(20_000_000 + b * 250 + i, 3, 0, 0, i, 0, 0, 0, "x", "y")
                 for i in range(250)]
        con.register("cb", pd.DataFrame(chunk, columns=cols))
        con.execute("BEGIN")
        con.execute("INSERT INTO messages SELECT * FROM cb")
        con.execute("COMMIT")
        con.unregister("cb")
        wb += 1
except Exception as e:
    res["concurrent_writer_error"] = str(e)
res["concurrent_writer_batches"] = wb
res["concurrent_writer_s"] = time.perf_counter() - t0
stop = True
for t in threads:
    t.join()
res["concurrent_reader_total_lookups"] = sum(read_counts)

res["db_file_bytes"] = dir_size(path) + dir_size(path + ".wal")
con.close()

t0 = time.perf_counter()
con2 = duckdb.connect(path)
con2.execute("SELECT count(*) FROM messages").fetchall()
res["open_plus_count_s"] = time.perf_counter() - t0
con2.close()
report("duckdb", res)
print(json.dumps(res, indent=1))
