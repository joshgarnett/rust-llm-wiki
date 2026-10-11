// Runner-only actual Obsidian Editor saves. No filesystem write substitutes for edits.
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import {spawn} from 'node:child_process';

const [binary, root, binaryHash] = process.argv.slice(2);
if (process.platform !== 'darwin' || process.env.GITHUB_ACTIONS !== 'true' || process.env.RUNNER_ENVIRONMENT !== 'github-hosted') {
  throw new Error('requires a fresh GitHub-hosted macOS VM');
}
const vault = path.join(root, 'vault'), profile = path.join(root, 'profile');
const result = {status: 'running', checks: [], operations: [], editorApi: 'Editor.replaceRange', nativeFolderOpen: 'pinned vault-open IPC', navigation: 'Workspace.openLinkText', move: 'FileManager.renameFile'};
const immutableFiles = new Map();
const monotonic = () => Number(process.hrtime.bigint()) / 1e9;
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
const hash = bytes => crypto.createHash('sha256').update(bytes).digest('hex');
const save = (name, value) => fs.writeFileSync(path.join(root, name), JSON.stringify(value, null, 2) + '\n');
const check = (name, passed, details = {}) => {
  result.checks.push({name, passed: Boolean(passed), required: true, ...details});
  save('result.json', result);
};
const must = (passed, message) => { if (!passed) throw new Error(message); };
const file = relative => {
  const resolved = path.resolve(vault, relative);
  must(resolved.startsWith(vault + path.sep), 'returned path escapes fixture');
  return resolved;
};
let child, appStarted, deadlineTimer, session;
const requests = new Map();
let nextId = 1, receive = Buffer.alloc(0);
const appEnv = {PATH: '/usr/bin:/bin', HOME: path.join(root, 'home'), TMPDIR: path.join(root, 'temp') + '/', XDG_CONFIG_HOME: profile, XDG_CACHE_HOME: path.join(root, 'cache'), LANG: 'en_US.UTF-8', TZ: 'UTC'};
const config = path.join(root, 'preferences.toml');
fs.writeFileSync(config, 'schema_version = "3"\n', {mode: 0o600});

function snapshotEditor() {
  const out = {};
  function walk(dir) {
    if (!fs.existsSync(dir)) return;
    for (const row of fs.readdirSync(dir, {withFileTypes: true})) {
      const p = path.join(dir, row.name);
      if (row.isDirectory()) walk(p);
      else if (row.isFile()) out[path.relative(vault, p)] = hash(fs.readFileSync(p));
      else throw new Error('fixture editor settings contain a symlink');
    }
  }
  walk(path.join(vault, '.obsidian'));
  return out;
}
async function cli(name, args, expectedOk = true) {
  must(!appStarted || monotonic() - appStarted < 270, 'app working deadline reached');
  // Let app-owned workspace autosaves settle before attributing a CLI interval.
  if (child) await sleep(1200);
  const before = snapshotEditor(), start = monotonic();
  const argv = ['--wiki', vault, '--config', config, '--offline', '--json', ...args];
  const p = spawn(binary, argv, {cwd: root, env: appEnv, stdio: ['ignore', 'pipe', 'pipe']});
  let stdout = '', stderr = '';
  p.stdout.on('data', d => stdout += d); p.stderr.on('data', d => stderr += d);
  const timer = setTimeout(() => p.kill('SIGKILL'), 10000);
  const code = await new Promise((resolve, reject) => {p.on('close', resolve); p.on('error', reject);});
  clearTimeout(timer);
  fs.writeFileSync(path.join(root, name + '.stdout'), stdout);
  fs.writeFileSync(path.join(root, name + '.stderr'), stderr);
  const after = snapshotEditor();
  result.operations.push({name, argv, code, elapsedSeconds: monotonic() - start, editorSettingsBefore: before, editorSettingsAfter: after});
  check(name + ': CLI preserves editor settings', JSON.stringify(before) === JSON.stringify(after));
  const envelope = JSON.parse(stdout);
  must(envelope.ok === expectedOk && (code === 0) === expectedOk, name + ': unexpected command outcome');
  must(envelope.meta.network_used === false, name + ': offline command used network');
  return envelope;
}
function cdp(method, params = {}, sid, timeout = 10000) {
  must(!appStarted || monotonic() - appStarted < 280, 'app CDP deadline reached');
  const id = nextId++;
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {requests.delete(id); reject(new Error(method + ' timed out'));}, timeout);
    requests.set(id, {resolve, reject, timer});
    child.stdio[3].write(JSON.stringify({id, method, params, ...(sid ? {sessionId: sid} : {})}) + '\0');
  });
}
async function evaluate(expression, sid = session) {
  const reply = await cdp('Runtime.evaluate', {expression, awaitPromise: true, returnByValue: true}, sid);
  fs.appendFileSync(path.join(root, 'renderer-observations.jsonl'), JSON.stringify({expression, sessionId: sid, value: reply.result?.value, exceptionDetails: reply.exceptionDetails}) + '\n');
  must(!reply.exceptionDetails, 'renderer exception: ' + JSON.stringify(reply.exceptionDetails));
  return reply.result.value;
}
async function renderer(requireApp) {
  const until = monotonic() + 45;
  while (monotonic() < until) {
    const targets = (await cdp('Target.getTargets')).targetInfos.filter(t => t.type === 'page' && t.url.startsWith('app://obsidian.md/'));
    for (const target of targets) {
      const sid = (await cdp('Target.attachToTarget', {targetId: target.targetId, flatten: true})).sessionId;
      await cdp('Runtime.enable', {}, sid);
      const state = await evaluate(`(() => {const ipc=require('electron').ipcRenderer; const remote=require('@electron/remote'); return {userData:remote.app.getPath('userData'),version:remote.app.getVersion(),vaults:ipc.sendSync('vault-list'),basePath:typeof app!=='undefined'&&app.vault?app.vault.adapter.getBasePath():null,ready:typeof app!=='undefined'&&app.workspace?app.workspace.layoutReady:false};})()`, sid);
      must(state.userData === profile && state.version === '1.10.6', 'unexpected runtime profile/version');
      if (!requireApp || state.basePath === vault && state.ready) {session = sid; save(requireApp ? 'editor-runtime.json' : 'manager-runtime.json', {target, ...state}); return state;}
      await cdp('Target.detachFromTarget', {sessionId: sid});
    }
    await sleep(250);
  }
  throw new Error('owned fixture renderer did not become ready');
}
async function open(relative) {
  const state = await evaluate(`(async()=>{const f=app.vault.getAbstractFileByPath(${JSON.stringify(relative)});if(!f)throw Error('fixture file unavailable');const leaf=app.workspace.getLeaf(false);await leaf.openFile(f,{active:true});await leaf.setViewState({type:'markdown',state:{file:f.path,mode:'source',source:true},active:true});app.workspace.setActiveLeaf(leaf,{focus:true});return {path:leaf.view.file.path,mode:leaf.view.getMode(),text:leaf.view.editor.getValue()};})()`);
  must(state.path === relative && state.mode === 'source', 'actual Source editor was not opened');
  return state;
}
async function screenshot(name) {
  const shot = await cdp('Page.captureScreenshot', {format: 'png'}, session);
  fs.writeFileSync(path.join(root, name + '.png'), Buffer.from(shot.data, 'base64'));
}
async function replace(relative, pairs) {
  const beforeDisk = fs.readFileSync(file(relative));
  await open(relative);
  const edited = await evaluate(`(() => {const view=app.workspace.activeLeaf.view;if(view.file.path!==${JSON.stringify(relative)}||view.getMode()!=='source')throw Error('wrong editor');const e=view.editor;const before=e.getValue();for(const [from,to] of ${JSON.stringify(pairs)}){const text=e.getValue(),offset=text.indexOf(from);if(offset<0||text.indexOf(from,offset+from.length)>=0)throw Error('replacement must be unique');e.replaceRange(to,e.offsetToPos(offset),e.offsetToPos(offset+from.length),'input');}return {before,after:e.getValue(),path:view.file.path};})()`);
  save('editor-edit-' + result.operations.length + '.json', edited);
  const until = Math.min(monotonic() + 15, appStarted + 275);
  let last, stable = 0, actual;
  while (monotonic() < until) {
    actual = fs.readFileSync(file(relative));
    const text = actual.toString('utf8');
    const done = !actual.equals(beforeDisk) && pairs.every(([, to]) => text.includes(to));
    stable = done && last && actual.equals(last) ? stable + 1 : 0;
    if (stable >= 2) break;
    last = actual;
    await sleep(200);
  }
  must(stable >= 2, 'real editor did not save stable edited bytes');
  await open('WIKI.md');
  const reopened = await open(relative);
  check(relative + ': real saved content survives reopen', pairs.every(([, to]) => reopened.text.includes(to)));
  return actual;
}
function captured(add, bytes, name) {
  const paths = add.data.plan.operations.map(x => x.path);
  const content = paths.find(x => x.endsWith('/content.md'));
  const original = paths.find(x => x.endsWith('/original.bin'));
  must(content && original, 'capture did not report immutable paths');
  check(name + ': exact immutable captured bytes', fs.readFileSync(file(content)).equals(bytes) && fs.readFileSync(file(original)).equals(bytes), {content, original});
  for (const p of paths.filter(p => p.includes('/revisions/'))) immutableFiles.set(p, hash(fs.readFileSync(file(p))));
  return {sourcePath: add.data.capture_paths.managed_source, sourceId: add.data.allocated_ids.source, content, original};
}
async function passage(name, sourceId, fact) {
  const env = await cli(name, ['context', 'launch window', '--source-id', sourceId, '--max-bytes', '6000', '--max-tokens', '1500']);
  const found = env.data.passages.find(p => p.text.includes(fact));
  must(found, name + ': fact absent from returned passages');
  const bytes = fs.readFileSync(file(found.locator.path)).subarray(found.span.start, found.span.end);
  check(name + ': physical range equals quote', bytes.toString('utf8') === found.text);
  const read = await cli(name + '-read', ['read', '--path', found.locator.path, '--start', String(found.span.start), '--end', String(found.span.end), '--max-bytes', '6000']);
  check(name + ': read authenticates same current citation', read.data.body === found.text && read.data.source_citation.eligibility === 'current' && JSON.stringify(read.data.source_citation.citation) === JSON.stringify(found.citations[0]));
  return found;
}
async function workflow() {
  fs.mkdirSync(vault); fs.mkdirSync(path.join(vault, 'notes')); fs.mkdirSync(path.join(vault, '.obsidian'));
  fs.writeFileSync(path.join(vault, '.obsidian', 'app.json'), '{"alwaysUpdateLinks":true}\n');
  const plain = Buffer.from('# Plain note\n\nThe plain launch window begins at 08:15 UTC.\n');
  const body = '---\n# Preserve this author comment\ncustom_owner: Zoë\ncustom_state: pending\nnested:\n  team: [Ada, Lin]\nunknown_scalar: retain-me\n---\n# Release runbook\n\nThe launch window begins at 09:20 UTC.\nUnicode: naïve café 東京.\n';
  const simpleBody = '---\ncustom_owner: Zoë\ncustom_state: pending\nunknown_scalar: retain-me\n---\n# Release runbook\n\nThe launch window begins at 09:20 UTC.\nUnicode: naïve café 東京.\n';
  const cases = [{path: 'notes/properties.md', bytes: Buffer.from('\ufeff' + body.replaceAll('\n', '\r\n'))}, {path: 'notes/simple.md', bytes: Buffer.from(simpleBody)}];
  fs.writeFileSync(file('notes/plain.md'), plain);
  for (const item of cases) {fs.writeFileSync(file(item.path), item.bytes); fs.writeFileSync(path.join(root, path.basename(item.path) + '.original.bin'), item.bytes);}
  fs.writeFileSync(file('notes/literal.md'), cases[0].bytes);
  await cli('init-preview', ['init', vault, '--existing', '--dry-run']);
  must(!fs.existsSync(file('WIKI.md')), 'init preview wrote wiki');
  await cli('init-existing', ['init', vault, '--existing']);
  const plainInspect = await cli('plain-inspect', ['inspect', '--path', 'notes/plain.md']);
  check('frontmatter-free note inspect returns original bytes', Buffer.from(plainInspect.data.bytes).equals(plain));
  await cli('plain-read', ['read', '--path', 'notes/plain.md', '--max-bytes', '6000']);
  const literal = await cli('literal-inspect', ['inspect', '--path', 'notes/literal.md']);
  const offset = cases[0].bytes.indexOf(Buffer.from('09:20'));
  save('literal-patch.json', {version: 'lwiki.literal-edit.v1', edits: [{range: {start: offset, end: offset + 5}, replacement_utf8: '09:35'}]});
  const literalArgs = ['edit', '--path', 'notes/literal.md', '--patch', path.join(root, 'literal-patch.json'), '--if-match', literal.data.hash];
  await cli('literal-preview', [...literalArgs, '--dry-run']);
  check('guarded CLI edit preview preserves bytes', fs.readFileSync(file('notes/literal.md')).equals(cases[0].bytes));
  await cli('literal-apply', literalArgs);
  const literalExpected = Buffer.from(cases[0].bytes.toString('utf8').replace('09:20', '09:35'));
  check('guarded CLI edit preserves exact BOM/CRLF/unknown bytes', fs.readFileSync(file('notes/literal.md')).equals(literalExpected));
  const literalStale = await cli('literal-stale', literalArgs, false);
  check('guarded CLI edit rejects stale hash without changing bytes', literalStale.error.code === 'CONTENT_CONFLICT' && fs.readFileSync(file('notes/literal.md')).equals(literalExpected));
  const plainCapture = captured(await cli('capture-plain', ['source', 'add', file('notes/plain.md'), '--title', 'Plain note']), plain, 'plain');
  for (const item of cases) {
    const inspected = await cli('inspect-original-' + path.basename(item.path), ['inspect', '--path', item.path]);
    await cli('read-original-' + path.basename(item.path), ['read', '--path', item.path, '--max-bytes', '6000']);
    check(item.path + ': inspect physical original', Buffer.from(inspected.data.bytes).equals(item.bytes));
    item.capture = captured(await cli('capture-' + path.basename(item.path), ['source', 'add', file(item.path), '--title', path.basename(item.path)]), item.bytes, item.path);
    item.old = await passage('old-context-' + path.basename(item.path), item.capture.sourceId, '09:20');
  }
  const draftBody = path.join(root, 'page-body.md'); fs.writeFileSync(draftBody, '# Summary heading\n\nDraft editor content.\n');
  save('page-source-refs.json', {schema_version: '1', citations: cases[0].old.citations});
  const page = await cli('page-init', ['page', 'init', '--file', draftBody, '--title', 'Editor summary', '--path', 'pages/editor-summary.md', '--source-refs', path.join(root, 'page-source-refs.json')]);
  const pageId = page.data.allocated_ids['pages/editor-summary.md'];
  const pageOriginal = fs.readFileSync(file('pages/editor-summary.md'));
  const wikiFields = pageOriginal.toString('utf8').split('\n').filter(l => l.startsWith('wiki_'));
  await cli('initial-rebuild', ['index', 'rebuild', '--normalized']);
  for (const item of cases) check(item.path + ': CLI leaves original and properties unchanged', fs.readFileSync(file(item.path)).equals(item.bytes));
  const originalSearch = await cli('ordinary-input-search', ['search', 'launch window', '--limit', '20']);
  check('ordinary input notes excluded from structured search', !originalSearch.data.hits.some(hit => ['notes/plain.md', 'notes/properties.md', 'notes/simple.md', 'notes/literal.md'].includes(hit.path)));
  const log = fs.openSync(path.join(root, 'app.log'), 'w');
  const argv = ['-f', path.join(root, 'network-only.sb'), path.join(root, 'Obsidian.app', 'Contents', 'MacOS', 'Obsidian'), '--user-data-dir=' + profile, '--disk-cache-dir=' + path.join(root, 'cache'), '--log-file=' + path.join(root, 'chromium.log'), '--remote-debugging-pipe', '--disable-background-networking', '--disable-component-update', '--disable-geolocation'];
  appStarted = monotonic();
  child = spawn('/usr/bin/sandbox-exec', argv, {cwd: root, env: appEnv, detached: true, stdio: ['ignore', log, log, 'pipe', 'pipe']});
  fs.closeSync(log); save('app-pid.json', {pid: child.pid, argv, env: appEnv});
  child.on('error', error => {result.spawnError = String(error);});
  deadlineTimer = setTimeout(() => {result.deadlineExceeded = true; try {process.kill(-child.pid, 'SIGKILL');} catch {} for (const request of requests.values()) {clearTimeout(request.timer); request.reject(new Error('app hard deadline'));} requests.clear();}, 285000);
  child.stdio[4].on('data', data => {
    receive = Buffer.concat([receive, data]);
    for (let at; (at = receive.indexOf(0)) >= 0;) {
      const row = JSON.parse(receive.subarray(0, at)); receive = receive.subarray(at + 1);
      const request = requests.get(row.id);
      if (request) {clearTimeout(request.timer); requests.delete(row.id); row.error ? request.reject(new Error(JSON.stringify(row.error))) : request.resolve(row.result);}
    }
  });
  const manager = await renderer(false);
  must(Object.keys(manager.vaults).length === 0, 'new profile unexpectedly contains a vault');
  must(await evaluate(`require('electron').ipcRenderer.sendSync('disable-update')`) === true, 'app updates are not disabled');
  must(await evaluate(`require('electron').ipcRenderer.sendSync('vault-open',${JSON.stringify(vault)},false)`) === true, 'app refused fixture vault');
  await renderer(true);
  await open('WIKI.md'); await screenshot('landing');
  await replace('WIKI.md', [['Browse [Pages](pages/) and [Sources](sources/).', '[Editor summary](pages/editor-summary.md)\n\nBrowse your Pages and Sources.']]);
  await evaluate("app.workspace.openLinkText('pages/editor-summary.md','WIKI.md',false)");
  check('landing Page link opens named draft', await evaluate('app.workspace.getActiveFile().path') === 'pages/editor-summary.md');
  await screenshot('landing-page-link');
  const generatedSourceLink = pageOriginal.toString('utf8').match(/\[Source\]\(([^)]+)\)/)?.[1];
  must(generatedSourceLink, 'generated Page Source link missing');
  await evaluate(`app.workspace.openLinkText(${JSON.stringify(generatedSourceLink)},'pages/editor-summary.md',false)`);
  check('generated Page Source link opens captured Source', await evaluate('app.workspace.getActiveFile().path') === cases[0].capture.sourcePath);
  const sourceView = await open(cases[0].capture.sourcePath);
  check('captured Source navigation shows its actual metadata', sourceView.text.includes(cases[0].capture.sourceId));
  const source = fs.readFileSync(file(cases[0].capture.sourcePath), 'utf8');
  const revisionLink = source.match(/^wiki_revision: "\[\[(.+)\]\]"$/m)?.[1];
  must(revisionLink, 'generated revision navigation is missing');
  await evaluate(`app.workspace.openLinkText(${JSON.stringify(revisionLink)},${JSON.stringify(cases[0].capture.sourcePath)},false)`);
  check('generated Source revision link opens correct file', await evaluate('app.workspace.getActiveFile().path') === revisionLink);
  check('generated revision navigation shows its actual metadata', await evaluate(`app.workspace.activeLeaf.view.editor.getValue().includes(${JSON.stringify(cases[0].old.citations[0].reference.source_revision)})`));
  await screenshot('generated-revision-link');
  for (const item of cases) {
    await open(item.path); await screenshot('before-' + path.basename(item.path));
    const pairs = [['09:20', '10:45'], ['# Release runbook', '# Release checklist'], ['custom_state: pending', 'custom_state: ready']];
    const actual = await replace(item.path, pairs);
    await screenshot('after-' + path.basename(item.path));
    fs.writeFileSync(path.join(root, path.basename(item.path) + '.editor-saved.bin'), actual);
    let expected = item.bytes.toString('utf8'); for (const [from, to] of pairs) expected = expected.replace(from, to);
    check(item.path + ': exact editor BOM/CRLF bytes preserved', actual.equals(Buffer.from(expected)), {required: false, beforeSha256: hash(item.bytes), actualSha256: hash(actual), expectedSha256: hash(Buffer.from(expected)), normalizedBom: item.bytes.subarray(0, 3).equals(Buffer.from([239,187,191])) && !actual.subarray(0,3).equals(Buffer.from([239,187,191])), normalizedCrlf: item.bytes.includes(Buffer.from('\r\n')) && !actual.includes(Buffer.from('\r\n'))});
    const normalize = text => text.replace(/^\ufeff/, '').replaceAll('\r\n', '\n');
    check(item.path + ': all author content survives editor save', normalize(actual.toString('utf8')) === normalize(expected));
    await cli('sync-only-' + path.basename(item.path), ['index', 'sync']);
    check(item.path + ': sync does not recapture input', fs.readFileSync(file(item.capture.content)).equals(item.bytes));
    await passage('sync-old-context-' + path.basename(item.path), item.capture.sourceId, '09:20');
    const inspected = await cli('inspect-saved-' + path.basename(item.path), ['inspect', '--path', item.path]);
    const refresh = await cli('refresh-' + path.basename(item.path), ['source', 'refresh', '--path', item.capture.sourcePath, '--file', file(item.path), '--input-if-match', inspected.data.hash]);
    captured(refresh, actual, 'refreshed ' + item.path);
    await cli('rebuild-' + path.basename(item.path), ['index', 'rebuild', '--normalized']);
    const current = await passage('new-context-' + path.basename(item.path), item.capture.sourceId, '10:45');
    check(item.path + ': new revision citation differs', current.citations[0].reference.source_revision !== item.old.citations[0].reference.source_revision);
    const oldRead = await cli('historical-read-' + path.basename(item.path), ['read', '--path', item.old.locator.path, '--start', String(item.old.span.start), '--end', String(item.old.span.end), '--max-bytes', '6000']);
    check(item.path + ': old exact citation remains historical', oldRead.data.body === item.old.text && oldRead.data.source_citation.eligibility === 'historical' && JSON.stringify(oldRead.data.source_citation.citation) === JSON.stringify(item.old.citations[0]));
    check(item.path + ': entire old captured revision retained exactly', fs.readFileSync(file(item.capture.content)).equals(item.bytes) && fs.readFileSync(file(item.capture.original)).equals(item.bytes));
    const secondSave = await replace(item.path, [['10:45', '11:30']]);
    const revisions = path.dirname(path.dirname(file(current.locator.path)));
    const revisionTree = fs.readdirSync(revisions).sort();
    const sourceBefore = fs.readFileSync(file(item.capture.sourcePath));
    const stale = await cli('stale-refresh-' + path.basename(item.path), ['source', 'refresh', '--path', item.capture.sourcePath, '--file', file(item.path), '--input-if-match', inspected.data.hash], false);
    check(item.path + ': stale refresh fails without new revision or lost bytes', stale.error.code === 'CONTENT_CONFLICT' && JSON.stringify(revisionTree) === JSON.stringify(fs.readdirSync(revisions).sort()) && fs.readFileSync(file(item.capture.sourcePath)).equals(sourceBefore) && fs.readFileSync(file(item.path)).equals(secondSave));
  }
  await replace('pages/editor-summary.md', [['title: "Editor summary"', 'title: "Edited summary"\ncustom_owner: Zoë'], ['# Summary heading', '# Edited summary heading'], ['Draft editor content.', 'Edited draft editor content.']]);
  await replace('pages/editor-summary.md', [['custom_owner: Zoë', 'custom_owner: Zoë reviewed']]);
  check('Page edit preserves every wiki field', wikiFields.every(line => fs.readFileSync(file('pages/editor-summary.md'), 'utf8').split('\n').includes(line)));
  await evaluate(`app.fileManager.renameFile(app.vault.getAbstractFileByPath('pages/editor-summary.md'),'pages/editor-summary-moved.md')`);
  await open('pages/editor-summary-moved.md'); await screenshot('moved-draft-page');
  await cli('page-move-sync', ['index', 'sync']); await cli('page-move-rebuild', ['index', 'rebuild', '--normalized']);
  const pageRead = await cli('page-move-read', ['read', '--path', 'pages/editor-summary-moved.md']);
  check('moved draft Page retains identity and body', pageRead.data.record?.wiki_id === pageId && pageRead.data.metadata?.wiki_status === 'draft' && pageRead.data.body.includes('Edited draft editor content.'));
  check('moved Page retains title, heading, unknown property and every wiki field', pageRead.data.metadata?.title === 'Edited summary' && pageRead.data.metadata?.custom_owner === 'Zoë reviewed' && pageRead.data.body.includes('# Edited summary heading') && wikiFields.every(line => fs.readFileSync(file('pages/editor-summary-moved.md'), 'utf8').split('\n').includes(line)));
  const pageSearch = await cli('moved-draft-search', ['search', 'Edited draft editor content', '--kind', 'page', '--status', 'draft']);
  check('moved edited draft is searchable with correct identity and title', pageSearch.data.hits.some(hit => hit.path === 'pages/editor-summary-moved.md' && hit.record_ref?.record_id === pageId && hit.title === 'Edited summary' && hit.authored_status === 'draft'));
  await open('WIKI.md');
  const movedLanding = fs.readFileSync(file('WIKI.md'), 'utf8');
  const movedLink = movedLanding.match(/\[Editor summary\]\(([^)]+)\)/)?.[1] ?? movedLanding.match(/\[\[([^|\]]+)\|Editor summary\]\]/)?.[1];
  must(movedLink, 'moved Page link absent from landing');
  await evaluate(`app.workspace.openLinkText(${JSON.stringify(movedLink)},'WIKI.md',false)`);
  check('landing navigation follows moved Page', await evaluate('app.workspace.getActiveFile().path') === 'pages/editor-summary-moved.md');
  await screenshot('moved-page-link');
  const draftContext = await cli('draft-default-context', ['context', 'Edited draft editor content', '--max-bytes', '6000', '--max-tokens', '1500']);
  check('draft Page excluded from default current context', !draftContext.data.passages.some(p => p.locator.path === 'pages/editor-summary-moved.md'));
  check('ordinary plain input stays unchanged', fs.readFileSync(file('notes/plain.md')).equals(plain));
  check('plain captured revision stays unchanged', fs.readFileSync(file(plainCapture.content)).equals(plain));
  check('all managed immutable revisions remain unchanged by editor', [...immutableFiles].every(([p, digest]) => hash(fs.readFileSync(file(p))) === digest));
  save('immutable-fixture-hashes.json', Object.fromEntries(immutableFiles));
  must(hash(fs.readFileSync(binary)) === binaryHash, 'pinned binary changed during qualification');
}

try {
  await workflow();
  result.status = result.checks.every(row => !row.required || row.passed) && !result.deadlineExceeded ? 'passed' : 'failed';
} catch (error) {
  result.status = 'failed'; result.error = String(error); result.stack = error.stack;
} finally {
  if (child) {
    try {await cdp('Runtime.evaluate', {expression: "require('@electron/remote').app.quit()"}, session, 1000);} catch {}
    await sleep(1000);
    const alive = () => {try {process.kill(-child.pid, 0); return true;} catch {return false;}};
    if (alive()) {try {process.kill(-child.pid, 'SIGTERM');} catch {} await sleep(1000);}
    if (alive()) {try {process.kill(-child.pid, 'SIGKILL');} catch {} await sleep(500);}
    result.ownedProcessGroupGone = !alive();
    result.appElapsedSeconds = monotonic() - appStarted;
    if (!result.ownedProcessGroupGone || result.appElapsedSeconds > 300) result.status = 'failed';
  }
  clearTimeout(deadlineTimer);
  for (const request of requests.values()) {clearTimeout(request.timer); request.reject(new Error('controller cleanup'));}
  save('result.json', result);
}
console.log(JSON.stringify({status: result.status, checks: result.checks.length}));
process.exit(result.status === 'passed' ? 0 : 1);
