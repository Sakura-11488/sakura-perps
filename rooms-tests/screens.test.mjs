import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { existsSync } from 'node:fs';
import { before, after, test } from 'node:test';
import { serveRoomsHarness } from './serve.mjs';
const require = createRequire(new URL('../integrations/sakura-rooms/package.json', import.meta.url));
const { chromium, expect } = require('@playwright/test');
let harness, browser;
before(async () => {
  harness = await serveRoomsHarness();
  browser = await chromium.launch({ headless: true,
    executablePath: process.env.SAKURA_BROWSER_EXECUTABLE || (existsSync('/Applications/Google Chrome.app/Contents/MacOS/Google Chrome') ? '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome' : undefined) });
});
after(async () => { await browser?.close(); await harness?.close(); });

test('real Rooms component: consent, signed allocation, settlement, reload and read-only public verification', { timeout: 60_000 }, async () => {
  const context = await browser.newContext({ viewport: { width: 390, height: 844 }, serviceWorkers: 'block', acceptDownloads: true });
  try {
    const page = await context.newPage();
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    await page.goto(`${harness.origin}/app/rooms-screen.html`);
    const main = page.locator('.rooms-page');
    await expect(main.getByText('No published plans yet.', { exact: false })).toBeVisible();
    const title = '<img src=x onerror=alert(1)> is just plan text';
    await main.getByLabel('Plan title', { exact: true }).fill(title);
    await main.getByLabel('My thesis', { exact: true }).fill('A measured paper allocation in the fixed scenario.');
    await main.getByLabel('What would change my mind?', { exact: true }).fill('Review at the known terminal price.');
    await main.getByRole('button', { name: 'Publish plan locally', exact: true }).click();
    await expect(main.getByRole('heading', { name: 'Your idea. Your allocation.' })).toBeVisible();
    await expect(main.locator('img')).toHaveCount(0);
    await main.getByLabel('Total paper budget', { exact: true }).fill('101');
    const approve = main.getByRole('button', { name: 'Approve my allocation', exact: true });
    await expect(approve).toBeDisabled();
    await main.getByLabel('Total paper budget', { exact: true }).fill('25');
    await expect(main.getByRole('region', { name: 'Paper allocation quote' })).toContainText('0.0499 credits');
    await expect(approve).toBeDisabled();
    const consent = main.getByRole('checkbox', { name: /I approve this one paper allocation/ });
    await consent.check();
    await main.getByRole('radio', { name: '1×', exact: true }).check();
    await expect(consent).not.toBeChecked();
    await main.getByRole('radio', { name: '2×', exact: true }).check();
    await expect(main.getByRole('region', { name: 'Paper allocation quote' })).toContainText('0.0499 credits');
    await consent.check();
    await approve.click();
    await expect(main).toContainText('No result has been generated yet.');
    await main.getByRole('button', { name: 'Run paper scenario', exact: true }).click();
    await expect(main.locator('.rooms-outcome')).toContainText('+1.9461');
    await expect(main.locator('.rooms-outcome')).toContainText('26.9461');
    const downloadPromise = page.waitForEvent('download');
    await main.getByRole('button', { name: 'Export public record', exact: true }).click();
    const download = await downloadPromise;
    const stream = await download.createReadStream();
    const chunks = []; for await (const chunk of stream) chunks.push(chunk);
    const data = Buffer.concat(chunks);
    const record = JSON.parse(data.toString());
    assert.equal(record.records.length, 8);
    assert.equal(record.plan.title, title);
    assert.equal(Object.hasOwn(record, 'privateKey'), false);
    await page.reload();
    await expect(main.locator('.rooms-outcome')).toContainText('26.9461');
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth + 1), true);
    await main.locator('input[type=file]').setInputFiles({ name: 'record.json', mimeType: 'application/json', buffer: data });
    await expect(main.getByText('VERIFIED EXPORT · READ ONLY', { exact: true })).toBeVisible();
    await expect(main.getByRole('button', { name: 'Run paper scenario', exact: true })).toHaveCount(0);
    await expect(main.locator('.rooms-history li')).toHaveCount(1);
    assert.deepEqual(errors, []);
  } finally { await context.close(); }
});
