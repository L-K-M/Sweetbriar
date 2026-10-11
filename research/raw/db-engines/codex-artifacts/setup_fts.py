"""Explicitly download the signed extension into the job, with provenance."""
import datetime,gzip,hashlib,json,pathlib,urllib.request
ROOT=pathlib.Path(__file__).resolve().parent
OUT=ROOT/'artifacts/extensions';OUT.mkdir(parents=True,exist_ok=True)
errors=[]
for protocol in ['https','http']:
    url=f'{protocol}://extensions.duckdb.org/v1.5.6/linux_amd64/fts.duckdb_extension.gz'
    try:
        request=urllib.request.Request(url,headers={'User-Agent':'Mozilla/5.0'})
        with urllib.request.urlopen(request,timeout=20) as r:data=r.read()
        expanded=gzip.decompress(data)
        (OUT/'fts.duckdb_extension.gz').write_bytes(data)
        (OUT/'fts.duckdb_extension').write_bytes(expanded)
        (OUT/'download.json').write_text(json.dumps({'date':datetime.datetime.now(datetime.timezone.utc).isoformat(),
            'url':url,'compressed_bytes':len(data),'bytes':len(expanded),
            'sha256':hashlib.sha256(expanded).hexdigest(),'errors':errors,
            'verification':'LOAD uses default signature checking; allow_unsigned_extensions is not enabled'},indent=2))
        print('Saved extension',len(expanded),'bytes');break
    except Exception as e:errors.append({'url':url,'error':repr(e)})
else:raise RuntimeError(errors)
