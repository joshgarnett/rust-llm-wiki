#!/usr/bin/env python3
"""Bounded macOS synthetic catalog diagnostic. Retains every artifact; never retries."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import signal
import stat
import subprocess
import sys
import time

GIB = 1024 ** 3
LIMITS = dict(seconds=900, tree_rss_bytes=8*GIB, allocated_bytes=4*GIB,
              free_bytes=32*GIB, rss_poll_seconds=1, disk_poll_seconds=5)
REPO = Path(__file__).resolve().parent.parent


def write_json(path, value):
    with path.open('x') as f:
        json.dump(value, f, indent=2, sort_keys=True)
        f.write('\n')


def digest(path):
    h = hashlib.sha256()
    with path.open('rb') as f:
        for chunk in iter(lambda: f.read(1024*1024), b''):
            h.update(chunk)
    return h.hexdigest()


def tree(path, hash_files=False):
    """lstat all entries; do not follow directory links or count hardlinks twice."""
    allocation = 0
    files = {}
    seen = set()
    for base, dirs, names in os.walk(path, followlinks=False):
        for name in sorted(dirs + names):
            p = Path(base)/name
            s = p.lstat()
            if stat.S_ISLNK(s.st_mode):
                raise ValueError(f'link forbidden: {p}')
            if not (stat.S_ISDIR(s.st_mode) or stat.S_ISREG(s.st_mode)):
                raise ValueError(f'nonregular entry: {p}')
            key = (s.st_dev, s.st_ino)
            if key not in seen:
                allocation += s.st_blocks * 512
                seen.add(key)
            if hash_files and stat.S_ISREG(s.st_mode):
                if s.st_nlink != 1:
                    raise ValueError(f'hardlink forbidden: {p}')
                files[p.relative_to(path).as_posix()] = dict(bytes=s.st_size, sha256=digest(p))
    return dict(allocated_bytes=allocation, files=files) if hash_files else allocation


def confined(path, account_root):
    if not path.is_absolute() or not path.resolve().is_relative_to(account_root.resolve()):
        raise ValueError('provenance path must be absolute and under account root')
    if path.resolve() != path:
        raise ValueError('provenance paths must be normalized and have no symlink components')
    return path


def prior_fixture(prior, account_root, stage):
    prior = confined(prior, account_root)
    summary_path = prior/'summary.json'
    summary = json.loads(summary_path.read_text())
    if summary.get('stage') != stage:
        raise ValueError('prior fixture stage differs')
    fixture = confined(Path(summary.get('fixture_path', str(prior/'fixture'))), account_root)
    if not fixture.is_dir() or fixture.is_symlink():
        raise ValueError('prior fixture is not a regular directory')
    commands = summary.get('commands', [])
    audited = any(c.get('name') in ('audit', 'audit-after') and c.get('passed') and
                  not c.get('expected_failure') for c in commands)
    generated = any(c.get('name') == 'generate' and c.get('passed') and
                    not c.get('expected_failure') for c in commands)
    replayed = isinstance(summary.get('fixture_provenance'), dict)
    if not audited or not (generated or replayed):
        raise ValueError('prior run lacks successful generation/replay provenance and audit')
    inventory = tree(fixture, True)
    if inventory != summary.get('fixture'):
        raise ValueError('prior fixture inventory/hash/size/allocation changed')
    manifest = fixture/'fixture-manifest.json'
    if json.loads(manifest.read_text()).get('sources') != int(stage):
        raise ValueError('prior fixture manifest source count differs')
    provenance = dict(prior_run=str(prior), prior_summary_sha256=digest(summary_path),
                      fixture_path=str(fixture), manifest_sha256=digest(manifest),
                      inventory=inventory, prior_status=summary.get('status'),
                      prior_pins=summary.get('pins'),
                      interpretation='Reuse authenticates retained bytes; previous binary pins may differ. Fresh inspections use current frozen pins.')
    return fixture, provenance


def pins(binary):
    paths = [binary, Path(__file__).resolve(), REPO/'examples/catalog_scaling_probe.rs']
    # Pin all Rust and manifest inputs; no Git subprocess or credential access.
    paths += sorted((REPO/'src').rglob('*.rs'))
    paths += [REPO/'Cargo.toml', REPO/'Cargo.lock']
    return {str(p): digest(p) for p in paths}


def rss(pid):
    result = subprocess.run(['/bin/ps', '-axo', 'pid=,ppid=,rss='],
                            capture_output=True, text=True, check=True, timeout=5)
    rows = [tuple(map(int, line.split())) for line in result.stdout.splitlines() if line.strip()]
    children = {pid}
    while True:
        new = children | {p for p, parent, _ in rows if parent in children}
        if new == children:
            break
        children = new
    return sum(k*1024 for p, _, k in rows if p in children)


class Runner:
    def __init__(self, args):
        self.args = args
        self.out = args.output
        self.commands = []
        self.frozen = pins(args.binary)
        self.protocol_hash = None

    def resources(self):
        return dict(allocated_bytes=tree(self.args.account_root),
                    free_bytes=shutil.disk_usage(self.args.account_root).free)

    def check_resources(self, sample):
        if sample['allocated_bytes'] > LIMITS['allocated_bytes']:
            return 'diagnostic allocated disk limit'
        if sample['free_bytes'] < LIMITS['free_bytes']:
            return 'free disk floor'
        return None

    def run(self, name, mode, fixture, count=None, failure=False):
        before = tree(fixture, True) if fixture.exists() else None
        if self.protocol_hash != digest(self.out/'protocol.json'):
            raise ValueError('protocol changed before command')
        if pins(self.args.binary) != self.frozen:
            raise ValueError('source/binary pins changed before command')
        resource = self.resources()
        reason = self.check_resources(resource)
        if reason:
            raise ValueError(reason)
        argv = ['/usr/bin/time', '-l', str(self.args.binary), mode, str(fixture)]
        if count is not None:
            argv.append(str(count))
        stdout = self.out/(name+'.stdout.json')
        stderr = self.out/(name+'.stderr.txt')
        record = dict(name=name, argv=argv, expected_failure=failure,
                      before=before, resource_before=resource, pins_before=self.frozen)
        write_json(self.out/(name+'.before.json'), record)
        start = time.monotonic()
        peak = 0
        violation = None
        last_disk = start
        with stdout.open('x') as out, stderr.open('x') as err:
            proc = subprocess.Popen(argv, stdout=out, stderr=err, start_new_session=True)
            try:
                while proc.poll() is None:
                    elapsed = time.monotonic()-start
                    peak = max(peak, rss(proc.pid))
                    if elapsed > LIMITS['seconds']:
                        violation = 'elapsed limit'
                    if peak > LIMITS['tree_rss_bytes']:
                        violation = 'sampled process-tree RSS limit'
                    if time.monotonic()-last_disk >= LIMITS['disk_poll_seconds']:
                        resource = self.resources()
                        last_disk = time.monotonic()
                        violation = violation or self.check_resources(resource)
                    if violation:
                        break
                    time.sleep(1)
            except BaseException:
                self.stop(proc)
                raise
            if violation:
                self.stop(proc)
            returncode = proc.wait()
        elapsed = time.monotonic()-start
        if elapsed > LIMITS['seconds']:
            violation = violation or 'elapsed limit'
        error_text = stderr.read_text()
        native = re.search(r'(\d+)\s+maximum resident set size', error_text)
        native_peak = int(native[1]) if native else None
        if native_peak is not None and native_peak > LIMITS['tree_rss_bytes']:
            violation = violation or 'native process RSS limit'
        if returncode == 0 and native_peak is None:
            violation = violation or 'native process RSS unavailable'
        after = tree(fixture, True) if fixture.exists() else None
        final_pins = pins(self.args.binary)
        final_resource = self.resources()
        violation = violation or self.check_resources(final_resource)
        unchanged = before == after if mode != 'generate' or failure else True
        expected_diagnostics = {
            'existing-generate': 'File exists',
            'payload-mutation': 'audit deterministic bytes/hash mismatch',
            'omitted-entry': 'unsupported fixture manifest',
            'duplicate-path': 'unsupported fixture manifest',
            'wrong-size': 'manifest path/size/canonical role mismatch',
            'wrong-role': 'manifest path/size/canonical role mismatch',
            'unlisted-canonical': 'unlisted fixture file',
        }
        intentional_failure = (returncode == 1 and 'catalog_scaling_probe:' in error_text
                               and expected_diagnostics.get(name, '\0') in error_text)
        record.update(returncode=returncode, external_elapsed_seconds=elapsed,
                      sampled_tree_peak_bytes=peak, native_peak_bytes=native_peak,
                      after=after, pins_after=final_pins, resource_after=final_resource,
                      watchdog_violation=violation, fixture_unchanged=unchanged,
                      sampling_limit='Polling may overshoot; native peak covers time child, sampled sum covers observed tree.',
                      passed=(not violation and unchanged and final_pins == self.frozen and
                              self.protocol_hash == digest(self.out/'protocol.json') and
                              (intentional_failure if failure else (returncode == 0))))
        if returncode == 0:
            try:
                record['result'] = json.loads(stdout.read_text())
            except ValueError:
                record['passed'] = False
        write_json(self.out/(name+'.after.json'), record)
        self.commands.append(record)
        if not record['passed']:
            raise ValueError(f'command failed protocol: {name}')
        return record

    @staticmethod
    def stop(proc):
        try:
            os.killpg(proc.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            try:
                os.killpg(proc.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            proc.wait()
        # Descendants may outlive their parent after TERM; kill the original group too.
        try:
            os.killpg(proc.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass

    def project(self, fixture):
        manifest = json.loads((fixture/'fixture-manifest.json').read_text())
        input_bytes = sum(e['bytes'] for e in manifest['files'])
        admission = dict(input_bytes=input_bytes, conservative_bytes=2*input_bytes,
                         admitted=2*input_bytes <= LIMITS['tree_rss_bytes'],
                         limitation='2x input is an auxiliary screening bound, not a proven peak estimate; watchdog remains required.')
        write_json(self.out/'project-admission.json', admission)
        if not admission['admitted']:
            raise ValueError('auxiliary full project memory admission refused')
        self.run('project', 'project', fixture)

    def smoke(self, fixture):
        self.run('generate', 'generate', fixture, 1)
        self.run('audit', 'audit', fixture)
        self.run('validate', 'validate', fixture)
        self.project(fixture)
        self.run('audit-after', 'audit', fixture)
        self.run('existing-generate', 'generate', fixture, 1, failure=True)
        for case in ['payload-mutation', 'omitted-entry', 'duplicate-path', 'wrong-size', 'wrong-role', 'unlisted-canonical']:
            target = self.out/case
            shutil.copytree(fixture, target, copy_function=shutil.copyfile)
            manifest_path = target/'fixture-manifest.json'
            m = json.loads(manifest_path.read_text())
            if case == 'payload-mutation':
                payload = target/next(e['path'] for e in m['files'] if not e['canonical'])
                with payload.open('r+b') as f:
                    byte = f.read(1)
                    f.seek(0)
                    f.write(bytes([byte[0] ^ 1]))
            elif case == 'unlisted-canonical':
                (target/'pages/unlisted.md').write_bytes((target/'pages/page_000000.md').read_bytes())
            else:
                if case == 'omitted-entry':
                    m['files'].pop()
                elif case == 'duplicate-path':
                    m['files'].insert(0, m['files'][0].copy())
                elif case == 'wrong-size':
                    m['files'][0]['bytes'] += 1
                elif case == 'wrong-role':
                    m['files'][0]['canonical'] = not m['files'][0]['canonical']
                manifest_path.write_text(json.dumps(m))
            self.run(case, 'audit', target, failure=True)

    def admission(self):
        baseline = self.args.baseline
        summary = json.loads((baseline/'summary.json').read_text())
        if summary['stage'] != '1000' or summary['status'] != 'passed' or summary['pins'] != self.frozen:
            raise ValueError('baseline is not passed 1000 with identical pins')
        baseline_fixture = confined(Path(summary.get('fixture_path', str(baseline/'fixture'))), self.args.account_root)
        if tree(baseline_fixture, True) != summary['fixture']:
            raise ValueError('baseline fixture pins changed')
        validate = next(c for c in summary['commands'] if c['name'] == 'validate')
        if validate['native_peak_bytes'] is None:
            raise ValueError('baseline native RSS unavailable')
        resource = self.resources()
        allocation = summary['fixture']['allocated_bytes']
        reserve = 256*1024*1024 if self.args.reuse_fixture_from else 12*allocation
        checks = dict(time=30*validate['external_elapsed_seconds'] < 900,
                      rss=20*max(validate['native_peak_bytes'], validate['sampled_tree_peak_bytes']) < 8*GIB,
                      disk=reserve+resource['allocated_bytes'] < 4*GIB,
                      free=resource['free_bytes']-reserve >= 32*GIB)
        admission = dict(checks=checks, admitted=all(checks.values()), baseline=str(baseline),
                         validation=validate, baseline_fixture_allocated_bytes=allocation,
                         current=resource, additional_disk_reserve_bytes=reserve,
                         variant='inspect-existing' if self.args.reuse_fixture_from else 'generate-new',
                         disk_interpretation='Reuse counts existing fixture in current allocation plus 256MiB log reserve; original generation extrapolation is not claimed passed.' if self.args.reuse_fixture_from else '12x new fixture allocation forecast.',
                         interpretation='Conservative safety admission only; no scaling claim.')
        write_json(self.out/'admission.json', admission)
        return admission['admitted']


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--binary', type=Path, required=True)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--account-root', type=Path, required=True)
    p.add_argument('--stage', choices=['smoke', '1000', '10000'], required=True)
    p.add_argument('--baseline', type=Path)
    p.add_argument('--reuse-fixture-from', type=Path)
    args = p.parse_args()
    for value in [args.binary, args.output, args.account_root] + ([args.baseline] if args.baseline else []):
        if not value.is_absolute():
            p.error('all paths must be absolute')
    if args.reuse_fixture_from and args.stage == 'smoke':
        p.error('reuse is allowed only for 1000/10000 stages')
    if args.reuse_fixture_from:
        try:
            confined(args.reuse_fixture_from, args.account_root)
        except ValueError as exc:
            p.error(str(exc))
    if sys.platform != 'darwin':
        p.error('requires macOS /usr/bin/time -l')
    if args.stage == '10000' and not args.baseline:
        p.error('10000 requires --baseline')
    if not args.account_root.is_dir() or not args.output.is_relative_to(args.account_root):
        p.error('output must lie under existing --account-root')
    if args.baseline and not args.baseline.is_relative_to(args.account_root):
        p.error('baseline must lie under --account-root')
    args.output.mkdir()  # refuses existing artifacts, including partial previous attempts
    runner = Runner(args)
    fixture = args.output/'fixture'
    provenance = None
    if args.reuse_fixture_from:
        try:
            fixture, provenance = prior_fixture(args.reuse_fixture_from, args.account_root, args.stage)
        except Exception as exc:
            write_json(args.output/'summary.json', dict(stage=args.stage, status='failed', error=str(exc), commands=[]))
            print(json.dumps(dict(status='failed', error=str(exc), output=str(args.output))))
            return 2
    protocol = dict(variant='inspect-existing' if provenance else 'generate-new',
                    fixture_provenance=provenance, schema='lwiki.metadata-probe-protocol.v1', stage=args.stage, limits=LIMITS,
                    sequence={'smoke':'generate1 audit validate project audit failures',
                              '1000':'generate1000 audit validate project audit',
                              '10000':'admission generate10000 audit validate audit'}[args.stage],
                    admission_10000='30*time<900;20*max(native,treeRSS)<8GiB;12*fixtureAllocation+accountAllocation<4GiB;free-12*fixtureAllocation>=32GiB',
                    manifest='fixture-manifest.json inside fixture, noncanonical, included in pins and allocation',
                    source_revision='Source content SHA256 map is authoritative; no Git process used.',
                    pins=runner.frozen)
    if provenance:
        protocol['sequence'] = 'audit validate project audit' if args.stage == '1000' else 'admission audit validate audit'
        protocol['admission_10000'] = '30*time<900;20*max(native,treeRSS)<8GiB;accountAllocation+256MiB<4GiB;free-256MiB>=32GiB'
        protocol['admission_explanation'] = 'Existing fixture is already counted in account allocation; 256MiB reserves log growth. This is a separately frozen inspect-existing protocol and does not claim original generation admission passed.'
    write_json(args.output/'protocol.json', protocol)
    runner.protocol_hash = digest(args.output/'protocol.json')
    write_json(args.output/'protocol-pin.json', {'sha256': runner.protocol_hash})
    status = 'failed'
    error = None
    try:
        if args.stage == '10000' and not runner.admission():
            status = 'refused'
        elif args.stage == 'smoke':
            runner.smoke(fixture)
            status = 'passed'
        else:
            if not provenance:
                runner.run('generate', 'generate', fixture, int(args.stage))
            runner.run('audit', 'audit', fixture)
            runner.run('validate', 'validate', fixture)
            if args.stage == '1000':
                runner.project(fixture)
            runner.run('audit-after', 'audit', fixture)
            status = 'passed'
    except Exception as exc:
        error = str(exc)
    try:
        final_fixture = tree(fixture, True) if fixture.exists() else None
        final_resource = runner.resources()
    except Exception as exc:
        final_fixture = None
        final_resource = {'error': str(exc)}
        error = error or str(exc)
        status = 'failed'
    if runner.protocol_hash != digest(args.output/'protocol.json') or pins(args.binary) != runner.frozen:
        status = 'failed'
        error = error or 'final protocol/source/binary pins changed'
    if provenance and digest(args.reuse_fixture_from/'summary.json') != provenance['prior_summary_sha256']:
        status = 'failed'
        error = error or 'prior summary changed during replay'
    summary = dict(fixture_path=str(fixture), fixture_provenance=provenance, stage=args.stage, status=status, error=error, pins=runner.frozen,
                   commands=runner.commands, fixture=final_fixture,
                   protocol_sha256=digest(args.output/'protocol.json'), resource_final=final_resource)
    write_json(args.output/'summary.json', summary)
    print(json.dumps(dict(status=status, output=str(args.output), error=error)))
    return 0 if status == 'passed' else 2


if __name__ == '__main__':
    sys.exit(main())
