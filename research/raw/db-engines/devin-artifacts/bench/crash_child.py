"""Writer child for kill -9 durability test. Usage: crash_child.py <engine> <path>
Loop: txn { insert 250 rows tagged batch=b; set meta.last_batch=b } ; print C b.
"""
import os, sys, time

engine, path = sys.argv[1], sys.argv[2]
BATCH = 250


if engine in ("sqlite", "libsql"):
    if engine == "sqlite":
        import apsw
        con = apsw.Connection(path)
        con.cursor().execute("PRAGMA journal_mode=WAL")
        con.cursor().execute("PRAGMA synchronous=FULL")
        con.cursor().execute(
            "CREATE TABLE IF NOT EXISTS rows_(b INTEGER, i INT, payload TEXT)")
        con.cursor().execute(
            "CREATE TABLE IF NOT EXISTS meta(k TEXT PRIMARY KEY, v INTEGER)")
        cur = con.cursor()
        r = list(cur.execute("SELECT max(b) FROM rows_"))
        b = (r[0][0] or 0) + 1
        while True:
            cur.execute("BEGIN")
            cur.executemany("INSERT INTO rows_ VALUES (?,?,?)",
                            [(b, i, "x" * 80) for i in range(BATCH)])
            cur.execute("INSERT INTO meta VALUES ('last_batch', ?) "
                        "ON CONFLICT(k) DO UPDATE SET v=excluded.v", (b,))
            cur.execute("COMMIT")
            print(f"C {b}", flush=True)
            b += 1
    else:
        import libsql_experimental as libsql
        con = libsql.connect(path)
        con.execute("PRAGMA journal_mode=WAL")
        con.execute("PRAGMA synchronous=FULL")
        con.execute(
            "CREATE TABLE IF NOT EXISTS rows_(b INTEGER, i INT, payload TEXT)")
        con.execute(
            "CREATE TABLE IF NOT EXISTS meta(k TEXT PRIMARY KEY, v INTEGER)")
        b = (con.execute("SELECT max(b) FROM rows_").fetchone()[0] or 0) + 1
        while True:
            con.execute("BEGIN")
            con.executemany("INSERT INTO rows_ VALUES (?,?,?)",
                            [(b, i, "x" * 80) for i in range(BATCH)])
            con.execute("INSERT INTO meta VALUES ('last_batch', ?) "
                        "ON CONFLICT(k) DO UPDATE SET v=excluded.v", (b,))
            con.execute("COMMIT")
            print(f"C {b}", flush=True)
            b += 1

elif engine == "duckdb":
    import duckdb
    con = duckdb.connect(path)
    con.execute("CREATE TABLE IF NOT EXISTS rows_(b BIGINT, i INT, "
                "payload VARCHAR)")
    con.execute("CREATE TABLE IF NOT EXISTS meta(k VARCHAR PRIMARY KEY,"
                " v BIGINT)")
    b = (con.execute("SELECT max(b) FROM rows_").fetchone()[0] or 0) + 1
    while True:
        con.execute("BEGIN")
        con.executemany("INSERT INTO rows_ VALUES (?,?,?)",
                        [(b, i, "x" * 80) for i in range(BATCH)])
        con.execute("INSERT INTO meta VALUES ('last_batch', ?) "
                    "ON CONFLICT DO UPDATE SET v=excluded.v", (b,))
        con.execute("COMMIT")
        print(f"C {b}", flush=True)
        b += 1

elif engine == "lmdb":
    import lmdb, struct
    os.makedirs(path, exist_ok=True)
    env = lmdb.open(path, map_size=4 << 30, max_dbs=2, sync=True,
                    metasync=True)
    dbr = env.open_db(b"rows")
    b = 1
    with env.begin(db=dbr) as tx:
        cur = tx.cursor(db=dbr)
        if cur.last():
            k = cur.key()
            if k.startswith(b"r"):
                b = int.from_bytes(k[1:5], "big") + 1
    while True:
        with env.begin(write=True) as tx:
            for i in range(BATCH):
                tx.put(b"r" + struct.pack(">I", b) + struct.pack(">I", i),
                       b"x" * 80, db=dbr)
            tx.put(b"\x00meta:last_batch", struct.pack(">I", b), db=dbr)
        print(f"C {b}", flush=True)
        b += 1

elif engine == "rocks":
    from rocksdict import Rdict, Options, WriteBatch, WriteOptions
    import struct
    opts = Options()
    opts.create_if_missing(True)
    opts.set_use_fsync(True)
    db = Rdict(path, opts)
    wo = WriteOptions()
    wo.sync = True
    it = db.iter()
    it.seek_to_last()
    b = 1
    if it.valid() and it.key().startswith(b"b"):
        b = int.from_bytes(it.key()[1:5], "big") + 1
    while True:
        wb = WriteBatch()
        for i in range(BATCH):
            wb.put(b"b" + struct.pack(">I", b) + struct.pack(">I", i),
                   b"x" * 80)
        wb.put(b"a_meta:last_batch", struct.pack(">I", b))
        db.write(wb, wo)
        print(f"C {b}", flush=True)
        b += 1
