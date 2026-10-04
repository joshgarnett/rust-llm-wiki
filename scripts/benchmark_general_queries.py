"""Finite G8 diagnostic: canonical-only seed copy, real build, direct CLI calls.

Uses existing fixture validation and citation checks. Never opens seed SQLite.
Pinned binaries, one new work directory per tier, no timed inventories.
"""
import argparse, importlib.util, json, os, re, shutil, signal, subprocess, threading, time
from pathlib import Path
from types import SimpleNamespace

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('bm', ROOT/'scripts/benchmark_source_refresh.py')
bm = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bm)
GIB = bm.GIB
p = argparse.ArgumentParser()
p.add_argument('--binary', type=Path, required=True)
p.add_argument('--binary-sha256', required=True)
p.add_argument('--unit-binary', type=Path, required=True)
p.add_argument('--unit-sha256', required=True)
p.add_argument('--account', type=Path, required=True)
p.add_argument('--preseed', type=Path, required=True)
p.add_argument('--workdir', type=Path, required=True)
p.add_argument('--tier', choices=['1000', '10000'], required=True)
a = p.parse_args()
for path in [a.binary, a.unit_binary, a.account, a.preseed]:
    assert path.is_absolute() and path.resolve() == path and path.exists(), path
for path in [a.binary,a.unit_binary,a.preseed]:
    assert a.account in path.parents, path
assert a.workdir.is_absolute() and a.workdir.parent == a.account and not a.workdir.exists()
assert a.workdir not in a.preseed.parents and a.preseed not in a.workdir.parents
assert bm.digest(a.binary) == a.binary_sha256 and bm.digest(a.unit_binary) == a.unit_sha256
a.workdir.mkdir(mode=0o700)
work = a.workdir
vault = work/'vault'
commands = work/'commands'
commands.mkdir()
env = {'PATH':'/usr/bin:/bin', 'LANG':'C.UTF-8', 'LC_ALL':'C.UTF-8',
       'XDG_CONFIG_HOME':str(work/'config')}
report = {'status':'running', 'tier':int(a.tier), 'protocol':'general-lexical-critic.json G8',
          'script_sha256':bm.digest(Path(__file__)), 'binary_sha256':a.binary_sha256,
          'validator_sha256':bm.digest(ROOT/'scripts/benchmark_source_refresh.py'),
          'unit_sha256':a.unit_sha256, 'commands':[], 'limits':{
              'account_allocated_bytes':40*GIB,'free_floor_bytes':32*GIB,
              'query_seconds':15,'query_rss_bytes':GIB,'setup_seconds':1800,
              'setup_rss_bytes':8*GIB,'command_output_bytes':16*1024*1024,'whole_seconds':3600},
          'monitoring':'No inventory or ps during query calls. RSS/output checked after completion; wall watchdog live. Setup resource samples separate.'}
def save(): bm.write_json(work/'report.json',report)
def resources():
    inventory = bm.inventory(a.account)
    assert inventory['allocated_bytes'] <= 40*GIB, inventory
    assert shutil.disk_usage(a.account).free >= 32*GIB
    return inventory
def run(name, argv, seconds=15, rss_limit=GIB, setup=False):
    assert shutil.disk_usage(work).free >= 32*GIB
    stem = commands/f'{len(report["commands"]):03d}-{name}'
    out, err = stem.with_suffix('.json'), stem.with_suffix('.stderr')
    event = {'name':name,'argv':argv,'stdout':str(out),'stderr':str(err)}
    report['commands'].append(event); save()
    ended = threading.Event()
    monitor_errors = []
    started = time.monotonic()
    with out.open('xb') as stdout, err.open('xb') as stderr:
        proc = subprocess.Popen(['/usr/bin/time','-l',*argv],cwd=work,env=env,
                                stdin=subprocess.DEVNULL,stdout=stdout,stderr=stderr,start_new_session=True)
        timer = threading.Timer(seconds,bm.stop,args=(proc,));timer.start()
        def monitor():
            while not ended.wait(10):
                try:
                    assert bm.process_tree_rss(proc.pid) <= rss_limit
                    assert shutil.disk_usage(work).free >= 32*GIB
                    assert report['account_before']['allocated_bytes'] + bm.inventory(work)['allocated_bytes'] <= 40*GIB
                except BaseException as error:
                    monitor_errors.append(repr(error));bm.stop(proc);return
        thread = threading.Thread(target=monitor,daemon=True) if setup else None
        if thread: thread.start()
        try: code = proc.wait()
        except BaseException:
            bm.stop(proc);proc.wait();raise
        finally:
            ended.set();timer.cancel()
            if thread: thread.join()
    event.update(exit_code=code,seconds=time.monotonic()-started,monitor_errors=monitor_errors)
    save()
    assert out.stat().st_size + err.stat().st_size <= 16*1024*1024
    raw = err.read_text()
    rss = re.search(r'(\d+)\s+maximum resident set size',raw)
    native = re.search(r'([0-9.]+) real\s+([0-9.]+) user\s+([0-9.]+) sys',raw)
    event.update(rss_bytes=int(rss[1]) if rss else None,
                 native_seconds=float(native[1]) if native else None)
    save()
    assert code == 0 and not monitor_errors and rss and int(rss[1]) <= rss_limit, event
    assert event['seconds'] <= seconds, event
    if setup: return out.read_text()
    value = json.loads(out.read_text(),object_pairs_hook=bm.strict_object)
    assert value['ok'] and value['meta']['network_used'] is False, value
    return value
def cli(name,args,seconds=15):
    return run(name,[str(a.binary),'--wiki',str(vault),'--json','--offline',*args],seconds)
def check(value,expected):
    assert set(expected) <= {'hit_ids_all','hit_paths_all','empty_hits','nonempty_hits','text_all','text_absent','first_reason','snapshot_context','stale_identity_only'}, expected
    assert expected, 'case has no correctness expectation'
    data = value['data']; hits = data.get('hits',[])
    ids = {h.get('locator',{}).get('record',{}).get('record_id') for h in hits if h.get('locator',{}).get('record')}
    paths = {h['path'] for h in hits}
    assert set(expected.get('hit_ids_all',[])) <= ids, (expected,ids)
    assert set(expected.get('hit_paths_all',[])) <= paths, (expected,paths)
    if expected.get('empty_hits'): assert hits == [], hits
    if expected.get('nonempty_hits'): assert hits, expected
    text = data.get('text','')
    assert all(t in text for t in expected.get('text_all',[])), (expected,text)
    assert all(t not in text for t in expected.get('text_absent',[])), (expected,text)
    if 'first_reason' in expected: assert expected['first_reason'] in hits[0]['reasons'], hits[0]
    if expected.get('stale_identity_only'):
        required = set(expected['hit_ids_all'])
        stale = [h for h in hits if h.get('record_ref',{}).get('record_id') in required]
        assert len(stale) == len(required)
        for hit in stale:
            assert hit['identity_eligibility'] == 'current' and hit['eligibility'] == 'stale', hit
            assert 'identity' in hit['reasons'] and hit['excerpt']['text'] == '', hit
            assert all(excerpt['text'] == '' for excerpt in hit['secondary_excerpts']), hit
    assert value['meta']['freshness'] == 'index_snapshot'
    assert value['meta'].get('verified_at') is None
    if expected.get('snapshot_context'):
        assert data['verification']['mode'] == 'index_snapshot'
        assert all(not passage['citations'] for passage in data['passages'])

CASE_NAMES = ['exact_id','exact_title_bucket','exact_alias_bucket','unicode_alias','rare_capture','popular','selective_late','selective_empty','source_long_metadata_positive','source_long_metadata_negative','stale_entity_identity','stale_description_hidden','authored_context','mixed_context']
def whole_timeout(signum,frame): raise TimeoutError('whole-run 3600 second deadline')
signal.signal(signal.SIGALRM,whole_timeout)
signal.alarm(3600)
started = time.monotonic(); save()
try:
    report['account_before'] = resources()
    source_args = SimpleNamespace(preseed=a.preseed,tier=a.tier,seed=731,bytes_per_source=100000,history_revisions=0)
    source, seed_hash = bm.validate_preseed(source_args,SimpleNamespace(check_resources=resources))
    report['seed_sha256'] = seed_hash
    # Forecast one fixture plus 2GiB extra overlay/build/log scratch; no old DB copy.
    forecast = report['account_before']['allocated_bytes'] + bm.inventory(a.preseed)['allocated_bytes'] + 2*GIB
    assert forecast <= 40*GIB, forecast
    assert shutil.disk_usage(work).free - (forecast-report['account_before']['allocated_bytes']) >= 32*GIB
    report['forecast_bytes'] = forecast
    vault.mkdir()
    copied = []
    for entry in source['files']:
        relative = entry['path']
        if relative == 'WIKI.md' or relative.startswith('sources/'):
            src, dest = a.preseed/'vault'/relative, vault/relative
            dest.parent.mkdir(parents=True,exist_ok=True)
            with src.open('rb') as source_file, dest.open('xb') as target:
                shutil.copyfileobj(source_file,target,1024*1024)
            assert dest.stat().st_size == entry['bytes'] and bm.blake_digest(dest) == entry['blake3']
            copied.append(entry)
        else:
            assert relative.startswith('.wiki/cache/') or relative in ('.wiki/state/operations.json','.wiki/state/writer.lock'), relative
    first = source['sources'][0]
    bm.write_json(work/'ownership.json',{'version':1,'kind':'general-query-owned-copy',
        'vault_path':str(vault),'original_seed_manifest_sha256':seed_hash,
        'source_count':int(a.tier),'first_source':{k:first[k] for k in ['source_id','revision_id','marker']},
        'wiki_blake3':bm.blake_digest(vault/'WIKI.md')})
    bm.write_json(work/'copied-canonical.json',copied)
    env['LWIKI_GENERAL_QUERY_SETUP'] = str(work)
    run('setup',[str(a.unit_binary),'app::general_query_fixture::prepare_general_query_fixture','--exact','--ignored','--nocapture'],1800,8*GIB,True)
    del env['LWIKI_GENERAL_QUERY_SETUP']
    setup = json.loads((work/'setup-result.json').read_text())
    for key, value in {'version':1,'kind':'general-query-fixture-setup','status':'published',
                       'vault_path':str(vault),'original_seed_manifest_sha256':seed_hash,
                       'ownership_blake3':bm.blake_digest(work/'ownership.json'),
                       'source_count':int(a.tier),'overlay_version':1,'overlay_pages':128,
                       'overlay_records':133,'proof_layout_version':2,'revision_ownership_version':1}.items():
        assert setup[key] == value, (key,setup.get(key),value)
    assert [c['name'] for c in setup['cases']] == CASE_NAMES
    assert setup['snapshot']['generation'] == 1
    assert setup['snapshot']['publication']['file_id'] == setup['selection']['file_id']
    assert len(setup['overlay_files']) == 133
    seen_overlay = set()
    for entry in setup['overlay_files']:
        relative = entry['path']; path = Path(relative)
        assert not path.is_absolute() and '..' not in path.parts and str(path) == relative
        assert path.parts[0] in ['pages','entities','assertions','evidence']
        assert relative not in seen_overlay; seen_overlay.add(relative)
        assert (vault/path).stat().st_size == entry['bytes'] and bm.blake_digest(vault/path) == entry['blake3']
    report['setup'] = setup
    report['account_after_setup'] = resources(); save()
    canonical_paths = [vault/e['path'] for e in copied]
    canonical_paths.extend(vault/e['path'] for e in setup['overlay_files'])
    canonical_paths.append(vault/'.wiki/state/operations.json')
    bindings = {str(path.relative_to(vault)):bm.blake_digest(path) for path in canonical_paths}
    bm.write_json(work/'readonly-before.json',bindings)
    batch_started = time.monotonic()
    for repetition in range(5):
        for case in setup['cases']:
            assert time.monotonic()-batch_started <= 900
            value = cli(f'{repetition}-{case["name"]}',case['args'])
            check(value,case['expected'])
            assert value['data']['snapshot'] == setup['snapshot']
            report['commands'][-1].update(passed=True,repetition=repetition,case=case['name']); save()
    after = {str(path.relative_to(vault)):bm.blake_digest(path) for path in canonical_paths}
    assert bindings == after
    bm.write_json(work/'readonly-after.json',after)
    report['account_after_queries'] = resources()
    body, marker = bm.payload(731,0,1,100000)
    input_path = work/'refresh-input.md'; input_path.write_bytes(body)
    refresh = cli('changed-refresh',['source','refresh',first['source_id'],'--file',str(input_path)],120)
    revision = refresh['data']['allocated_ids']['revision']; snapshot = refresh['data']['snapshot']
    # The very next query after acknowledgment is general discovery.
    found = cli('first-search-after-refresh',['search',marker,'--no-sync','--source-id',first['source_id']])
    assert found['data']['snapshot'] == snapshot
    assert any(hit['owner_revision'] == revision for hit in found['data']['hits'])
    report['commands'][-1]['passed'] = True; save()
    cached = cli('snapshot-after-refresh',['context',marker,'--scope','snapshot','--target','documents','--no-sync','--source-id',first['source_id']])
    check(cached,{'text_all':[marker],'snapshot_context':True})
    assert cached['data']['snapshot'] == snapshot
    cited = cli('indexed-after-refresh',['context',marker,'--scope','indexed-evidence','--no-sync','--source-id',first['source_id']])
    bm.verify_context(cited['data'],vault,first['source_id'],revision,body,marker)
    assert cited['data']['snapshot'] == snapshot
    report['workflow_passed'] = True
    _, final_seed_hash = bm.validate_preseed(source_args,SimpleNamespace(check_resources=resources))
    assert final_seed_hash == seed_hash
    assert bm.digest(a.binary) == a.binary_sha256 and bm.digest(a.unit_binary) == a.unit_sha256
    assert report['script_sha256'] == bm.digest(Path(__file__))
    assert report['validator_sha256'] == bm.digest(ROOT/'scripts/benchmark_source_refresh.py')
    report['statistics'] = {case['name']:bm.nearest_rank([c['seconds'] for c in report['commands'] if c.get('case') == case['name']]) for case in setup['cases']}
    assert all(s['p95_seconds'] <= 5 for s in report['statistics'].values())
    assert set(report['statistics']) == set(CASE_NAMES) and all(s['count'] == 5 for s in report['statistics'].values())
    report['account_after'] = resources(); report['status'] = 'passed'
except BaseException as error:
    report['status']='failed';report['error']=repr(error)
finally:
    signal.alarm(0)
    report['elapsed_seconds']=time.monotonic()-started;save()
print(json.dumps({k:report[k] for k in ['status','elapsed_seconds']}))
raise SystemExit(0 if report['status']=='passed' else 1)
