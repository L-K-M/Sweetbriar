"""Download release source archives for inspection, without a compiler or install."""
import hashlib,json,pathlib,tarfile,urllib.request
from concurrent.futures import ThreadPoolExecutor
ROOT=pathlib.Path(__file__).resolve().parent
OUT=ROOT/'sources/crates';OUT.mkdir(exist_ok=True)
def run(item):
    name,version=item;url=f'https://crates.io/api/v1/crates/{name}/{version}/download'
    with urllib.request.urlopen(url,timeout=40) as r:data=r.read();final=r.url
    path=OUT/(name+'-'+version+'.crate');path.write_bytes(data)
    with tarfile.open(path) as tar:
        for member in tar.getmembers():
            if not member.isfile():continue
            dest=(OUT/member.name).resolve()
            assert dest.is_relative_to(OUT)
            dest.parent.mkdir(parents=True,exist_ok=True)
            dest.write_bytes(tar.extractfile(member).read())
    record={'url':url,'final_url':final,'date':'2026-10-11','sha256':hashlib.sha256(data).hexdigest(),'path':str(path.relative_to(ROOT))}
    return record
with ThreadPoolExecutor(max_workers=4) as ex:
    records=list(ex.map(run,[('redb','4.3.0'),('sanakirja','2.0.0-beta.3'),('fjall','3.1.12'),('libsqlite3-sys','0.38.1')]))
(OUT/'manifest.json').write_text(json.dumps(records,indent=2));print(json.dumps(records,indent=2))
