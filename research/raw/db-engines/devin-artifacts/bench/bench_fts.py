"""FTS: SQLite FTS5 vs DuckDB fts extension, 200k docs, subject+body."""
import os, random, sys, time
sys.path.insert(0, os.path.dirname(__file__))
import apsw, duckdb
from common import *

res = {}
rng = random.Random(99)

# ---------- SQLite FTS5 --------------------------------------------------------
sp = os.path.join(DATA, "fts_sqlite.db")
for suf in ("", "-wal", "-shm"):
    if os.path.exists(sp + suf):
        os.remove(sp + suf)
con = apsw.Connection(sp)
c = con.cursor()
c.execute("PRAGMA journal_mode=WAL")
c.execute("PRAGMA synchronous=FULL")
c.execute("CREATE TABLE mail(id INTEGER PRIMARY KEY, subject TEXT, body TEXT)")
c.execute("CREATE VIRTUAL TABLE mail_fts USING fts5(subject, body, "
          "content='mail', content_rowid='id')")

docs = list(fts_docs(FTS_DOCS, rng))
t0 = time.perf_counter()
c.execute("BEGIN")
c.executemany("INSERT INTO mail(id,subject,body) VALUES (?,?,?)", docs)
c.execute("COMMIT")
res["sqlite_doc_load_s"] = time.perf_counter() - t0
t0 = time.perf_counter()
c.execute("INSERT INTO mail_fts(mail_fts) VALUES('rebuild')")
res["sqlite_fts_build_s"] = time.perf_counter() - t0

queries = []
for _ in range(FTS_QUERIES):
    t = rng.choice(WORDS)
    k = rng.randrange(0, 3)
    if k == 0:
        queries.append(f"{t} OR {rng.choice(WORDS)}")
    elif k == 1:
        queries.append(f'"{t} {rng.choice(WORDS)}"')
    else:
        queries.append(f"{t}*")
t0 = time.perf_counter()
hits = 0
for q in queries:
    r = list(c.execute(
        "SELECT count(*) FROM mail_fts WHERE mail_fts MATCH ? "
        "ORDER BY rank LIMIT 25".replace("?", f"'{q}'")))
    hits += r[0][0]
res["sqlite_fts_queries"] = FTS_QUERIES
res["sqlite_fts_total_s"] = time.perf_counter() - t0
res["sqlite_fts_ms_per_q"] = res["sqlite_fts_total_s"] / FTS_QUERIES * 1000
res["sqlite_fts_hits"] = hits

# incremental update cost: insert 2000 docs into content+fts in small txns
new = list(fts_docs(2000, rng))
t0 = time.perf_counter()
for s in range(0, 2000, 250):
    c.execute("BEGIN")
    for d in new[s:s + 250]:
        c.execute("INSERT INTO mail(id,subject,body) VALUES (?,?,?)",
                  (d[0] + FTS_DOCS, d[1], d[2]))
        c.execute("INSERT INTO mail_fts(rowid,subject,body) VALUES (?,?,?)",
                  (d[0] + FTS_DOCS, d[1], d[2]))
    c.execute("COMMIT")
res["sqlite_fts_incremental_2k_s"] = time.perf_counter() - t0

res["sqlite_fts_file_bytes"] = dir_size(sp) + dir_size(sp + "-wal")
con.close()

# ---------- DuckDB FTS -----------------------------------------------------------
dp = os.path.join(DATA, "fts_duckdb.db")
for suf in ("", ".wal"):
    if os.path.exists(dp + suf):
        os.remove(dp + suf)
dcon = duckdb.connect(dp)
dcon.execute("LOAD fts")
dcon.execute("CREATE TABLE mail(id BIGINT PRIMARY KEY, subject VARCHAR, "
             "body VARCHAR)")
import pandas as pd
t0 = time.perf_counter()
dcon.register("docs_df", pd.DataFrame(docs, columns=["id", "subject", "body"]))
dcon.execute("INSERT INTO mail SELECT * FROM docs_df")
dcon.unregister("docs_df")
res["duck_doc_load_s"] = time.perf_counter() - t0
res["duck_doc_load_method"] = "df INSERT SELECT"
t0 = time.perf_counter()
dcon.execute("PRAGMA create_fts_index('mail','id','subject','body')")
res["duck_fts_build_s"] = time.perf_counter() - t0

t0 = time.perf_counter()
hits = 0
for q in queries:
    qq = q.replace('"', '').replace('*', '')
    r = dcon.execute(
        "SELECT count(*) FROM (SELECT id, "
        f"fts_main_mail.match_bm25(id, '{qq}') AS score FROM mail) t "
        "WHERE score IS NOT NULL").fetchall()
    hits += r[0][0]
res["duck_fts_queries"] = FTS_QUERIES
res["duck_fts_total_s"] = time.perf_counter() - t0
res["duck_fts_ms_per_q"] = res["duck_fts_total_s"] / FTS_QUERIES * 1000
res["duck_fts_hits"] = hits

# incremental update: insert 2000 docs -> does index update automatically?
new = list(fts_docs(2000, rng))
t0 = time.perf_counter()
for s in range(0, 2000, 250):
    dcon.register("inc",
                  pd.DataFrame([(d[0] + FTS_DOCS, d[1], d[2])
                                for d in new[s:s + 250]],
                               columns=["id", "subject", "body"]))
    dcon.execute("BEGIN")
    dcon.execute("INSERT INTO mail SELECT * FROM inc")
    dcon.execute("COMMIT")
    dcon.unregister("inc")
res["duck_fts_incremental_2k_s"] = time.perf_counter() - t0
# check whether new docs are searchable without rebuild
probe = dcon.execute(
    f"SELECT count(*) FROM (SELECT id, fts_main_mail.match_bm25(id, "
    f"'{WORDS[0]}') AS score FROM mail) t WHERE score IS NOT NULL").fetchone()[0]
res["duck_probe_hits_after_incremental"] = probe

res["duck_fts_file_bytes"] = dir_size(dp) + dir_size(dp + ".wal")
dcon.close()
report("fts", res)
print(json.dumps(res, indent=1))
