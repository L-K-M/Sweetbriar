"""Driver sensitivity: same normalized 250-row inserts, prepared Arrow inputs.

Use on a completed benchmark DB; extra rows start at id 2,000,000. Results
are separate from the original VALUES benchmark and exclude Arrow conversion.
"""
import argparse, json, pathlib, time
import pyarrow as pa
from bench import ROOT,open_engine,row,timed,summary

p=argparse.ArgumentParser()
p.add_argument('--run',default='artifacts/run1')
p.add_argument('--output',default='artifacts/duckdb_arrow_batches.json')
args=p.parse_args()
path=(ROOT/args.run/'duckdb.db').resolve();target=(ROOT/args.output).resolve()
assert path.is_relative_to(ROOT) and target.is_relative_to(ROOT)
target.parent.mkdir(parents=True,exist_ok=True)
conn=open_engine('duckdb',path)
start_id=max(2000000,conn.query('SELECT coalesce(max(id),0)+1000 FROM messages')[0][0])
start_txn=max(50000,conn.query('SELECT coalesce(max(txn),0)+1 FROM intents')[0][0])
def arrow_data(rows,txn):
    return [
      ('messages',pa.table({'id':[r[0] for r in rows],'content_hash':[r[1] for r in rows],'body':[r[2] for r in rows]})),
      ('memberships',pa.table({'id':[r[0] for r in rows],'message_id':[r[0] for r in rows],
        'mailbox':[1]*250,'date':[r[3] for r in rows],'flags':[r[4] for r in rows],
        'subject':[r[5] for r in rows],'sender':[r[6] for r in rows],'uidvalidity':[42]*250,'uid':[r[0] for r in rows]})),
      ('intents',pa.table({'txn':[txn]*250,'id':[r[0] for r in rows],'op':['fetch']*250}))]
inputs=[arrow_data([row(start_id+j*250+k) for k in range(250)],start_txn+j) for j in range(100)]
times=[]
for data in inputs:
    t=time.perf_counter();conn.begin()
    for table,arrow in data:
        conn.c.register('input_rows',arrow);conn.execute('INSERT INTO '+table+' SELECT * FROM input_rows');conn.c.unregister('input_rows')
    conn.commit();times.append((time.perf_counter()-t)*1000)
result={'engine':'duckdb','rows':25000,'method':'registered Arrow tables -> INSERT SELECT; table conversion excluded',
        'transactions_250':summary(times),'pyarrow':pa.__version__,
        'input_database':str(path.relative_to(ROOT)),'start_id':start_id,'start_txn':start_txn}
conn.checkpoint();conn.close()
target.write_text(json.dumps(result,indent=2))
print({k:v for k,v in result['transactions_250'].items() if k!='samples_ms'},flush=True)
