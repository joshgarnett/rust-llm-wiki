#!/usr/bin/env python3
"""Frozen public rebuild/check controls. Never opens seed SQLite or prebuilds a cache."""
import argparse
import ctypes
import importlib.util
import json
import os
from pathlib import Path, PurePosixPath
import platform
import re
import shutil
import signal
import stat
import subprocess
import threading
import time
import tempfile
from types import SimpleNamespace

ROOT = Path(__file__).resolve().parents[1]
HELPER = ROOT / 'scripts/benchmark_source_refresh.py'
PROTOCOL = ROOT / 'docs/full-check-resource-protocol.json'
spec = importlib.util.spec_from_file_location('full_check_bm', HELPER)
bm = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bm)
GIB = 1024 ** 3


def require(condition, message):
    if not condition:
        raise ValueError(message)


def regular(path, cap=None):
    info = path.lstat()
    require(stat.S_ISREG(info.st_mode) and info.st_nlink == 1,
            f'unsafe regular file: {path}')
    if cap is not None:
        require(info.st_size <= cap, f'oversized metadata: {path}')
    return info


def object_file(path, cap):
    regular(path, cap)
    value = json.loads(path.read_text(), object_pairs_hook=bm.strict_object)
    require(isinstance(value, dict), f'object required: {path}')
    return value


def build_provenance(args):
    value = object_file(args.build_provenance, 1024 * 1024)
    required = {'version', 'kind', 'compilation_mode', 'rust_opt_level',
                'binary_sha256', 'holder_sha256', 'source_pins_sha256', 'rust_actions'}
    require(required <= value.keys(), 'incomplete build provenance')
    valid_hash = lambda v: isinstance(v, str) and re.fullmatch(r'[0-9a-fA-F]{64}', v)
    require(type(value['version']) is int and value['version'] == 1
            and value['kind'] == 'full-check-build-provenance'
            and value['compilation_mode'] == 'opt' and value['rust_opt_level'] == '3'
            and value['binary_sha256'] == args.binary_sha256
            and value['holder_sha256'] == args.holder_sha256
            and valid_hash(value['source_pins_sha256']), 'build provenance profile or pins differ')
    actions = value['rust_actions']
    targets = {'//:lwiki_lib', '//:lwiki', '//:unit_tests'}
    require(isinstance(actions, list) and len(actions) == len(targets), 'three Rust actions required')
    seen = set()
    for action in actions:
        require(isinstance(action, dict) and set(action) == {'target', 'opt_level', 'action_sha256'},
                'invalid Rust action envelope')
        require(action['target'] in targets and action['target'] not in seen
                and action['opt_level'] == '3' and valid_hash(action['action_sha256']),
                'Rust action profile or target differs')
        seen.add(action['target'])
    # Preserve optional build-command/compiler metadata. Envelope checks bind
    # supplied evidence; root independently verifies actual aquery actions.
    return value


def relative(value):
    require(isinstance(value, str), 'relative path must be string')
    path = PurePosixPath(value)
    require(value and not path.is_absolute() and str(path) == value
            and '..' not in path.parts and '\\' not in value, f'unsafe path: {value}')
    return path


def new_json(path, value):
    with path.open('x') as stream:
        json.dump(value, stream, indent=2, sort_keys=True)
        stream.write('\n')


def atomic_release(path, value):
    # Native macOS renamex_np(RENAME_EXCL), declared in sys/stdio.h, publishes
    # one single-link complete file atomically and refuses an existing name.
    with tempfile.NamedTemporaryFile(mode='w', dir=path.parent, prefix='.release-', delete=False) as stream:
        json.dump(value, stream); stream.flush(); os.fsync(stream.fileno())
        stage = Path(stream.name)
    rename = ctypes.CDLL(None, use_errno=True).renamex_np
    rename.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_uint]
    rename.restype = ctypes.c_int
    if rename(os.fsencode(stage), os.fsencode(path), 0x00000004) != 0:
        raise OSError(ctypes.get_errno(), 'atomic no-clobber holder release failed')


def snapshot_file_id(snapshot):
    require(isinstance(snapshot, dict), 'missing snapshot')
    file_id = snapshot.get('publication', {}).get('file_id', '')
    require(re.fullmatch(r'[0-9a-f]{32}', file_id), 'invalid snapshot file ID')
    require(type(snapshot.get('generation')) is int and snapshot['generation'] > 0,
            'invalid snapshot generation')
    return file_id


def original_bindings(root, entries):
    result = {}
    for entry in entries:
        name = entry['path']
        path = root.joinpath(*relative(name).parts)
        info = regular(path)
        require(info.st_size == entry['bytes'] and bm.blake_digest(path) == entry['blake3'],
                f'fixture file pin differs: {name}')
        result[name] = {'bytes': info.st_size, 'blake3': entry['blake3'],
                        'mtime_ns': info.st_mtime_ns}
    return result


def readonly_state(vault):
    # Called ONLY outside measured intervals. SHM reader marks and writer lock
    # diagnostics are excluded; all other names/bytes/mtimes are compared.
    bm.inventory(vault)
    result = {}
    for base, _, files in os.walk(vault, followlinks=False):
        for leaf in files:
            path = Path(base) / leaf
            name = str(path.relative_to(vault))
            if name.endswith('.sqlite-shm') or name == '.wiki/state/writer.lock':
                continue
            info = regular(path)
            result[name] = {'bytes': info.st_size, 'sha256': bm.digest(path),
                            'mtime_ns': info.st_mtime_ns}
    return result


class Supervisor:
    def __init__(self, args, protocol, build):
        self.a, self.protocol = args, protocol
        self.limits = protocol['limits']
        self.start = time.monotonic()
        self.vault = args.workdir / 'vault'
        self.commands_dir = args.workdir / 'commands'
        self.commands_dir.mkdir(mode=0o700)
        self.env = {'PATH': '/usr/bin:/bin', 'LANG': 'C.UTF-8', 'LC_ALL': 'C.UTF-8',
                    'XDG_CONFIG_HOME': str(args.workdir / 'config')}
        self.holder = self.holder_timer = None
        self.holder_ready = None
        self.holder_logs = None
        self.active = None
        self.report = {'version': 1, 'kind': 'full-check-resource-control', 'status': 'start',
            'tier': args.tier, 'source_count': 2 if args.tier == 'tiny' else int(args.tier),
            'original_seed_count': 1000 if args.tier == 'tiny' else int(args.tier),
            'subset_rehearsal': args.tier == 'tiny',
            'arguments': {k: str(v) if isinstance(v, Path) else v for k, v in vars(args).items()},
            'limits': self.limits, 'pins': {'binary': args.binary_sha256,
                'holder': args.holder_sha256, 'supervisor': bm.digest(Path(__file__)),
                'validator': bm.digest(HELPER), 'protocol': bm.digest(PROTOCOL),
                'build_provenance': bm.digest(args.build_provenance)},
            'build': build,
            'host': {'platform': platform.platform(), 'cpu_count': os.cpu_count(),
                'power_background': 'unavailable; root records external observation'},
            'commands': [], 'pairs': [], 'failures': [],
            'measurement_limits': ['Supervised elapsed includes monitoring overhead.',
                'Named-file samples and RSS sampling do not establish exact transient peaks.',
                'Provenance envelope validation alone does not verify actual compilation; root reviews build actions.',
                'No cache purge, cold-cache, statistical tail,100k or all-mode claim.']}
        self.save()

    def save(self):
        bm.write_json(self.a.workdir / 'report.json', self.report)

    def guard(self):
        require(time.monotonic() - self.start < self.limits['whole_tier_seconds'],
                'whole-tier elapsed limit')
        if self.holder is not None:
            require(self.holder.poll() is None, 'holder exited before release')

    def resources(self):
        self.guard()
        value = bm.inventory(self.a.account)
        value['free_bytes'] = shutil.disk_usage(self.a.account).free
        require(value['allocated_bytes'] <= self.limits['account_allocated_bytes'],
                'account allocated-byte limit')
        require(value['free_bytes'] >= self.limits['minimum_free_bytes'], 'free-space floor')
        return value

    def pins(self):
        for path, key in [(self.a.binary, 'binary'), (self.a.holder_binary, 'holder'),
                          (Path(__file__), 'supervisor'), (HELPER, 'validator'), (PROTOCOL, 'protocol'),
                          (self.a.build_provenance, 'build_provenance')]:
            if key == 'build_provenance': regular(path, 1024 * 1024)
            require(bm.digest(path) == self.report['pins'][key], f'pin changed: {key}')

    def previous_gate(self):
        needed = 'tiny' if self.a.tier == '1000' else '1000'
        if self.a.tier == 'tiny':
            return
        matches = []
        for child in self.a.account.iterdir():
            if child == self.a.workdir or child.is_symlink() or not child.is_dir():
                continue
            path = child / 'report.json'
            if not path.is_file():
                continue
            info = regular(path, 32 * 1024 * 1024)
            value = object_file(path, 32 * 1024 * 1024)
            if (value.get('kind') == 'full-check-resource-control'
                    and value.get('status') == 'completed' and value.get('tier') == needed
                    and value.get('pins') == self.report['pins']):
                matches.append((info.st_mtime_ns, path, value))
        require(matches, f'no completed pinned {needed} prerequisite')
        _, path, previous = max(matches, key=lambda item: item[0])
        self.report['prerequisite'] = {'path': str(path), 'sha256': bm.digest(path)}
        if self.a.tier == '10000':
            peaks = [p['check_peak_rss_bytes'] for p in previous['pairs']]
            scratch = [max(p['sampled_scratch_allocated_bytes'],p['returned_scratch_logical_bytes']) for p in previous['pairs']]
            projected = {'pair_seconds': max(p['seconds'] for p in previous['pairs']) * 20,
                         'rss_bytes': max(peaks) * 20, 'scratch_bytes': max(scratch) * 20}
            self.report['one_k_projection'] = projected
            require(projected['pair_seconds'] <= self.limits['check_plus_cited_query_seconds']
                    and projected['rss_bytes'] <= self.limits['bulk_process_tree_rss_bytes']
                    and projected['scratch_bytes'] <= 32 * GIB,
                    '1k projected work/resources do not admit10k')

    def mutable_allocation(self):
        cache = self.vault / '.wiki/cache'
        return (bm.inventory(cache)['allocated_bytes'] if cache.exists() else 0) + bm.inventory(self.commands_dir)['allocated_bytes'] + (self.a.workdir/'report.json').stat().st_blocks*512

    def command(self, name, words, seconds, query=False, pair_baseline=None):
        before, mutable_before = pair_baseline if pair_baseline is not None else (self.resources(), self.mutable_allocation())
        if pair_baseline is None: self.pins()
        directory = self.commands_dir / f'{len(self.report["commands"]):03d}-{name}'
        directory.mkdir(mode=0o700)
        temp = directory / 'tmp'
        temp.mkdir(mode=0o700)
        out, err = directory / 'stdout.json', directory / 'stderr.txt'
        argv = [str(self.a.binary), '--wiki', str(self.vault), '--json', '--offline', *words]
        env = {**self.env, 'TMPDIR': str(temp), 'SQLITE_TMPDIR': str(temp)}
        record = {'name': name, 'argv': argv, 'cwd': str(self.a.workdir), 'environment': env,
            'stdout': str(out), 'stderr': str(err), 'temp': str(temp), 'before': before,
            'seconds_limit': seconds, 'samples': [], 'monitor_seconds': 0.0,
            'peak_rss_bytes': 0, 'peak_bulk_rss_bytes': 0, 'scratch_peak_allocated_bytes': 0,
            'scratch_peak_logical_bytes': 0, 'temp_names_observed': [], 'vanished_during_samples': 0}
        self.report['commands'].append(record)
        self.save()
        done = threading.Event()
        violations = []
        own_limit = self.limits['query_process_tree_rss_bytes' if query else 'bulk_process_tree_rss_bytes']
        def monitor(proc):
            while not done.wait(.05 if self.a.tier == 'tiny' else 1):
                started = time.monotonic()
                try:
                    self.guard()
                    own_rss = bm.process_tree_rss(proc.pid)
                    held_rss = bm.process_tree_rss(self.holder.pid) if self.holder is not None else 0
                    temporary = bm.inventory(temp)
                    mutable_now = self.mutable_allocation()
                    outputs = sum(regular(p).st_size for p in (out, err))
                    # Only command-output tree/cache/temp are traversed here.
                    estimated = before['allocated_bytes'] - mutable_before + mutable_now
                    free = shutil.disk_usage(self.a.workdir).free
                    names = [str(p.relative_to(temp)) for p in temp.rglob('*')]
                    require(len(names) <= 4096, 'command temporary name bound')
                    record['temp_names_observed'] = sorted(set(record['temp_names_observed']) | set(names))
                    sample = {'seconds': time.monotonic()-call_start, 'rss_bytes': own_rss,
                        'held_rss_bytes': held_rss, 'temp': temporary, 'mutable_bytes': mutable_now,
                        'estimated_account_bytes': estimated, 'free_bytes': free, 'output_bytes': outputs}
                    record['samples'].append(sample)
                    record['peak_rss_bytes'] = max(record['peak_rss_bytes'], own_rss)
                    record['peak_bulk_rss_bytes'] = max(record['peak_bulk_rss_bytes'], own_rss+held_rss)
                    record['scratch_peak_allocated_bytes'] = max(record['scratch_peak_allocated_bytes'],temporary['allocated_bytes'])
                    record['scratch_peak_logical_bytes'] = max(record['scratch_peak_logical_bytes'],temporary['logical_bytes'])
                    record['vanished_during_samples'] += temporary['vanished_during_sample']
                    require(own_rss <= own_limit and own_rss+held_rss <= self.limits['bulk_process_tree_rss_bytes'], 'liveRSS limit')
                    require(outputs <= self.limits['stdout_stderr_bytes_per_command'], 'command output limit')
                    require(estimated <= self.limits['account_allocated_bytes'], 'live account estimate limit')
                    require(free >= self.limits['minimum_free_bytes'], 'live free-space floor')
                except BaseException as error:
                    violations.append(repr(error)); bm.stop(proc); return
                finally:
                    record['monitor_seconds'] += time.monotonic()-started
        call_start = time.monotonic()
        record['started_monotonic'] = call_start
        with out.open('xb') as stdout, err.open('xb') as stderr:
            proc = subprocess.Popen(['/usr/bin/time', '-l', *argv], cwd=self.a.workdir, env=env,
                stdin=subprocess.DEVNULL, stdout=stdout, stderr=stderr, start_new_session=True)
            self.active = proc
            timer = threading.Timer(seconds, bm.stop, args=(proc,)); timer.start()
            watcher = threading.Thread(target=monitor, args=(proc,), daemon=True); watcher.start()
            try:
                code = proc.wait()
            except BaseException:
                bm.stop(proc); proc.wait(); raise
            finally:
                record['seconds'] = time.monotonic()-call_start
                record['ended_monotonic'] = time.monotonic()
                timer.cancel(); done.set(); watcher.join(); self.active = None
        record.update(exit_code=code, monitor_errors=violations)
        self.save()
        regular(out, self.limits['stdout_stderr_bytes_per_command'])
        regular(err, self.limits['stdout_stderr_bytes_per_command'])
        require(out.stat().st_size+err.stat().st_size <= self.limits['stdout_stderr_bytes_per_command'], 'combined output limit')
        raw = err.read_text()
        rss = re.search(r'(\d+)\s+maximum resident set size', raw)
        require(rss, 'native maxRSS unavailable')
        record['native_peak_rss_bytes'] = int(rss[1])
        native = re.search(r'([0-9.]+) real\s+([0-9.]+) user\s+([0-9.]+) sys', raw)
        record['native_time'] = dict(zip(('real','user','sys'),map(float,native.groups()))) if native else None
        for key, label in [('block_inputs','block input operations'),('block_outputs','block output operations')]:
            match = re.search(r'(\d+)\s+'+label,raw); record[key] = int(match[1]) if match else None
        if pair_baseline is None: record['after'] = self.resources()
        else: record['after_inventory_deferred_to_pair_end'] = True
        record['temp_after'] = bm.inventory(temp)
        self.save()
        require(code == 0 and not violations and record['seconds'] <= seconds, f'command failed: {name}')
        require(int(rss[1]) <= own_limit, 'nativeRSS limit')
        require(not any(temp.iterdir()), f'unexpected temporary occupant after {name}')
        value = object_file(out,self.limits['stdout_stderr_bytes_per_command'])
        require(value.get('ok') is True and value.get('meta',{}).get('network_used') is False,
                f'CLI envelope failed: {name}')
        record['passed'] = True; self.save()
        if pair_baseline is None: self.pins()
        return value, record

    def start_holder(self, snapshot):
        directory = self.commands_dir/'holder'
        directory.mkdir(mode=0o700); temp=directory/'tmp'; temp.mkdir(mode=0o700)
        out,err=directory/'stdout.txt',directory/'stderr.txt'
        self.holder_logs=(out.open('xb'),err.open('xb'))
        env={**self.env,'TMPDIR':str(temp),'SQLITE_TMPDIR':str(temp),'LWIKI_FULL_CHECK_HOLDER':str(self.a.workdir)}
        argv=[str(self.a.holder_binary),'app::full_check_holder::hold_full_check_snapshot','--exact','--ignored','--nocapture']
        self.holder=subprocess.Popen(['/usr/bin/time','-l',*argv],cwd=self.a.workdir,env=env,
            stdin=subprocess.DEVNULL,stdout=self.holder_logs[0],stderr=self.holder_logs[1],start_new_session=True)
        self.holder_timer=threading.Timer(self.limits['holder_total_watchdog_seconds'],bm.stop,args=(self.holder,))
        self.holder_timer.start()
        self.report['holder']={'argv':argv,'environment':env,'stdout':str(out),'stderr':str(err),'temp':str(temp)}
        self.save(); started=time.monotonic()
        while not (self.a.workdir/'holder-ready.json').exists():
            self.guard(); require(time.monotonic()-started < self.limits['holder_ready_seconds'], 'holder ready timeout')
            require(bm.process_tree_rss(self.holder.pid)<=self.limits['query_process_tree_rss_bytes'],'holder readyRSS')
            require(sum(regular(p).st_size for p in (out,err)) <= self.limits['stdout_stderr_bytes_per_command'],'holder output limit')
            time.sleep(.1)
        ready=object_file(self.a.workdir/'holder-ready.json',64*1024)
        require(ready.get('version')==1 and ready.get('kind')=='full-check-held-snapshot'
                and ready.get('snapshot')==snapshot and ready.get('wiki_hash')==self.ownership['wiki_blake3']
                and ready.get('release_probe_budget_rearmed') is True,'invalid holder ready')
        require(type(ready.get('pid')) is int and ready['pid']>0,'invalid holderPID')
        self.verify_probes(ready)
        self.holder_ready=ready; self.report['holder']['ready']=ready; self.save()

    def verify_probes(self, value):
        probes=value.get('probes')
        require(isinstance(probes,list),'holder probes missing')
        actual={p['path']:p['hash'] for p in probes}
        require(actual.get(self.ownership['captured_path'])==self.ownership['captured_blake3']
                and actual.get(self.ownership['authored_path'])==self.ownership['authored_blake3'],'holder document bindings differ')

    def release_holder(self, success):
        if self.holder is None: return
        if self.holder_ready is not None and self.holder.poll() is None:
            path=self.a.workdir/'holder-release.json'
            if not path.exists():
                atomic_release(path,{'version':1,'kind':'release-full-check-holder',
                              'file_id':snapshot_file_id(self.holder_ready['snapshot'])})
            try: code=self.holder.wait(timeout=35)
            except subprocess.TimeoutExpired:
                bm.stop(self.holder); code=self.holder.wait()
        else:
            bm.stop(self.holder); code=self.holder.wait()
        self.holder_timer.cancel()
        for stream in self.holder_logs: stream.close()
        self.report['holder']['exit_code']=code
        stdout=Path(self.report['holder']['stdout']); stderr=Path(self.report['holder']['stderr'])
        require(sum(regular(p).st_size for p in (stdout,stderr))<=self.limits['stdout_stderr_bytes_per_command'],'holder final output limit')
        native=re.search(r'(\d+)\s+maximum resident set size',stderr.read_text())
        if success:
            require(native,'holder nativeRSS unavailable')
            self.report['holder']['native_peak_rss_bytes']=int(native[1])
            for command in self.report['commands']:
                require(command['native_peak_rss_bytes']+int(native[1])<=self.limits['bulk_process_tree_rss_bytes'],
                        'conservative CLI nativeRSS plus holder nativeRSS limit')
        self.holder=None
        if success:
            value=object_file(self.a.workdir/'holder-result.json',64*1024)
            require(code==0 and value.get('version')==1 and value.get('status')=='released'
                    and value.get('snapshot')==self.holder_ready['snapshot']
                    and value.get('wiki_hash')==self.ownership['wiki_blake3']
                    and value.get('release_probe_budget_rearmed') is True,'holder final proof failed')
            self.verify_probes(value)
            self.report['holder']['result']=value
            temp=Path(self.report['holder']['temp']); require(not any(temp.iterdir()),'holder temporary cleanup failed')
        self.save()

    def positive_pair(self, label, source, snapshot, expected, marker):
        before=readonly_state(self.vault)
        new_json(self.a.workdir/f'{label}-readonly-before.json',before)
        self.pins()
        pair_baseline=(self.resources(),self.mutable_allocation())
        checked,record=self.command(label+'-check',['check'],self.limits['check_plus_cited_query_seconds'],pair_baseline=pair_baseline)
        data=checked['data']
        require(data.get('complete') is True and data.get('canonical_check_performed') is True
                and data.get('cache_integrity_check_performed') is True and data.get('cache_matches_canonical') is True
                and data.get('error_count')==0 and data.get('diagnostics')==[] and data.get('checked_snapshot')==snapshot,'full check oracle failed')
        audit=data.get('audit',{})
        require(audit.get('layout')=='normalized' and audit.get('scratch_cleaned') is True
                and audit.get('revision_owner_history_checked') is True and audit.get('unused_retained_payloads_checked') is False
                and isinstance(audit.get('search_index'),dict),'audit completeness fields missing')
        cited_record=self.cited(label+'-cited',source,snapshot,expected,marker,pair_baseline)
        elapsed=cited_record['ended_monotonic']-record['started_monotonic']
        require(elapsed<=self.limits['check_plus_cited_query_seconds'],'check+cited pair deadline')
        pair_after=self.resources(); self.pins()
        record['after_pair_inventory']=pair_after
        authored,_=self.command(label+'-authored',['search','page_general_000','--mode','lexical','--no-sync'],self.limits['each_query_seconds'],True)
        require(authored['data'].get('snapshot')==snapshot and any(h.get('record_ref',{}).get('record_id')=='page_general_000'
                or h.get('locator',{}).get('record',{}).get('record_id')=='page_general_000' for h in authored['data'].get('hits',[])), 'authored identity missing')
        after=readonly_state(self.vault); new_json(self.a.workdir/f'{label}-readonly-after.json',after)
        require(before==after,'check/query changed readonly vault membership/hash/mtime')
        self.report['pairs'].append({'label':label,'seconds':elapsed,'snapshot':snapshot,'check':data,
            'check_peak_rss_bytes':max(record['native_peak_rss_bytes'],record['peak_bulk_rss_bytes']),
            'sampled_scratch_allocated_bytes':record['scratch_peak_allocated_bytes'],
            'returned_scratch_logical_bytes':audit['scratch_bytes'],
            'scratch_actual_tmpdir_observed':any(re.fullmatch(r'lwiki-check-[^/]+/reference\.sqlite',name) for name in record['temp_names_observed']),
            'started_monotonic':record['started_monotonic'],'ended_monotonic':cited_record['ended_monotonic']})
        if self.a.tier=='tiny':
            require(any(re.fullmatch(r'lwiki-check-[^/]+/reference\.sqlite',name) for name in record['temp_names_observed']),
                    'tiny did not observe actual scratch placement under commandTMPDIR')
        self.save()

    def cited(self,label,source,snapshot,expected,marker,pair_baseline=None):
        remaining=self.limits['each_query_seconds']
        if pair_baseline is not None:
            check_record=self.report['commands'][-1]
            remaining=min(remaining,self.limits['check_plus_cited_query_seconds']-(time.monotonic()-check_record['started_monotonic']))
            require(remaining>0,'check+cited pair deadline before query')
        value,record=self.command(label,['context',marker,'--scope','indexed-evidence','--mode','lexical','--no-sync',
            '--source-id',source['source_id'],'--limit','3','--candidates','16','--max-bytes','12000','--max-tokens','3000'],
            remaining,True,pair_baseline)
        require(value['data'].get('snapshot')==snapshot,'cited snapshot differs')
        bm.verify_context(value['data'],self.vault,source['source_id'],source['revision_id'],expected,marker)
        return record


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    for name in ('binary','holder-binary','account','preseed','overlay','workdir','build-provenance'):
        parser.add_argument('--'+name,type=Path,required=True)
    for name in ('binary-sha256','holder-sha256'): parser.add_argument('--'+name,required=True)
    parser.add_argument('--tier',choices=['tiny','1000','10000'],required=True)
    args=parser.parse_args()
    require(platform.system()=='Darwin','frozen supervisor requires native macOS')
    require(bm.blake3 is not None,'existing Python blake3 required; no dependency installation')
    for path in (args.binary,args.holder_binary,args.account,args.preseed,args.overlay,args.build_provenance):
        require(path.is_absolute() and path.resolve(strict=True)==path,'absolute canonical input path required')
        require(path==args.account or args.account in path.parents,'all inputs must be inside ownedaccount')
    require(args.workdir.is_absolute() and args.workdir.parent==args.account and ' ' in args.workdir.name
            and not args.workdir.exists() and not args.workdir.is_symlink(),'new directchild workdir with spaces required')
    for path,pin in ((args.binary,args.binary_sha256),(args.holder_binary,args.holder_sha256)):
        regular(path); require(os.access(path,os.X_OK) and bm.digest(path)==pin,'executable pin differs')
    build=build_provenance(args)
    protocol=object_file(PROTOCOL,64*1024)
    require(type(protocol.get('version')) is int and protocol['version']==2
            and protocol['tiny_rehearsal']['accepted'] is True
            and protocol.get('build_profile', {}).get('compilation_mode') == build['compilation_mode']
            and protocol.get('build_profile', {}).get('rust_opt_level') == build['rust_opt_level'],
            'unapproved protocol or build profile')
    args.workdir.mkdir(mode=0o700)
    runner=Supervisor(args,protocol,build)
    def expired(_signal,_frame): raise TimeoutError('whole-tier deadline')
    signal.signal(signal.SIGALRM,expired); signal.alarm(protocol['limits']['whole_tier_seconds'])
    success=False
    try:
        runner.report['account_before']=runner.resources(); runner.previous_gate()
        original_tier='1000' if args.tier=='tiny' else args.tier
        seed_args=SimpleNamespace(preseed=args.preseed,tier=original_tier,seed=731,bytes_per_source=100000,history_revisions=0)
        corpus,seed_sha=bm.validate_preseed(seed_args,SimpleNamespace(check_resources=runner.resources))
        seed_before={entry['path']:(regular(args.preseed/'vault'/entry['path']).st_size,
                                    regular(args.preseed/'vault'/entry['path']).st_mtime_ns) for entry in corpus['files']}
        setup_path=args.overlay/'setup-result.json'
        setup=object_file(setup_path,128*1024); owner=object_file(args.overlay/'ownership.json',16*1024)
        old_report=object_file(args.overlay/'report.json',512*1024)
        require(old_report.get('status')=='passed' and old_report.get('setup')==setup
                and setup.get('original_seed_manifest_sha256')==seed_sha and old_report.get('seed_sha256')==seed_sha
                and owner.get('original_seed_manifest_sha256')==seed_sha,'overlay/seed acceptance binding differs')
        require(setup.get('version')==1 and setup.get('kind')=='general-query-fixture-setup' and setup.get('status')=='published'
                and setup.get('vault_path')==str(args.overlay/'vault') and owner.get('vault_path')==str(args.overlay/'vault')
                and setup.get('ownership_blake3')==bm.blake_digest(args.overlay/'ownership.json')
                and setup.get('source_count')==int(original_tier) and owner.get('source_count')==int(original_tier)
                and setup.get('overlay_version')==1 and setup.get('overlay_pages')==128 and setup.get('overlay_records')==133
                and setup.get('proof_layout_version')==2 and setup.get('revision_ownership_version')==1,'overlay metadata differs')
        first=corpus['sources'][0]
        require(owner.get('first_source')=={k:first[k] for k in ('source_id','revision_id','marker')},'overlay first-source binding differs')
        files=setup.get('overlay_files'); require(isinstance(files,list) and len(files)==133,'133overlay files required')
        names=set()
        for item in files:
            require(set(item)=={'path','bytes','blake3'},'overlay file envelope differs')
            name=relative(item['path']); require(name.parts[0] in ('pages','entities','assertions','evidence')
                    and item['path'] not in names and type(item['bytes']) is int and 0<item['bytes']<=1024*1024
                    and re.fullmatch(r'blake3:[0-9a-f]{64}',item['blake3']),'unsafe overlayentry')
            names.add(item['path'])
        overlay_before=original_bindings(args.overlay/'vault',files)
        selected=corpus['sources'][:2] if args.tier=='tiny' else corpus['sources']
        selected_ids={s['source_id'] for s in selected}
        copied=[]
        for item in corpus['files']:
            name=item['path']
            parts=relative(name).parts
            if name=='WIKI.md' or (len(parts)>1 and parts[0]=='sources' and parts[1] in selected_ids): copied.append(item)
            elif name.startswith('sources/') and args.tier=='tiny': continue
            else: require(name.startswith('.wiki/cache/') or name in ('.wiki/state/operations.json','.wiki/state/writer.lock'),'unexpected omitted seed path')
        forecast=runner.report['account_before']['allocated_bytes']+3*bm.inventory(args.preseed)['allocated_bytes']+32*GIB+2*GIB
        runner.report.update(seed_manifest_sha256=seed_sha,overlay_manifest_sha256=bm.digest(setup_path),
            forecast_allocated_bytes=forecast,overlay_semantics={'reviewed_pages':127,'draft_pages':1,'records':133,'expected_diagnostics':[]})
        require(forecast<=protocol['limits']['account_allocated_bytes'] and runner.report['account_before']['free_bytes']-(forecast-runner.report['account_before']['allocated_bytes'])>=protocol['limits']['minimum_free_bytes'],'conservative allocation forecast refuses tier')
        runner.vault.mkdir(mode=0o700)
        for origin,entries in ((args.preseed/'vault',copied),(args.overlay/'vault',files)):
            for item in entries:
                source=origin.joinpath(*relative(item['path']).parts); regular(source)
                dest=runner.vault/item['path']; dest.parent.mkdir(parents=True,exist_ok=True)
                with source.open('rb') as inp,dest.open('xb') as out: shutil.copyfileobj(inp,out,1024*1024)
                require(dest.stat().st_size==item['bytes'] and bm.blake_digest(dest)==item['blake3'],'copied file pin differs')
        require(not (runner.vault/'.wiki').exists(),'private prebuild forbidden')
        # Authenticate all structural graph references without editing original
        # IDs. Overlay field formatting is the frozen canonical JSON-in-YAML.
        allowed_ids={s[k] for s in selected for k in ('source_id','revision_id')}
        for item in files:
            text=(runner.vault/item['path']).read_text()
            match=re.search(r'^wiki_id: (.+)$',text,re.M); require(match,'overlay ID missing')
            allowed_ids.add(json.loads(match[1]))
        for item in files:
            text=(runner.vault/item['path']).read_text()
            for field in ('wiki_source_id','wiki_source_revision','wiki_subject_id','wiki_object_id','wiki_assertion_id','wiki_depends_on_ids'):
                match=re.search(r'^'+field+r': (.+)$',text,re.M)
                if match:
                    value=json.loads(match[1]); values=value if isinstance(value,list) else [value]
                    require(all(value in allowed_ids for value in values),'outside subset structural reference')
        captured=f"sources/{first['source_id']}/revisions/{first['revision_id']}/content.md"
        authored='pages/general/000.md'
        runner.ownership={'version':1,'kind':'full-check-owned-copy','vault_path':str(runner.vault),
            'seed_manifest_sha256':seed_sha,'overlay_manifest_sha256':bm.digest(setup_path),
            'wiki_blake3':bm.blake_digest(runner.vault/'WIKI.md'),'source_count':len(selected),
            'original_seed_count':int(original_tier),'subset_rehearsal':args.tier=='tiny',
            'captured_path':captured,'captured_blake3':bm.blake_digest(runner.vault/captured),
            'authored_path':authored,'authored_blake3':bm.blake_digest(runner.vault/authored)}
        new_json(args.workdir/'ownership.json',runner.ownership)
        new_json(args.workdir/'copied-canonical.json',{'subset_rehearsal':args.tier=='tiny',
            'source_count':len(selected),'original_seed_count':int(original_tier),'seed_manifest_sha256':seed_sha,'files':copied,'overlay_files':files})
        runner.report['overlay_metadata_pins']={name:bm.digest(args.overlay/name) for name in ('setup-result.json','ownership.json','report.json')}
        runner.save()
        first_build,_=runner.command('rebuild-first',['index','rebuild','--normalized'],protocol['limits']['each_rebuild_watchdog_seconds'])
        first_data=first_build['data']; snap=first_data['report']['snapshot']; predecessor=snapshot_file_id(snap)
        require(first_data.get('dry_run') is False and first_data.get('report',{}).get('reused') is False
                and first_data.get('maintenance',{}).get('layout')=='normalized' and first_data['maintenance'].get('build') is not None,'first public rebuild oracle failed')
        build=first_data['maintenance']['build']
        require(build.get('records')==2*len(selected)+134 and build.get('documents')==3*len(selected)+134
                and build.get('graph_rows')==4 and build.get('diagnostics')==0,'public rebuild fixture counts differ')
        runner.start_holder(snap)
        rebuilt,_=runner.command('rebuild-second',['index','rebuild','--normalized'],protocol['limits']['each_rebuild_watchdog_seconds'])
        data=rebuilt['data']; current=data['report']['snapshot']; current_id=snapshot_file_id(current)
        require(current_id!=predecessor and current['generation']>snap['generation']
                and data['maintenance'].get('retirement_deferred') is True and data['report'].get('reused') is False,'second rebuild/held retirement oracle failed')
        require((runner.vault/f'.wiki/cache/catalogs/{predecessor}.sqlite').is_file(),'held predecessor disappeared')
        second=selected[1]; path=f"sources/{second['source_id']}/revisions/{second['revision_id']}/content.md"
        expected=(runner.vault/path).read_bytes(); old_original=(runner.vault/path).with_name('original.bin')
        old_binding={'content':bm.blake_digest(runner.vault/path),'original':bm.blake_digest(old_original)}
        runner.positive_pair('before-refresh',second,current,expected,second['marker'])
        payload,marker=bm.payload(731,1,1,100000); refresh_input=args.workdir/'refresh-input.md'
        with refresh_input.open('xb') as stream: stream.write(payload)
        refreshed,_=runner.command('managed-refresh',['source','refresh',second['source_id'],'--file',str(refresh_input)],protocol['limits']['managed_refresh_watchdog_seconds'])
        revision=refreshed['data']['allocated_ids']['revision']; updated=refreshed['data']['snapshot']
        require(revision!=second['revision_id'] and snapshot_file_id(updated)==current_id
                and updated['generation']>current['generation'],'managed refresh epoch/revision oracle failed')
        changed={**second,'revision_id':revision}
        runner.cited('immediate-after-refresh',changed,updated,payload,marker)
        require(old_binding=={'content':bm.blake_digest(runner.vault/path),'original':bm.blake_digest(old_original)},'old immutable payload changed')
        runner.positive_pair('after-refresh',changed,updated,payload,marker)
        runner.release_holder(True)
        _,final_seed=bm.validate_preseed(seed_args,SimpleNamespace(check_resources=runner.resources))
        require(final_seed==seed_sha and original_bindings(args.overlay/'vault',files)==overlay_before,'original fixture changed')
        require(seed_before=={entry['path']:(regular(args.preseed/'vault'/entry['path']).st_size,
                                           regular(args.preseed/'vault'/entry['path']).st_mtime_ns) for entry in corpus['files']},'original seed file metadata changed')
        for name,pin in runner.report['overlay_metadata_pins'].items(): require(bm.digest(args.overlay/name)==pin,'overlay metadata changed')
        runner.pins(); runner.report['account_after']=runner.resources()
        runner.report['status']='completed'; success=True
    except BaseException as error:
        runner.report['status']='failed'; runner.report['failures'].append(repr(error))
    finally:
        signal.alarm(0)
        if runner.active is not None: bm.stop(runner.active); runner.active.wait()
        if runner.holder is not None:
            try: runner.release_holder(False)
            except BaseException as error:
                runner.report['failures'].append('holder cleanup: '+repr(error))
                if runner.holder is not None: bm.stop(runner.holder); runner.holder.wait()
        runner.report['elapsed_seconds']=time.monotonic()-runner.start
        runner.save()
    print(json.dumps({'status':runner.report['status'],'report':str(args.workdir/'report.json')}))
    return 0 if success else 1


if __name__=='__main__':
    raise SystemExit(main())
