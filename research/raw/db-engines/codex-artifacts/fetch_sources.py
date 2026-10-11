"""Fetch consulted primary sources into the job directory; retain provenance."""
import datetime, hashlib, html.parser, json, pathlib, re, sys, urllib.request, urllib.parse

ROOT = pathlib.Path(__file__).resolve().parent
OUT = ROOT / 'sources'
OUT.mkdir(exist_ok=True)

class Plain(html.parser.HTMLParser):
    def __init__(self):
        super().__init__(); self.parts = []; self.skip = 0
    def handle_starttag(self, tag, attrs):
        if tag in ('script', 'style'): self.skip += 1
        if tag in ('p','br','div','li','tr','h1','h2','h3','h4','pre'): self.parts.append('\n')
    def handle_endtag(self, tag):
        if tag in ('script', 'style'): self.skip = max(0, self.skip - 1)
    def handle_data(self, data):
        if not self.skip: self.parts.append(data)

def fetch(url):
    stem = hashlib.sha256(url.encode()).hexdigest()[:16]
    record = {'url':url, 'consulted_at':datetime.datetime.now(datetime.timezone.utc).isoformat()}
    for target in [url, url.replace('https://','http://',1)]:
        try:
            req = urllib.request.Request(target,headers={'User-Agent':'Sweetbriar research/1.0'})
            with urllib.request.urlopen(req,timeout=40) as r:
                data = r.read(); record.update(status=r.status, final_url=r.url, content_type=r.headers.get('content-type'))
            # DuckDB's /stable pages use HTML redirects rather than HTTP redirects.
            for _ in range(4):
                src = data.decode('utf-8', errors='replace')
                m = re.search(r'<meta[^>]*http-equiv=[\"\']refresh[\"\'][^>]*content=[\"\'][^\"\']*?url=([^\"\']+)', src, re.I)
                if not m: break
                redirected = urllib.parse.urljoin(record['final_url'], html.parser.unescape(m.group(1))) if hasattr(html.parser, 'unescape') else urllib.parse.urljoin(record['final_url'], m.group(1))
                with urllib.request.urlopen(urllib.request.Request(redirected,headers={'User-Agent':'Sweetbriar research/1.0'}),timeout=40) as r:
                    data = r.read(); record.update(status=r.status, final_url=r.url, content_type=r.headers.get('content-type'))
            (OUT / (stem+'.source')).write_bytes(data)
            source = data.decode('utf-8',errors='replace')
            if 'html' in (record['content_type'] or ''):
                p = Plain(); p.feed(source); source = '\n'.join(x.strip() for x in ''.join(p.parts).splitlines() if x.strip())
            (OUT / (stem+'.txt')).write_text(source)
            record.update(path='sources/'+stem+'.txt', sha256=hashlib.sha256(data).hexdigest())
            break
        except Exception as e:
            record.setdefault('errors',[]).append({'target':target,'error':str(e)})
    with (OUT/'manifest.jsonl').open('a') as f: f.write(json.dumps(record)+'\n')
    return record

if __name__ == '__main__':
    from concurrent.futures import ThreadPoolExecutor
    urls = sys.argv[1:] if len(sys.argv)>1 else [x.strip() for x in sys.stdin if x.strip()]
    with ThreadPoolExecutor(max_workers=8) as ex:
        for result in ex.map(fetch,urls): print(json.dumps(result))
