import argparse,datetime,importlib.metadata,json,os,pathlib,platform,subprocess
ROOT=pathlib.Path(__file__).resolve().parent
p=argparse.ArgumentParser();p.add_argument('--out',default='artifacts/environment.json')
args=p.parse_args();target=(ROOT/args.out).resolve();assert target.is_relative_to(ROOT)
target.parent.mkdir(parents=True,exist_ok=True)
def command(args):
    r=subprocess.run(args,capture_output=True,text=True);return {'returncode':r.returncode,'stdout':r.stdout,'stderr':r.stderr}
def read(path):
    try:return pathlib.Path(path).read_text()
    except OSError as e:return str(e)
env={'collected_at':datetime.datetime.now(datetime.timezone.utc).isoformat(),'platform':platform.platform(),
    'python':platform.python_version(),'lscpu':command(['lscpu']),'df':command(['df','-T',str(ROOT)]),
    'os_release':read('/etc/os-release'),'meminfo':read('/proc/meminfo'),'loadavg':read('/proc/loadavg'),
    'cgroup_memory_max':read('/sys/fs/cgroup/memory.max'),'cgroup_cpu_max':read('/sys/fs/cgroup/cpu.max'),
    'packages':{x:importlib.metadata.version(x) for x in ['duckdb','apsw','lmdb','rocksdict','libsql-experimental','redb','pyarrow']},
    'native_libraries':{str(p.relative_to(ROOT)):p.stat().st_size for p in (ROOT/'.venv').rglob('*.so')},
    'benchmark_settings':{'duckdb':{'threads':4,'memory_limit':'1GB','autoload_known_extensions':False,'autoinstall_known_extensions':False},
      'sqlite_and_libsql':{'journal_mode':'WAL','synchronous':'FULL','fullfsync':'ON','checkpoint_fullfsync':'ON','cache_size_KiB':65536,'foreign_keys':'ON'},
      'lmdb':{'sync':True,'metasync':True,'writemap':False,'map_size_bytes':8*1024**3},'rocksdb':{'sync':True,'disable_wal':False}}}
target.write_text(json.dumps(env,indent=2));print(json.dumps({k:env[k] for k in ['packages','cgroup_memory_max','cgroup_cpu_max','loadavg']},indent=2))
