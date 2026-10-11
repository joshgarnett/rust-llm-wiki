// Fresh hosted Mac only. Use the shipped browser download manager and protections.
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import {spawn} from 'node:child_process';

if (process.platform !== 'darwin' || process.getuid() === 0 || process.env.GITHUB_ACTIONS !== 'true' || process.env.RUNNER_ENVIRONMENT !== 'github-hosted') throw Error('requires a fresh nonroot GitHub-hosted Mac');
const [root, url, expectedHash] = process.argv.slice(2);
if (!/^https:\/\/raw\.githubusercontent\.com\/joshgarnett\/rust-llm-wiki\/[0-9a-f]{40}\/scripts\/install\.sh$/.test(url) || !/^[0-9a-f]{64}$/.test(expectedHash)) throw Error('requires pinned public installer URL and hash');
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
const result = {status: 'running', url, pageURL, downloadURL, expectedHash, browserExecutable: chrome, browserExecutableSHA256: hash(fs.readFileSync(chrome)), seededPreferences: preferences, downloadManager: 'normal Chrome delegate; no CDP download override', approvals: 'none attempted', headed: true};
const args = [`--user-data-dir=${profile}`, '--remote-debugging-pipe', '--no-first-run', '--no-default-browser-check', 'about:blank'];
result.argv = args;
save('browser-result.json', result);
const child = spawn(chrome, args, {detached: true, env: process.env, stdio: ['ignore', 'pipe', 'pipe', 'pipe', 'pipe']});
save('browser-pid.json', {pid: child.pid});
child.stdout.pipe(fs.createWriteStream(path.join(root, 'browser.stdout')));
child.stderr.pipe(fs.createWriteStream(path.join(root, 'browser.stderr')));
const begin = process.hrtime.bigint();
const elapsed = () => Number(process.hrtime.bigint() - begin) / 1e9;
let next = 1, pending = new Map(), commands = new Map(), receive = Buffer.alloc(0), session, spawnError, mainNavigation, explicitRefusal;
const trace = (name, value) => fs.appendFileSync(path.join(root, name + '.jsonl'), JSON.stringify({elapsedSeconds: elapsed(), ...value}) + '\n');
const phase = name => {result.phase = name; save('browser-result.json', result);};
child.on('error', error => {spawnError = String(error);});
function cdp(method, params = {}, sid, timeout = 10000) {
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
child.stdio[3].on('error', error => {spawnError = String(error);});
child.stdio[4].on('data', bytes => {
  receive = Buffer.concat([receive, bytes]);
  while (receive.includes(0)) {
    const end = receive.indexOf(0), raw = receive.subarray(0, end).toString();
    receive = receive.subarray(end + 1);
    if (!raw) continue;
    const message = JSON.parse(raw);
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
const deadline = setTimeout(() => {try {process.kill(-child.pid, 'SIGKILL');} catch {}}, 110000);
async function snapshot(name) {
  const timeout = Math.max(1, Math.min(2000, (105 - elapsed()) * 1000));
  const operations = [
    ['frameTree', 'Page.getFrameTree', {}],
    ['document', 'Runtime.evaluate', {expression: '({url:location.href,title:document.title,ready:document.readyState,text:document.body?.innerText?.slice(0,2000)})', returnByValue: true}],
    ['screenshot', 'Page.captureScreenshot', {format: 'png'}],
  ];
  const rows = await Promise.allSettled(operations.map(async ([key, method, params]) => {
    const value = await cdp(method, params, session, timeout);
    if (key === 'screenshot') {fs.writeFileSync(path.join(root, name + '.png'), Buffer.from(value.data, 'base64')); return {key, captured: true};}
    return {key, value};
  }));
  save(name + '.json', {mainNavigation, explicitRefusal, inspections: rows.map((row, i) => row.status === 'fulfilled' ? row.value : {key: operations[i][0], error: String(row.reason)})});
}
async function desktopFailure() {
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
  // GitHub raw text has CSP sandbox without allow-downloads. Initiate from its
  // ordinary unsandboxed same-origin file page, following GitHub's raw redirect.
  const click = await cdp('Runtime.evaluate', {expression: `(()=>{const a=document.createElement('a');a.href=${JSON.stringify(downloadURL)};a.download='install.sh';a.textContent='Download install.sh';document.body.appendChild(a);a.click();return {href:a.href,download:a.download};})()`, userGesture: true, returnByValue: true}, session);
  if (click.exceptionDetails) throw Error('download gesture failed: ' + JSON.stringify(click.exceptionDetails));
  result.downloadGesture = click.result?.value;
  const until = Math.min(95, elapsed() + 50), installer = path.join(downloads, 'install.sh');
  while (elapsed() < until && !fs.existsSync(installer)) {
    if (explicitRefusal) throw Error(explicitRefusal);
    await sleep(250);
  }
  if (explicitRefusal) throw Error(explicitRefusal);
  // Record the actual download UI, including any warning, without clicking Keep.
  await cdp('Page.navigate', {url: 'chrome://downloads/'}, session);
  await sleep(1000);
  const shot = await cdp('Page.captureScreenshot', {format: 'png'}, session);
  fs.writeFileSync(path.join(root, 'browser-downloads.png'), Buffer.from(shot.data, 'base64'));
  const rows = await cdp('Runtime.evaluate', {expression: `(()=>{const m=document.querySelector('downloads-manager');return {text:document.body.innerText,items:m?.items};})()`, returnByValue: true}, session);
  save('browser-download-ui.json', rows);
  if (!fs.existsSync(installer)) throw Error('browser did not finish install.sh download; no warning accepted');
  result.downloadedSHA256 = hash(fs.readFileSync(installer));
  if (result.downloadedSHA256 !== expectedHash) throw Error('browser installer bytes differ from pinned commit');
  if (explicitRefusal) throw Error(explicitRefusal);
  result.status = 'passed';
} catch (error) {
  result.status = 'failed'; result.error = String(error);
  if (session && elapsed() < 105) await Promise.allSettled([snapshot('browser-failure-page'), desktopFailure()]);
} finally {
  result.mainNavigation = mainNavigation;
  result.explicitRefusal = explicitRefusal;
  result.elapsedSeconds = elapsed();
  save('browser-result.json', result);
  try {await cdp('Browser.close', {}, undefined, 2000);} catch {}
  await sleep(1500);
  try {process.kill(-child.pid, 'SIGKILL'); result.forcedCleanup = true;} catch {result.forcedCleanup = false;}
  clearTimeout(deadline);
  for (const item of pending.values()) {clearTimeout(item.timer); item.reject(Error('browser closed'));}
  pending.clear();
  result.elapsedSecondsAfterCleanup = elapsed();
  save('browser-result.json', result);
}
if (result.status !== 'passed') process.exitCode = 1;
