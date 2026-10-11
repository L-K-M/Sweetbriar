"""Synthetic metadata pagination benchmark; not a Sweetbriar or macOS benchmark."""
from pathlib import Path
import json, math, platform, sqlite3, statistics, time

root = Path(__file__).resolve().parent
out = root / 'bench-data'
out.mkdir(exist_ok=True)
db = out / 'metadata.sqlite'
connection = sqlite3.connect(db)
connection.execute('PRAGMA page_size=4096')
connection.execute('PRAGMA journal_mode=WAL')
connection.execute('PRAGMA synchronous=FULL')
connection.execute('PRAGMA cache_size=-32768')
connection.execute('PRAGMA mmap_size=0')
connection.execute('CREATE TABLE IF NOT EXISTS summary(mailbox_id INTEGER NOT NULL, message_id INTEGER NOT NULL, received_at INTEGER NOT NULL, sender TEXT NOT NULL, subject TEXT NOT NULL, flags INTEGER NOT NULL, PRIMARY KEY(mailbox_id,message_id)) WITHOUT ROWID')
if connection.execute('SELECT count(*) FROM summary').fetchone()[0] != 1_000_000:
    connection.execute('DELETE FROM summary')
    connection.executemany('INSERT INTO summary VALUES(?,?,?,?,?,?)', ((1, i, 1700000000+i//4, f'sender{i%1000}@example.invalid', f'Synthetic message {i}', i%8) for i in range(1,1_000_001)))
    connection.commit()
connection.execute('CREATE INDEX IF NOT EXISTS by_date ON summary(mailbox_id,received_at DESC,message_id DESC,sender,subject,flags)')
connection.commit()
connection.execute('ANALYZE')
connection.execute('PRAGMA wal_checkpoint(TRUNCATE)')
select = 'SELECT message_id,received_at,sender,subject,flags FROM summary '
queries = {
    'first_100': (select+'WHERE mailbox_id=1 ORDER BY received_at DESC,message_id DESC LIMIT 100', ()),
    'offset_900000': (select+'WHERE mailbox_id=1 ORDER BY received_at DESC,message_id DESC LIMIT 100 OFFSET 900000', ()),
    'keyset_same_page': (select+'WHERE mailbox_id=1 AND (received_at,message_id)<(?,?) ORDER BY received_at DESC,message_id DESC LIMIT 100', (1700000000+100001//4,100001)),
}
results={}
for name,(sql,params) in queries.items():
    for _ in range(5): connection.execute(sql,params).fetchall()
    samples=[]
    for _ in range(50):
        start=time.perf_counter_ns()
        rows=connection.execute(sql,params).fetchall()
        samples.append((time.perf_counter_ns()-start)/1e6)
    sorted_samples=sorted(samples)
    results[name]={'median_ms':statistics.median(samples),'p95_ms':sorted_samples[math.ceil(.95*len(samples))-1],'max_ms':max(samples),'n':len(samples),'rows':len(rows),'first_id':rows[0][0],'query_plan':connection.execute('EXPLAIN QUERY PLAN '+sql,params).fetchall()}
cpu='unrecorded'
cpuinfo=Path('/proc/cpuinfo')
if cpuinfo.exists():
    for line in cpuinfo.read_text().splitlines():
        if line.startswith('model name'):
            cpu=line.split(':',1)[1].strip()
            break
result={'date':'2026-10-11','scope':'Synthetic metadata only, Linux shared host; warm repetitions, OS cache not flushed; helpers running concurrently; not a macOS, end-to-end UI, durability, or cold-cache benchmark','python':platform.python_version(),'sqlite':sqlite3.sqlite_version,'platform':platform.system()+' '+platform.release(),'architecture':platform.machine(),'cpu':cpu,'rows':1_000_000,'page_size':4096,'cache_size_kib':32768,'mmap_bytes':0,'journal_mode':'WAL','synchronous':'FULL','database_bytes':db.stat().st_size,'queries':results}
connection.close()
(root/'bench-results.json').write_text(json.dumps(result,indent=2)+'\n')
print(json.dumps(result,indent=2))
