"""Two readers continue paging for the entire duration of 100 durable writes."""
import argparse,datetime,json,pathlib,shutil,threading,time
from bench import ROOT,open_engine,timed,summary
p=argparse.ArgumentParser();p.add_argument('--run',default='artifacts/run1')
p.add_argument('--out',default='artifacts/sustained-'+datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%SZ'))
args=p.parse_args();source_dir=(ROOT/args.run).resolve();out=(ROOT/args.out).resolve()
assert source_dir.is_relative_to(ROOT) and out.is_relative_to(ROOT)
out.mkdir(parents=True,exist_ok=False);results={}
for name in ['sqlite','duckdb','libsql','lmdb','rocksdb']:
    source=source_dir/(name+('.db' if name in ['sqlite','duckdb','libsql'] else ''))
    path=out/source.name
    if source.is_dir():shutil.copytree(source,path)
    else:shutil.copyfile(source,path)
    writer=open_engine(name,path);barrier=threading.Barrier(3);done=threading.Event();reads=[[],[]];errors=[]
    def reader(slot):
        conn=None
        try:
            conn=open_engine(name,path,reader=True,shared=writer if name in ['duckdb','lmdb','rocksdb'] else None)
            barrier.wait();j=0
            while not done.is_set():
                d=1700000000000+10000+(j*613)%80000
                ms,rows=timed(lambda:conn.page(d,3000000));assert len(rows)==50
                reads[slot].append(ms);j+=1
        except BaseException as e:errors.append(repr(e));done.set();barrier.abort()
        finally:
            if conn:conn.close()
    workers=[threading.Thread(target=reader,args=(i,)) for i in range(2)]
    for w in workers:w.start()
    barrier.wait();start=time.perf_counter();writes=[]
    try:
        for i in range(100):
            ms,_=timed(lambda:writer.change(60001+i,100000+i));writes.append(ms)
    finally:done.set()
    duration=time.perf_counter()-start
    for w in workers:w.join()
    assert not errors,errors
    writer.close()
    results[name]={'writer_duration_s':duration,'reader_pages':summary(reads[0]+reads[1]),
                   'writer_commits':summary(writes),'reader_counts':list(map(len,reads)),'errors':errors,
                   'note':'copy of '+str(source_dir.relative_to(ROOT))+'; any Arrow-experiment rows are retained'}
    (out/'results.json').write_text(json.dumps(results,indent=2))
    print(name,round(duration,3),results[name]['reader_pages']['p95_ms'],results[name]['writer_commits']['p95_ms'],flush=True)
