#!/usr/bin/env python3
"""Observe exact browser installer download on a fresh hosted Mac, never a personal host."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import plistlib
import pwd
import re
import shlex
import signal
import sqlite3
import stat
import subprocess
import sys
import time


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def save(path, value):
    path.write_text(json.dumps(value, indent=2) + '\n')


def attrs(path):
    # macOS's stock Python does not expose the Linux-only os xattr helpers.
    # Use the native read-only tool; -px represents arbitrary values as hex.
    path = Path(path).absolute()
    deadline = time.monotonic() + 2
    env = {'PATH': '/usr/bin:/bin', 'LC_ALL': 'C', 'LANG': 'C'}

    def read(arguments):
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise RuntimeError('native xattr observation exceeded its deadline')
        response = subprocess.run(['/usr/bin/xattr', *arguments], env=env,
                                  stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                  timeout=remaining, check=False)
        if response.returncode:
            # Short-lived installer staging files may disappear between samples.
            if not path.exists():
                raise FileNotFoundError(path)
            raise RuntimeError('native xattr read failed: ' + response.stderr.decode(errors='replace').strip())
        return response.stdout

    names = read(['--', str(path)]).decode('utf-8').splitlines()
    if len(names) > 256 or len(set(names)) != len(names):
        raise RuntimeError('native xattr returned too many or duplicate names')
    return {name: bytes.fromhex(read(['-px', '--', name, str(path)]).decode('ascii')).hex()
            for name in names}


def hosted_browser_home():
    account = pwd.getpwuid(os.getuid())
    home = Path(account.pw_dir).resolve(strict=True)
    inherited = Path(os.environ['HOME']).resolve(strict=True)
    if not home.is_dir() or home.stat().st_uid != os.getuid() or inherited != home:
        raise RuntimeError('normal runner HOME must match the owned passwd account directory')
    return home


def download_directory_observation(directory):
    limits = {'maxEntries': 64, 'maxNameBytes': 512}
    observation = {'limits': limits, 'entries': [], 'truncated': False}
    try:
        if directory.is_symlink() or not directory.is_dir() or directory.stat().st_uid != os.getuid():
            raise RuntimeError('owned download directory is unavailable')
        with os.scandir(directory) as entries:
            for entry in entries:
                if len(observation['entries']) >= limits['maxEntries']:
                    observation['truncated'] = True
                    break
                metadata = entry.stat(follow_symlinks=False)
                encoded_name = entry.name.encode('utf-8', errors='replace')
                observation['entries'].append({
                    'name': encoded_name[:limits['maxNameBytes']].decode('utf-8', errors='ignore'),
                    'nameTruncated': len(encoded_name) > limits['maxNameBytes'],
                    'bytes': metadata.st_size,
                    'type': 'file' if stat.S_ISREG(metadata.st_mode) else 'directory' if stat.S_ISDIR(metadata.st_mode) else 'symlink' if stat.S_ISLNK(metadata.st_mode) else 'other',
                    'partial': entry.name.endswith('.crdownload'),
                })
        observation['entries'].sort(key=lambda entry: entry['name'])
        observation['status'] = 'observed'
    except (OSError, RuntimeError) as error:
        observation.update(status='unavailable', error=str(error))
    return observation


def download_history_observation(history):
    limits = {'maxRowsPerTable': 64, 'maxTextBytes': 2048, 'maxDatabaseBytes': 64 * 1024 * 1024,
              'maxSQLValueCharacters': 2049, 'maxElapsedSeconds': 2}
    observation = {'limits': limits, 'scope': 'read-only download tables only; no visits, cookies, credentials or profile upload',
                   'downloads': [], 'urlChains': [], 'truncated': {'downloads': False, 'urlChains': False, 'fieldValues': False}}
    begin = time.monotonic()
    deadline = begin + limits['maxElapsedSeconds']
    try:
        metadata = history.lstat()
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != os.getuid() or metadata.st_size > limits['maxDatabaseBytes']:
            raise RuntimeError('owned regular bounded browser History is unavailable')
        observation['databaseBytes'] = metadata.st_size
        with sqlite3.connect(history.absolute().as_uri() + '?mode=ro', uri=True, timeout=1) as connection:
            connection.execute('PRAGMA query_only=ON')
            connection.set_progress_handler(lambda: time.monotonic() >= deadline, 1000)
            connection.row_factory = sqlite3.Row
            tables = [
                ('downloads', 'downloads', ['id', 'guid', 'current_path', 'target_path', 'start_time', 'end_time',
                                          'received_bytes', 'total_bytes', 'state', 'danger_type', 'interrupt_reason',
                                          'opened', 'transient', 'mime_type', 'original_mime_type'], ['id'],
                 {'id', 'current_path', 'target_path', 'received_bytes', 'total_bytes', 'state', 'danger_type', 'interrupt_reason'}),
                ('downloads_url_chains', 'urlChains', ['id', 'chain_index', 'url'], ['id', 'chain_index'],
                 {'id', 'chain_index', 'url'}),
            ]
            for table, key, allowed, ordering, required in tables:
                available = {row['name'] for row in connection.execute('SELECT name FROM pragma_table_info(?) LIMIT 65', [table])}
                if not required <= available:
                    raise RuntimeError('browser History download schema is unavailable: ' + table)
                columns = [column for column in allowed if column in available]
                selected = ','.join(
                    'CASE WHEN typeof("' + column + '\") IN (\'text\',\'blob\') THEN substr("' + column
                    + '\",1,' + str(limits['maxSQLValueCharacters']) + ') ELSE "' + column + '\" END AS "' + column + '\"'
                    for column in columns)
                ordered = ','.join('"' + column + '"' for column in ordering)
                rows = connection.execute('SELECT ' + selected + ' FROM "' + table + '" ORDER BY ' + ordered + ' LIMIT ?',
                                          [limits['maxRowsPerTable'] + 1]).fetchall()
                observation['truncated'][key] = len(rows) > limits['maxRowsPerTable']
                for row in rows[:limits['maxRowsPerTable']]:
                    decoded = dict(row)
                    for column, value in decoded.items():
                        if isinstance(value, str):
                            encoded = value.encode('utf-8')
                            if len(encoded) > limits['maxTextBytes']:
                                decoded[column] = encoded[:limits['maxTextBytes']].decode('utf-8', errors='ignore')
                                observation['truncated']['fieldValues'] = True
                        elif value is not None and not isinstance(value, (int, float)):
                            raise RuntimeError('browser History download field has an unsupported type')
                    observation[key].append(decoded)
                if time.monotonic() >= deadline:
                    raise RuntimeError('browser History observation exceeded its deadline')
        observation['status'] = 'observed'
    except (OSError, sqlite3.Error, RuntimeError) as error:
        observation.update(status='unavailable', error=str(error))
    observation['elapsedSeconds'] = time.monotonic() - begin
    return observation


def validate_download_history(history, browser, installer, clicked_url, final_url):
    if (history.get('status') != 'observed'
            or any(history.get('truncated', {}).get(key) is not False for key in ['downloads', 'urlChains', 'fieldValues'])):
        raise RuntimeError('browser download History observation is unavailable or incomplete')
    if (browser.get('status') != 'passed' or browser.get('forcedCleanup') is not False
            or browser.get('shutdown', {}).get('normalExit') is not True
            or type(browser.get('childExit', {}).get('code')) is not int
            or browser['childExit']['code'] != 0
            or 'signal' not in browser.get('childExit', {})
            or browser.get('childExit', {}).get('signal') is not None):
        raise RuntimeError('browser did not establish completion followed by normal child/process-group exit')
    completion = browser.get('downloadCompletion', {})
    ui_id = completion.get('identity', {}).get('id')
    if completion.get('status') != 'complete' or not isinstance(ui_id, str) or not re.fullmatch('[0-9]+', ui_id):
        raise RuntimeError('browser completed UI download ID is unavailable')
    downloads = history.get('downloads', [])
    if len(downloads) != 1:
        raise RuntimeError('browser History must contain exactly the one completed UI download')
    row = downloads[0]
    expected = {'id': int(ui_id), 'received_bytes': 37470, 'total_bytes': 37470,
                'state': 1, 'danger_type': 0, 'interrupt_reason': 0}
    if any(type(row.get(key)) is not int or row[key] != value for key, value in expected.items()):
        raise RuntimeError('browser History ID, bytes or clean completion state differs from the completed UI item')
    if row.get('current_path') != str(installer) or row.get('target_path') != str(installer):
        raise RuntimeError('browser History paths differ from the owned completed installer')
    chain = history.get('urlChains', [])
    if (len(chain) < 2 or any(type(member.get('id')) is not int or member['id'] != row['id']
                              or type(member.get('chain_index')) is not int or not isinstance(member.get('url'), str)
                              for member in chain)):
        raise RuntimeError('browser History URL chain does not belong to the completed download ID')
    if [member['chain_index'] for member in chain] != list(range(len(chain))):
        raise RuntimeError('browser History URL chain is not a complete ordered chain')
    if chain[0]['url'] != clicked_url or chain[-1]['url'] != final_url:
        raise RuntimeError('browser History URL chain does not bind the exact clicked Raw URL to the exact final URL')
    return {'uiID': ui_id, 'historyID': row['id'], 'download': row, 'orderedURLChain': chain}


def validate_where_froms(attributes, final_url, page_url):
    encoded = attributes.get('com.apple.metadata:kMDItemWhereFroms')
    if not isinstance(encoded, str) or len(encoded) > 128 * 1024:
        raise RuntimeError('browser WhereFroms metadata is unavailable or exceeds its bound')
    try:
        origins = plistlib.loads(bytes.fromhex(encoded))
    except (ValueError, plistlib.InvalidFileException) as error:
        raise RuntimeError('browser WhereFroms metadata is not a valid property list') from error
    if (not isinstance(origins, list) or len(origins) > 64
            or any(not isinstance(value, str) or len(value) > 2048 for value in origins)
            or final_url not in origins or page_url not in origins):
        raise RuntimeError('browser WhereFroms metadata does not bind the exact final installer URL and source page')
    return origins


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
    browser_evidence_collected = False
    browser_cleanup_done = False

    def command(name, argv, timeout=30, allow_failure=False, command_env=None):
        start = time.monotonic()
        process = subprocess.Popen(argv, env=env if command_env is None else command_env, cwd=root, stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
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

    def observe_browser_download():
        nonlocal browser_evidence_collected
        browser_evidence_collected = True
        result['browserDownloadDirectory'] = download_directory_observation(root / 'downloads')
        history = download_history_observation(root / 'browser-profile/Default/History')
        save(root / 'browser-download-history.json', history)
        result['browserDownloadHistoryEvidence'] = history
        result['browserDownloadHistory'] = history['downloads']
        result['browserDownloadURLChains'] = history['urlChains']
        save(root / 'result.json', result)

    def cleanup_browser_group():
        nonlocal browser_cleanup_done
        browser_cleanup_done = True
        pidfile = root / 'browser-pid.json'
        if not pidfile.exists():
            return
        pid = json.loads(pidfile.read_text())['pid']
        try:
            os.killpg(pid, 0)
            os.killpg(pid, signal.SIGKILL)
            result['supervisorForcedCleanup'] = True
            result['status'] = 'failed'
            result.setdefault('error', 'browser required supervisor forced cleanup')
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
            result.setdefault('error', 'owned browser process group survived cleanup')
        except ProcessLookupError:
            result['browserProcessGroupGone'] = True

    try:
        result['macOS'] = command('macos-version', ['/usr/bin/sw_vers'])[1].decode()
        result['gatekeeper'] = command('gatekeeper', ['/usr/sbin/spctl', '--status'])[1].decode().strip()
        if result['gatekeeper'] != 'assessments enabled':
            raise RuntimeError('host Gatekeeper is not enabled; cannot qualify default protection')
        command('sip-state', ['/usr/bin/csrutil', 'status'], allow_failure=True)
        result['archiveAttributesBefore'] = attrs(archive)
        browser_home = hosted_browser_home()
        browser_env = dict(env, HOME=str(browser_home))
        result['homeIsolation'] = {'browserHOME': str(browser_home), 'controllerInstallerCLIHOME': env['HOME'], 'browserProfile': str(root / 'browser-profile'), 'keychainOperations': 'read-only default/search-list paths; no create/unlock/reset/preference changes'}
        result['keychainHomes'] = {}
        for label, keychain_env in [('fixture', env), ('normal-runner', browser_env)]:
            observation = {'home': keychain_env['HOME'], 'commands': {}}
            for operation in ['default-keychain', 'list-keychains']:
                argv = ['/usr/bin/security', operation, '-d', 'user']
                try:
                    code, output = command('keychain-' + label + '-' + operation, argv, timeout=5, allow_failure=True, command_env=keychain_env)
                    paths = shlex.split(output.decode())
                    if any(not Path(value).is_absolute() for value in paths):
                        raise RuntimeError('keychain path observation returned a nonabsolute path')
                    observation['commands'][operation] = {'argv': argv, 'exit': code, 'paths': paths}
                except RuntimeError as error:
                    observation['commands'][operation] = {'argv': argv, 'error': str(error)}
            result['keychainHomes'][label] = observation
            save(root / 'result.json', result)
        normal = result['keychainHomes']['normal-runner']['commands']
        default_paths = normal['default-keychain'].get('paths', [])
        if (normal['default-keychain'].get('exit') != 0 or normal['list-keychains'].get('exit') != 0
                or len(default_paths) != 1 or not Path(default_paths[0]).is_file()):
            raise RuntimeError('normal runner default keychain is unavailable; no creation, unlock, reset or fallback attempted')
        node = subprocess.check_output(['/usr/bin/which', 'node'], text=True).strip()
        url = 'https://raw.githubusercontent.com/joshgarnett/rust-llm-wiki/' + args.installer_commit + '/scripts/install.sh'
        result['installerURL'] = url
        page_url = 'https://github.com/joshgarnett/rust-llm-wiki/blob/' + args.installer_commit + '/scripts/install.sh'
        result['publicTransportDiagnostic'] = {'url': page_url, 'scope': 'system curl transport only; not browser download proof'}
        try:
            code, timing = command('public-transport-head', ['/usr/bin/curl', '-q', '--proto', '=https', '--proto-redir', '=https', '--head', '--silent', '--show-error', '--connect-timeout', '5', '--max-time', '10', '--dump-header', str(root / 'public-transport-headers.stdout'), '--output', '/dev/null', '--write-out', 'http=%{http_code} dns=%{time_namelookup} connect=%{time_connect} tls=%{time_appconnect} first_byte=%{time_starttransfer} total=%{time_total}\n', page_url], timeout=12, allow_failure=True)
            result['publicTransportDiagnostic'].update(exit=code, metrics=timing.decode())
        except RuntimeError as error:
            # The independent transport diagnostic must not replace browser evidence.
            result['publicTransportDiagnostic']['error'] = str(error)
        browser_code, _ = command('browser', [node, str(Path(__file__).with_name('browser.mjs')), str(root), url, args.installer_sha256, str(browser_home)], timeout=120, allow_failure=True)
        browser = json.loads((root / 'browser-result.json').read_text())
        if browser_code:
            result['status'] = 'failed'
            result.setdefault('error', 'browser download failed: ' + str(browser.get('error', 'retained actual browser UI/result')))
        cleanup_browser_group()
        observe_browser_download()
        installer = root / 'downloads/install.sh'
        if installer.exists():
            result['installerAttributesBefore'] = attrs(installer)
        if browser_code:
            raise RuntimeError('browser download failed; retained actual browser UI/result')
        if result.get('supervisorForcedCleanup') or result.get('browserProcessGroupGone') is not True:
            raise RuntimeError('browser did not finish normal shutdown before the original History observation')
        metadata = installer.lstat()
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != os.getuid() or metadata.st_size != 37470:
            raise RuntimeError('browser installer is not the exact owned regular 37470-byte file')
        if digest(installer) != args.installer_sha256:
            raise RuntimeError('downloaded installer changed')
        result['installerAttributesBefore'] = attrs(installer)
        if not result['installerAttributesBefore'].get('com.apple.quarantine'):
            raise RuntimeError('browser download has no genuine quarantine; cannot claim browser trust evidence')
        result['installerWhereFroms'] = validate_where_froms(result['installerAttributesBefore'], url, page_url)
        history = result['browserDownloadHistoryEvidence']
        clicked_url = 'https://github.com/joshgarnett/rust-llm-wiki/raw/' + args.installer_commit + '/scripts/install.sh'
        result['browserDownloadHistoryBinding'] = validate_download_history(history, browser, installer, clicked_url, url)
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
        result.setdefault('error', str(error))
    finally:
        if not browser_cleanup_done:
            cleanup_browser_group()
        if not browser_evidence_collected:
            observe_browser_download()
        save(root / 'result.json', result)
    return 0 if result['status'] == 'passed' else 1


if __name__ == '__main__':
    sys.exit(main())
