"""kill -9 crash-durability driver.

For each engine: start writer child, collect 'C <batch>' stdout lines
(emitted only after commit returns), sleep a random short interval,
kill -9, reopen, verify:
  * every logged batch has exactly 250 rows   (durability)
  * every present batch has exactly 250 rows  (atomicity, no torn commits)
  * meta.last_batch == max present batch      (transactional consistency)
  * engine integrity check passes on reopen
Cycles continue on the same file so batches accumulate.
"""
import json, os, random, signal, subprocess, sys, time
sys.path.insert(0, os.path.dirname(__file__))
from common import DATA, RESULTS

CYCLES = int(sys.argv[2]) if len(sys.argv) > 2 else 15
HERE = os.path.dirname(__file__)
PY = os.path.join(HERE, "..", ".venv", "bin", "python")


def verify(engine, path, committed):
    """Returns (ok, details dict)."""
    det = {}
    if engine in ("sqlite", "libsql"):
        if engine == "sqlite":
            import apsw
            con = apsw.Connection(path, flags=apsw.SQLITE_OPEN_READONLY)
            cur = con.cursor()
        else:
            import libsql_experimental as libsql
            con = libsql.connect(path)
            cur = con
        def fetchall(q):
            r = cur.execute(q)
            return r.fetchall() if hasattr(r, "fetchall") else list(r)
        try:
            ic = fetchall("PRAGMA integrity_check")
            det["integrity"] = ic[0][0]
        except Exception as e:
            det["integrity_error"] = str(e)
        counts = {}
        for (b,) in fetchall("SELECT b FROM rows_"):
            counts[b] = counts.get(b, 0) + 1
        det["counts"] = counts
        try:
            lb = fetchall("SELECT v FROM meta WHERE k='last_batch'")[0][0]
        except Exception:
            lb = None
        det["meta_last"] = lb
        con.close()
    elif engine == "duckdb":
        import duckdb
        con = duckdb.connect(path, read_only=False)
        try:
            con.execute("FORCE CHECKPOINT")
        except Exception:
            pass
        counts = {}
        for (b, n) in con.execute(
                "SELECT b, count(*) FROM rows_ GROUP BY b").fetchall():
            counts[b] = n
        det["counts"] = counts
        try:
            lb = con.execute(
                "SELECT v FROM meta WHERE k='last_batch'").fetchone()[0]
        except Exception:
            lb = None
        det["meta_last"] = lb
        con.close()
    elif engine == "lmdb":
        import lmdb, struct
        env = lmdb.open(path, map_size=4 << 30, max_dbs=2)
        dbr = env.open_db(b"rows")
        counts = {}
        lb = None
        with env.begin(db=dbr) as tx:
            cur = tx.cursor(db=dbr)
            for k, v in cur:
                if k.startswith(b"r"):
                    b = int.from_bytes(k[1:5], "big")
                    counts[b] = counts.get(b, 0) + 1
                elif k == b"\x00meta:last_batch":
                    lb = int.from_bytes(v, "big")
        det["counts"] = counts
        det["meta_last"] = lb
        env.close()
    elif engine == "rocks":
        from rocksdict import Rdict, Options
        opts = Options()
        db = Rdict(path, opts)
        counts = {}
        lb = None
        it = db.iter()
        it.seek_to_first()
        while it.valid():
            k = it.key()
            if k.startswith(b"b"):
                b = int.from_bytes(k[1:5], "big")
                counts[b] = counts.get(b, 0) + 1
            elif k == b"a_meta:last_batch":
                lb = int.from_bytes(it.value(), "big")
            it.next()
        det["counts"] = counts
        det["meta_last"] = lb
        db.close()
    else:
        return False, {"error": "unknown engine"}

    problems = []
    for b in committed:
        if counts.get(b, 0) != 250:
            problems.append(f"committed batch {b} has {counts.get(b, 0)} rows")
    for b, n in counts.items():
        if n != 250:
            problems.append(f"torn batch {b}: {n} rows")
    if det["meta_last"] is not None and counts:
        if det["meta_last"] != max(counts):
            problems.append(
                f"meta.last_batch={det['meta_last']} != max present "
                f"{max(counts)}")
    return (len(problems) == 0), {**det, "problems": problems}


def run(engine):
    path = os.path.join(DATA, f"crash_{engine}.db" if engine != "lmdb"
                        else f"crash_{engine}")
    if engine in ("sqlite", "libsql", "duckdb"):
        for suf in ("", "-wal", "-shm", ".wal"):
            if os.path.exists(path + suf):
                os.remove(path + suf)
    else:
        import shutil
        shutil.rmtree(path, ignore_errors=True)

    committed = []
    results = {"engine": engine, "cycles": [], "violations": []}
    for cyc in range(CYCLES):
        proc = subprocess.Popen(
            [PY, os.path.join(HERE, "crash_child.py"), engine, path],
            stdout=subprocess.PIPE, text=True, bufsize=1)
        # let it commit at least a few batches, then kill at a random moment
        seen = []
        deadline = time.time() + 8
        while len(seen) < 3 and time.time() < deadline:
            line = proc.stdout.readline()
            if line.startswith("C "):
                seen.append(int(line.split()[1]))
        time.sleep(random.uniform(0.02, 0.30))
        proc.send_signal(signal.SIGKILL)
        proc.wait()
        # drain remaining committed-batch notifications after child death
        try:
            for extra in proc.stdout.read().splitlines():
                if extra.startswith("C "):
                    seen.append(int(extra.split()[1]))
        except Exception:
            pass
        committed.extend(seen)
        committed = sorted(set(committed))
        try:
            ok, det = verify(engine, path, committed)
        except Exception as e:
            ok, det = False, {"problems": [f"verify exception: {e}"]}
        det["cycle"] = cyc
        det["committed_total"] = len(committed)
        det["killed_at"] = seen[-1] if seen else None
        results["cycles"].append(det)
        if not ok:
            results["violations"].append(det)
        print(f"[{engine}] cycle {cyc}: committed={len(committed)} "
              f"ok={ok} problems={det.get('problems')}", flush=True)
    with open(os.path.join(RESULTS, f"crash_{engine}.json"), "w") as f:
        json.dump(results, f, indent=1, default=str)
    return results


if __name__ == "__main__":
    engines = sys.argv[1].split(",")
    for e in engines:
        run(e)
