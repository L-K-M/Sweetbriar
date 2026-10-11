from pathlib import Path
import re,json,csv
base=Path('audit/source')
qpat=r'"(?:\\.|[^"\\])*"'
def strings(s):
 return [m.group()[1:-1].replace('\\"','"') for m in re.finditer(qpat,s)]
def blocks(p):
 s=p.read_text()
 for m in re.finditer(r"resource\s+'([^']+)'\s*\(([^\n]*?)\)\s*\{",s):
  start=m.end()-1; i=start; depth=0; quote=False; comment=False
  while i<len(s):
   c=s[i]
   if quote:
    if c=='\\':i+=2;continue
    if c=='"':quote=False
   elif comment:
    if s[i:i+2]=='*/':comment=False;i+=2;continue
   elif s[i:i+2]=='/*':comment=True;i+=2;continue
   elif s[i:i+2]=='//':
    nxt=s.find('\n',i);i=len(s) if nxt<0 else nxt;continue
   elif c=='"':quote=True
   elif c=='{':depth+=1
   elif c=='}':
    depth-=1
    if depth==0:break
   i+=1
  yield {'type':m[1],'spec':m[2],'line':s[:m.start()].count('\n')+1,'file':'src/'+str(p.relative_to(base)),'body':s[start+1:i]}
menus=[]; panels=[]; allres=[]
for p in sorted(base.rglob('*.r')):
 for b in blocks(p):
  allres.append({k:v for k,v in b.items() if k!='body'})
  body=b['body']
  if b['type']=='MENU':
   ss=strings(body.split('{',1)[0]); title=ss[-1] if ss else '(untitled)'
   items=[]
   for m in re.finditer('('+qpat+r'(?:\s*'+qpat+r')*)\s*,\s*(?:noIcon|\d+)\s*,\s*([^,]+),\s*([^,]+),\s*([^,\n}]+)',body):
    items.append({'text':''.join(strings(m[1])),'key':m[2].strip(),'mark':m[3].strip(),'line':b['line']+body[:m.start()].count('\n')})
   menus.append({k:v for k,v in b.items() if k!='body'}|{'title':title,'items':items})
  elif b['type']=='DITL' and p.name in ('SettingsTwo.rsrc.r','EsotericSettings60.r'):
   entries=[]
   for m in re.finditer(r'(StaticText|EditText|CheckBox|RadioButton|Button|Control|UserItem)\s*\{([^}]*)\}',body):
    entries.append({'kind':m[1],'text':''.join(strings(m[2])),'raw':m[2].strip(),'line':b['line']+body[:m.start()].count('\n')})
   panels.append({k:v for k,v in b.items() if k!='body'}|{'entries':entries})
Path('audit/menus.json').write_text(json.dumps(menus,indent=2,ensure_ascii=False))
Path('audit/panels.json').write_text(json.dumps(panels,indent=2,ensure_ascii=False))
Path('audit/resources.json').write_text(json.dumps(allres,indent=2,ensure_ascii=False))
with Path('audit/menu-catalog.md').open('w') as f:
 f.write('| Resource / menu | All declared entries in order (— = separator) | Source |\n|---|---|---|\n')
 for b in menus:
  its='; '.join(x['text'].replace('|','\\|') if x['text']!='-' else '—' for x in b['items']) or '(empty; runtime supplied)'
  f.write(f"| {b['spec'].split(',')[0]}: {b['title']} | {its} | {b['file']}:{b['line']} |\n")
with Path('audit/panel-controls.md').open('w') as f:
 for b in panels:
  f.write(f"\n### {b['spec']} [{b['file']}:{b['line']}]\n\n")
  for e in b['entries']:f.write(f"- {e['line']} {e['kind']}: {e['text'] or e['raw']}\n")
# Independent complete documented setting inventory; one entry per leading URL.
settings=[]
for i,line in enumerate((base/'X-Eudora-Settings.txt').read_text().splitlines(),1):
 m=re.match(r'<x-eudora-setting:([^>]+)>\s*(.*)',line)
 if m:settings.append({'id':m[1],'description':m[2],'source_line':i})
with Path('audit/x-eudora-settings.csv').open('w') as f:
 w=csv.DictWriter(f,fieldnames=['id','description','source_line']);w.writeheader();w.writerows(settings)
print('MENU resources:',len(menus),'nonseparator entries:',sum(sum(i['text']!='-' for i in m['items']) for m in menus))
print('Settings DITL resources:',len(panels),'documented setting entries:',len(settings),'unique ids:',len(set(e['id'] for e in settings)))
