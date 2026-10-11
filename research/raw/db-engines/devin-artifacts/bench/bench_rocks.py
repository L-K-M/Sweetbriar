"""RocksDB via rocksdict - single keyspace: 'm'+id records, 20B index keys."""
import os, random, sys, threading, time
sys.path.insert(0, os.path.dirname(__file__))
from rocksdict import Rdict, Options, WriteBatch, WriteOptions, AccessType
from common import *

path = os.path.join(DATA, "rocks")
if os.path.isdir(path):
    import shutil
    shutil.rmtree(path)

opts = Options()
opts.create_if_missing(True)
opts.set_use_fsync(True)
opts.set_bytes_per_sync(1 << 20)
opts.increase_parallelism(4)
opts.set_max_background_jobs(4)

db = Rdict(path, opts)
wo = WriteOptions()
wo.sync = True
wo_nb = WriteOptions()
wo_nb.sync = False

res = {"engine": "rocksdict", "note": "use_fsync=1, sync=True on commit batches"}
rng = random.Random(42)

# ---- bulk load ---------------------------------------------------------------
rows = gen_rows(ROWS, rng)
t0 = time.perf_counter()
B = 10_000
for s in range(0, len(rows), B):
    wb = WriteBatch()
    for r in rows[s:s + B]:
        mid, mbox, uid, uvv, date, flags, size, thread, sender, subj = r
        wb.put(msg_key(mid), pack_msg(r))
        wb.put(idx_key(mbox, date, mid), b"")
    db.write(wb, wo_nb)
res["bulk_load_s"] = time.perf_counter() - t0
res["bulk_load_rows"] = len(rows)
res["bulk_load_method"] = "WriteBatch 10k rows, WAL nosync"

# ---- sync batches (250 puts + 250 idx = one wb per batch, synced) -------------
t0 = time.perf_counter()
for b in range(SYNC_BATCHES):
    wb = WriteBatch()
    for i in range(SYNC_BATCH_ROWS):
        mid = 10_000_000 + b * SYNC_BATCH_ROWS + i
        wb.put(msg_key(mid), pack_msg((mid, 3, 0, 0, i, 0, 0, 0, "x", "y")))
        wb.put(idx_key(3, i, mid), b"")
    db.write(wb, wo)
res["sync_batch_s"] = time.perf_counter() - t0
res["sync_batches"] = SYNC_BATCHES
res["sync_batch_rows"] = SYNC_BATCH_ROWS

# ---- single-row commits --------------------------------------------------------
t0 = time.perf_counter()
for i in range(SINGLE_COMMITS):
    mid = i % 900_000
    wb = WriteBatch()
    wb.put(msg_key(mid), pack_msg((mid, 1, 0, 0, i, 9, 0, 0, "x", "y")))
    db.write(wb, wo)
res["single_commit_s"] = time.perf_counter() - t0
res["single_commits"] = SINGLE_COMMITS
res["single_commit_us"] = (res["single_commit_s"] / SINGLE_COMMITS) * 1e6

# ---- point lookups -------------------------------------------------------------
ids = [rng.randrange(0, ROWS) for _ in range(POINT_LOOKUPS)]
t0 = time.perf_counter()
for i in ids:
    db.get(msg_key(i))
res["point_lookup_s"] = time.perf_counter() - t0
res["point_lookups"] = POINT_LOOKUPS
res["point_lookup_us"] = (res["point_lookup_s"] / POINT_LOOKUPS) * 1e6

# ---- keyset pagination ---------------------------------------------------------
prefix = idx_key(BIG_MBOX, 0, 0)[:4]
it = db.iter()
t0 = time.perf_counter()
pages = 0
start = prefix
for _ in range(KEYSET_PAGES):
    it.seek(start)
    n, last = 0, None
    while it.valid():
        k = it.key()
        if not k.startswith(prefix):
            break
        last = k
        n += 1
        if n >= KEYSET_PAGE_SIZE:
            break
        it.next()
    if last is None:
        break
    start = bytes(last) + b"\x00"
    pages += 1
res["keyset_pages"] = pages
res["keyset_total_s"] = time.perf_counter() - t0
res["keyset_ms_per_page"] = res["keyset_total_s"] / max(pages, 1) * 1000

# ---- mark 10k read --------------------------------------------------------------
t0 = time.perf_counter()
updated = 0
wb = WriteBatch()
it.seek(prefix)
while it.valid() and updated < MARK_READ:
    k = it.key()
    if not k.startswith(prefix):
        break
    mid = (1 << 64) - 1 - int.from_bytes(k[12:20], "big")
    v = db.get(msg_key(mid))
    if v is not None:
        b = bytearray(v)
        b[36] |= 1  # flags field offset in packed struct
        wb.put(msg_key(mid), bytes(b))
        updated += 1
    it.next()
db.write(wb, wo)
res["mark_read_s"] = time.perf_counter() - t0
res["mark_read_rows"] = updated

# ---- concurrent readers -----------------------------------------------------------
stop = False
read_counts = [0] * 4


def reader(k):
    n, r = 0, random.Random(k)
    while not stop:
        db.get(msg_key(r.randrange(0, ROWS)))
        n += 1
    read_counts[k] = n


threads = [threading.Thread(target=reader, args=(k,)) for k in range(4)]
for t in threads:
    t.start()
t0 = time.perf_counter()
wb_done = 0
for b in range(100):
    wb = WriteBatch()
    for i in range(250):
        mid = 20_000_000 + b * 250 + i
        wb.put(msg_key(mid), pack_msg((mid, 3, 0, 0, i, 0, 0, 0, "x", "y")))
        wb.put(idx_key(3, i, mid), b"")
    db.write(wb, wo)
    wb_done += 1
res["concurrent_writer_batches"] = wb_done
res["concurrent_writer_s"] = time.perf_counter() - t0
stop = True
for t in threads:
    t.join()
res["concurrent_reader_total_lookups"] = sum(read_counts)

res["db_file_bytes"] = dir_size(path)
del it
db.close()
import gc
gc.collect()
for _ in range(50):  # wait for LOCK release
    try:
        _probe = Rdict(path, opts); _probe.close(); break
    except Exception:
        time.sleep(0.1)

t0 = time.perf_counter()
db2 = Rdict(path, opts)
it = db2.iter()
it.seek_to_first()
cnt = 0
while it.valid():
    cnt += 1
    it.next()
res["open_plus_count_s"] = time.perf_counter() - t0
res["count_all_keys"] = cnt
db2.close()
report("rocksdict", res)
print(json.dumps(res, indent=1))
