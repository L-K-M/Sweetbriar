"""Run/retry FTS after an extension has been explicitly installed locally."""
import argparse, json
from bench import ROOT,open_engine,timed,summary,file_sizes,total_size

p=argparse.ArgumentParser();p.add_argument('--engine',default='duckdb');p.add_argument('--run',default='artifacts/run1')
a=p.parse_args();run=ROOT/a.run;raw=run/(a.engine+'.json');r=json.loads(raw.read_text());conn=open_engine(a.engine,run/(a.engine+'.db'))
if 'fts_error' in r:r['fts_initial_error']=r.pop('fts_error');r.pop('fts_traceback',None)
r['fts_build_ms'],_=timed(conn.build_fts);r['fts_queries']={}
for term in ['aardvark','project','invoice budget']:
    conn.fts(term)
    r['fts_queries'][term]=summary([timed(lambda:conn.fts(term))[0] for _ in range(20)])
r['fts_freshness_probe']=conn.fts_stale_probe(r['rows_initial']+25001)
r['checkpoint_fts_ms'],_=timed(conn.checkpoint);conn.close()
r['files_with_fts']=file_sizes(run/(a.engine+'.db'));r['size_with_fts_bytes']=total_size(r['files_with_fts'])
raw.write_text(json.dumps(r,indent=2));print('FTS build milliseconds',r['fts_build_ms'],'freshness',r['fts_freshness_probe'],flush=True)
