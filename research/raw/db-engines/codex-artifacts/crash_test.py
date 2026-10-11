"""Deterministic SIGKILL recovery tests with an fsynced parent commit witness.

Not a power-cut, kernel-crash, disk-full, torn-sector, or fsync-failure simulator.
Every kill occurs with a live transaction paused before commit.
"""
import argparse, hashlib, json, os, pathlib, random, signal, subprocess, sys, time
from bench import ROOT, SQL, KV, open_engine, row, encode, decode, msgkey, listkey, remkey, journalkey

def worker(name,path,start,cycle):
    conn=open_engine(name,path)
    # These completed transactions must survive all subsequent process kills.
    for j in range(3):
        rows=[row(start+j*20+k) for k in range(20)]
        if isinstance(conn,SQL):conn.begin()
        conn.insert_rows(rows,cycle*100+j)
        if isinstance(conn,SQL):conn.commit()
        print(json.dumps({'event':'committed','ids':[r[0] for r in rows],'txn':cycle*100+j}),flush=True)
    ids=[start+60+i for i in range(250)]
    phase=cycle%3
    if isinstance(conn,SQL):
        conn.begin()
        # Vary boundaries: effect only; half the transaction; all SQL writes before COMMIT.
        count=[1,125,250][phase]
        rows=[row(i) for i in ids[:count]]
        if phase==0:
            conn._insert('messages',[(r[0],r[1],r[2]) for r in rows])
        else:conn.insert_rows(rows,cycle*100+99)
        print(json.dumps({'event':'uncommitted','ids':ids,'phase':phase}),flush=True)
        time.sleep(30)
    elif name=='lmdb':
        with conn.db.begin(write=True) as tx:
            for i in ids[:[1,125,250][phase]]:
                r=row(i);tx.put(msgkey(i),encode(r))
                if phase!=0:
                    tx.put(listkey(r),encode([r[0],r[3],r[4],r[5],r[6]]));tx.put(remkey(r),i.to_bytes(8,'big'));tx.put(journalkey(cycle*100+99,i),b'fetch')
            print(json.dumps({'event':'uncommitted','ids':ids,'phase':phase}),flush=True)
            time.sleep(30)
    else:
        from rocksdict import WriteBatch
        wb=WriteBatch(raw_mode=True)
        for i in ids:
            r=row(i);wb[msgkey(i)]=encode(r);wb[listkey(r)]=encode([r[0],r[3],r[4],r[5],r[6]])
            wb[remkey(r)]=i.to_bytes(8,'big');wb[journalkey(cycle*100+99,i)]=b'fetch'
        # Submit a larger native WriteBatch. Parent kills at varied offsets around
        # db.write(): recovery may retain the entire unwitnessed batch or none.
        ids=[start+60+i for i in range(500)]
        wb=WriteBatch(raw_mode=True)
        for i in ids:
            r=row(i);wb[msgkey(i)]=encode(r);wb[listkey(r)]=encode([r[0],r[3],r[4],r[5],r[6]])
            wb[remkey(r)]=i.to_bytes(8,'big');wb[journalkey(cycle*100+99,i)]=b'fetch'
        print(json.dumps({'event':'uncommitted','ids':ids,'phase':'around-native-WriteBatch-write'}),flush=True)
        conn.db.write(wb)
        time.sleep(30)

def read_state(conn):
    if isinstance(conn,SQL):
        ids={r[0] for r in conn.query('SELECT id FROM messages')}
        members={r[0] for r in conn.query('SELECT id FROM memberships')}
        intents={r[0] for r in conn.query('SELECT id FROM intents')}
        if conn.name!='duckdb':
            check=conn.query('PRAGMA integrity_check');fk=conn.query('PRAGMA foreign_key_check')
            assert check==[('ok',)] and not fk,(check,fk)
        else:
            check='no full native integrity-check command; full scans + exact data/hash/membership/intent checks'
            conn.query('SELECT sum(length(body)),sum(length(content_hash)) FROM messages')
            for ident,content_hash,body in conn.query('SELECT id,content_hash,body FROM messages'):
                r=row(ident);assert content_hash==r[1] and body==r[2]
    else:
        ids=set();members=set();intents=set();locators=set()
        tx=conn.db.begin() if conn.name=='lmdb' else None
        it=tx.cursor() if tx else conn.db.items()
        for k,v in it:
            if k[:1]==b'm':
                r=decode(v);assert encode(row(r[0]))==v;ids.add(r[0])
            elif k[:1]==b'l':members.add(decode(v)[0])
            elif k[:1]==b'j':intents.add(int.from_bytes(k[-8:],'big'))
            elif k[:1]==b'r':locators.add(int.from_bytes(v,'big'))
        del it
        if tx:tx.abort()
        assert locators==ids
        check='full application key/data traversal; no standalone native checker exposed by wheel'
    assert ids==members==intents,(len(ids),len(members),len(intents))
    return ids,check

def main():
    p=argparse.ArgumentParser();p.add_argument('--engine',required=True);p.add_argument('--cycles',type=int,default=30)
    p.add_argument('--out',default='artifacts/crash');p.add_argument('--worker',action='store_true')
    p.add_argument('--path');p.add_argument('--start',type=int);p.add_argument('--cycle',type=int)
    args=p.parse_args()
    if args.worker:return worker(args.engine,pathlib.Path(args.path),args.start,args.cycle)
    out=(ROOT/args.out).resolve();assert out.is_relative_to(ROOT);out.mkdir(parents=True,exist_ok=True)
    path=out/(args.engine+('.db' if args.engine in ['sqlite','duckdb','libsql'] else ''))
    conn=open_engine(args.engine,path);conn.setup()
    if isinstance(conn,SQL):conn.execute('CREATE INDEX mailbox_idx ON memberships(mailbox,date DESC,id DESC,flags,subject,sender)')
    conn.close();expected=set();result={'engine':args.engine,'cycles':[], 'scope':'SIGKILL with pre-commit transaction paused; parent fsynced acknowledged-commit witness'}
    witness=out/(args.engine+'_witness.jsonl')
    for cycle in range(1,args.cycles+1):
        start=cycle*1000
        proc=subprocess.Popen([sys.executable,str(ROOT/'crash_test.py'),'--worker','--engine',args.engine,
                               '--path',str(path),'--start',str(start),'--cycle',str(cycle)],
                              stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,cwd=ROOT)
        acknowledged=[];pending=None
        for line in proc.stdout:
            event=json.loads(line)
            if event['event']=='committed':
                with witness.open('a') as f:f.write(json.dumps({'cycle':cycle,**event})+'\n');f.flush();os.fsync(f.fileno())
                expected.update(event['ids']);acknowledged+=event['ids']
            else:
                pending=event
                if args.engine=='rocksdb':time.sleep([0,.001,.003,.01,.03][cycle%5])
                proc.kill();break
        proc.wait(timeout=10)
        err=proc.stderr.read()
        assert pending is not None and proc.returncode==-signal.SIGKILL,(proc.returncode,err)
        t=time.perf_counter();conn=open_engine(args.engine,path);open_ms=(time.perf_counter()-t)*1000
        found,check=read_state(conn)
        assert not expected-found,{'missing':sorted(expected-found)}
        unknown=found-expected
        if args.engine=='rocksdb':
            assert unknown in (set(),set(pending['ids'])),{'partial':len(unknown)}
            expected.update(unknown)
        else:
            assert not unknown,{'unexpected':sorted(unknown)}
            assert not found.intersection(pending['ids'])
        # Reopen/write on next iteration exercises recovered index maintenance too.
        conn.close()
        result['cycles'].append({'cycle':cycle,'new_acknowledged_rows':len(acknowledged),
            'total_acknowledged_rows':len(expected),'phase':pending['phase'],'open_recovery_ms':open_ms,
            'unwitnessed_atomic_batch_rows':len(unknown),'integrity_check':check,'missing_rows':0,'partial_transactions':0})
        (out/(args.engine+'.json')).write_text(json.dumps(result,indent=2))
        print(args.engine,'cycle',cycle,'ok',flush=True)
    result['passed']=True;result['acknowledged_rows']=args.cycles*60;result['total_recovered_rows']=len(expected)
    (out/(args.engine+'.json')).write_text(json.dumps(result,indent=2))

if __name__=='__main__':main()
