#!/usr/bin/env python3
"""Observe exact browser installer download on a fresh hosted Mac, never a personal host."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import sqlite3
import subprocess
import sys
import time


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def save(path, value):
    path.write_text(json.dumps(value, indent=2) + '\n')


def attrs(path):
    return {name: os.getxattr(path, name).hex() for name in os.listxattr(path)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for key in ['assets', 'archive-sha256', 'manifest-sha256', 'binary-sha256', 'installer-commit', 'installer-sha256', 'source-provenance', 'evidence']:
        parser.add_argument('--' + key, required=True)
    args = parser.parse_args()
    if sys.platform != 'darwin' or os.getuid() == 0 or os.environ.get('GITHUB_ACTIONS') != 'true' or os.environ.get('RUNNER_ENVIRONMENT') != 'github-hosted':
        parser.error('requires a fresh nonroot GitHub-hosted Mac')
    if os.uname().machine != 'arm64':
        parser.error('requires Apple Silicon')
    if not re.fullmatch('[0-9a-f]{40}', args.installer_commit) or any(not re.fullmatch('[0-9a-f]{64}', value) for value in [args.archive_sha256, args.manifest_sha256, args.binary_sha256, args.installer_sha256]):
        parser.error('requires full lowercase commit and SHA256 pins')
    runner_temp = Path(os.environ['RUNNER_TEMP']).resolve(strict=True)
    root = Path(args.evidence).resolve()
    if runner_temp not in root.parents:
        parser.error('evidence must be a new directory below RUNNER_TEMP')
    assets = Path(args.assets).resolve(strict=True)
    archive = assets / 'lwiki-0.3.0-aarch64-apple-darwin.tar.gz'
    manifest = assets / 'release-manifest.txt'
    if digest(archive) != args.archive_sha256 or digest(manifest) != args.manifest_sha256:
        parser.error('candidate assets differ from verified pins')
    provenance = json.loads(Path(args.source_provenance).read_text())
    root.mkdir(mode=0o700)
    for name in ['home', 'temp', 'config', 'cache']:
        (root / name).mkdir(mode=0o700)
    env = {'PATH': '/usr/bin:/bin:/usr/sbin:/sbin', 'HOME': str(root / 'home'), 'TMPDIR': str(root / 'temp') + '/', 'LANG': 'en_US.UTF-8', 'TZ': 'UTC', 'XDG_CONFIG_HOME': str(root / 'config'), 'XDG_CACHE_HOME': str(root / 'cache'), 'GITHUB_ACTIONS': 'true', 'RUNNER_ENVIRONMENT': 'github-hosted'}
    result = {'status': 'running', 'archiveSHA256': args.archive_sha256, 'binarySHA256': args.binary_sha256, 'manifestSHA256': args.manifest_sha256, 'installerCommit': args.installer_commit, 'installerSHA256': args.installer_sha256, 'candidateOrigin': provenance, 'candidateTransport': 'ordinary HTTPS Actions artifact download; not browser-downloaded archive', 'publicReleaseURL': 'unobserved before publication', 'offlineBrowserArchiveRoute': 'unobserved; no synthetic quarantine substituted', 'securityChanges': 'none', 'approvalActions': 'none', 'runner': {key: os.environ.get(key) for key in ['ImageOS', 'ImageVersion', 'RUNNER_OS', 'RUNNER_ARCH', 'GITHUB_SHA', 'GITHUB_RUN_ID']}}
    save(root / 'result.json', result)
    commands = []

    def command(name, argv, timeout=30, allow_failure=False):
        start = time.monotonic()
        process = subprocess.Popen(argv, env=env, cwd=root, stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
        try:
            if name == 'install':
                observations = []
                while True:
                    stage = root / 'prefix/.lwiki-install/.stage'
                    for member in ['archive.tar.gz', 'lwiki', 'version.out', 'version.err']:
                        path = stage / member
                        try:
                            row = {'elapsedSeconds': time.monotonic() - start, 'member': member, 'bytes': path.stat().st_size, 'attributes': attrs(path)}
                            if member.startswith('version.'):
                                row['text'] = path.read_bytes()[:1024].decode(errors='replace')
                            if any({k: v for k, v in previous.items() if k != 'elapsedSeconds'} == {k: v for k, v in row.items() if k != 'elapsedSeconds'} for previous in observations):
                                continue
                            observations.append(row)
                        except FileNotFoundError:
                            pass
                    save(root / 'stage-observations.json', observations)
                    remaining = start + timeout - time.monotonic()
                    if remaining <= 0:
                        raise subprocess.TimeoutExpired(argv, timeout)
                    try:
                        stdout, stderr = process.communicate(timeout=min(0.1, remaining))
                        break
                    except subprocess.TimeoutExpired:
                        continue
            else:
                stdout, stderr = process.communicate(timeout=timeout)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            stdout, stderr = process.communicate()
            commands.append({'name': name, 'argv': argv, 'exit': process.returncode, 'timeout': True, 'elapsedSeconds': time.monotonic() - start})
            (root / (name + '.stdout')).write_bytes(stdout)
            (root / (name + '.stderr')).write_bytes(stderr)
            save(root / 'commands.json', commands)
            raise RuntimeError(name + ' timed out')
        commands.append({'name': name, 'argv': argv, 'exit': process.returncode, 'elapsedSeconds': time.monotonic() - start})
        (root / (name + '.stdout')).write_bytes(stdout)
        (root / (name + '.stderr')).write_bytes(stderr)
        save(root / 'commands.json', commands)
        if process.returncode and not allow_failure:
            raise RuntimeError(name + ' failed; retained stdout/stderr')
        return process.returncode, stdout

    try:
        result['macOS'] = command('macos-version', ['/usr/bin/sw_vers'])[1].decode()
        result['gatekeeper'] = command('gatekeeper', ['/usr/sbin/spctl', '--status'])[1].decode().strip()
        if result['gatekeeper'] != 'assessments enabled':
            raise RuntimeError('host Gatekeeper is not enabled; cannot qualify default protection')
        command('sip-state', ['/usr/bin/csrutil', 'status'], allow_failure=True)
        result['archiveAttributesBefore'] = attrs(archive)
        node = subprocess.check_output(['/usr/bin/which', 'node'], text=True).strip()
        url = 'https://raw.githubusercontent.com/joshgarnett/rust-llm-wiki/' + args.installer_commit + '/scripts/install.sh'
        result['installerURL'] = url
        browser_code, _ = command('browser', [node, str(Path(__file__).with_name('browser.mjs')), str(root), url, args.installer_sha256], timeout=120, allow_failure=True)
        installer = root / 'downloads/install.sh'
        if installer.exists():
            result['installerAttributesBefore'] = attrs(installer)
        if browser_code:
            raise RuntimeError('browser download failed; retained actual browser UI/result')
        if digest(installer) != args.installer_sha256:
            raise RuntimeError('downloaded installer changed')
        result['installerAttributesBefore'] = attrs(installer)
        if not result['installerAttributesBefore'].get('com.apple.quarantine'):
            raise RuntimeError('browser download has no genuine quarantine; cannot claim browser trust evidence')
        history = root / 'browser-profile/Default/History'
        with sqlite3.connect('file:' + str(history) + '?mode=ro', uri=True) as connection:
            connection.row_factory = sqlite3.Row
            result['browserDownloadHistory'] = [dict(row) for row in connection.execute('SELECT * FROM downloads')]
            result['browserDownloadURLChains'] = [dict(row) for row in connection.execute('SELECT * FROM downloads_url_chains')]
        if not any(row.get('url') == url for row in result['browserDownloadURLChains']):
            raise RuntimeError('browser history does not establish exact public download URL')
        prefix = root / 'prefix'
        code, _ = command('install', ['/bin/sh', str(installer), '--version', '0.3.0', '--prefix', str(prefix), '--from-dir', str(assets)], timeout=60, allow_failure=True)
        result['installExit'] = code
        result['installerAttributesAfter'] = attrs(installer)
        result['archiveAttributesAfter'] = attrs(archive)
        result['installerSHA256After'] = digest(installer)
        result['archiveSHA256After'] = digest(archive)
        if code:
            result['installation'] = 'refused; no approval or quarantine change attempted'
            raise RuntimeError('shell installer refused candidate; retained actual diagnostic')
        installed = prefix / 'bin/lwiki'
        result['installedRealPath'] = str(installed.resolve(strict=True))
        result['installedSHA256'] = digest(installed)
        result['installedAttributes'] = attrs(installed)
        if result['installedSHA256'] != args.binary_sha256:
            raise RuntimeError('installed executable differs from final archive')
        result['version'] = command('installed-version', [str(installed), '--version'])[1].decode().strip()
        if result['version'] != 'lwiki 0.3.0':
            raise RuntimeError('installed executable reports unexpected version')
        if result['archiveAttributesBefore'] != result['archiveAttributesAfter'] or result['installerAttributesBefore'] != result['installerAttributesAfter']:
            raise RuntimeError('input quarantine or other attributes changed')
        if result['archiveSHA256After'] != args.archive_sha256 or result['installerSHA256After'] != args.installer_sha256:
            raise RuntimeError('input asset bytes changed')
        result['status'] = 'passed'
    except Exception as error:
        result['status'] = 'failed'
        result['error'] = str(error)
    finally:
        pidfile = root / 'browser-pid.json'
        if pidfile.exists():
            pid = json.loads(pidfile.read_text())['pid']
            try:
                os.killpg(pid, 0)
                os.killpg(pid, signal.SIGKILL)
                result['supervisorForcedCleanup'] = True
            except ProcessLookupError:
                result['supervisorForcedCleanup'] = False
            until = time.monotonic() + 3
            while time.monotonic() < until:
                try:
                    os.killpg(pid, 0)
                except ProcessLookupError:
                    break
                time.sleep(0.1)
            try:
                os.killpg(pid, 0)
                result['status'] = 'failed'
                result['error'] = 'owned browser process group survived cleanup'
            except ProcessLookupError:
                result['browserProcessGroupGone'] = True
        save(root / 'result.json', result)
    return 0 if result['status'] == 'passed' else 1


if __name__ == '__main__':
    sys.exit(main())
