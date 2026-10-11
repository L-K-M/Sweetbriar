#!/usr/bin/env python3
"""Sweetbriar-style workload benchmark: SQLite vs local PostgreSQL (pgserver).

Simulates an email metadata store:
- `messages` table with a folder listing index (folder_id, date_ts DESC, id DESC)
- unique index on message_id (RFC822 Message-ID)
- FTS sidecar: SQLite FTS5 external-content table vs Postgres tsvector + GIN
Measures: batched inserts, single-row commits, point lookups, keyset paging, FTS.
"""
import os, random, string, sys, time, sqlite3, shutil, statistics

random.seed(42)
N_ROWS = 500_000
N_FOLDERS = 50
WORDS = ("invoice meeting lunch report update project review draft agenda "
         "deadline budget proposal schedule quarterly urgent reminder welcome "
         "summary notes followup contract invoice payment newsletter").split()

def gen_row(i):
    folder = i % N_FOLDERS
    return (
        folder, 1 + (i % 3), 1000 + i, random.getrandbits(30),
        f"<{i:09d}.{random.getrandbits(40):x}@example.com>",
        random.getrandbits(30),
        " ".join(random.choices(WORDS, k=random.randint(2, 8))),
        f"user{random.randint(1, 2000)}@example.com",
        1_700_000_000 + i + random.randint(0, 999),
        random.randint(600, 2_000_000), random.getrandbits(6), i % 7 == 0,
        " ".join(random.choices(WORDS, k=random.randint(6, 20))),
    )

COLS = "(folder_id, account_id, uid, thread_id, message_id, thread_ref, subject, sender, date_ts, size, flags, has_attach, snippet)"

DDL = """CREATE TABLE messages(
  id INTEGER PRIMARY KEY,
  folder_id BIGINT NOT NULL, account_id BIGINT NOT NULL, uid BIGINT,
  thread_id BIGINT, message_id TEXT, thread_ref BIGINT,
  subject TEXT, sender TEXT, date_ts BIGINT NOT NULL,
  size BIGINT, flags BIGINT, has_attach BOOLEAN, snippet TEXT
)"""
IDX = """CREATE INDEX idx_folder_date ON messages(folder_id, date_ts DESC, id DESC);
CREATE UNIQUE INDEX uq_msgid ON messages(message_id);"""

def t(): return time.perf_counter()

def report(name, dur, n, unit="rows"):
    rate = n / dur if dur else 0
    print(f"  {name:38s} {dur:8.3f}s  {n:>8,} {unit}  {rate:>12,.0f} {unit}/s")
    return rate

def bench_sqlite(path):
    print("== SQLite ==", sqlite3.sqlite_version)
    if os.path.exists(path): os.remove(path)
    db = sqlite3.connect(path)
    db.execute("PRAGMA journal_mode=WAL")
    db.execute("PRAGMA synchronous=FULL")
    db.execute("PRAGMA temp_store=MEMORY")
    db.execute(DDL)

    # 1. batched inserts (1000-row txns), index created after bulk load
    t0 = t(); i = 0
    while i < N_ROWS:
        rows = [gen_row(j) for j in range(i, min(i + 1000, N_ROWS))]
        with db:
            db.executemany(f"INSERT INTO messages {COLS} VALUES ({','.join('?'*13)})", rows)
        i += len(rows)
    report("bulk insert 500k (no idx yet, 1k/txn)", t() - t0, N_ROWS)
    t0 = t(); db.executescript(IDX); db.execute("ANALYZE")
    report("create indexes + analyze", t() - t0, 2, "idx")

    # 2. single-row commits
    t0 = t()
    for j in range(2000):
        with db:
            db.execute(f"INSERT INTO messages {COLS} VALUES ({','.join('?'*13)})", gen_row(N_ROWS + j))
    report("single-row commits (sync=FULL)", t() - t0, 2000)

    # 3. point lookups
    keys = [f"<{random.randint(0, N_ROWS):09d}." for _ in range(0)]
    ids = [random.randint(0, N_ROWS) for _ in range(20_000)]
    msgids = [db.execute("SELECT message_id FROM messages WHERE id=?", (k,)).fetchone()[0] for k in ids[:5000]]
    t0 = t()
    for m in msgids:
        db.execute("SELECT id FROM messages WHERE message_id=?", (m,)).fetchone()
    report("point lookup by message_id", t() - t0, len(msgids), "qry")

    # 4. keyset paging: walk 100 pages x 50 in a busy folder
    folder = 0
    t0 = t(); pages = 0; d = 2**62; mid = 2**62
    for _ in range(100):
        rows = db.execute(
            "SELECT id, subject, sender, date_ts FROM messages "
            "WHERE folder_id=? AND (date_ts, id) < (?, ?) "
            "ORDER BY date_ts DESC, id DESC LIMIT 50", (folder, d, mid)).fetchall()
        if not rows: break
        d, mid = rows[-1][3], rows[-1][0]; pages += 1
    dur = t() - t0
    print(f"  keyset page x{pages} (50 rows)          {dur:8.3f}s  {dur/pages*1e6:>8,.0f} us/page")

    # 5. FTS5
    db.execute("CREATE VIRTUAL TABLE fts USING fts5(subject, sender, snippet, content='messages', content_rowid='id')")
    t0 = t(); db.execute("INSERT INTO fts(fts) VALUES('rebuild')"); db.commit()
    report("FTS5 rebuild 502k rows", t() - t0, N_ROWS)
    terms = [random.choice(WORDS) + " " + random.choice(WORDS) for _ in range(500)]
    t0 = t(); hits = 0
    for q in terms:
        hits += len(db.execute("SELECT rowid FROM fts WHERE fts MATCH ? LIMIT 50", (q,)).fetchall())
    report("FTS5 2-term match", t() - t0, len(terms), "qry")

    # 6. concurrent reader during writer (WAL)
    db.isolation_level = None
    db.execute("BEGIN IMMEDIATE")
    t0 = t()
    r = sqlite3.connect(path); r.execute("PRAGMA journal_mode=WAL")
    n = sum(1 for _ in r.execute("SELECT id FROM messages LIMIT 10000"))
    rd = t() - t0
    db.execute("ROLLBACK"); r.close()
    print(f"  reader during open write txn        {rd:8.4f}s for {n} rows")

    db.execute("PRAGMA wal_checkpoint(TRUNCATE)"); db.close()
    print(f"  db size: {os.path.getsize(path)/1e6:.0f} MB")

def bench_pg(pgdata):
    import pgserver, psycopg
    shutil.rmtree(pgdata, ignore_errors=True)
    srv = pgserver.get_server(pgdata)
    ver = srv.psql("SELECT version()").split("\n")[2].strip()
    print("== PostgreSQL ==", ver)
    srv.psql("ALTER SYSTEM SET synchronous_commit='on'")  # default anyway; explicit
    srv.psql("SELECT pg_reload_conf()")
    con = psycopg.connect(srv.get_uri()); con.autocommit = True
    cur = con.cursor()
    cur.execute(DDL.replace("INTEGER PRIMARY KEY", "BIGINT PRIMARY KEY GENERATED ALWAYS AS IDENTITY"))
    con.commit()

    # 1. batched inserts via executemany, 1000/txn
    t0 = t(); i = 0
    while i < N_ROWS:
        rows = [gen_row(j) for j in range(i, min(i + 1000, N_ROWS))]
        with con.transaction():
            cur.executemany(f"INSERT INTO messages {COLS} VALUES ({','.join(['%s']*13)})", rows)
        i += len(rows)
    report("bulk insert 500k (no idx, 1k/txn)", t() - t0, N_ROWS)

    # COPY comparison
    import io
    t0 = t()
    buf = io.StringIO()
    for j in range(200_000):
        buf.write("\t".join(str(x) for x in gen_row(j + 900_000)).replace("\\", "\\\\") + "\n")
    buf.seek(0)
    with con.transaction():
        with cur.copy(f"COPY messages {COLS} FROM STDIN") as cp:
            while data := buf.read(1 << 20):
                cp.write(data)
    report("COPY 200k rows", t() - t0, 200_000)

    t0 = t()
    for stmt in IDX.split(";"):
        if stmt.strip(): cur.execute(stmt)
    cur.execute("ANALYZE messages")
    report("create indexes + analyze", t() - t0, 2, "idx")

    # 2. single-row commits
    t0 = t()
    for j in range(2000):
        with con.transaction():
            cur.execute(f"INSERT INTO messages {COLS} VALUES ({','.join(['%s']*13)})", gen_row(N_ROWS + 300_000 + j))
    report("single-row commits (sync_commit=on)", t() - t0, 2000)

    # 3. point lookups (prepared)
    ids = [random.randint(1, N_ROWS + 2000) for _ in range(5000)]
    msgids = [cur.execute("SELECT message_id FROM messages WHERE id=%s", (k,)).fetchone()[0] for k in ids]
    t0 = t()
    for m in msgids:
        cur.execute("SELECT id FROM messages WHERE message_id=%s", (m,)).fetchone()
    report("point lookup by message_id", t() - t0, len(msgids), "qry")

    # 4. keyset paging
    folder = 0; t0 = t(); pages = 0; d = 2**62; mid = 2**62
    for _ in range(100):
        cur.execute(
            "SELECT id, subject, sender, date_ts FROM messages "
            "WHERE folder_id=%s AND (date_ts, id) < (%s, %s) "
            "ORDER BY date_ts DESC, id DESC LIMIT 50", (folder, d, mid))
        rows = cur.fetchall()
        if not rows: break
        d, mid = rows[-1][3], rows[-1][0]; pages += 1
    dur = t() - t0
    print(f"  keyset page x{pages} (50 rows)          {dur:8.3f}s  {dur/pages*1e6:>8,.0f} us/page")

    # 5. tsvector + GIN
    t0 = t()
    cur.execute("ALTER TABLE messages ADD COLUMN tsv tsvector "
                "GENERATED ALWAYS AS (to_tsvector('simple', coalesce(subject,'') || ' ' || coalesce(sender,'') || ' ' || coalesce(snippet,''))) STORED")
    cur.execute("CREATE INDEX idx_tsv ON messages USING gin(tsv)")
    con.commit()
    report("tsvector col + GIN index (700k rows)", t() - t0, N_ROWS + 200_000)
    terms = [random.choice(WORDS) + " & " + random.choice(WORDS) for _ in range(500)]
    t0 = t(); hits = 0
    for q in terms:
        hits += len(cur.execute("SELECT id FROM messages WHERE tsv @@ to_tsquery('simple', %s) LIMIT 50", (q,)).fetchall())
    report("tsquery 2-term AND", t() - t0, len(terms), "qry")

    # 6. reader during writer
    con2 = psycopg.connect(srv.get_uri()); con2.autocommit = True
    t0 = t()
    n = sum(1 for _ in con2.execute("SELECT id FROM messages LIMIT 10000"))
    print(f"  reader scan (idle writer)           {t()-t0:8.4f}s for {n} rows")
    con2.close(); con.close(); srv.cleanup()

if __name__ == "__main__":
    here = os.path.dirname(os.path.abspath(__file__))
    if os.environ.get("SKIP_SQLITE") != "1":
        bench_sqlite(os.path.join(here, "bench.db"))
    bench_pg(os.path.join(here, "pgdata"))
