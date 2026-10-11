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
let next = 1, pending = new Map(), receive = Buffer.alloc(0), session, spawnError;
child.on('error', error => {spawnError = String(error);});
function cdp(method, params = {}, sid) {
  const id = next++;
  return new Promise((resolve, reject) => {
    if (spawnError) return reject(Error(spawnError));
    const timer = setTimeout(() => {pending.delete(id); reject(Error(method + ' timed out'));}, 10000);
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
    if (message.id && pending.has(message.id)) {
      const item = pending.get(message.id); pending.delete(message.id); clearTimeout(item.timer);
      if (message.error) item.reject(Error(JSON.stringify(message.error))); else item.resolve(message.result);
    } else if (message.method === 'Network.responseReceived') {
      const r = message.params.response;
      fs.appendFileSync(path.join(root, 'browser-network.jsonl'), JSON.stringify({url: r.url, status: r.status, mimeType: r.mimeType, protocol: r.protocol, securityState: r.securityState}) + '\n');
    }
  }
});
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
const deadline = setTimeout(() => {try {process.kill(-child.pid, 'SIGKILL');} catch {}}, 110000);
try {
  result.version = await cdp('Browser.getVersion');
  const target = await cdp('Target.createTarget', {url: 'about:blank'});
  session = (await cdp('Target.attachToTarget', {targetId: target.targetId, flatten: true})).sessionId;
  await cdp('Page.enable', {}, session);
  await cdp('Runtime.enable', {}, session);
  await cdp('Network.enable', {}, session);
  const navigation = await cdp('Page.navigate', {url: pageURL}, session);
  if (navigation.errorText) throw Error('installer navigation failed: ' + navigation.errorText);
  let state;
  const ready = elapsed() + 25;
  do {
    state = (await cdp('Runtime.evaluate', {expression: '({url:location.href,ready:document.readyState,raw:document.querySelector("[data-testid=raw-button]")?.href})', returnByValue: true}, session)).result?.value;
    if (state?.url === pageURL && state.ready === 'complete' && state.raw === downloadURL) break;
    await sleep(200);
  } while (elapsed() < ready);
  if (state?.url !== pageURL || state.ready !== 'complete' || state.raw !== downloadURL) throw Error('pinned GitHub file page did not load');
  save('installer-document.json', state);
  // GitHub raw text has CSP sandbox without allow-downloads. Initiate from its
  // ordinary unsandboxed same-origin file page, following GitHub's raw redirect.
  const click = await cdp('Runtime.evaluate', {expression: `(()=>{const a=document.createElement('a');a.href=${JSON.stringify(downloadURL)};a.download='install.sh';a.textContent='Download install.sh';document.body.appendChild(a);a.click();return {href:a.href,download:a.download};})()`, userGesture: true, returnByValue: true}, session);
  if (click.exceptionDetails) throw Error('download gesture failed: ' + JSON.stringify(click.exceptionDetails));
  result.downloadGesture = click.result?.value;
  const until = Math.min(95, elapsed() + 50), installer = path.join(downloads, 'install.sh');
  while (elapsed() < until && !fs.existsSync(installer)) await sleep(250);
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
  result.status = 'passed';
} catch (error) {
  result.status = 'failed'; result.error = String(error);
} finally {
  result.elapsedSeconds = elapsed();
  try {await cdp('Browser.close');} catch {}
  await sleep(1500);
  try {process.kill(-child.pid, 'SIGKILL'); result.forcedCleanup = true;} catch {result.forcedCleanup = false;}
  clearTimeout(deadline);
  for (const item of pending.values()) {clearTimeout(item.timer); item.reject(Error('browser closed'));}
  pending.clear();
  save('browser-result.json', result);
}
if (result.status !== 'passed') process.exitCode = 1;
