import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { existsSync } from 'node:fs';
import { mkdtemp, mkdir, rm, readFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { startServer, issueAdmission } from '../community/server.mjs';

const root = fileURLToPath(new URL('../', import.meta.url));
const require = createRequire(new URL('../integrations/sakura-rooms/package.json', import.meta.url));
const { chromium, expect: baseExpect } = require('@playwright/test');
const expect = baseExpect.configure({ timeout: 20_000 });
const surface = page => page.locator('.community-page:visible');
const step = (page, label) => surface(page).getByRole('navigation', { name: 'Community room steps' }).getByRole('button', { name: new RegExp(`${label}$`) });
async function overflow(page) {
  const value = await surface(page).evaluate(element => ({ width: element.clientWidth, scroll: element.scrollWidth, document: document.documentElement.scrollWidth, viewport: innerWidth }));
  assert.ok(value.scroll <= value.width + 1 && value.document <= value.viewport + 1, `No horizontal overflow: ${JSON.stringify(value)}`);
}
async function screenshot(page, directory, name) {
  if (!directory) return;
  await mkdir(directory, { recursive: true });
  await surface(page).evaluate(element => element.scrollTo(0, 0));
  await page.screenshot({ path: path.join(directory, `${name}.png`) });
}
async function theme(page, value) {
  if (await surface(page).getAttribute('data-theme') !== value) await surface(page).getByRole('button', { name: `Switch to ${value} mode` }).click();
  await expect(surface(page)).toHaveAttribute('data-theme', value);
}
async function publicExport(page) {
  const download = page.waitForEvent('download');
  await surface(page).getByRole('button', { name: 'Export my public record', exact: true }).click();
  const artifact = await download;
  return readFile(await artifact.path(), 'utf8');
}
async function allocation(page, budget, leverage, direction) {
  await step(page, 'My allocation').click();
  await surface(page).getByLabel('My total paper budget', { exact: true }).fill(String(budget));
  await surface(page).getByRole('radio', { name: `${leverage}×`, exact: true }).check();
  await surface(page).getByRole('radio', { name: direction === 'long' ? 'Long · price rises' : 'Short · price falls', exact: true }).check();
  await expect(surface(page).getByRole('region', { name: 'My paper allocation quote' })).toBeVisible();
  const consent = surface(page).getByRole('checkbox', { name: /I approve my own paper allocation/ });
  await expect(surface(page).getByRole('button', { name: 'Approve my allocation', exact: true })).toBeDisabled();
  await consent.check();
  await surface(page).getByRole('radio', { name: '1×', exact: true }).check();
  await expect(consent).not.toBeChecked();
  await surface(page).getByRole('radio', { name: `${leverage}×`, exact: true }).check();
  await expect(consent).toBeEnabled();
  await consent.check();
}

/** Identical real UI/server/storage flow for the CI shell and full Expo export. */
export async function runCommunityBrowserFlow({ appDist = path.join(root, 'target/rooms-harness'), artifacts, fullApp = false } = {}) {
  const temporary = await mkdtemp(path.join(tmpdir(), 'sakura-community-ui-'));
  const dbPath = path.join(temporary, 'private', 'rooms.sqlite');
  let server = await startServer({ dbPath, appDist, initialize: true, rateLimit: 1000 });
  const origin = server.origin;
  const admission = await issueAdmission({ dbPath });
  const browser = await chromium.launch({ headless: true, executablePath: process.env.SAKURA_BROWSER_EXECUTABLE || (existsSync('/Applications/Google Chrome.app/Contents/MacOS/Google Chrome') ? '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome' : undefined) });
  const contexts = [];
  try {
    for (const mobile of [false, true]) {
      const context = await browser.newContext({ viewport: mobile ? { width: 390, height: 844 } : { width: 1440, height: 1080 }, isMobile: mobile, hasTouch: mobile, reducedMotion: 'reduce', serviceWorkers: 'block', acceptDownloads: true });
      await context.route('**/*', route => {
        const url = route.request().url();
        return new URL(url).origin === origin || url.startsWith('data:') || url.startsWith('blob:') ? route.continue() : route.abort();
      });
      contexts.push(context);
    }
    const [host, member] = await Promise.all(contexts.map(context => context.newPage()));
    const pageErrors = [];
    for (const page of [host, member]) { page.setDefaultTimeout(25_000); page.on('pageerror', error => pageErrors.push(error.message)); }
    const url = `${server.origin}/app/perps/community`;
    const infoPattern = '**/api/rooms/v1/info';
    await contexts[0].route(infoPattern, route => route.fulfill({ status: 503, contentType: 'application/json', body: JSON.stringify({ error: { code: 'FIXTURE_OUTAGE' } }) }));
    await host.goto(url);
    await expect(surface(host).getByRole('heading', { name: /A shared idea/ })).toBeVisible();
    await expect(surface(host).getByText('Connecting and verifying saved community history…')).toHaveCount(0);
    await expect(surface(host).getByRole('alert')).toBeVisible();
    await expect(surface(host).getByRole('button', { name: 'Publish shared plan', exact: true })).toBeDisabled();
    await contexts[0].unroute(infoPattern);
    await surface(host).getByRole('button', { name: 'Recover saved request', exact: true }).click();
    await expect(surface(host).getByRole('alert')).toHaveCount(0);
    await expect(surface(host).getByText('No community rooms on this device yet.', { exact: true })).toBeVisible();
    await theme(host, 'dark');
    await surface(host).getByLabel('Host admission code', { exact: true }).fill(admission.token);
    await surface(host).getByLabel('Plan title', { exact: true }).fill('A shared recovery');
    await surface(host).getByLabel('Our thesis', { exact: true }).fill('Compare a measured long and short against the fixed SOL scenario.');
    await surface(host).getByLabel('What would change our minds?', { exact: true }).fill('A dip below 94 invites discussion; this note does not place a stop order.');
    await surface(host).getByRole('button', { name: 'Publish shared plan', exact: true }).click();
    await expect(surface(host).getByRole('heading', { name: 'A shared recovery', exact: true })).toBeVisible();
    await expect(surface(host).getByLabel('Plan title', { exact: true })).toHaveCount(0);
    const hostKey = await surface(host).getByTestId('community-public-key').textContent();
    const roomId = await surface(host).getByTestId('community-room-id').textContent();
    await surface(host).getByRole('button', { name: 'Create private invitation', exact: true }).click();
    const invitation = await surface(host).getByLabel('New private invitation', { exact: true }).inputValue();
    const invitationToken = new URLSearchParams(new URL(invitation).hash.slice(1)).get('invite');
    assert.ok(invitationToken);
    await overflow(host);
    await screenshot(host, artifacts, 'desktop-plan');

    await member.goto(invitation);
    await expect(surface(member).getByRole('heading', { name: /A shared idea/ })).toBeVisible();
    await expect(surface(member).getByText('Connecting and verifying saved community history…')).toHaveCount(0);
    assert.equal(new URL(member.url()).hash, '', 'The secret fragment is removed without submitting an invitation');
    await expect(surface(member).getByLabel('Private invitation link', { exact: true })).toHaveValue(invitation);
    await expect(surface(member).getByText('No community rooms on this device yet.', { exact: true })).toBeVisible();
    await expect(surface(member).getByRole('button', { name: 'Join invited room', exact: true })).toBeDisabled();
    await surface(member).getByRole('checkbox', { name: /I want to join this private paper room/ }).check();
    await surface(member).getByRole('button', { name: 'Join invited room', exact: true }).click();
    await expect(surface(member).getByRole('heading', { name: 'A shared recovery', exact: true })).toBeVisible();
    const memberKey = await surface(member).getByTestId('community-public-key').textContent();
    assert.match(hostKey, /^[a-f0-9]{64}$/); assert.match(memberKey, /^[a-f0-9]{64}$/);
    assert.notEqual(hostKey, memberKey, 'Independent browser contexts have separate signing identities');
    assert.equal(await surface(member).getByTestId('community-room-id').textContent(), roomId);
    await expect(surface(member).getByRole('button', { name: 'Create private invitation', exact: true })).toHaveCount(0);
    await expect(surface(member).getByRole('button', { name: /Revoke membership/ })).toHaveCount(0);
    await surface(host).getByRole('button', { name: 'Refresh shared history', exact: true }).click();
    await expect(surface(host).locator('.community-member-list li')).toHaveCount(2);
    await allocation(host, 25, 2, 'long');
    await surface(host).getByRole('button', { name: 'Approve my allocation', exact: true }).click();
    await expect(surface(host).getByRole('button', { name: 'Run my paper scenario', exact: true })).toBeVisible();
    await surface(member).getByRole('button', { name: 'Refresh shared history', exact: true }).click();
    await allocation(member, 10, 3, 'short');
    await theme(member, 'light'); await overflow(member); await screenshot(member, artifacts, 'mobile-allocation');

    // The service commits the allocation, but the acknowledgement never reaches
    // this browser. Recovery must reuse the exact persisted signed request.
    let dropped = false;
    const dropAcknowledgement = async route => {
      const packet = route.request().postDataJSON();
      if (!dropped && packet?.command?.type === 'commit_ledger') {
        dropped = true;
        const response = await route.fetch(); assert.equal(response.status(), 200);
        await route.abort('failed');
      } else await route.continue();
    };
    await contexts[1].route('**/api/rooms/v1/command', dropAcknowledgement);
    await surface(member).getByRole('button', { name: 'Approve my allocation', exact: true }).click();
    await expect(surface(member).getByRole('alert')).toBeVisible();
    await expect(surface(member).getByRole('button', { name: 'Recover pending action', exact: true })).toBeVisible();
    assert.equal(dropped, true);
    await contexts[1].unroute('**/api/rooms/v1/command', dropAcknowledgement);
    await surface(host).getByRole('button', { name: 'Refresh shared history', exact: true }).click();
    await expect(surface(host).locator(`[data-member="${memberKey}"]`)).toContainText('10 credits · short · 3×');
    await expect(surface(host).locator(`[data-member="${memberKey}"]`)).toContainText('Allocation approved');
    const acceptedHead = await surface(host).getByLabel('Shared history head', { exact: true }).textContent();

    const restartPort = server.port, serviceId = server.serviceId;
    await server.close();
    server = null;
    server = await startServer({ dbPath, appDist, port: restartPort, rateLimit: 1000 });
    assert.equal(server.serviceId, serviceId, 'A server restart keeps its service signing identity');
    await member.reload();
    await expect(surface(member).getByRole('button', { name: 'Recover pending action', exact: true })).toBeVisible();
    assert.equal(await surface(member).getByTestId('community-public-key').textContent(), memberKey);
    await surface(member).getByRole('button', { name: 'Recover pending action', exact: true }).click();
    await expect(surface(member).getByRole('button', { name: 'Recover pending action', exact: true })).toHaveCount(0);
    await expect(surface(member).getByRole('alert')).toHaveCount(0);
    await step(member, 'Review').click();
    await expect(surface(member).getByRole('button', { name: 'Run my paper scenario', exact: true })).toBeVisible();
    await expect(surface(member).getByLabel('Shared history head', { exact: true })).toHaveText(acceptedHead);
    await surface(host).getByRole('button', { name: 'Refresh shared history', exact: true }).click();
    await expect(surface(host).getByLabel('Shared history head', { exact: true })).toHaveText(acceptedHead);
    await Promise.all([host, member].map(page => surface(page).getByRole('button', { name: 'Run my paper scenario', exact: true }).click()));
    await expect(surface(host).getByRole('region', { name: 'My verified paper result' })).toContainText('+1.9461');
    await expect(surface(member).getByRole('region', { name: 'My verified paper result' })).toContainText('−1.22631');
    await surface(host).getByRole('button', { name: 'Refresh shared history', exact: true }).click();
    await surface(member).getByRole('button', { name: 'Refresh shared history', exact: true }).click();
    await expect(surface(host).locator(`[data-member="${memberKey}"]`)).toContainText('−1.22631 credits');
    await expect(surface(member).locator(`[data-member="${hostKey}"]`)).toContainText('+1.9461 credits');
    assert.equal(await surface(host).getByLabel('Shared history head', { exact: true }).textContent(), await surface(member).getByLabel('Shared history head', { exact: true }).textContent());
    await theme(host, 'light'); await theme(member, 'dark');
    await overflow(host); await overflow(member);
    await screenshot(host, artifacts, 'desktop-review'); await screenshot(member, artifacts, 'mobile-review');
    const exported = await publicExport(member);
    assert.equal(exported.includes('privateKey'), false);
    assert.equal(exported.includes(admission.token), false);
    assert.equal(exported.includes(invitationToken), false);
    const parsed = JSON.parse(exported);
    assert.equal(parsed.events.filter(event => event.request.command.type === 'commit_ledger' && event.request.actor === memberKey).length, 2, 'Exactly one allocation and one settlement, including after lost acknowledgement');
    await member.setViewportSize({ width: 320, height: 844 }); await overflow(member);

    await surface(host).getByRole('button', { name: `Revoke membership ${memberKey.slice(0, 8)}…${memberKey.slice(-6)}`, exact: true }).click();
    await expect(surface(host).getByRole('region', { name: 'Confirm access revocation' })).toBeVisible();
    await surface(host).getByRole('button', { name: 'Confirm revocation', exact: true }).click();
    await expect(surface(host).locator(`[data-member="${memberKey}"]`)).toContainText('Access revoked');
    await surface(member).getByRole('button', { name: 'Refresh shared history', exact: true }).click();
    await expect(surface(member).getByText('Membership revoked · your record remains available', { exact: true })).toBeVisible();
    await expect(surface(member).locator('.community-member-list li')).toHaveCount(1);
    await expect(surface(member).getByRole('region', { name: 'My verified paper result' })).toContainText('−1.22631');
    assert.equal(JSON.parse(await publicExport(member)).member.publicKey, memberKey);
    await step(member, 'My allocation').click();
    await expect(surface(member).getByRole('button', { name: 'Approve my allocation', exact: true })).toHaveCount(0);
    if (fullApp) {
      await surface(host).locator('.perps-back').click();
      await expect(host.locator('.rooms-page:not(.community-page):visible').getByRole('heading', { name: /Think it through/ })).toBeVisible();
      await host.getByRole('link', { name: 'Explore invite-only Community Rooms', exact: true }).click();
      await expect(surface(host).getByRole('heading', { name: 'A shared recovery', exact: true })).toBeVisible();
    }
    assert.deepEqual(pageErrors, [], 'No unhandled UI errors');
  } catch (error) {
    const alerts = [];
    for (const context of contexts) for (const page of context.pages()) {
      alerts.push(...await surface(page).getByRole('alert').allTextContents().catch(() => []));
    }
    error.message += `\nVisible recovery errors: ${JSON.stringify(alerts)}`;
    throw error;
  } finally {
    await Promise.all(contexts.map(context => context.close())); await browser.close(); await server?.close(); await rm(temporary, { recursive: true, force: true });
  }
}
