"""Recovery trials on copies of completed large SQL benchmark stores.

Every cycle checks exact id/journal sets and fresh record contents. SQLite
also runs its native integrity/foreign-key checks. Full baseline-table
SHA-256 digests before and after all kills detect changes to existing data.
This remains a process-kill experiment, not a power-cut simulation.
"""
import argparse, hashlib, json, os, pathlib, shutil, signal, subprocess, sys, time
from bench import ROOT, open_engine, row

NEW_ID = 3_000_000

def table_digest(conn, table):
    order = 'txn,id' if table == 'intents' else 'id'
    cursor = conn.execute('SELECT * FROM '+table+' WHERE id<? ORDER BY '+order, (NEW_ID,))
    digest = hashlib.sha256(); count = 0
    if conn.name == 'duckdb':
        def records():
            while True:
                batch = cursor.fetchmany(4096)
                if not batch: break
                yield from batch
        stream = records()
    else:
        stream = cursor
    for record in stream:
        digest.update(json.dumps(record,ensure_ascii=False,separators=(',',':')).encode())
        digest.update(b'\n'); count += 1
    return {'rows':count,'sha256':digest.hexdigest()}

def main():
    p=argparse.ArgumentParser();p.add_argument('--engine',choices=['sqlite','duckdb'],required=True)
    p.add_argument('--run',default='artifacts/run1');p.add_argument('--out',default='artifacts/crash-large')
    p.add_argument('--cycles',type=int,default=30);args=p.parse_args()
    source=(ROOT/args.run/(args.engine+'.db')).resolve();out=(ROOT/args.out).resolve()
    assert source.is_relative_to(ROOT) and out.is_relative_to(ROOT)
    out.mkdir(parents=True,exist_ok=True);path=out/source.name;assert not path.exists()
    for suffix in ['-wal','.wal']:
        wal=pathlib.Path(str(source)+suffix);assert not wal.exists() or wal.stat().st_size==0
    shutil.copyfile(source,path);conn=open_engine(args.engine,path)
    base_ids={r[0] for r in conn.query('SELECT id FROM messages')}
    base_members={r[0] for r in conn.query('SELECT id FROM memberships')}
    base_journal=set(conn.query('SELECT txn,id,op FROM intents'))
    assert base_ids==base_members and max(base_ids)<NEW_ID
    baseline={t:table_digest(conn,t) for t in ['messages','memberships','intents']}
    conn.close();expected=set();journal=set();cycles=[]
    result={'engine':args.engine,'source':str(source.relative_to(ROOT)),
            'initial_messages':len(base_ids),'baseline_digests':baseline,'cycles':cycles,
            'scope':'SIGKILL before commit on a copy of a completed 500k benchmark store'}
    target=out/(args.engine+'.json');witness=out/(args.engine+'_witness.jsonl')
    for cycle in range(1,args.cycles+1):
        proc=subprocess.Popen([sys.executable,str(ROOT/'crash_test.py'),'--worker','--engine',args.engine,
            '--path',str(path),'--start',str(NEW_ID+cycle*1000),'--cycle',str(cycle)],
            stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,cwd=ROOT)
        pending=None
        for line in proc.stdout:
            event=json.loads(line)
            if event['event']=='committed':
                with witness.open('a') as f:
                    f.write(json.dumps({'cycle':cycle,**event})+'\n');f.flush();os.fsync(f.fileno())
                expected.update(event['ids']);journal.update((event['txn'],i,'fetch') for i in event['ids'])
            else:
                pending=event;proc.kill();break
        proc.wait(timeout=10);err=proc.stderr.read()
        assert pending and proc.returncode==-signal.SIGKILL,(proc.returncode,err)
        start=time.perf_counter();conn=open_engine(args.engine,path)
        recovery_ms=(time.perf_counter()-start)*1000
        ids={r[0] for r in conn.query('SELECT id FROM messages')}
        members={r[0] for r in conn.query('SELECT id FROM memberships')}
        found_journal=set(conn.query('SELECT txn,id,op FROM intents'))
        assert ids==base_ids|expected and members==ids
        assert found_journal==base_journal|journal
        assert not ids.intersection(pending['ids'])
        for ident,content_hash,body in conn.query('SELECT id,content_hash,body FROM messages WHERE id>=?',(NEW_ID,)):
            record=row(ident);assert (content_hash,body)==(record[1],record[2])
        for actual in conn.query('SELECT * FROM memberships WHERE id>=?',(NEW_ID,)):
            r=row(actual[0]);assert actual==(r[0],r[0],1,r[3],r[4],r[5],r[6],42,r[0])
        if args.engine=='sqlite':
            assert conn.query('PRAGMA integrity_check')==[('ok',)]
            assert not conn.query('PRAGMA foreign_key_check')
            integrity='SQLite integrity_check + foreign_key_check + exact application sets/content'
        else:
            conn.query('SELECT sum(length(body)),sum(length(content_hash)) FROM messages')
            integrity='full scans + exact application sets/content; no comprehensive native checker'
        conn.close();cycles.append({'cycle':cycle,'phase':pending['phase'],
            'acknowledged_new_rows':len(expected),'retained_messages':len(ids),
            'open_recovery_ms':recovery_ms,'integrity':integrity,'missing_rows':0,'partial_transactions':0})
        target.write_text(json.dumps(result,indent=2));print(args.engine,'large cycle',cycle,'ok',flush=True)
    conn=open_engine(args.engine,path)
    final_digests={t:table_digest(conn,t) for t in ['messages','memberships','intents']}
    assert final_digests==baseline;conn.close()
    result.update(passed=True,acknowledged_rows=len(expected),final_messages=len(base_ids)+len(expected),
                  final_baseline_digests=final_digests)
    target.write_text(json.dumps(result,indent=2))

if __name__=='__main__':main()
