"""Profile refresh path work on verified disposable fixture clones.

Requires a pinned native unit-test executable and Python blake3. Preserves all
outputs and failures. Run tiny before 1k/10k; this measures direct app calls,
not public CLI latency or full-vault capacity.
"""
import argparse, hashlib, importlib.util, json, os, re, selectors, shutil, signal
import subprocess, sys, time, threading
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('refresh_benchmark', ROOT / 'scripts/benchmark_source_refresh.py')
bm = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bm)
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary', type=Path, required=True)
p.add_argument('--sha256', required=True)
p.add_argument('--preseed', type=Path, required=True)
p.add_argument('--account-root', type=Path, required=True)
p.add_argument('--workdir', type=Path, required=True)
p.add_argument('--tier', choices=['tiny','1000','10000'], required=True)
p.add_argument('--max-disk-gib', type=int, default=20,
               help='Combined disposable account ceiling, 1..100 GiB (default 20)')
a = p.parse_args()
if not 1 <= a.max_disk_gib <= 100:
    p.error('disk ceiling must be 1..100 GiB')
a.account_root = a.account_root.resolve(strict=True)
a.preseed = a.preseed.resolve(strict=True)
a.binary = a.binary.resolve(strict=True)
a.workdir = a.workdir.absolute()
if a.workdir.exists() or a.workdir.is_symlink():
    p.error('workdir must be new')
if a.workdir.parent.resolve(strict=True) != a.account_root:
    p.error('new workdir must be a direct child of account root')
if not a.preseed.is_relative_to(a.account_root) or not a.binary.is_relative_to(a.account_root):
    p.error('seed and pinned binary must be in the disposable account root')
if a.workdir.is_relative_to(a.preseed) or a.preseed.is_relative_to(a.workdir):
    p.error('seed and workdir must be disjoint')
if not re.fullmatch('[0-9a-f]{64}', a.sha256) or bm.digest(a.binary) != a.sha256:
    p.error('executable pin differs')
a.seed, a.bytes_per_source, a.history_revisions = 731, 100000, 0
a.run_seconds = 1800
a.workdir.mkdir(mode=0o700)
runner = bm.Runner(a, {str(a.binary): a.sha256})
report = dict(version=1, status='incomplete', binary_sha256=a.sha256,
              supervisor_sha256=bm.digest(Path(__file__)),
              imported_harness_sha256=bm.digest(ROOT/'scripts/benchmark_source_refresh.py'),
              limits=dict(whole_seconds=1800, child_seconds=120, rss_bytes=8*bm.GIB,
                          allocated_bytes=a.max_disk_gib*bm.GIB, free_floor_bytes=32*bm.GIB, output_bytes=4*1024*1024),
              scope='Instrumented direct app calls; excludes CLI argument/config work. One sample per case; setup and cache verification warm filesystem. Sampled resource enforcement can overshoot. Supervisor RSS is outside child tree. No live provider or capacity claim.')
proc = None
child_timer = None
child_timed_out = threading.Event()
started = time.monotonic()
def expired(*_):
    if proc is not None: bm.stop(proc)
    raise TimeoutError('whole experiment elapsed ceiling')
signal.signal(signal.SIGALRM, expired)
signal.signal(signal.SIGTERM, expired)
signal.setitimer(signal.ITIMER_REAL, 1800)
try:
    report['resource_before'], _ = runner.check_resources()
    corpus, seed_sha = bm.validate_preseed(a, runner)
    seed_allocated = bm.inventory(a.preseed/'vault')['allocated_bytes']
    forecast = seed_allocated + 512*1024*1024
    resource, _ = runner.check_resources()
    if resource['allocated_bytes'] + forecast > a.max_disk_gib*bm.GIB or resource['free_bytes'] - forecast < 32*bm.GIB:
        raise ValueError('clone forecast exceeds disk admission')
    clone = a.workdir/'clone'
    clone.mkdir()
    shutil.copytree(a.preseed/'vault', clone/'vault')
    shutil.copyfile(a.preseed/'corpus.json', clone/'corpus.json')
    for entry in corpus['files']:
        if bm.blake_digest(clone/'vault'/entry['path']) != entry['blake3']:
            raise ValueError('closed clone hash differs')
    bm.write_json(clone/'refresh-profile-clone.json', dict(version=1, kind='disposable-refresh-profile-clone',
                  original_export=str(a.preseed), corpus_blake3=bm.blake_digest(clone/'corpus.json')))
    report['resource_pre_command'], _ = runner.check_resources()
    report['setup_seconds'] = time.monotonic()-started
    report['seed_manifest_sha256'] = seed_sha
    env = {'PATH':'/usr/bin:/bin', 'LANG':'C.UTF-8', 'LC_ALL':'C.UTF-8',
           'LWIKI_REFRESH_PROFILE_CLONE':str(clone),
           'LWIKI_REFRESH_PROFILE_REPORT':str(clone/'profile.json')}
    argv = [str(a.binary),'app::refresh_path_profile::profile_normalized_source_refresh','--ignored','--exact','--nocapture']
    timed = ['/usr/bin/time', '-l' if sys.platform=='darwin' else '-v', *argv]
    report.update(argv=argv, environment=env, child_started_seconds=time.monotonic()-started,
                  rss_peak_bytes=0, ps_polls=0, ps_seconds=0., timed_inventory_sweeps=0, timed_inventory_seconds=0.)
    child_start = time.monotonic()
    next_disk, next_rss = child_start+5, child_start
    output_bytes = 0
    with (a.workdir/'stdout.log').open('xb') as out, (a.workdir/'stderr.log').open('xb') as err:
        proc = subprocess.Popen(timed, cwd=a.workdir, env=env, stdin=subprocess.DEVNULL,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
        def child_expired():
            child_timed_out.set()
            bm.stop(proc)
        child_timer = threading.Timer(120, child_expired)
        child_timer.daemon = True
        child_timer.start()
        with selectors.DefaultSelector() as sel:
            for pipe, sink in [(proc.stdout,out),(proc.stderr,err)]:
                os.set_blocking(pipe.fileno(),False)
                sel.register(pipe,selectors.EVENT_READ,sink)
            while sel.get_map() or proc.poll() is None:
                now=time.monotonic()
                if now-child_start>120: raise TimeoutError('child elapsed ceiling')
                if now>=next_rss and proc.poll() is None:
                    sample_start=time.monotonic(); rss=bm.process_tree_rss(proc.pid)
                    report['ps_seconds']+=time.monotonic()-sample_start; report['ps_polls']+=1
                    report['rss_peak_bytes']=max(report['rss_peak_bytes'],rss)
                    if rss>8*bm.GIB: raise ValueError('child RSS ceiling')
                    if shutil.disk_usage(a.workdir).free<32*bm.GIB: raise ValueError('free space floor')
                    next_rss=time.monotonic()+.2
                if now>=next_disk:
                    sweep=time.monotonic();runner.check_resources()
                    report['timed_inventory_seconds']+=time.monotonic()-sweep;report['timed_inventory_sweeps']+=1
                    next_disk=time.monotonic()+5
                for key,_ in sel.select(.05):
                    block=os.read(key.fileobj.fileno(),65536)
                    if not block:
                        sel.unregister(key.fileobj);key.fileobj.close();continue
                    remaining=4*1024*1024-output_bytes
                    key.data.write(block[:remaining]);output_bytes+=len(block)
                    if output_bytes>4*1024*1024:raise ValueError('child output ceiling')
        report['returncode']=proc.wait(timeout=5)
    child_timer.cancel()
    report['child_seconds']=time.monotonic()-child_start
    if child_timed_out.is_set():raise TimeoutError('child elapsed ceiling')
    stdout=(a.workdir/'stdout.log').read_text()
    stderr=(a.workdir/'stderr.log').read_text()
    match=re.search(r'(\d+)\s+maximum resident set size',stderr) if sys.platform=='darwin' else re.search(r'Maximum resident set size \(kbytes\):\s*(\d+)',stderr)
    if not match:raise ValueError('native RSS missing')
    report['native_peak_rss_bytes']=int(match.group(1))*(1 if sys.platform=='darwin' else 1024)
    if report['native_peak_rss_bytes']>8*bm.GIB:raise ValueError('native RSS ceiling')
    if report['returncode']!=0 or not re.search(r'test result: ok\. 1 passed; 0 failed; 0 ignored;',stdout):
        raise ValueError('exact profile test failed or did not run')
    profile=clone/'profile.json'
    if not profile.is_file():raise ValueError('profile report missing')
    data=json.loads(profile.read_text(),object_pairs_hook=bm.strict_object)
    if (data.get('version')!=1 or data.get('kind')!='refresh-path-attribution'
            or data.get('status')!='passed' or data.get('complete') is not True or data.get('source_count')!=bm.TIERS[a.tier]
            or data.get('clone')!=str(clone) or data.get('original_export')!=str(a.preseed)
            or data.get('corpus_blake3')!=bm.blake_digest(clone/'corpus.json')
            or data.get('test_binary_blake3')!=bm.blake_digest(a.binary)
            or data.get('source_id')!=corpus['sources'][0]['source_id']):
        raise ValueError('profile identity or completeness differs from requested experiment')
    cases=data.get('cases',[])
    if [case.get('case') for case in cases]!=['noop','title-only','changed'] or any(case.get('passed') is not True or case.get('result',{}).get('ok') is not True or case.get('result',{}).get('context',{}).get('exact_citations') is not True for case in cases):
        raise ValueError('profile cases missing or correctness check failed')
    report['profile_path']=str(profile);report['profile_sha256']=bm.digest(profile)
    report['phase_timing_contaminated']=report['timed_inventory_sweeps']>0
    report['attribution']='counts only: child overlapped whole-account inventory' if report['phase_timing_contaminated'] else 'instrumented phase timing with reported concurrent ps overhead'

    if bm.digest(a.binary)!=a.sha256:raise ValueError('pinned binary changed')
    _, after_sha=bm.validate_preseed(a,runner)
    if after_sha!=seed_sha:raise ValueError('original seed changed')
    report['resource_after'],_=runner.check_resources()
    report['status']='passed-attribution-diagnostic'
except BaseException as error:
    if proc is not None:
        bm.stop(proc)
        try:proc.wait(timeout=5)
        except subprocess.TimeoutExpired:pass
    report.update(status='failed-or-refused',failure=str(error) or type(error).__name__)
finally:
    if child_timer is not None:child_timer.cancel()
    signal.setitimer(signal.ITIMER_REAL,0)
    report['total_seconds']=time.monotonic()-started
    bm.write_json(a.workdir/'report.json',report)
print(json.dumps({'status':report['status'],'report':str(a.workdir/'report.json')}))
sys.exit(0 if report['status']=='passed-attribution-diagnostic' else 1)
