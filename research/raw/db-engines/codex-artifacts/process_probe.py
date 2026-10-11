"""Cross-process live-writer read-only access, distinct from thread readers."""
import argparse,datetime,json,pathlib,subprocess,sys
from bench import ROOT,open_engine,SQL

p=argparse.ArgumentParser()
p.add_argument('--out',default='artifacts/process-probe-'+datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%SZ'))
args=p.parse_args();out=(ROOT/args.out).resolve();assert out.is_relative_to(ROOT)
out.mkdir(parents=True,exist_ok=False);results={}
for name in ['sqlite','duckdb','libsql','lmdb']:
    path=out/(name+('.db' if name in ['sqlite','duckdb','libsql'] else ''))
    conn=open_engine(name,path);conn.setup()
    if isinstance(conn,SQL):
        conn.begin();conn.execute("INSERT INTO messages VALUES (1,'hash','body')");conn.commit();conn.begin()
        conn.execute("INSERT INTO messages VALUES (2,'uncommitted','body')")
    else:
        conn.batch([(b'key',b'value')])
    if name=='sqlite':code="import apsw; c=apsw.Connection(PATH,flags=apsw.SQLITE_OPEN_READONLY); print(c.execute('SELECT count(*) FROM messages').fetchall())"
    elif name=='duckdb':code="import duckdb; c=duckdb.connect(PATH,read_only=True); print(c.execute('SELECT count(*) FROM messages').fetchall())"
    elif name=='libsql':code="import libsql_experimental as libsql; c=libsql.connect('file:'+PATH+'?mode=ro',uri=True,isolation_level=None); print(c.execute('SELECT count(*) FROM messages').fetchall())"
    else:code="import lmdb; c=lmdb.open(PATH,readonly=True,lock=True); tx=c.begin(); print(tx.get(b'key'))"
    r=subprocess.run([sys.executable,'-c','PATH='+repr(str(path))+';'+code],capture_output=True,text=True,cwd=ROOT,timeout=20)
    results[name]={'writer_open':True,'read_only_child_returncode':r.returncode,'stdout':r.stdout,'stderr':r.stderr}
    if isinstance(conn,SQL):conn.execute('ROLLBACK')
    conn.close()
(out/'results.json').write_text(json.dumps(results,indent=2));print(json.dumps(results,indent=2))
