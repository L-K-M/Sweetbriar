"""Reproducible synthetic email-metadata benchmark; all files stay beside this script.

Install local dependencies as recorded in install.log / apsw-install.log, then:
PYTHONPATH=benchmark/deps python3 benchmark/benchmark.py
No TCP listener, no unsafe database settings, no system installation.
"""
from pathlib import Path
import hashlib
import json
import os
import platform
import random
import statistics
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT / "deps"))
import apsw
import psycopg
import psutil

BATCH = 1000
PAGE = 100
PAGES = 500
POINTS = 10000
REPEATS = 3
PG_BIN = ROOT / "deps/pgserver/pginstall/bin"
DATA = ROOT / "pgdata"
SOCKET = ROOT / "socket"
SOCKET.mkdir(exist_ok=True, mode=0o700)
os.environ["TMPDIR"] = str(ROOT / "tmp")
os.environ["XDG_RUNTIME_DIR"] = str(ROOT / "runtime")

def rows(start, stop):
    return [(i, i % 10, 1700000000 + i // 3, i % 8,
             f"Subject {i % 10000:05d} project mail", f"sender{i % 1000:04d}@example.test",
             hashlib.sha256(str(i).encode()).hexdigest()) for i in range(start, stop)]

def percentile(values, fraction):
    return sorted(values)[min(len(values)-1, int(len(values) * fraction))]

def query_stats(execute, count):
    rng = random.Random(20261011)
    ids = [rng.randrange(count) for _ in range(POINTS)]
    for i in ids[:200]:
        assert len(execute("point", (i,))) == 1
    samples = []
    for i in ids:
        begin = time.perf_counter_ns()
        result = execute("point", (i,))
        samples.append((time.perf_counter_ns() - begin) / 1000)
        assert len(result) == 1
    point = {"median_us": statistics.median(samples), "p95_us": percentile(samples, .95)}
    for _ in range(100):
        execute("page", (0, 2000000000, count + 1))
    samples = []
    total = 0
    cursor = (2000000000, count + 1)
    for _ in range(PAGES):
        begin = time.perf_counter_ns()
        result = execute("page", (0, *cursor))
        samples.append((time.perf_counter_ns() - begin) / 1000)
        total += len(result)
        if len(result) < PAGE:
            cursor = (2000000000, count + 1)
        else:
            cursor = (result[-1][1], result[-1][0])
    page = {"median_us": statistics.median(samples), "p95_us": percentile(samples, .95), "returned_rows": total}
    return {"point": point, "page100": page}

BASE_COLUMNS = "mailbox_id INTEGER NOT NULL, received_at BIGINT NOT NULL, flags INTEGER NOT NULL, subject TEXT NOT NULL, sender TEXT NOT NULL, object_hash TEXT NOT NULL"
POINT_SQL = "SELECT id,flags,subject,sender,object_hash FROM messages WHERE id={p}"
PAGE_SQL = "SELECT id,received_at,flags,subject,sender FROM messages WHERE mailbox_id={p} AND (received_at,id)<({p},{p}) ORDER BY received_at DESC,id DESC LIMIT 100"

def sqlite_run(count, repeat):
    path = ROOT / f"sqlite-{count}-{repeat}.db"
    for candidate in [path, Path(str(path)+"-wal"), Path(str(path)+"-shm")]:
        candidate.unlink(missing_ok=True)
    db = apsw.Connection(str(path))
    db.execute("PRAGMA journal_mode=WAL")
    db.execute("PRAGMA synchronous=FULL")
    db.execute("PRAGMA cache_size=-65536")
    db.execute("PRAGMA foreign_keys=ON")
    db.execute(f"CREATE TABLE messages(id INTEGER PRIMARY KEY,{BASE_COLUMNS})")
    db.execute("CREATE INDEX message_list ON messages(mailbox_id,received_at DESC,id DESC,flags,subject,sender)")
    settings = {key: list(db.execute(f"PRAGMA {key}"))[0][0] for key in ["journal_mode","synchronous","cache_size","wal_autocheckpoint","fullfsync"]}
    sql = "INSERT INTO messages VALUES(?,?,?,?,?,?,?)"
    elapsed = 0
    for first in range(0, count, BATCH):
        batch = rows(first, min(first+BATCH, count))
        begin = time.perf_counter()
        db.execute("BEGIN IMMEDIATE")
        db.executemany(sql, batch)
        db.execute("COMMIT")
        elapsed += time.perf_counter() - begin
    checkpoint_start = time.perf_counter()
    list(db.execute("PRAGMA wal_checkpoint(TRUNCATE)"))
    checkpoint = time.perf_counter() - checkpoint_start
    db.execute("ANALYZE")
    point = POINT_SQL.format(p="?")
    page = PAGE_SQL.format(p="?")
    query = query_stats(lambda kind,args: list(db.execute(point if kind=="point" else page,args)),count)
    plan = list(db.execute("EXPLAIN QUERY PLAN " + page,(0,2000000000,count+1)))
    assert list(db.execute("PRAGMA integrity_check")) == [("ok",)]
    assert list(db.execute("SELECT count(*) FROM messages"))[0][0] == count
    db.close()
    result = {"engine":"SQLite","rows":count,"repeat":repeat,"method":"executemany","insert_seconds":elapsed,"checkpoint_seconds":checkpoint,"database_bytes":path.stat().st_size,"settings":settings,"plan":plan,**query}
    path.unlink()
    return result

def pg_run(connection, count, repeat, method):
    connection.execute("DROP TABLE IF EXISTS messages")
    connection.execute(f"CREATE TABLE messages(id BIGINT PRIMARY KEY,{BASE_COLUMNS})")
    connection.execute("CREATE INDEX message_list ON messages(mailbox_id,received_at DESC,id DESC) INCLUDE(flags,subject,sender)")
    settings = {key: connection.execute("SHOW "+key).fetchone()[0] for key in ["fsync","synchronous_commit","full_page_writes","wal_sync_method","shared_buffers","data_checksums"]}
    elapsed = 0
    sql = "INSERT INTO messages VALUES(%s,%s,%s,%s,%s,%s,%s)"
    for first in range(0, count, BATCH):
        batch = rows(first, min(first+BATCH,count))
        begin = time.perf_counter()
        with connection.transaction():
            with connection.cursor() as cursor:
                if method == "executemany":
                    cursor.executemany(sql,batch)
                else:
                    with cursor.copy("COPY messages FROM STDIN") as copy:
                        for row in batch:
                            copy.write_row(row)
        elapsed += time.perf_counter() - begin
    checkpoint_start = time.perf_counter()
    connection.execute("CHECKPOINT")
    checkpoint = time.perf_counter() - checkpoint_start
    connection.execute("VACUUM ANALYZE messages")
    point = POINT_SQL.format(p="%s")
    page = PAGE_SQL.format(p="%s")
    query = query_stats(lambda kind,args: connection.execute(point if kind=="point" else page,args,prepare=True).fetchall(),count)
    plan = [r[0] for r in connection.execute("EXPLAIN (ANALYZE, BUFFERS) " + page,(0,2000000000,count+1))]
    assert connection.execute("SELECT count(*) FROM messages").fetchone()[0] == count
    size = connection.execute("SELECT pg_total_relation_size('messages')").fetchone()[0]
    return {"engine":"PostgreSQL","rows":count,"repeat":repeat,"method":method,"insert_seconds":elapsed,"checkpoint_seconds":checkpoint,"database_bytes":size,"settings":settings,"plan":plan,**query}

def footprint(server):
    processes = [psutil.Process(server.pid), *psutil.Process(server.pid).children(recursive=True)]
    infos = [p.memory_full_info() for p in processes]
    return {"process_count":len(processes),"sum_rss_bytes":sum(p.rss for p in infos),"sum_pss_bytes":sum(getattr(p,"pss",0) for p in infos)}

def main():
    env = {"date":"2026-10-11","python":sys.version,"platform":platform.platform(),"cpus":os.cpu_count(),"memory_bytes":psutil.virtual_memory().total,"sqlite":apsw.sqlitelibversion(),"apsw":apsw.apswversion(),"psycopg":psycopg.__version__,"postgres":subprocess.check_output([str(PG_BIN/"postgres"),"--version"],text=True).strip(),"batch":BATCH,"page":PAGE,"points":POINTS,"pages":PAGES,"repeats":REPEATS,"cpu_models":sorted({l.split(':',1)[1].strip() for l in Path('/proc/cpuinfo').read_text().splitlines() if l.startswith('model name')}),"filesystem":subprocess.check_output(['df','-T',str(ROOT)],text=True).strip()}
    if DATA.exists():
        raise RuntimeError("Existing pgdata: move/remove this benchmark's prior cluster before rerunning")
    init_start = time.perf_counter()
    init = subprocess.run([str(PG_BIN/"initdb"),"-D",str(DATA),"-U","postgres","--encoding=UTF8","--locale=C","--auth-local=trust","--auth-host=reject","--data-checksums"],capture_output=True,text=True,check=True)
    (ROOT/"initdb.log").write_text(init.stdout+init.stderr)
    env['initdb_seconds'] = time.perf_counter()-init_start
    env['bundled_pginstall_bytes'] = sum(p.stat().st_size for p in (PG_BIN.parent).rglob('*') if p.is_file())
    result={"environment":env,"runs":[]}
    with (ROOT/"postgres.log").open('w') as log:
        begin=time.perf_counter()
        server=subprocess.Popen([str(PG_BIN/"postgres"),"-D",str(DATA),"-k",str(SOCKET),"-h","","-c","shared_buffers=64MB","-c","max_connections=10","-c","fsync=on","-c","synchronous_commit=on","-c","full_page_writes=on","-c","wal_sync_method=fdatasync"],stdout=log,stderr=log)
        connection=None
        try:
            for _ in range(1000):
                if server.poll() is not None:
                    raise RuntimeError((ROOT/"postgres.log").read_text())
                try:
                    connection=psycopg.connect(host=str(SOCKET),dbname="postgres",user="postgres",autocommit=True)
                    break
                except psycopg.OperationalError:
                    time.sleep(.02)
            if connection is None:
                raise RuntimeError("PostgreSQL failed to become ready")
            env['startup_to_connection_seconds']=time.perf_counter()-begin
            env['idle_footprint_with_one_client']=footprint(server)
            for count in [100000,1000000]:
                for repeat in range(REPEATS):
                    for mode in ["sqlite","executemany","copy"]:
                        run=sqlite_run(count,repeat) if mode=="sqlite" else pg_run(connection,count,repeat,mode)
                        result['runs'].append(run)
                        (ROOT/"results.json").write_text(json.dumps(result,indent=2))
                        print(json.dumps({k:run[k] for k in ['engine','rows','repeat','method','insert_seconds','point','page100']}),flush=True)
            env['loaded_footprint_with_one_client']=footprint(server)
            (ROOT/"results.json").write_text(json.dumps(result,indent=2))
        finally:
            if connection:
                connection.close()
            server.send_signal(2)
            server.wait(timeout=30)

if __name__=='__main__':
    main()
