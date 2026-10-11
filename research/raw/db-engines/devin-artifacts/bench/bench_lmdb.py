"""LMDB (py-lmdb) benchmark - KV model: record store + mailbox index."""
import os, random, sys, threading, time
sys.path.insert(0, os.path.dirname(__file__))
import lmdb
from common import *

path = os.path.join(DATA, "lmdb")
if os.path.isdir(path):
    import shutil
    shutil.rmtree(path)

env = lmdb.open(path, map_size=16 << 30, max_dbs=4, sync=True,
                metasync=True, writemap=False)
msgs = env.open_db(b"msgs")
idx = env.open_db(b"idx")

res = {"engine": "lmdb", "lmdb_version": ".".join(map(str, lmdb.version()))}
rng = random.Random(42)

# ---- bulk load --------------------------------------------------------------
rows = gen_rows(ROWS, rng)
t0 = time.perf_counter()
B = 10_000
for s in range(0, len(rows), B):
    with env.begin(write=True, db=msgs) as tx:
        for r in rows[s:s + B]:
            mid, mbox, uid, uvv, date, flags, size, thread, sender, subj = r
            tx.put(msg_key(mid), pack_msg(r), db=msgs)
            tx.put(idx_key(mbox, date, mid), b"", db=idx)
res["bulk_load_s"] = time.perf_counter() - t0
res["bulk_load_rows"] = len(rows)

# ---- sync batches (250-op txns) ----------------------------------------------
t0 = time.perf_counter()
for b in range(SYNC_BATCHES):
    with env.begin(write=True) as tx:
        for i in range(SYNC_BATCH_ROWS):
            mid = 10_000_000 + b * SYNC_BATCH_ROWS + i
            tx.put(msg_key(mid), pack_msg((mid, 3, 0, 0, i, 0, 0, 0, "x", "y")),
                   db=msgs)
            tx.put(idx_key(3, i, mid), b"", db=idx)
res["sync_batch_s"] = time.perf_counter() - t0
res["sync_batches"] = SYNC_BATCHES
res["sync_batch_rows"] = SYNC_BATCH_ROWS

# ---- single-row commits --------------------------------------------------------
t0 = time.perf_counter()
for i in range(SINGLE_COMMITS):
    mid = i % 900_000
    with env.begin(write=True) as tx:
        tx.put(msg_key(mid), pack_msg((mid, 1, 0, 0, i, 9, 0, 0, "x", "y")),
               db=msgs)
res["single_commit_s"] = time.perf_counter() - t0
res["single_commits"] = SINGLE_COMMITS
res["single_commit_us"] = (res["single_commit_s"] / SINGLE_COMMITS) * 1e6

# ---- point lookups -------------------------------------------------------------
ids = [rng.randrange(0, ROWS) for _ in range(POINT_LOOKUPS)]
t0 = time.perf_counter()
with env.begin(db=msgs) as tx:
    for i in ids:
        tx.get(msg_key(i), db=msgs)
res["point_lookup_s"] = time.perf_counter() - t0
res["point_lookups"] = POINT_LOOKUPS
res["point_lookup_us"] = (res["point_lookup_s"] / POINT_LOOKUPS) * 1e6

# ---- keyset pagination ---------------------------------------------------------
t0 = time.perf_counter()
pages = 0
with env.begin(db=idx) as tx:
    cur = tx.cursor(db=idx)
    prefix = idx_key(BIG_MBOX, 0, 0)[:4]
    if cur.set_range(prefix):
        start = cur.key()
        resume = False
        for _ in range(KEYSET_PAGES):
            cur.set_range(start)
            if resume:
                cur.next()          # skip the resume key itself
            n, last = 0, None
            for k in cur.iternext(keys=True, values=False):
                if not k.startswith(prefix):
                    break
                last = k
                n += 1
                if n >= KEYSET_PAGE_SIZE:
                    break
            if last is None:
                break
            start = bytes(last)
            resume = True
            pages += 1
res["keyset_pages"] = pages
res["keyset_total_s"] = time.perf_counter() - t0
res["keyset_ms_per_page"] = res["keyset_total_s"] / max(pages, 1) * 1000

# ---- mark 10k read --------------------------------------------------------------
t0 = time.perf_counter()
updated = 0
with env.begin(write=True) as tx:
    cur = tx.cursor(db=idx)
    prefix = idx_key(BIG_MBOX, 0, 0)[:4]
    for k, _ in cur:
        if not k.startswith(prefix) or updated >= MARK_READ:
            break
        mid = (1 << 64) - 1 - int.from_bytes(k[12:20], "big")
        v = tx.get(msg_key(mid), db=msgs)
        if v is not None:
            b = bytearray(v)
            b[36] |= 1  # flags field offset in packed struct
            tx.put(msg_key(mid), bytes(b), db=msgs)
            updated += 1
res["mark_read_s"] = time.perf_counter() - t0
res["mark_read_rows"] = updated

# ---- concurrent readers ---------------------------------------------------------
stop = False
read_counts = [0] * 4


def reader(k):
    # LMDB forbids a second env open in-process; share env, one read txn
    # per thread (snapshots are unaffected by the concurrent writer).
    n, r = 0, random.Random(k)
    with env.begin(db=msgs) as tx:
        while not stop:
            tx.get(msg_key(r.randrange(0, ROWS)), db=msgs)
            n += 1
    read_counts[k] = n


threads = [threading.Thread(target=reader, args=(k,)) for k in range(4)]
for t in threads:
    t.start()
t0 = time.perf_counter()
wb = 0
for b in range(100):
    with env.begin(write=True) as tx:
        for i in range(250):
            mid = 20_000_000 + b * 250 + i
            tx.put(msg_key(mid), pack_msg((mid, 3, 0, 0, i, 0, 0, 0, "x", "y")),
                   db=msgs)
            tx.put(idx_key(3, i, mid), b"", db=idx)
    wb += 1
res["concurrent_writer_batches"] = wb
res["concurrent_writer_s"] = time.perf_counter() - t0
stop = True
for t in threads:
    t.join()
res["concurrent_reader_total_lookups"] = sum(read_counts)

env.sync()
res["db_file_bytes"] = dir_size(path)
env.close()

# reopen
t0 = time.perf_counter()
env2 = lmdb.open(path, map_size=16 << 30, max_dbs=4)
m2 = env2.open_db(b"msgs")
with env2.begin() as tx:
    st = tx.stat(db=m2)
    cnt = st["entries"]
res["open_plus_count_s"] = time.perf_counter() - t0
res["count"] = cnt
env2.close()
report("lmdb", res)
print(json.dumps(res, indent=1))
