#!/usr/bin/env python3
"""Check reviewed documentation inventory, fresh-clone links and prose policy."""
import argparse
from pathlib import Path, PurePosixPath
import json
import re
import subprocess
import unicodedata
import urllib.parse

MANIFEST = 'docs/developer/documentation-policy.json'
CLASSES = {'user_guide', 'developer_contract', 'developer_protocol', 'developer_reference',
           'agent_instruction', 'generated_reference', 'documentation_asset', 'policy', 'changelog'}
ROOT_DOCS = {'README.md', 'AGENTS.md', 'PROGRESS.md'}
DENIED_NAMES = {'state.md', 'progress.md', 'worklog.md', 'session.md'}
DENIED_PARTS = {'reports', 'reviews', 'logs', 'checkpoints', 'working-plans'}


def prose_lines(text):
    """Omit exact code and quoted evidence; preserve line numbers for diagnostics."""
    fence = None
    for number, line in enumerate(text.splitlines(), 1):
        marker = re.match(r'^\s*(`{3,}|~{3,})', line)
        if marker:
            token = marker.group(1)
            if fence is None:
                fence = token
            elif token[0] == fence[0] and len(token) >= len(fence):
                fence = None
            continue
        if fence is None and not line.lstrip().startswith('>'):
            yield number, line


def heading_anchors(text):
    anchors, counts = set(), {}
    for _, line in prose_lines(text):
        match = re.match(r'^#{1,6}\s+(.+?)\s*#*$', line)
        if not match:
            continue
        title = re.sub(r'\[([^]]+)\]\([^)]*\)', r'\1', match.group(1))
        title = re.sub(r'<[^>]+>', '', title).lower()
        slug = ''.join(c for c in title if c in '_- ' or unicodedata.category(c)[0] in 'LNM')
        slug = slug.replace(' ', '-')
        count = counts.get(slug, 0)
        counts[slug] = count + 1
        anchors.add(slug + (f'-{count}' if count else ''))
    return anchors


def documentation_path(path):
    return path in ROOT_DOCS or path.startswith(('docs/', 'skills/llm-wiki/'))


def prose_issues(text, classification):
    lines = list(prose_lines(text))
    for number, line in lines:
        unquoted = re.sub(r'`+[^`]*`+|https?://\S+|“[^”]*”|"[^"\n]*"', '', line)
        if '\u2014' in unquoted:
            yield number, 'em dash in prose; use ordinary punctuation'
        if any(0x1F000 <= ord(c) <= 0x1FAFF or 0x2600 <= ord(c) <= 0x27BF for c in unquoted):
            yield number, 'emoji or pictograph in prose'
    # Structural run-ledger signals, not generic bans on words such as protocol,
    # checkpoint or candidate. Explicit reviewed release changelogs may use dates.
    diary_title = re.compile(r'^#\s+(?:progress|execution log|session log|worklog|implementation diary|session checkpoint)\b', re.I)
    for number, line in lines:
        if diary_title.search(line):
            yield number, 'session/worklog title belongs in ignored local evidence'
    if classification == 'changelog':
        return
    dated = [(number, line) for number, line in lines
             if re.match(r'^#{1,4}\s+(?:20\d\d-\d\d-\d\d\b|(?:attempt|session|candidate)[ -]?\d+\b)', line, re.I)]
    if len(dated) >= 3:
        yield dated[0][0], 'repeated dated/attempt sections resemble a run diary; curate enduring findings'
    metadata = [(number, line) for number, line in lines
                if re.match(r'^(?:Current checkpoint|Goal active|Worker|Lease|Files changed|Next step|Commit):', line, re.I)]
    if len(metadata) >= 2:
        yield metadata[0][0], 'session metadata ledger belongs in ignored local evidence'


def visible_paths(root):
    result = subprocess.run(['git', 'ls-files', '-z', '--cached', '--others', '--exclude-standard'],
                            cwd=root, check=True, stdout=subprocess.PIPE)
    return {path.decode('utf-8') for path in result.stdout.split(b'\0') if path}


def validate(root, visible, manifest=None):
    root = Path(root).resolve()
    if manifest is None:
        manifest = json.loads((root / MANIFEST).read_text(encoding='utf-8'))
    errors = []
    if manifest.get('schema_version') != 'lwiki.documentation-policy.v1':
        return [f'{MANIFEST}: unsupported documentation policy version']
    entries = {}
    for entry in manifest.get('documents', []):
        path = entry.get('path', '')
        parts = PurePosixPath(path).parts
        if not path or PurePosixPath(path).is_absolute() or '..' in parts or path in entries:
            errors.append(f'{MANIFEST}: invalid or duplicate inventory path {path!r}')
            continue
        if entry.get('classification') not in CLASSES or len(entry.get('purpose', '').strip()) < 20:
            errors.append(f'{MANIFEST}: {path}: explicit enduring classification and purpose required')
        entries[path] = entry
    redirects = manifest.get('migration_paths', {})
    for path in sorted(entries):
        if not (root / path).is_file() or path not in visible:
            errors.append(f'{path}: reviewed document missing from visible checkout')
    for path in sorted(p for p in visible if documentation_path(p) and (root / p).is_file()):
        parts = PurePosixPath(path).parts
        if parts[-1].lower() in DENIED_NAMES or any(part.lower() in DENIED_PARTS for part in parts):
            errors.append(f'{path}: session state/raw report tree cannot be published documentation')
        if path not in entries:
            errors.append(f'{path}: absent from reviewed documentation inventory')
            continue
        if not path.endswith('.md'):
            continue
        text = (root / path).read_text(encoding='utf-8')
        for line, reason in prose_issues(text, entries[path]['classification']):
            errors.append(f'{path}:{line}: {reason}')
        for number, line in prose_lines(text):
            link_line = re.sub(r'`+[^`]*`+', '', line)
            for match in re.finditer(r'\]\(([^\s)]+)(?:\s+"[^"\n]*")?\)', link_line):
                target = match.group(1).strip('<>')
                parsed = urllib.parse.urlsplit(target)
                if parsed.scheme or parsed.netloc:
                    continue
                candidate = (root / path).parent / urllib.parse.unquote(parsed.path)
                candidate = candidate.resolve() if parsed.path else (root / path).resolve()
                try:
                    relative = candidate.relative_to(root).as_posix()
                except ValueError:
                    errors.append(f'{path}:{number}: local link escapes repository: {target}')
                    continue
                present = relative in visible if candidate.is_file() else any(p.startswith(relative + '/') for p in visible)
                if not candidate.exists() or not present:
                    hint = f'; use {redirects[relative]}' if relative in redirects else ''
                    errors.append(f'{path}:{number}: link unavailable in fresh clone: {target}{hint}')
                    continue
                if parsed.fragment and candidate.suffix == '.md':
                    anchor = urllib.parse.unquote(parsed.fragment)
                    if anchor not in heading_anchors(candidate.read_text(encoding='utf-8')):
                        errors.append(f'{path}:{number}: missing heading anchor: {target}')
    return errors


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, default=Path(__file__).resolve().parent.parent)
    args = parser.parse_args()
    try:
        errors = validate(args.root, visible_paths(args.root))
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f'Documentation check failed: {error}')
        return 1
    for error in errors:
        print(error)
    print(f'Documentation policy: {len(errors)} issue(s).')
    return int(bool(errors))


if __name__ == '__main__':
    raise SystemExit(main())
