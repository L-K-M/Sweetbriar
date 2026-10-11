"""Shared SQL benchmark ops (SQLite dialect: apsw + libsql)."""
import os, random, time
from common import *


def schema_sql(engine="sqlite"):
    return [
        "CREATE TABLE messages(id BIGINT PRIMARY KEY, mailbox INT NOT NULL,"
        " uid INT NOT NULL, uidvalidity INT NOT NULL, date BIGINT NOT NULL,"
        " flags INT NOT NULL, size INT NOT NULL, thread BIGINT NOT NULL,"
        " sender TEXT NOT NULL, subject TEXT NOT NULL)",
        "CREATE INDEX idx_mbox_date ON messages(mailbox, date DESC, id DESC)",
        "CREATE TABLE meta(k TEXT PRIMARY KEY, v BIGINT)",
    ]


def run_sql_bench(engine, con, dbpath, insert_rows, executemany,
                execute, commit_hook=None, supports_desc_index=True):
    """Generic op suite. `insert_rows` pre-generated list of tuples."""
    rng = random.Random(1234)
    res = {}

    # ---- bulk load -------------------------------------------------------
    rows = insert_rows
    t0 = time.perf_counter()
    executemany(
        "INSERT INTO messages VALUES (?,?,?,?,?,?,?,?,?,?)", rows)
    res["bulk_load_s"] = time.perf_counter() - t0
    res["bulk_load_rows"] = len(rows)

    # ---- sync batches: 250-row transactions ------------------------------
    extra = [(10_000_000 + i, 3, 200_000 + i, 26_003, 1_700_500_000 - i,
              rng.randrange(0, 8), rng.randrange(500, 50_000), i,
              f"sync{i}@ex.com", word_stream(6, rng))
             for i in range(SYNC_BATCHES * SYNC_BATCH_ROWS)]
    t0 = time.perf_counter()
    for b in range(SYNC_BATCHES):
        chunk = extra[b * SYNC_BATCH_ROWS:(b + 1) * SYNC_BATCH_ROWS]
        executemany("INSERT INTO messages VALUES (?,?,?,?,?,?,?,?,?,?)", chunk)
    res["sync_batch_s"] = time.perf_counter() - t0
    res["sync_batches"] = SYNC_BATCHES
    res["sync_batch_rows"] = SYNC_BATCH_ROWS

    # ---- single-row commits ----------------------------------------------
    t0 = time.perf_counter()
    for i in range(SINGLE_COMMITS):
        execute("BEGIN")
        execute(f"UPDATE messages SET flags=flags|8 WHERE id={i % 900_000}")
        execute("COMMIT")
    res["single_commit_s"] = time.perf_counter() - t0
    res["single_commits"] = SINGLE_COMMITS
    res["single_commit_us"] = (res["single_commit_s"] / SINGLE_COMMITS) * 1e6

    # ---- point lookups ----------------------------------------------------
    ids = [rng.randrange(0, ROWS) for _ in range(POINT_LOOKUPS)]
    t0 = time.perf_counter()
    for i in ids:
        cur = execute(f"SELECT sender,subject FROM messages WHERE id={i}")
        list(cur)
    res["point_lookup_s"] = time.perf_counter() - t0
    res["point_lookups"] = POINT_LOOKUPS
    res["point_lookup_us"] = (res["point_lookup_s"] / POINT_LOOKUPS) * 1e6

    # ---- keyset pagination on (mailbox, date DESC, id DESC) ---------------
    (d, i) = list(execute(
        f"SELECT date,id FROM messages WHERE mailbox={BIG_MBOX} "
        f"ORDER BY date DESC,id DESC LIMIT 1"))[0]
    t0 = time.perf_counter()
    for _ in range(KEYSET_PAGES):
        page = list(execute(
            f"SELECT id,date FROM messages WHERE mailbox={BIG_MBOX} AND "
            f"(date < {d} OR (date = {d} AND id < {i})) "
            f"ORDER BY date DESC,id DESC LIMIT {KEYSET_PAGE_SIZE}"))
        if not page:
            break
        d, i = page[-1][0], page[-1][1]
    res["keyset_pages"] = KEYSET_PAGES
    res["keyset_total_s"] = time.perf_counter() - t0
    res["keyset_ms_per_page"] = res["keyset_total_s"] / KEYSET_PAGES * 1000

    # ---- mark 10k as read -------------------------------------------------
    t0 = time.perf_counter()
    execute(
        f"UPDATE messages SET flags=flags|1 WHERE id IN "
        f"(SELECT id FROM messages WHERE mailbox={BIG_MBOX} "
        f" AND (flags & 1)=0 ORDER BY date DESC LIMIT {MARK_READ})")
    res["mark_read_s"] = time.perf_counter() - t0
    res["mark_read_rows"] = MARK_READ

    return res
