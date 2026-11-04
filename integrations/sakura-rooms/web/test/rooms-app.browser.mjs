import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { createRequire } from 'node:module';
import { existsSync } from 'node:fs';
import { mkdir, readFile, stat } from 'node:fs/promises';
import { extname, join, resolve, sep } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

// Full exported Sakura app, actual browser WASM engine, real Ed25519 signatures,
// and IndexedDB. No wallet or external network access. Build with npm run build.
// SAKURA_BROWSER_TEST_NODE_MODULES=/path/to/node_modules \
//   node --test web/test/rooms-app.browser.mjs
const root = fileURLToPath(new URL('../../', import.meta.url));
const dependencies = createRequire(resolve(process.env.SAKURA_BROWSER_TEST_NODE_MODULES
  || join(root, '../sakura-perps/integrations/sakura-rooms/node_modules'), '__rooms_test__.cjs'));
const { chromium, expect: baseExpect } = dependencies('@playwright/test');
const expect = baseExpect.configure({ timeout: 20_000 });
const dist = resolve(process.env.SAKURA_EXPORT_DIST || join(root, 'web/dist'));
const artifacts = resolve(process.env.SAKURA_ROOMS_ARTIFACTS || join(tmpdir(), 'sakura-rooms-ui-artifacts'));

async function serveExport() {
  assert.ok(existsSync(join(dist, 'index.html')), 'Build the full Sakura Expo export first.');
  const mime = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.json': 'application/json', '.wasm': 'application/wasm', '.png': 'image/png', '.webp': 'image/webp', '.svg': 'image/svg+xml', '.ttf': 'font/ttf', '.woff2': 'font/woff2' };
  const server = createServer(async (req, res) => {
    try {
      const path = decodeURIComponent(new URL(req.url, 'http://localhost').pathname);
      if (!/^\/app(?:\/|$)/.test(path)) { res.writeHead(404).end(); return; }
      let file = resolve(dist, path.replace(/^\/app\/?/, '') || 'index.html');
      if (file !== dist && !file.startsWith(dist + sep)) { res.writeHead(403).end(); return; }
      if (!(await stat(file).catch(() => null))?.isFile()) {
        if (extname(path)) { res.writeHead(404).end(); return; }
        file = join(dist, 'index.html');
      }
      res.writeHead(200, { 'Content-Type': mime[extname(file)] || 'application/octet-stream', 'Cache-Control': 'no-store' });
      res.end(await readFile(file));
    } catch { res.writeHead(500).end(); }
  });
  await new Promise(done => server.listen(0, '127.0.0.1', done));
  return { server, origin: `http://127.0.0.1:${server.address().port}` };
}

async function browser() {
  return chromium.launch({ headless: true, executablePath: process.env.SAKURA_BROWSER_EXECUTABLE
    || (existsSync('/Applications/Google Chrome.app/Contents/MacOS/Google Chrome') ? '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome' : undefined) });
}

async function contextFor(instance, origin, mobile = false) {
  const context = await instance.newContext({ viewport: mobile ? { width: 390, height: 844 } : { width: 1440, height: 1080 },
    isMobile: mobile, hasTouch: mobile, serviceWorkers: 'block', reducedMotion: 'reduce', acceptDownloads: true });
  await context.route('**/*', route => {
    const url = route.request().url();
    return new URL(url).origin === origin || url.startsWith('data:') || url.startsWith('blob:') ? route.continue() : route.abort();
  });
  return context;
}

function main(page) { return page.locator('.rooms-page:visible'); }
function step(page, label) { return main(page).getByRole('navigation', { name: 'Room steps' }).getByRole('button', { name: new RegExp(`${label}$`) }); }
async function open(page, origin) {
  const response = await page.goto(`${origin}/app/perps/rooms`);
  assert.equal(response.status(), 200);
  await expect(main(page).getByRole('heading', { name: /Think it through/ })).toBeVisible();
  await expect(main(page).getByText('Loading and verifying local history…')).toHaveCount(0);
  assert.equal(new URL(page.url()).pathname, '/app/perps/rooms');
}
async function theme(page, name) {
  const surface = main(page);
  if (await surface.getAttribute('data-theme') !== name) await surface.getByRole('button', { name: `Switch to ${name} mode` }).click();
  await expect(surface).toHaveAttribute('data-theme', name);
  await expect(surface).toHaveCSS('background-color', name === 'light' ? 'rgb(252, 249, 247)' : 'rgb(20, 18, 22)');
}
async function noOverflow(page) {
  const measure = await main(page).evaluate(element => ({ width: element.clientWidth, scroll: element.scrollWidth,
    document: document.documentElement.scrollWidth, viewport: window.innerWidth }));
  assert.ok(measure.scroll <= measure.width + 1, `Rooms has no horizontal overflow: ${JSON.stringify(measure)}`);
  assert.ok(measure.document <= measure.viewport + 1, `Document has no horizontal overflow: ${JSON.stringify(measure)}`);
}
async function screenshot(page, name) {
  await mkdir(artifacts, { recursive: true });
  await main(page).evaluate(element => element.scrollTo(0, 0));
  await page.screenshot({ path: join(artifacts, `${name}.png`) });
}
async function draft(page, title) {
  await main(page).getByLabel('Plan title', { exact: true }).fill(title);
  await main(page).getByLabel('My thesis', { exact: true }).fill('I will use a small allocation to examine the fixed dip and recovery.');
  await main(page).getByLabel('What would change my mind?', { exact: true }).fill('A price below 94 would invalidate my thesis. This text is not an automatic stop order.');
}
async function publicExport(page) {
  const promised = page.waitForEvent('download');
  await main(page).getByRole('button', { name: 'Export public record', exact: true }).click();
  const download = await promised;
  const data = await readFile(await download.path(), 'utf8');
  const exported = JSON.parse(data);
  assert.deepEqual(Object.keys(exported).sort(), ['createdAt', 'engine', 'genesis', 'head', 'id', 'nonce', 'plan', 'publicKey', 'records', 'scenario', 'schema'].sort());
  assert.equal(exported.engine, 'sakura-rooms-paper/1');
  assert.equal(Object.hasOwn(exported, 'privateKey'), false);
  assert.match(exported.publicKey, /^[a-f0-9]{64}$/);
  return { data, exported };
}

test('Rooms full app: Plan → allocation approval → actual replayed outcome, history, exports, keyboard and responsive themes', { timeout: 150_000 }, async () => {
  const { server, origin } = await serveExport();
  const instance = await browser();
  try {
    for (const mobile of [false, true]) {
      const context = await contextFor(instance, origin, mobile);
      try {
        const page = await context.newPage();
        page.setDefaultTimeout(25_000);
        const errors = [];
        page.on('pageerror', error => errors.push(error.message));
        await open(page, origin);
        await expect(main(page).getByText('No published plans yet.', { exact: false })).toBeVisible();
        await expect(step(page, 'My allocation')).toBeDisabled();
        await expect(step(page, 'Review')).toBeDisabled();
        await theme(page, mobile ? 'light' : 'dark');
        const title = mobile ? 'A measured short' : 'A measured recovery';
        await draft(page, 'An editable draft');
        await main(page).getByLabel('Plan title', { exact: true }).fill(title);
        await noOverflow(page);
        await screenshot(page, `${mobile ? 'mobile' : 'desktop'}-plan`);
        const publish = main(page).getByRole('button', { name: 'Publish plan locally', exact: true });
        await publish.focus();
        await page.keyboard.press('Enter');
        await expect(main(page).getByRole('heading', { name: 'Your idea. Your allocation.' })).toBeVisible();
        await step(page, 'Plan').focus();
        await page.keyboard.press('Space');
        await expect(main(page).getByRole('heading', { name: title, exact: true })).toBeVisible();
        await expect(main(page).getByLabel('Plan title', { exact: true })).toHaveCount(0);
        await expect(main(page).getByRole('button', { name: 'Publish plan locally', exact: true })).toHaveCount(0);
        await step(page, 'My allocation').click();
        await main(page).getByLabel('Total paper budget', { exact: true }).fill('101');
        await expect(main(page).getByRole('button', { name: 'Approve my allocation', exact: true })).toBeDisabled();
        await main(page).getByLabel('Total paper budget', { exact: true }).fill(mobile ? '10' : '25');
        if (mobile) {
          await main(page).getByRole('radio', { name: 'Short · price falls' }).check();
          await main(page).getByRole('radio', { name: '3×', exact: true }).check();
        }
        await expect(main(page).getByRole('region', { name: 'Paper allocation quote' })).toBeVisible();
        const preview = main(page).getByRole('region', { name: 'Paper allocation quote' });
        // Independent known examples: 10 bps on notional, exact integer rounding,
        // and the final +4% price move. Assertions cover actual engine output.
        await expect(preview).toContainText(mobile ? '9.97 credits' : '24.95 credits');
        await expect(preview).toContainText(mobile ? '0.02991 credits' : '0.0499 credits');
        await expect(main(page).getByRole('button', { name: 'Approve my allocation', exact: true })).toBeDisabled();
        await theme(page, mobile ? 'dark' : 'light');
        await noOverflow(page);
        await screenshot(page, `${mobile ? 'mobile' : 'desktop'}-allocation`);
        const approval = main(page).getByRole('checkbox', { name: /I approve this one paper allocation/ });
        await approval.focus();
        await page.keyboard.press('Space');
        // A change invalidates prior UI consent even if the eventual quote is equal.
        await main(page).getByRole('radio', { name: '1×', exact: true }).check();
        await expect(approval).not.toBeChecked();
        await main(page).getByRole('radio', { name: mobile ? '3×' : '2×', exact: true }).check();
        await expect(preview).toContainText(mobile ? '9.97 credits' : '24.95 credits');
        await approval.check();
        await main(page).getByRole('button', { name: 'Approve my allocation', exact: true }).click();
        await expect(main(page).getByRole('button', { name: 'Run paper scenario', exact: true })).toBeVisible();
        await expect(main(page)).toContainText('No result has been generated yet.');
        const openRecord = await publicExport(page);
        assert.equal(openRecord.exported.records.length, 6);
        await main(page).getByRole('button', { name: 'Run paper scenario', exact: true }).click();
        await expect(main(page).getByText('Net result after entry fee', { exact: true })).toBeVisible();
        await expect(main(page).locator('.rooms-outcome')).toContainText(mobile ? '−1.22631' : '+1.9461');
        await expect(main(page).locator('.rooms-outcome')).toContainText(mobile ? '8.77369' : '26.9461');
        await expect(main(page).getByRole('button', { name: 'Run paper scenario', exact: true })).toHaveCount(0);
        await theme(page, mobile ? 'light' : 'dark');
        await noOverflow(page);
        await screenshot(page, `${mobile ? 'mobile' : 'desktop'}-review`);
        const finalRecord = await publicExport(page);
        assert.equal(finalRecord.exported.records.length, 8);
        assert.equal(finalRecord.exported.plan.title, title);
        assert.notEqual(finalRecord.exported.head, openRecord.exported.head);
        await page.reload();
        await expect(main(page).getByText('Net result after entry fee', { exact: true })).toBeVisible();
        const restored = await publicExport(page);
        assert.deepEqual(restored.exported, finalRecord.exported, 'Reload replays the same durable signed history');
        await main(page).locator('input[type=file]').setInputFiles({ name: 'public-room.json', mimeType: 'application/json', buffer: Buffer.from(finalRecord.data) });
        await expect(main(page).getByText('VERIFIED EXPORT · READ ONLY', { exact: true })).toBeVisible();
        await expect(main(page).getByRole('button', { name: 'Export public record', exact: true })).toHaveCount(0);
        await expect(main(page).getByRole('button', { name: 'Run paper scenario', exact: true })).toHaveCount(0);
        await expect(main(page).locator('.rooms-history li')).toHaveCount(1);
        const changed = { ...finalRecord.exported, plan: { ...finalRecord.exported.plan, title: 'Tampered published plan' } };
        await main(page).locator('input[type=file]').setInputFiles({ name: 'tampered.json', mimeType: 'application/json', buffer: Buffer.from(JSON.stringify(changed)) });
        await expect(main(page).getByRole('alert')).toContainText('Room verification failed');
        await expect(main(page).getByRole('heading', { name: title, exact: true })).toBeVisible();
        await main(page).getByRole('button', { name: 'Reload local history', exact: true }).click();
        await expect(main(page).getByRole('alert')).toHaveCount(0);
        assert.deepEqual((await publicExport(page)).exported, finalRecord.exported, 'Failed import did not overwrite the record');
        if (mobile) {
          await page.setViewportSize({ width: 320, height: 844 });
          await noOverflow(page);
        }
        await main(page).locator('.perps-back').click();
        const roadmap = page.locator('.perps-page:not(.rooms-page):visible');
        const pilot = roadmap.getByRole('link', { name: 'Try Sakura Rooms · paper pilot', exact: true });
        await expect(pilot).toBeVisible();
        assert.equal(new URL(await pilot.getAttribute('href'), origin).pathname, '/app/perps/rooms');
        await pilot.click();
        await expect(main(page).getByText('Net result after entry fee', { exact: true })).toBeVisible();
        assert.equal(new URL(page.url()).pathname, '/app/perps/rooms');
        await main(page).getByRole('button', { name: 'Start a new plan', exact: true }).click();
        await draft(page, 'Keep this unpublished draft');
        await main(page).getByRole('button', { name: 'Reload & verify', exact: true }).click();
        await expect(main(page).getByLabel('Plan title', { exact: true })).toHaveValue('Keep this unpublished draft');
        await expect(main(page).locator('.rooms-history li')).toHaveCount(1);
        assert.deepEqual(errors, [], 'No unhandled page errors');
      } finally { await context.close(); }
    }
  } finally { await instance.close(); await new Promise(done => server.close(done)); }
});

test('Rooms engine-load failure preserves editable plan and recovers without a phantom published record', { timeout: 90_000 }, async () => {
  const { server, origin } = await serveExport();
  const instance = await browser();
  const context = await contextFor(instance, origin);
  try {
    const page = await context.newPage();
    const enginePattern = '**/app/rooms-engine/*.js';
    await context.route(enginePattern, route => route.fulfill({ status: 503, body: 'Fixture outage' }));
    await open(page, origin);
    await draft(page, 'My draft survives an outage');
    await main(page).getByRole('button', { name: 'Publish plan locally', exact: true }).click();
    await expect(main(page).getByRole('alert')).toContainText('verified paper engine could not load');
    await expect(main(page).getByLabel('Plan title', { exact: true })).toHaveValue('My draft survives an outage');
    await expect(main(page).locator('.rooms-history li')).toHaveCount(0);
    await context.unroute(enginePattern);
    await main(page).getByRole('button', { name: 'Reload local history', exact: true }).click();
    await expect(main(page).getByRole('alert')).toHaveCount(0);
    await expect(main(page).getByLabel('Plan title', { exact: true })).toHaveValue('My draft survives an outage');
    await main(page).getByRole('button', { name: 'Publish plan locally', exact: true }).click();
    await expect(main(page).getByRole('heading', { name: 'Your idea. Your allocation.' })).toBeVisible();
    await expect(main(page).locator('.rooms-history li')).toHaveCount(1);
    await step(page, 'Review').click();
    const original = await publicExport(page);
    // Remove only the generated PAPER key in this isolated test context. Never
    // touch the host application's wallet database or any real signing key.
    await page.evaluate(() => new Promise((resolve, reject) => {
      const request = indexedDB.open('sakura-rooms-paper-v1', 1);
      request.onerror = () => reject(new Error('Fixture database open failed'));
      request.onsuccess = () => {
        const db = request.result;
        const tx = db.transaction('rooms', 'readwrite');
        const store = tx.objectStore('rooms');
        const rows = store.getAll();
        rows.onsuccess = () => {
          if (rows.result.length !== 1) { tx.abort(); return; }
          const publicOnly = { ...rows.result[0] };
          delete publicOnly.privateKey;
          store.put(publicOnly);
        };
        tx.oncomplete = () => { db.close(); resolve(); };
        tx.onabort = tx.onerror = () => { db.close(); reject(new Error('Fixture key removal failed')); };
      };
    }));
    await main(page).getByRole('button', { name: 'Reload & verify', exact: true }).click();
    await expect(main(page).getByText('Read-only recovery', { exact: true })).toBeVisible();
    await expect(main(page).getByRole('button', { name: 'Approve my allocation', exact: true })).toHaveCount(0);
    await step(page, 'Review').click();
    await expect(main(page).getByRole('button', { name: 'Choose my allocation', exact: true })).toHaveCount(0);
    await expect(main(page).getByRole('button', { name: 'Run paper scenario', exact: true })).toHaveCount(0);
    assert.deepEqual((await publicExport(page)).exported, original.exported, 'Missing local key preserves public review and export');
  } finally { await context.close(); await instance.close(); await new Promise(done => server.close(done)); }
});
