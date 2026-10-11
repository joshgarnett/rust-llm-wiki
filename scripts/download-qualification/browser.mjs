// Fresh hosted Mac only. Use the shipped browser download manager and protections.
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import {spawn} from 'node:child_process';

if (process.platform !== 'darwin' || process.getuid() === 0 || process.env.GITHUB_ACTIONS !== 'true' || process.env.RUNNER_ENVIRONMENT !== 'github-hosted') throw Error('requires a fresh nonroot GitHub-hosted Mac');
const [root, url, expectedHash, browserHome] = process.argv.slice(2);
if (!/^https:\/\/raw\.githubusercontent\.com\/joshgarnett\/rust-llm-wiki\/[0-9a-f]{40}\/scripts\/install\.sh$/.test(url) || !/^[0-9a-f]{64}$/.test(expectedHash)) throw Error('requires pinned public installer URL and hash');
if (!browserHome || !path.isAbsolute(browserHome) || !fs.statSync(browserHome).isDirectory() || fs.statSync(browserHome).uid !== process.getuid()) throw Error('browser requires the verified owned normal runner HOME');
const commit = url.split('/')[5];
const pageURL = `https://github.com/joshgarnett/rust-llm-wiki/blob/${commit}/scripts/install.sh`;
const downloadURL = `https://github.com/joshgarnett/rust-llm-wiki/raw/${commit}/scripts/install.sh`;
const chrome = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const profile = path.join(root, 'browser-profile'), downloads = path.join(root, 'downloads');
fs.mkdirSync(path.join(profile, 'Default'), {recursive: true, mode: 0o700});
fs.mkdirSync(downloads, {mode: 0o700});
// Only the destination differs from a new profile. Do not set Safe Browsing,
// quarantine, download restriction, automatic-download or security preferences.
const preferences = {download: {default_directory: downloads}};
fs.writeFileSync(path.join(profile, 'Default', 'Preferences'), JSON.stringify(preferences));
const hash = bytes => crypto.createHash('sha256').update(bytes).digest('hex');
const save = (name, value) => fs.writeFileSync(path.join(root, name), JSON.stringify(value, null, 2) + '\n');
const result = {status: 'running', url, pageURL, downloadURL, expectedHash, browserHOME: browserHome, controllerHOME: process.env.HOME, browserExecutable: chrome, browserExecutableSHA256: hash(fs.readFileSync(chrome)), seededPreferences: preferences, downloadManager: 'normal Chrome delegate; no CDP download override', approvals: 'none attempted', headed: true};
const args = [`--user-data-dir=${profile}`, '--remote-debugging-pipe', '--no-first-run', '--no-default-browser-check', 'about:blank'];
result.argv = args;
save('browser-result.json', result);
const browserEnv = Object.fromEntries(['PATH', 'TMPDIR', 'LANG', 'TZ', 'XDG_CONFIG_HOME', 'XDG_CACHE_HOME', 'GITHUB_ACTIONS', 'RUNNER_ENVIRONMENT'].filter(key => process.env[key] !== undefined).map(key => [key, process.env[key]]));
browserEnv.HOME = browserHome;
const child = spawn(chrome, args, {detached: true, env: browserEnv, stdio: ['ignore', 'pipe', 'pipe', 'pipe', 'pipe']});
save('browser-pid.json', {pid: child.pid});
child.stdout.pipe(fs.createWriteStream(path.join(root, 'browser.stdout')));
child.stderr.pipe(fs.createWriteStream(path.join(root, 'browser.stderr')));
const begin = process.hrtime.bigint();
const elapsed = () => Number(process.hrtime.bigint() - begin) / 1e9;
let next = 1, pending = new Map(), commands = new Map(), receive = Buffer.alloc(0), session, spawnError, mainNavigation, explicitRefusal, downloadUIAttempted = false, desktopFailureAttempted = false, closing = false, childExit, childClose;
const trace = (name, value) => fs.appendFileSync(path.join(root, name + '.jsonl'), JSON.stringify({elapsedSeconds: elapsed(), ...value}) + '\n');
const phase = name => {result.phase = name; save('browser-result.json', result);};
function fail(error, failurePhase = result.phase) {
  result.status = 'failed';
  if (!result.error) {result.error = String(error); result.failurePhase = failurePhase;}
  else if (result.secondaryErrors?.length < 32 || !result.secondaryErrors) (result.secondaryErrors ||= []).push({error: String(error), phase: failurePhase, elapsedSeconds: elapsed()});
}
function rejectPending(error) {
  for (const item of pending.values()) {clearTimeout(item.timer); item.reject(error);}
  pending.clear();
}
function pipeEvent(pipe, event, error) {
  trace('browser-lifecycle', {phase: result.phase, pipe, event, ...(error ? {error: String(error), code: error.code} : {})});
  const expected = closing && (!error || ['EPIPE', 'ERR_STREAM_DESTROYED'].includes(error.code));
  if (!expected) fail(error || Error(pipe + ' ' + event + ' before Browser.close'));
  spawnError ||= String(error || pipe + ' ' + event);
  const failure = Error(spawnError);
  if (expected) failure.code = 'EXPECTED_BROWSER_CLOSED';
  rejectPending(failure);
}
child.on('error', error => {spawnError = String(error); trace('browser-lifecycle', {phase: result.phase, event: 'child-error', error: spawnError}); fail(error); rejectPending(error);});
child.on('exit', (code, signal) => {
  childExit = {code, signal, elapsedSeconds: elapsed(), phase: result.phase};
  result.childExit = childExit;
  trace('browser-lifecycle', {event: 'child-exit', ...childExit});
  if (!closing || code !== 0 || signal !== null) fail(Error('browser child exited unexpectedly: ' + JSON.stringify({code, signal})));
});
child.on('close', (code, signal) => {childClose = {code, signal, elapsedSeconds: elapsed(), phase: result.phase}; result.childClose = childClose; trace('browser-lifecycle', {event: 'child-close', ...childClose});});
function cdp(method, params = {}, sid, timeout = 10000) {
  if (closing && method !== 'Browser.close') return Promise.reject(Error('browser work after close request is forbidden'));
  timeout = Math.max(1, Math.min(timeout, (105 - elapsed()) * 1000));
  const id = next++;
  commands.set(id, {method, sessionId: sid});
  trace('browser-cdp', {event: 'command', id, method, sessionId: sid, ...(params.url ? {url: params.url} : {})});
  return new Promise((resolve, reject) => {
    if (spawnError) return reject(Error(spawnError));
    const timer = setTimeout(() => {pending.delete(id); trace('browser-cdp', {event: 'timeout', id, method}); reject(Error(method + ' timed out'));}, timeout);
    pending.set(id, {resolve, reject, timer});
    child.stdio[3].write(JSON.stringify({id, method, params, ...(sid ? {sessionId: sid} : {})}) + '\0');
  });
}
for (const [number, name] of [[3, 'cdp-input'], [4, 'cdp-output']]) {
  for (const event of ['error', 'end', 'close']) child.stdio[number].on(event, error => pipeEvent(name, event, event === 'error' ? error : undefined));
}
child.stdio[4].on('data', bytes => {
  receive = Buffer.concat([receive, bytes]);
  while (receive.includes(0)) {
    const end = receive.indexOf(0), raw = receive.subarray(0, end).toString();
    receive = receive.subarray(end + 1);
    if (!raw) continue;
    let message;
    try {message = JSON.parse(raw);} catch (error) {fail(error); rejectPending(error); continue;}
    if (message.id) {
      const command = commands.get(message.id);
      trace('browser-cdp', {event: pending.has(message.id) ? 'response' : 'late-response', id: message.id, ...command, error: message.error, resultKeys: Object.keys(message.result || {}), ...(command?.method === 'Page.navigate' ? {navigation: message.result} : {})});
    }
    if (message.id && pending.has(message.id)) {
      const item = pending.get(message.id); pending.delete(message.id); clearTimeout(item.timer);
      if (message.error) item.reject(Error(JSON.stringify(message.error))); else item.resolve(message.result);
    } else if (message.sessionId === session && message.method) {
      const p = message.params, method = message.method;
      if (method === 'Network.requestWillBeSent') {
        trace('browser-network', {method, requestId: p.requestId, loaderId: p.loaderId, frameId: p.frameId, type: p.type, url: p.request.url, requestMethod: p.request.method, timestamp: p.timestamp, redirectURL: p.redirectResponse?.url, redirectStatus: p.redirectResponse?.status});
        if (mainNavigation && p.frameId === mainNavigation.frameId && p.type === 'Document' && (p.request.url === pageURL || p.requestId === mainNavigation.requestId)) Object.assign(mainNavigation, {requestId: p.requestId, loaderId: p.loaderId, requestObserved: true, currentURL: p.request.url});
      } else if (method === 'Network.responseReceived') {
        const r = p.response;
        trace('browser-network', {method, requestId: p.requestId, loaderId: p.loaderId, frameId: p.frameId, type: p.type, url: r.url, status: r.status, mimeType: r.mimeType, protocol: r.protocol, securityState: r.securityState, timestamp: p.timestamp});
        if (mainNavigation?.requestId === p.requestId) {
          Object.assign(mainNavigation, {responseObserved: true, responseStatus: r.status});
          if (r.status >= 400) explicitRefusal ||= 'main document HTTP refusal: ' + r.status;
        }
      } else if (method === 'Network.loadingFailed' || method === 'Network.loadingFinished') {
        trace('browser-network', {method, requestId: p.requestId, type: p.type, timestamp: p.timestamp, errorText: p.errorText, canceled: p.canceled, blockedReason: p.blockedReason, corsErrorStatus: p.corsErrorStatus});
        if (mainNavigation?.requestId === p.requestId) {
          mainNavigation[method === 'Network.loadingFailed' ? 'failed' : 'finished'] = true;
          if (method === 'Network.loadingFailed') explicitRefusal ||= 'main document loading failed: ' + p.errorText;
        }
      } else if (['Page.frameStartedNavigating', 'Page.frameStartedLoading', 'Page.frameStoppedLoading', 'Page.frameNavigated', 'Page.lifecycleEvent', 'Page.interstitialShown', 'Page.javascriptDialogOpening'].includes(method)) {
        trace('browser-page', {method, ...p});
        if (method === 'Page.interstitialShown' || method === 'Page.javascriptDialogOpening') explicitRefusal ||= method + ': no approval or dialog handling attempted';
        if (mainNavigation && method === 'Page.frameStartedNavigating' && p.frameId === mainNavigation.frameId && p.url === pageURL) mainNavigation.frameProgressObserved = true;
        if (mainNavigation && method === 'Page.frameNavigated' && p.frame.id === mainNavigation.frameId && p.frame.url === pageURL) mainNavigation.commitObserved = true;
      }
    }
  }
});
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
const deadline = setTimeout(() => {
  fail(Error('110-second browser kill watchdog expired'));
  result.forcedCleanup = true;
  result.forcedCleanupReason = '110-second browser kill watchdog';
  trace('browser-lifecycle', {phase: result.phase, event: 'watchdog-kill'});
  try {process.kill(-child.pid, 'SIGKILL');} catch (error) {result.watchdogKillError = String(error);}
}, 110000);
function windowTimeout(until) {
  const remaining = until - elapsed();
  if (remaining <= 0) throw Error('download completion deadline exhausted');
  return Math.min(2000, remaining * 1000);
}
async function snapshot(name, until = 105, strict = false) {
  const operations = [
    ['frameTree', 'Page.getFrameTree', {}],
    ['document', 'Runtime.evaluate', {expression: '({url:location.href,title:document.title,ready:document.readyState,text:document.body?.innerText?.slice(0,2000)})', returnByValue: true}],
    ['screenshot', 'Page.captureScreenshot', {format: 'png'}],
  ];
  const rows = await Promise.allSettled(operations.map(async ([key, method, params]) => {
    const value = await cdp(method, params, session, windowTimeout(until));
    if (key === 'screenshot') {fs.writeFileSync(path.join(root, name + '.png'), Buffer.from(value.data, 'base64')); return {key, captured: true};}
    return {key, value};
  }));
  save(name + '.json', {mainNavigation, explicitRefusal, inspections: rows.map((row, i) => row.status === 'fulfilled' ? row.value : {key: operations[i][0], error: String(row.reason)})});
  if (strict && rows.some(row => row.status === 'rejected')) throw Error('source-page completion diagnostic failed');
  if (strict) windowTimeout(until);
}
async function desktopFailure() {
  if (desktopFailureAttempted) return;
  desktopFailureAttempted = true;
  const output = path.join(root, 'browser-failure-desktop.png');
  const p = spawn('/usr/sbin/screencapture', ['-x', output], {env: process.env, stdio: ['ignore', 'pipe', 'pipe']});
  const state = {argv: ['/usr/sbin/screencapture', '-x', output], permissions: 'no grants or prompts accepted'};
  let stderr = '';
  p.stderr.on('data', bytes => {stderr = (stderr + bytes.toString()).slice(0,4096);});
  const timer = setTimeout(() => {p.kill('SIGKILL'); state.timeout = true;}, 2000);
  await new Promise(resolve => {p.on('error', error => {state.error = String(error); resolve();}); p.on('close', code => {state.exit = code; resolve();});});
  clearTimeout(timer); state.stderr = stderr; state.captured = fs.existsSync(output);
  save('browser-failure-desktop.json', state);
}
async function navigatePublic() {
  mainNavigation = {url: pageURL, frameId: (await cdp('Page.getFrameTree', {}, session)).frameTree.frame.id};
  const navigation = cdp('Page.navigate', {url: pageURL}, session, 30000);
  let outcome;
  const observe = navigation.then(value => {outcome = {value};}, error => {outcome = {error: String(error)};});
  const until = elapsed() + 10;
  while (!outcome && !explicitRefusal && elapsed() < until) await sleep(100);
  if (!outcome && !explicitRefusal) {
    await snapshot('browser-navigation-checkpoint');
    if (explicitRefusal) throw Error(explicitRefusal);
    if (!outcome && !(mainNavigation.requestObserved || mainNavigation.frameProgressObserved || mainNavigation.commitObserved)) throw Error('navigation unresolved at10s: no matching main-document request/progress observed');
    result.navigationContinuedAfterCheckpoint = !outcome;
  }
  while (!outcome && !explicitRefusal) await sleep(100);
  if (explicitRefusal) throw Error(explicitRefusal);
  await observe;
  result.navigationOutcome = outcome;
  if (outcome.error) throw Error(outcome.error);
  if (outcome.value.errorText) throw Error('installer navigation failed: ' + outcome.value.errorText);
  return outcome.value;
}
function rawLinkObservation(expectedHref, scroll) {
  const anchors = document.querySelectorAll('a[data-testid="raw-button"]');
  const observation = {url: location.href, anchorCount: anchors.length};
  if (anchors.length !== 1) return {...observation, error: 'real Raw anchor is missing or ambiguous'};
  const anchor = anchors[0];
  observation.href = anchor.href;
  observation.label = anchor.innerText.slice(0, 256);
  observation.attributes = [...anchor.attributes].slice(0, 32).map(attribute => ({name: attribute.name.slice(0, 128), value: attribute.value.slice(0, 512)}));
  observation.attributesTruncated = anchor.attributes.length > 32;
  if (anchor.href !== expectedHref) return {...observation, error: 'real Raw anchor has another href'};
  if (scroll) anchor.scrollIntoView({block: 'center', inline: 'center', behavior: 'instant'});
  const bounds = anchor.getBoundingClientRect();
  observation.bounds = {x: bounds.x, y: bounds.y, width: bounds.width, height: bounds.height};
  const point = {x: bounds.x + bounds.width / 2, y: bounds.y + bounds.height / 2};
  observation.point = point;
  const style = getComputedStyle(anchor);
  if (![point.x, point.y, bounds.width, bounds.height].every(Number.isFinite) || bounds.width <= 0 || bounds.height <= 0 || point.x < 0 || point.x >= innerWidth || point.y < 0 || point.y >= innerHeight || style.display === 'none' || style.visibility !== 'visible') return {...observation, error: 'real Raw anchor is not visible in the viewport'};
  const hit = document.elementFromPoint(point.x, point.y);
  observation.hitTest = {sameAnchor: hit?.closest('a') === anchor, tagName: hit?.tagName, anchorHref: hit?.closest('a')?.href?.slice(0, 2048)};
  if (!observation.hitTest.sameAnchor) return {...observation, error: 'real Raw anchor center is covered'};
  return observation;
}
async function relocateRawLink(originalControl) {
  const begin = elapsed(), until = Math.min(105, begin + 2);
  const evidence = {status: 'checking', originalPoint: originalControl.point, beginElapsedSeconds: begin, limits: {maxSamples: 10, maxElapsedSeconds: 2, sampleIntervalMs: 100}, samples: []};
  result.rawLinkRelocation = evidence;
  let previous;
  try {
    for (let index = 0; index < evidence.limits.maxSamples && elapsed() < until; index++) {
      if (explicitRefusal) throw Error(explicitRefusal);
      const sample = {index, beginElapsedSeconds: elapsed()};
      evidence.samples.push(sample);
      save('browser-raw-link-relocation.json', evidence);
      let response;
      try {
        response = await cdp('Runtime.evaluate', {expression: `(${rawLinkObservation.toString()})(${JSON.stringify(downloadURL)},false)`, returnByValue: true}, session, Math.max(1, (until - elapsed()) * 1000));
      } catch (error) {
        sample.error = String(error);
        throw error;
      } finally {
        sample.completedElapsedSeconds = elapsed();
      }
      sample.observation = response.result?.value;
      if (response.exceptionDetails) throw Error('Raw link relocation failed: ' + JSON.stringify(response.exceptionDetails));
      const current = sample.observation;
      if (!current || current.error || current.url !== pageURL || current.href !== downloadURL) throw Error(current?.error || 'pinned Raw link changed during relocation');
      if (current.bounds.width !== originalControl.bounds.width || current.bounds.height !== originalControl.bounds.height) throw Error('real Raw anchor dimensions changed during relocation');
      if (explicitRefusal) throw Error(explicitRefusal);
      if (elapsed() >= until) throw Error('Raw link relocation exceeded its observation deadline');
      if (previous && ['x', 'y', 'width', 'height'].every(key => previous.bounds[key] === current.bounds[key]) && previous.point.x === current.point.x && previous.point.y === current.point.y) {
        evidence.status = 'stable';
        evidence.finalPoint = current.point;
        return current;
      }
      previous = current;
      save('browser-raw-link-relocation.json', evidence);
      if (index + 1 < evidence.limits.maxSamples) {
        if (until - elapsed() <= 0.1) break;
        await sleep(100);
      }
    }
    throw Error('Raw link did not settle within 10 samples or its observation deadline');
  } catch (error) {
    evidence.status = 'failed';
    evidence.error = String(error);
    throw error;
  } finally {
    evidence.completedElapsedSeconds = elapsed();
    evidence.elapsedSeconds = evidence.completedElapsedSeconds - begin;
    save('browser-raw-link-relocation.json', evidence);
  }
}
function downloadsVisibleText() {
  const maxNodes = 4096, maxCharacters = 12000, maxDepth = 32;
  const stack = [{node: document.body, depth: 0}], seen = new Set(), text = [];
  let characters = 0, truncated = false;
  while (stack.length && seen.size < maxNodes && characters < maxCharacters) {
    const {node, depth} = stack.pop();
    if (!node || seen.has(node)) continue;
    seen.add(node);
    if (depth > maxDepth) {truncated = true; continue;}
    if (node.nodeType === Node.TEXT_NODE) {
      const parent = node.parentElement || node.getRootNode().host;
      if (!parent || !parent.getClientRects().length) continue;
      const value = node.textContent.trim();
      if (value) {const separator = text.length ? 1 : 0; const bounded = value.slice(0, Math.max(0, maxCharacters - characters - separator)); text.push(bounded); characters += bounded.length + separator; truncated ||= bounded.length < value.length;}
      continue;
    }
    if (node.nodeType === Node.ELEMENT_NODE) {
      if (['SCRIPT', 'STYLE', 'TEMPLATE'].includes(node.tagName)) continue;
      const style = getComputedStyle(node);
      if (style.display === 'none' || style.visibility === 'hidden' || style.visibility === 'collapse') continue;
    }
    const children = node.shadowRoot ? node.shadowRoot.childNodes : node.tagName === 'SLOT' ? node.assignedNodes({flatten: true}) : node.childNodes;
    if (children.length + stack.length + seen.size > maxNodes) truncated = true;
    for (let index = Math.min(children.length, maxNodes - seen.size - stack.length) - 1; index >= 0; index--) stack.push({node: children[index], depth: depth + 1});
  }
  return {url: location.href, title: document.title, text: text.join('\n'), visitedNodes: seen.size, truncated: truncated || stack.length > 0, limits: {maxNodes, maxCharacters, maxDepth}};
}
function downloadsModelObservation() {
  const limits = {maxNodes: 4096, maxDepth: 32, maxItems: 64, maxStringCharacters: 2048};
  const observation = {url: location.href, ready: document.readyState, records: [], truncated: false, limits};
  const managers = document.querySelectorAll('downloads-manager');
  observation.managerCount = managers.length;
  if (managers.length !== 1) return observation;
  const stack = [{node: managers[0], depth: 0}], seen = new Set();
  const fields = ['id', 'filePath', 'fileName', 'url', 'state', 'isDangerous', 'isInsecure', 'fileExternallyRemoved', 'dangerType', 'total'];
  while (stack.length && seen.size < limits.maxNodes) {
    const {node, depth} = stack.pop();
    if (!node || seen.has(node)) continue;
    seen.add(node);
    if (depth > limits.maxDepth) {observation.truncated = true; continue;}
    if (node.nodeType === Node.ELEMENT_NODE && node.tagName === 'DOWNLOADS-ITEM') {
      if (observation.records.length >= limits.maxItems) {observation.truncated = true; break;}
      const data = node.data;
      const row = {populated: data !== undefined && data !== null, missing: [], invalid: []};
      observation.records.push(row);
      if (row.populated) {
        if (typeof data !== 'object') row.invalid.push('data is not a record');
        else for (const field of fields) {
          if (!(field in data)) {row.missing.push(field); continue;}
          let value = data[field];
          if (field === 'url') {
            if (value !== null && (typeof value !== 'object' || !('url' in value))) {row.invalid.push('url lacks its string field'); continue;}
            value = value === null ? null : value.url;
          }
          if (field === 'total' && typeof value === 'bigint') value = value.toString();
          if (typeof value === 'string') {
            row[field] = value.slice(0, limits.maxStringCharacters);
            observation.truncated ||= value.length > limits.maxStringCharacters;
          } else if (value === null || ['number', 'boolean'].includes(typeof value)) row[field] = value;
          else row.invalid.push(field + ' has an unsupported type');
        }
      }
    }
    const children = node.shadowRoot ? node.shadowRoot.childNodes : node.tagName === 'SLOT' ? node.assignedNodes({flatten: true}) : node.childNodes;
    if (children.length + stack.length + seen.size > limits.maxNodes) observation.truncated = true;
    for (let index = Math.min(children.length, limits.maxNodes - seen.size - stack.length) - 1; index >= 0; index--) stack.push({node: children[index], depth: depth + 1});
  }
  observation.visitedNodes = seen.size;
  observation.truncated ||= stack.length > 0;
  return observation;
}
function classifyDownload(observation, identity, expectedPath, expectedURL, sourceURL) {
  if (!observation || observation.truncated || !Array.isArray(observation.records) || !Number.isInteger(observation.managerCount)) throw Error('Downloads model observation is unavailable or truncated');
  if (observation.url !== 'chrome://downloads/') {
    if (!identity.pageObserved && observation.url === sourceURL && observation.managerCount === 0 && observation.records.length === 0) return {status: 'pending', reason: 'Downloads navigation has not committed'};
    throw Error('Downloads model has another page URL');
  }
  identity.pageObserved = true;
  if (observation.managerCount > 1) throw Error('Downloads manager is ambiguous');
  const rows = observation.records.filter(row => row.populated);
  if (rows.length > 1) throw Error('Downloads model has extra populated items');
  if (!rows.length) {
    if (identity.id) throw Error('identified download item disappeared');
    return {status: 'pending', reason: 'download item is not populated'};
  }
  const row = rows[0];
  if (!Array.isArray(row.missing) || !Array.isArray(row.invalid) || row.missing.length || row.invalid.length) throw Error('Downloads item schema is incompatible: ' + JSON.stringify({missing: row.missing, invalid: row.invalid}));
  if (typeof row.id !== 'string' || !/^[0-9]+$/.test(row.id) || typeof row.filePath !== 'string' || typeof row.fileName !== 'string' || (row.url !== null && typeof row.url !== 'string')) throw Error('Downloads item identity schema is incompatible');
  if (!Number.isInteger(row.state) || !Number.isInteger(row.dangerType) || row.dangerType < 0 || !['isDangerous', 'isInsecure', 'fileExternallyRemoved'].every(field => typeof row[field] === 'boolean')) throw Error('Downloads item state/flag schema is incompatible');
  const total = typeof row.total === 'number' && Number.isSafeInteger(row.total) ? String(row.total) : typeof row.total === 'string' && /^-?(0|[1-9][0-9]*)$/.test(row.total) ? row.total : undefined;
  if (total === undefined) throw Error('Downloads total is not an exact integer representation');
  if (identity.id && identity.id !== row.id) throw Error('Downloads item ID changed');
  identity.id ||= row.id;
  if (row.filePath && row.filePath !== expectedPath) throw Error('Downloads target path differs from owned installer');
  if (row.url !== null && row.url !== expectedURL) throw Error('Downloads final URL differs from pinned installer');
  const fullIdentity = row.filePath === expectedPath && row.url === expectedURL;
  if (identity.complete && !fullIdentity) throw Error('Downloads item identity changed after observation');
  identity.complete ||= fullIdentity;
  if (![0, 2, 7].includes(row.state)) throw Error('Downloads item is terminal, blocked or in an unknown state: ' + row.state);
  if (row.fileExternallyRemoved || row.isInsecure || (row.state !== 7 && (row.isDangerous || row.dangerType !== 0))) throw Error('Downloads item has a danger, insecurity or removal flag');
  if (row.state !== 2) return {status: 'pending', reason: row.state === 7 ? 'passive asynchronous scanning' : 'download is active', item: row};
  if (!fullIdentity || row.isDangerous || row.dangerType !== 0 || total !== '37470') throw Error('Downloads completion identity, flags or total differs from pinned installer');
  return {status: 'complete', item: row};
}
async function navigateDownloads(until) {
  downloadUIAttempted = true;
  const evidence = result.downloadUI = {url: 'chrome://downloads/', approvals: 'none attempted', navigationBeginElapsedSeconds: elapsed()};
  try {
    evidence.navigation = await cdp('Page.navigate', {url: evidence.url}, session, windowTimeout(until));
    if (evidence.navigation.errorText) throw Error(evidence.navigation.errorText);
    windowTimeout(until);
  } catch (error) {evidence.error = String(error); throw error;}
  finally {evidence.navigationEndElapsedSeconds = elapsed(); save('browser-download-ui.json', evidence);}
}
async function captureDownloads(until) {
  const evidence = result.downloadUI;
  evidence.captureBeginElapsedSeconds = elapsed();
  try {
    const operations = [
      ['Page.captureScreenshot', {format: 'png'}],
      ['Runtime.evaluate', {expression: `(${downloadsVisibleText.toString()})()`, returnByValue: true}],
    ];
    const observations = await Promise.allSettled(operations.map(async ([method, params]) => {
      const rows = await cdp(method, params, session, windowTimeout(until));
      if (method === 'Page.captureScreenshot') {fs.writeFileSync(path.join(root, 'browser-downloads.png'), Buffer.from(rows.data, 'base64')); return {captured: true};}
      if (rows.exceptionDetails) throw Error(JSON.stringify(rows.exceptionDetails));
      const value = rows.result?.value;
      if (value?.url !== evidence.url || typeof value.text !== 'string' || !value.text || value.truncated) throw Error('visible Downloads text unavailable or truncated');
      return value;
    }));
    evidence.screenshot = observations[0].status === 'fulfilled' ? observations[0].value : {error: String(observations[0].reason)};
    evidence.visibleText = observations[1].status === 'fulfilled' ? observations[1].value : {error: String(observations[1].reason)};
    if (observations.some(row => row.status === 'rejected')) throw Error('Downloads UI observation incomplete');
    windowTimeout(until);
  } catch (error) {
    evidence.error = String(error);
    throw error;
  } finally {
    evidence.captureEndElapsedSeconds = elapsed();
    save('browser-download-ui.json', evidence);
  }
}
async function waitForDownload(until, installer) {
  const evidence = result.downloadCompletion = {deadlineElapsedSeconds: until, limits: {maxSamples: 201, sampleIntervalMs: 250, maxCallSeconds: 2}, identity: {}, samples: []};
  async function sample(final = false) {
    if (evidence.samples.length >= evidence.limits.maxSamples) throw Error('download completion sample bound exhausted');
    if (result.error) throw Error(result.error);
    if (explicitRefusal) throw Error(explicitRefusal);
    const current = {index: evidence.samples.length, final, beginElapsedSeconds: elapsed()};
    evidence.samples.push(current);
    try {
      const response = await cdp('Runtime.evaluate', {expression: `(${downloadsModelObservation.toString()})()`, returnByValue: true}, session, windowTimeout(until));
      if (response.exceptionDetails) throw Error('Downloads model evaluation failed: ' + JSON.stringify(response.exceptionDetails));
      current.observation = response.result?.value;
      windowTimeout(until);
      current.classification = classifyDownload(current.observation, evidence.identity, installer, url, pageURL);
      return current.classification;
    } catch (error) {current.error = String(error); throw error;}
    finally {current.endElapsedSeconds = elapsed(); save('browser-download-completion.json', evidence);}
  }
  function verifyFile() {
    const metadata = fs.lstatSync(installer);
    if (!metadata.isFile() || metadata.uid !== process.getuid() || metadata.size !== 37470) throw Error('completed installer is not the exact owned regular 37470-byte file');
    result.downloadedSHA256 = hash(fs.readFileSync(installer));
    if (result.downloadedSHA256 !== expectedHash) throw Error('browser installer bytes differ from pinned commit');
    windowTimeout(until);
  }
  try {
    while (elapsed() < until && evidence.samples.length < evidence.limits.maxSamples - 1) {
      const state = await sample();
      if (state.status === 'complete') {
        verifyFile();
        evidence.eligibleElapsedSeconds = elapsed();
        await captureDownloads(until);
        const nextSample = evidence.samples[evidence.samples.length - 1].beginElapsedSeconds + 0.25;
        if (elapsed() < nextSample) {if (nextSample >= until) throw Error('download completion deadline exhausted before final sample'); await sleep((nextSample - elapsed()) * 1000);}
        if ((await sample(true)).status !== 'complete') throw Error('Downloads completion changed while evidence was captured');
        verifyFile();
        if (result.error || explicitRefusal) throw Error(result.error || explicitRefusal);
        evidence.status = 'complete';
        evidence.completedElapsedSeconds = elapsed();
        return;
      }
      if (until - elapsed() <= 0.25) break;
      await sleep(250);
    }
    if (elapsed() < until) await sleep((until - elapsed()) * 1000);
    throw Error('browser did not reach clean download completion within the shared deadline');
  } catch (error) {evidence.status = 'failed'; evidence.error = String(error); throw error;}
  finally {evidence.endElapsedSeconds = elapsed(); save('browser-download-completion.json', evidence);}
}
function processGroupPresent() {
  try {process.kill(-child.pid, 0); return true;}
  catch (error) {if (error.code === 'ESRCH') return false; throw error;}
}
async function closeBrowser() {
  phase('browser normal shutdown');
  const evidence = result.shutdown = {requestElapsedSeconds: elapsed(), observations: []};
  evidence.deadlineElapsedSeconds = Math.min(evidence.requestElapsedSeconds + 10, 109);
  closing = true;
  try {await cdp('Browser.close', {}, undefined, 2000); evidence.ackElapsedSeconds = elapsed();}
  catch (error) {evidence.error = String(error); evidence.errorElapsedSeconds = elapsed(); if (error.code !== 'EXPECTED_BROWSER_CLOSED') fail(error);}
  let normal = false, groupPresent;
  try {
    while (true) {
      groupPresent = processGroupPresent();
      const observedAt = elapsed();
      evidence.observations.push({elapsedSeconds: observedAt, groupPresent, childExit: childExit || null});
      if (observedAt > evidence.deadlineElapsedSeconds) break;
      if (childExit?.code === 0 && childExit.signal === null && !groupPresent) {normal = true; break;}
      const remaining = evidence.deadlineElapsedSeconds - elapsed();
      if (remaining <= 0) break;
      if (remaining < 0.1) {await sleep(remaining * 1000); break;}
      await sleep(100);
    }
  } catch (error) {evidence.observationError = String(error); fail(error);}
  evidence.normalExit = normal;
  result.forcedCleanup ||= false;
  if (!normal) {
    fail(Error('browser normal child/process-group exit was not established within its shutdown bound'));
    if (elapsed() < evidence.deadlineElapsedSeconds) await sleep((evidence.deadlineElapsedSeconds - elapsed()) * 1000);
    if (groupPresent !== false) {
      result.forcedCleanup = true;
      result.forcedCleanupReason ||= 'normal shutdown bound exhausted or group observation failed';
      trace('browser-lifecycle', {phase: result.phase, event: 'bounded-cleanup-kill', reason: result.forcedCleanupReason});
      try {process.kill(-child.pid, 'SIGKILL');} catch (error) {evidence.killError = String(error);}
    }
  }
  evidence.endElapsedSeconds = elapsed();
  clearTimeout(deadline);
  rejectPending(Error('browser controller cleanup complete'));
  result.elapsedSecondsAfterCleanup = elapsed();
  save('browser-result.json', result);
}
try {
  phase('browser setup');
  result.version = await cdp('Browser.getVersion');
  const target = await cdp('Target.createTarget', {url: 'about:blank'});
  session = (await cdp('Target.attachToTarget', {targetId: target.targetId, flatten: true})).sessionId;
  await cdp('Page.enable', {}, session);
  await cdp('Runtime.enable', {}, session);
  await cdp('Network.enable', {}, session);
  await cdp('Page.setLifecycleEventsEnabled', {enabled: true}, session);
  phase('network-free navigation control');
  const controlURL = 'data:text/html,<title>lwiki navigation control</title><p>owned browser control</p>';
  const controlNavigation = await cdp('Page.navigate', {url: controlURL}, session);
  if (controlNavigation.errorText) throw Error('network-free navigation refused: ' + controlNavigation.errorText);
  const controlReady = elapsed() + 3;
  do {
    if (explicitRefusal) throw Error(explicitRefusal);
    result.rendererControl = await cdp('Runtime.evaluate', {expression: '({title:document.title,text:document.body?.innerText})', returnByValue: true}, session, 2000);
    if (result.rendererControl.result?.value?.title === 'lwiki navigation control' && result.rendererControl.result?.value?.text === 'owned browser control') break;
    await sleep(100);
  } while (elapsed() < controlReady);
  if (result.rendererControl.result?.value?.title !== 'lwiki navigation control' || result.rendererControl.result?.value?.text !== 'owned browser control') throw Error('network-free renderer marker did not load');
  phase('public GitHub navigation');
  await navigatePublic();
  let state;
  const ready = elapsed() + 25;
  do {
    if (explicitRefusal) throw Error(explicitRefusal);
    state = (await cdp('Runtime.evaluate', {expression: '({url:location.href,ready:document.readyState,raw:document.querySelector("[data-testid=raw-button]")?.href})', returnByValue: true}, session)).result?.value;
    if (state?.url === pageURL && state.ready === 'complete' && state.raw === downloadURL) break;
    await sleep(200);
  } while (elapsed() < ready);
  if (state?.url !== pageURL || state.ready !== 'complete' || state.raw !== downloadURL) throw Error('pinned GitHub file page did not load');
  save('installer-document.json', state);
  phase('normal browser download');
  // Chrome's documented Mac shortcut downloads the existing link with Option.
  // The pinned real control must be visible and its center must hit that anchor.
  const link = await cdp('Runtime.evaluate', {expression: `(${rawLinkObservation.toString()})(${JSON.stringify(downloadURL)},true)`, returnByValue: true}, session);
  if (link.exceptionDetails) throw Error('Raw link inspection failed: ' + JSON.stringify(link.exceptionDetails));
  const control = link.result?.value;
  save('browser-raw-link.json', control || {error: 'Raw link observation absent'});
  if (!control || control.error || control.url !== pageURL) throw Error(control?.error || 'pinned Raw link observation unavailable');
  const beforeGesture = await cdp('Page.captureScreenshot', {format: 'png'}, session);
  fs.writeFileSync(path.join(root, 'browser-raw-link.png'), Buffer.from(beforeGesture.data, 'base64'));
  const finalControl = await relocateRawLink(control);
  result.rawLinkRecheck = finalControl;
  if (explicitRefusal) throw Error(explicitRefusal);
  result.downloadGesture = {kind: 'one real Raw-link Option-click', href: finalControl.href, point: finalControl.point, button: 'left', modifiers: 1, clickCount: 1, mousePressed: false, mouseReleased: false};
  save('browser-result.json', result);
  await cdp('Input.dispatchMouseEvent', {type: 'mousePressed', ...finalControl.point, button: 'left', modifiers: 1, clickCount: 1}, session);
  result.downloadGesture.mousePressed = true;
  save('browser-result.json', result);
  await cdp('Input.dispatchMouseEvent', {type: 'mouseReleased', ...finalControl.point, button: 'left', modifiers: 1, clickCount: 1}, session);
  const until = Math.min(95, elapsed() + 50);
  result.downloadGesture.mouseReleased = true;
  result.completionDeadlineElapsedSeconds = until;
  save('browser-result.json', result);
  phase('browser download completion');
  await snapshot('browser-download-attempt-page', until, true);
  await navigateDownloads(until);
  await waitForDownload(until, path.join(downloads, 'install.sh'));
  result.status = 'passed';
} catch (error) {
  fail(error);
  if (session && elapsed() < 105) await Promise.allSettled([snapshot('browser-failure-page'), desktopFailure()]);
  if (session && !downloadUIAttempted && elapsed() < 105) {
    try {await navigateDownloads(105); await captureDownloads(105);} catch (observationError) {result.downloadUIError = String(observationError);}
  } else if (session && result.downloadUI && !result.downloadUI.captureEndElapsedSeconds && elapsed() < 105) {
    try {await captureDownloads(105);} catch (observationError) {result.downloadUIError = String(observationError);}
  }
} finally {
  result.mainNavigation = mainNavigation;
  result.explicitRefusal = explicitRefusal;
  result.elapsedSeconds = elapsed();
  save('browser-result.json', result);
  await closeBrowser();
}
if (result.status !== 'passed') process.exitCode = 1;
