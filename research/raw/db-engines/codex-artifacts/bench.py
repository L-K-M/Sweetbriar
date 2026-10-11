"""Mail-shaped durable-storage benchmark. All output stays below this directory.

Run: .venv/bin/python bench.py --engine sqlite --rows 500000 --out artifacts/run1
The five engines run sequentially. SQL and KV results are deliberately separated.
"""
import argparse, csv, hashlib, importlib.metadata, json, math, os, pathlib
import platform, random, resource, statistics, struct, subprocess, sys, threading, time, traceback

ROOT = pathlib.Path(__file__).resolve().parent
(ROOT/'.cache/tmp').mkdir(parents=True,exist_ok=True)
os.environ['SQLITE_TMPDIR']=str(ROOT/'.cache/tmp')
os.environ['TMPDIR']=str(ROOT/'.cache/tmp')
MAX = (1 << 63) - 1
PAGE_SQL = ('SELECT id,date,flags,subject,sender FROM memberships '
            'WHERE mailbox=1 AND (date,id)<(?,?) ORDER BY date DESC,id DESC LIMIT 50')
INDEX_SQL = 'CREATE INDEX mailbox_cover ON memberships(mailbox,date DESC,id DESC,flags,subject,sender)'
SCHEMA = [
    'CREATE TABLE messages(id INTEGER PRIMARY KEY,content_hash TEXT NOT NULL,body TEXT NOT NULL)',
    'CREATE TABLE memberships(id INTEGER PRIMARY KEY,message_id INTEGER NOT NULL REFERENCES messages(id),'
    'mailbox INTEGER NOT NULL,date INTEGER NOT NULL,flags INTEGER NOT NULL,subject TEXT NOT NULL,'
    'sender TEXT NOT NULL,uidvalidity INTEGER NOT NULL,uid INTEGER NOT NULL,UNIQUE(mailbox,uidvalidity,uid))',
    'CREATE TABLE intents(txn INTEGER NOT NULL,id INTEGER NOT NULL,op TEXT NOT NULL,PRIMARY KEY(txn,id))'
]

def row(i):
    date = 1700000000000 + ((i * 7919) % 100000)
    words = ['hello','sweetbriar','message','discussion','regards','team',
             ['meeting','invoice','project','travel','release'][i % 5]]
    if i % 97 == 0: words += ['budget','invoice']
    if i % 503 == 0: words += ['aardvark']
    words += ['token' + chr(97 + (i // 7) % 26), 'topic' + chr(97 + (i // 19) % 26)]
    body = ' '.join(words * 3)
    return [i, hashlib.sha256(('original-mail-'+str(i)).encode()).hexdigest(), body,
            date, 0, 'Discussion '+str(i % 10000), 'person'+str(i % 1000)+'@example.invalid']

def encode(r): return json.dumps(r,separators=(',',':')).encode()
def decode(b): return json.loads(b)
def msgkey(i): return b'm'+struct.pack('>Q',i)
def listkey(r): return b'l'+struct.pack('>QQQ',1,MAX-r[3],MAX-r[0])
def remkey(r): return b'r'+struct.pack('>QQQ',1,42,r[0])
def journalkey(txn,i): return b'j'+struct.pack('>QQ',txn,i)

def summary(samples):
    s = sorted(samples)
    return {'n':len(s),'p50_ms':statistics.median(s),'p95_ms':s[min(len(s)-1,math.ceil(.95*len(s))-1)],
            'mean_ms':statistics.mean(s),'min_ms':s[0],'max_ms':s[-1],'samples_ms':samples}

def timed(fn):
    t = time.perf_counter(); result = fn(); return (time.perf_counter()-t)*1000, result

def file_sizes(path):
    files = list(path.rglob('*')) if path.is_dir() else list(path.parent.glob(path.name+'*'))
    return {str(p.relative_to(ROOT)):{'bytes':p.stat().st_size,'allocated_bytes':p.stat().st_blocks*512}
            for p in files if p.is_file()}

def total_size(files): return sum(v['bytes'] for v in files.values())

class SQL:
    def __init__(self,name,path,reader=False,shared=None):
        self.name=name; self.path=path
        if name=='sqlite':
            import apsw
            self.c=apsw.Connection(str(path))
            self.c.set_busy_timeout(10000)
            if not reader: self.c.execute('PRAGMA journal_mode=WAL').fetchall()
            for stmt in ['PRAGMA synchronous=FULL','PRAGMA foreign_keys=ON',
                         'PRAGMA fullfsync=ON','PRAGMA checkpoint_fullfsync=ON','PRAGMA cache_size=-65536']:
                self.c.execute(stmt)
            for name_ in ('SQLITE_DBCONFIG_DQS_DML','SQLITE_DBCONFIG_DQS_DDL'):
                self.c.config(getattr(apsw,name_),False)
            if reader: self.c.execute('PRAGMA query_only=ON')
        elif name=='duckdb':
            import duckdb
            self.c=shared.c.cursor() if shared else duckdb.connect(str(path),config={
                'threads':4,'memory_limit':'1GB','extension_directory':str(ROOT/'artifacts/extensions'),
                'temp_directory':str(path.parent/(path.name+'.tmp')),
                'autoinstall_known_extensions':False,'autoload_known_extensions':False})
        elif name=='libsql':
            import libsql_experimental
            self.c=libsql_experimental.connect(str(path),isolation_level=None,check_same_thread=False)
            for stmt in ['PRAGMA journal_mode=WAL','PRAGMA synchronous=FULL','PRAGMA foreign_keys=ON',
                         'PRAGMA fullfsync=ON','PRAGMA checkpoint_fullfsync=ON','PRAGMA cache_size=-65536']:
                self.c.execute(stmt).fetchall()
            if reader: self.c.execute('PRAGMA query_only=ON')
    def execute(self,sql,args=()): return self.c.execute(sql,args)
    def query(self,sql,args=()): return self.execute(sql,args).fetchall()
    def begin(self): self.execute('BEGIN TRANSACTION' if self.name=='duckdb' else 'BEGIN IMMEDIATE')
    def commit(self): self.execute('COMMIT')
    def setup(self):
        for stmt in SCHEMA:
            self.execute(stmt.replace('INTEGER','BIGINT') if self.name=='duckdb' else stmt+' STRICT')
    def insert_rows(self,rows,txn=None):
        values1=[(r[0],r[1],r[2]) for r in rows]
        values2=[(r[0],r[0],1,r[3],r[4],r[5],r[6],42,r[0]) for r in rows]
        self._insert('messages',values1)
        self._insert('memberships',values2)
        if txn is not None: self._insert('intents',[(txn,r[0],'fetch') for r in rows])
    def _insert(self,table,values):
        if self.name=='duckdb':
            width=len(values[0]); params=','.join(['('+','.join(['?']*width)+')']*len(values))
            self.execute('INSERT INTO '+table+' VALUES '+params,[x for r in values for x in r])
        else:
            self.c.executemany('INSERT INTO '+table+' VALUES ('+','.join(['?']*len(values[0]))+')',values)
    def bulk(self,csvpath,n):
        self.begin()
        if self.name=='duckdb':
            self.execute('CREATE TEMP TABLE staging(id BIGINT,content_hash VARCHAR,body VARCHAR,date BIGINT,flags BIGINT,subject VARCHAR,sender VARCHAR)')
            # csvpath is generated here, not user input.
            self.execute("COPY staging FROM '"+str(csvpath).replace("'","''")+"' (HEADER false)")
            self.execute('INSERT INTO messages SELECT id,content_hash,body FROM staging')
            self.execute('INSERT INTO memberships SELECT id,id,1,date,flags,subject,sender,42,id FROM staging')
            self.execute('DROP TABLE staging')
        else:
            with csvpath.open() as f:
                batch=[]
                for r in csv.reader(f):
                    r[0]=int(r[0]);r[3]=int(r[3]);r[4]=int(r[4]);batch.append(r)
                    if len(batch)==5000: self.insert_rows(batch);batch=[]
                if batch:self.insert_rows(batch)
        self.commit()
    def point(self,i):
        r=self.query('SELECT content_hash,body FROM messages WHERE id=?',(i,))
        assert len(r)==1
        return r[0]
    def page(self,date,i): return self.query(PAGE_SQL,(date,i))
    def change(self,i,txn):
        self.begin();self.execute('UPDATE memberships SET flags=flags|1 WHERE id=?',(i,))
        self.execute('INSERT INTO intents VALUES (?,?,?)',(txn,i,'read'));self.commit()
    def mark(self,txn):
        self.begin();self.execute('UPDATE memberships SET flags=flags|1 WHERE id BETWEEN 1 AND 10000')
        self.execute("INSERT INTO intents SELECT ?,id,'read' FROM memberships WHERE id BETWEEN 1 AND 10000",(txn,));self.commit()
    def checkpoint(self):
        self.query('CHECKPOINT' if self.name=='duckdb' else 'PRAGMA wal_checkpoint(TRUNCATE)')
    def close(self): self.c.close()
    def verify(self,n):
        counts={t:self.query('SELECT count(*) FROM '+t)[0][0] for t in ['messages','memberships','intents']}
        assert counts['messages']==n and counts['memberships']==n,counts
        if self.name!='duckdb':
            checks=self.query('PRAGMA integrity_check'); fks=self.query('PRAGMA foreign_key_check')
            assert checks==[('ok',)] and not fks,(checks,fks)
        else:
            checks='No SQLite-equivalent PRAGMA integrity_check; full row/count/referential scan plus checkpoint/reopen'
            assert not self.query('SELECT id FROM memberships WHERE message_id NOT IN(SELECT id FROM messages) LIMIT 1')
            self.query('SELECT sum(id),sum(length(content_hash)),sum(length(body)) FROM messages')
            self.query('SELECT sum(id),sum(date),sum(flags),sum(length(subject)),sum(length(sender)) FROM memberships')
        return {'counts':counts,'integrity_check':checks}
    def build_fts(self):
        if self.name=='duckdb':
            local=ROOT/'artifacts/extensions/fts.duckdb_extension'
            if local.exists():self.execute("LOAD '"+str(local).replace("'","''")+"'")
            else:
                self.execute("INSTALL fts FROM 'http://extensions.duckdb.org'");self.execute('LOAD fts')
            self.execute("PRAGMA create_fts_index('messages','id','body',stemmer='porter',stopwords='none')")
        else:
            self.execute("CREATE VIRTUAL TABLE search USING fts5(body,tokenize='porter unicode61')")
            self.begin();self.execute('INSERT INTO search(rowid,body) SELECT id,body FROM messages');self.commit()
    def fts(self,term):
        if self.name=='duckdb':
            return self.query('SELECT id,fts_main_messages.match_bm25(id,?,conjunctive:=1) AS score FROM messages '
                              'WHERE score IS NOT NULL ORDER BY score DESC,id LIMIT 50',(term,))
        return self.query('SELECT rowid,bm25(search) FROM search WHERE search MATCH ? ORDER BY bm25(search),rowid LIMIT 50',(term,))
    def fts_stale_probe(self,ident):
        self.begin();self.insert_rows([[ident,'f'*64,'zzsweetbriarunique',1700000000000,0,'probe','probe@example.invalid']],77777)
        if self.name!='duckdb':self.execute('INSERT INTO search(rowid,body) VALUES (?,?)',(ident,'zzsweetbriarunique'))
        self.commit()
        if self.name=='duckdb':
            return self.query("SELECT id FROM messages WHERE fts_main_messages.match_bm25(id,'zzsweetbriarunique') IS NOT NULL")
        return self.query("SELECT rowid FROM search WHERE search MATCH 'zzsweetbriarunique'")

class KV:
    def __init__(self,name,path,reader=False,shared=None):
        self.name=name;self.path=path
        if shared:self.db=shared.db;self.shared=True;return
        self.shared=False
        if name=='lmdb':
            import lmdb
            path.mkdir(exist_ok=True)
            self.db=lmdb.open(str(path),map_size=8*1024**3,sync=True,metasync=True,writemap=False,max_readers=126)
        else:
            from rocksdict import Rdict,Options,WriteOptions
            opts=Options(raw_mode=True);opts.create_if_missing(True)
            self.db=Rdict(str(path),opts)
            wo=WriteOptions();wo.sync=True;wo.disable_wal=False;self.db.set_write_options(wo)
    def setup(self): pass
    def batch(self,items):
        if self.name=='lmdb':
            with self.db.begin(write=True) as tx:
                for k,v in items:tx.put(k,v)
        else:
            from rocksdict import WriteBatch
            wb=WriteBatch(raw_mode=True)
            for k,v in items:wb[k]=v
            self.db.write(wb)
    def insert_rows(self,rows,txn=None):
        items=[]
        for r in rows:
            b=encode(r);items += [(msgkey(r[0]),b),(listkey(r),encode([r[0],r[3],r[4],r[5],r[6]])),(remkey(r),struct.pack('>Q',r[0]))]
            if txn is not None:items.append((journalkey(txn,r[0]),b'fetch'))
        self.batch(items)
    def bulk(self,csvpath,n):
        # Same logical atomic bulk transaction as SQL; memory used for WriteBatch in RocksDB.
        def gen():
            with csvpath.open() as f:
                for r in csv.reader(f):
                    r[0]=int(r[0]);r[3]=int(r[3]);r[4]=int(r[4]);b=encode(r)
                    yield msgkey(r[0]),b;yield listkey(r),encode([r[0],r[3],r[4],r[5],r[6]])
                    yield remkey(r),struct.pack('>Q',r[0])
        self.batch(gen())
    def point(self,i):
        if self.name=='lmdb':
            with self.db.begin() as tx:b=tx.get(msgkey(i))
        else:b=self.db[msgkey(i)]
        assert b is not None
        r=decode(b);return (r[1],r[2])
    def page(self,date,i):
        start=b'l'+struct.pack('>QQQ',1,MAX-date,MAX-i);result=[]
        if self.name=='lmdb':
            with self.db.begin() as tx:
                with tx.cursor() as cur:
                    if cur.set_range(start):
                        for k,v in cur:
                            if k==start:continue
                            if not k.startswith(b'l'):break
                            result.append(decode(v))
                            if len(result)==50:break
        else:
            snap=self.db.snapshot();it=snap.iter();it.seek(start)
            while it.valid() and it.key().startswith(b'l') and len(result)<50:
                if it.key()!=start:result.append(decode(it.value()))
                it.next()
            del it,snap
        return result
    def change(self,i,txn):
        if self.name=='lmdb':
            with self.db.begin(write=True) as tx:
                r=decode(tx.get(msgkey(i)));r[4]|=1
                tx.put(msgkey(i),encode(r));tx.put(listkey(r),encode([r[0],r[3],r[4],r[5],r[6]]));tx.put(journalkey(txn,i),b'read')
        else:
            r=decode(self.db[msgkey(i)]);r[4]|=1
            self.batch([(msgkey(i),encode(r)),(listkey(r),encode([r[0],r[3],r[4],r[5],r[6]])),(journalkey(txn,i),b'read')])
    def mark(self,txn):
        if self.name=='lmdb':
            with self.db.begin(write=True) as tx:
                for i in range(1,10001):
                    r=decode(tx.get(msgkey(i)));r[4]|=1
                    tx.put(msgkey(i),encode(r));tx.put(listkey(r),encode([r[0],r[3],r[4],r[5],r[6]]));tx.put(journalkey(txn,i),b'read')
        else:
            items=[]
            for i in range(1,10001):
                r=decode(self.db[msgkey(i)]);r[4]|=1
                items +=[(msgkey(i),encode(r)),(listkey(r),encode([r[0],r[3],r[4],r[5],r[6]])),(journalkey(txn,i),b'read')]
            self.batch(items)
    def checkpoint(self):
        if self.name=='lmdb':self.db.sync(True)
        else:self.db.flush();self.db.flush_wal(True)
    def close(self):
        if not self.shared:self.db.close()
    def verify(self,n):
        counts={'m':0,'l':0,'r':0,'j':0};bad=[]
        if self.name=='lmdb':
            tx=self.db.begin();it=tx.cursor()
        else:tx=None;it=self.db.items()
        for k,v in it:
            counts[k[:1].decode()]+=1
            if k[:1]==b'm':
                r=decode(v)
                if msgkey(r[0])!=k:bad.append(r[0])
        del it
        if tx:tx.abort()
        assert all(counts[k]==n for k in ['m','l','r']) and not bad,(counts,bad)
        # Application-level traversal; wheels provide no comprehensive db-check utility.
        return {'counts':counts,'integrity_check':'full key traversal and decoded primary-key check; no standalone native checker exposed by wheel'}

def open_engine(name,path,**kwargs):return SQL(name,path,**kwargs) if name in ['sqlite','duckdb','libsql'] else KV(name,path,**kwargs)

def csv_data(path,n):
    if path.exists():return
    with path.open('w',newline='') as f:
        w=csv.writer(f)
        for i in range(1,n+1):w.writerow(row(i))

def concurrent_test(name,path,writer,n):
    barrier=threading.Barrier(3);errors=[];reads=[[],[]];writes=[]
    def reader(slot):
        try:
            conn=open_engine(name,path,reader=True,shared=writer if name in ['duckdb','lmdb','rocksdb'] else None)
            barrier.wait()
            for j in range(100):
                d=1700000000000+20000+(j*613)%60000
                ms,rows=timed(lambda:conn.page(d,n+1));assert len(rows)==50
                reads[slot].append(ms)
            conn.close()
        except BaseException as e:errors.append(repr(e));barrier.abort()
    threads=[threading.Thread(target=reader,args=(i,)) for i in range(2)]
    for t in threads:t.start()
    barrier.wait()
    for j in range(100):
        ms,_=timed(lambda:writer.change(20001+j,20000+j));writes.append(ms)
    for t in threads:t.join()
    assert not errors,errors
    return {'reader_pages':summary(reads[0]+reads[1]),'writer_commits':summary(writes),'errors':errors}

def benchmark(name,n,out):
    out.mkdir(parents=True,exist_ok=True)
    csvpath=out.parent/('corpus-'+str(n)+'.csv');csv_data(csvpath,n)
    path=out/(name+('.db' if name in ['sqlite','duckdb','libsql'] else ''))
    result={'engine':name,'rows_initial':n,'data_csv':str(csvpath.relative_to(ROOT)),
            'environment':{'date':time.strftime('%Y-%m-%d'),'platform':platform.platform(),'python':platform.python_version(),
                           'cpu_count':os.cpu_count(),'packages':{p:importlib.metadata.version(p) for p in ['duckdb','apsw','lmdb','rocksdict','libsql-experimental','redb']}},
            'durability':'SQLite/libSQL WAL FULL + fullfsync + checkpoint_fullfsync; DuckDB native durable WAL; LMDB sync/metasync true; RocksDB WAL enabled sync true',
            'samples':{}}
    raw=out/(name+'.json')
    def save():raw.write_text(json.dumps(result,indent=2));print(name,list(result.keys())[-1],flush=True)
    conn=open_engine(name,path);conn.setup()
    result['bulk_load_ms'],_=timed(lambda:conn.bulk(csvpath,n));save()
    if isinstance(conn,SQL):result['secondary_index_build_ms'],_=timed(lambda:conn.execute(INDEX_SQL))
    else:result['secondary_index_build_ms']=None
    result['checkpoint_initial_ms'],_=timed(conn.checkpoint)
    result['files_initial']=file_sizes(path);result['size_initial_bytes']=total_size(result['files_initial']);save()
    rng=random.Random(314159)
    # Timed insertion data is prepared outside each transaction.
    batches=[[row(n+j*250+k+1) for k in range(250)] for j in range(100)]
    samples=[]
    for j,rows in enumerate(batches):
        def batch():
            if isinstance(conn,SQL):conn.begin()
            conn.insert_rows(rows,1000+j)
            if isinstance(conn,SQL):conn.commit()
        ms,_=timed(batch);samples.append(ms)
    result['batches_250']=summary(samples);save()
    n+=25000
    samples=[]
    for j in range(500):
        i=rng.randrange(10001,n+1);ms,_=timed(lambda:conn.change(i,3000+j));samples.append(ms)
    result['single_row_commit']=summary(samples);save()
    points=[rng.randrange(1,n+1) for _ in range(1000)]
    for i in points[:20]:conn.point(i)
    result['point_lookup']=summary([timed(lambda:conn.point(i))[0] for i in points]);save()
    cursors=[(1700000000000+rng.randrange(10000,90000),n+1) for _ in range(200)]
    samples=[]
    for d,i in cursors:
        ms,rows=timed(lambda:conn.page(d,i));assert len(rows)==50
        assert all((r[1],r[0])<(d,i) for r in rows)
        assert list(rows)==sorted(rows,key=lambda r:(r[1],r[0]),reverse=True)
        samples.append(ms)
    result['keyset_page_50']=summary(samples)
    if isinstance(conn,SQL):
        result['page_plan']=conn.query(('EXPLAIN ' if name=='duckdb' else 'EXPLAIN QUERY PLAN ')+PAGE_SQL,cursors[0])
    result['mark_10000_ms'],_=timed(lambda:conn.mark(9000));save()
    result['concurrent']=concurrent_test(name,path,conn,n);save()
    result['verify']=conn.verify(n);result['checkpoint_final_ms'],_=timed(conn.checkpoint)
    conn.close()
    result['files_store']=file_sizes(path);result['size_store_bytes']=total_size(result['files_store'])
    opens=[]
    for _ in range(5):
        ms,conn=timed(lambda:open_engine(name,path));opens.append(ms);conn.close()
    result['open_warm_ms']=summary(opens);save()
    # An aggregate illustrates a workload DuckDB was built to accelerate.
    conn=open_engine(name,path)
    if isinstance(conn,SQL):
        result['aggregate_ms'],result['aggregate']=timed(lambda:conn.query('SELECT flags,count(*),sum(date) FROM memberships GROUP BY flags'))
        try:
            result['fts_build_ms'],_=timed(conn.build_fts);result['fts_queries']={}
            for term in ['aardvark','project','invoice budget']:
                conn.fts(term)
                result['fts_queries'][term]=summary([timed(lambda:conn.fts(term))[0] for _ in range(20)])
            result['fts_freshness_probe']=conn.fts_stale_probe(n+1)
            result['checkpoint_fts_ms'],_=timed(conn.checkpoint)
        except Exception as e:result['fts_error']=repr(e);result['fts_traceback']=traceback.format_exc()
    conn.close();result['files_with_fts']=file_sizes(path);result['size_with_fts_bytes']=total_size(result['files_with_fts'])
    result['peak_rss_process_KiB']=resource.getrusage(resource.RUSAGE_SELF).ru_maxrss;save()

def main():
    p=argparse.ArgumentParser();p.add_argument('--engine',choices=['sqlite','duckdb','libsql','lmdb','rocksdb'],required=True)
    p.add_argument('--rows',type=int,default=500000);p.add_argument('--out',default='artifacts/run1');args=p.parse_args()
    out=(ROOT/args.out).resolve();assert out.is_relative_to(ROOT)
    benchmark(args.engine,args.rows,out)

if __name__=='__main__':main()
