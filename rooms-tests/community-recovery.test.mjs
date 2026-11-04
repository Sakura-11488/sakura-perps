import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { existsSync, mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { before, after, test } from 'node:test';
import { DatabaseSync } from 'node:sqlite';
import { initializeStore, issueAdmission, startServer } from '../community/server.mjs';
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const require = createRequire(new URL('../integrations/sakura-rooms/package.json', import.meta.url));
const { chromium } = require('@playwright/test');
const DATABASE = 'sakura-rooms-community-v1';
const plan = { title: 'Independent community choices', thesis: 'Compare personal limits in the known paper scenario.', invalidation: 'Review the result; this note is not an automatic stop.' };
let browser;
before(async () => { browser = await chromium.launch({ headless: true,
  executablePath: process.env.SAKURA_BROWSER_EXECUTABLE || (existsSync('/Applications/Google Chrome.app/Contents/MacOS/Google Chrome') ? '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome' : undefined) }); });
after(async () => { await browser?.close(); });
async function fixture(run, options = {}) {
  const directory = mkdtempSync(path.join(tmpdir(), 'sakura-community-browser-'));
  const dbPath = path.join(directory, 'rooms.sqlite'); initializeStore(dbPath);
  const config = { dbPath, appDist: path.join(root, 'target/rooms-harness'), rateLimit: 1000, ...options };
  let server = await startServer(config);
  const contexts = [];
  const f = { dbPath, get server() { return server; },
    async restart() { const port = server.port; await server.close(); server = null; server = await startServer({ ...config, port }); },
    async actor() {
      const context = await browser.newContext({ serviceWorkers: 'block' }); contexts.push(context);
      const page = await context.newPage(); await open(page, server.origin);
      const session = await page.evaluate(() => window.communityClient.connectCommunity());
      return { context, page, session };
    },
  };
  try { await run(f); }
  finally { for (const context of contexts) await context.close(); await server?.close(); }
}
async function open(page, origin) { await page.goto(`${origin}/app/community-client.html`); await page.waitForFunction(() => typeof window.communityClient?.connectCommunity === 'function'); }
async function publish(f, actor) {
  const { token } = await issueAdmission({ dbPath: f.dbPath });
  return actor.page.evaluate(({ plan, token }) => window.communityClient.createCommunity(plan, token), { plan, token });
}
async function approve(actor, room, budget = 25) {
  return actor.page.evaluate(({ room, budget }) => window.communityClient.approveCommunityAllocation(room.roomId, { budget, leverage: 2, side: 'long' }, room.head), { room, budget });
}
async function profile(page) {
  return page.evaluate(database => new Promise((resolve, reject) => {
    const opening = indexedDB.open(database, 1); opening.onerror = () => reject(opening.error);
    opening.onsuccess = () => { const db = opening.result, tx = db.transaction('profile', 'readonly'), read = tx.objectStore('profile').get('identity');
      tx.oncomplete = () => { db.close(); const { privateKey, ...publicData } = read.result; resolve({ ...publicData, keyAvailable: !!privateKey, extractable: privateKey?.extractable }); };
      tx.onabort = () => { db.close(); reject(tx.error); }; };
  }), DATABASE);
}
async function replaceKey(page) {
  await page.evaluate(async database => {
    const { privateKey } = await crypto.subtle.generateKey('Ed25519', false, ['sign', 'verify']);
    await new Promise((resolve, reject) => {
      const opening = indexedDB.open(database, 1); opening.onerror = () => reject(opening.error);
      opening.onsuccess = () => { const db = opening.result, tx = db.transaction('profile', 'readwrite', { durability: 'strict' }), store = tx.objectStore('profile'), read = store.get('identity');
        read.onsuccess = () => store.put({ ...read.result, privateKey });
        tx.oncomplete = () => { db.close(); resolve(); }; tx.onabort = () => { db.close(); reject(tx.error); }; };
    });
  }, DATABASE);
}
function events(f, roomId) {
  const db = new DatabaseSync(f.dbPath);
  try { return JSON.parse(db.prepare('SELECT events FROM rooms WHERE id=?').get(roomId).events); }
  finally { db.close(); }
}

test('two browser identities complete one shared room and retain exact independent Rust results after service restart', { timeout: 60_000 }, async () => {
  await fixture(async f => {
    const leader = await f.actor(), member = await f.actor();
    assert.notEqual(leader.session.publicKey, member.session.publicKey);
    const room = await publish(f, leader);
    const invitation = await leader.page.evaluate(id => window.communityClient.createCommunityInvite(id), room.roomId);
    const joined = await member.page.evaluate(link => window.communityClient.joinCommunity(link), invitation.inviteUrl);
    assert.equal(joined.members.length, 2);
    const memberOpen = await approve(member, joined);
    const leaderFresh = await leader.page.evaluate(id => window.communityClient.refreshCommunity(id), room.roomId);
    const leaderOpen = await approve(leader, leaderFresh, 10);
    assert.equal(memberOpen.members.find(value => value.publicKey === member.session.publicKey).quote.entryFee, '49900');
    assert.equal(leaderOpen.members.find(value => value.publicKey === leader.session.publicKey).quote.entryFee, '19960');
    await f.restart();
    await member.page.reload();
    const restored = await member.page.evaluate(() => window.communityClient.connectCommunity());
    assert.equal(restored.publicKey, member.session.publicKey);
    const latest = await member.page.evaluate(id => window.communityClient.refreshCommunity(id), room.roomId);
    const settled = await member.page.evaluate(room => window.communityClient.runCommunityScenario(room.roomId, room.head), latest);
    const own = settled.members.find(value => value.publicKey === member.session.publicKey);
    assert.equal(own.outcome.netPnl, '1946100'); assert.equal(own.outcome.endingBalance, '26946100');
    assert.equal(settled.members.find(value => value.publicKey === leader.session.publicKey).phase, 'open');
    const exported = JSON.parse(await member.page.evaluate(id => window.communityClient.exportCommunityRecord(id), room.roomId));
    assert.equal(Object.hasOwn(exported, 'privateKey'), false);
    assert.equal(JSON.stringify(exported).includes(invitation.inviteUrl.split('invite=')[1]), false);
    assert.equal((await profile(member.page)).extractable, false);
  });
});

test('committed allocation with lost HTTP acknowledgement retries the same durable request after server restart exactly once', { timeout: 60_000 }, async () => {
  let loseAcknowledgement = false;
  await fixture(async f => {
    const actor = await f.actor(), room = await publish(f, actor);
    loseAcknowledgement = true;
    await assert.rejects(() => approve(actor, room));
    const pending = await profile(actor.page);
    assert.equal(pending.pending.packet.command.type, 'commit_ledger');
    const requestId = pending.pending.packet.requestId;
    assert.equal(events(f, room.roomId).filter(event => event.request.requestId === requestId).length, 1);
    await f.restart(); await actor.page.reload();
    assert.equal((await actor.page.evaluate(() => window.communityClient.connectCommunity())).pending, true);
    const recovered = await actor.page.evaluate(() => window.communityClient.recoverCommunityPending());
    assert.equal(recovered.pending, false);
    assert.equal(recovered.rooms[0].members[0].phase, 'open');
    assert.equal(events(f, room.roomId).filter(event => event.request.requestId === requestId).length, 1);
    assert.equal((await profile(actor.page)).pending, null);
  }, { testHooks: { afterCommit({ request }) { if (loseAcknowledgement && request.command.type === 'commit_ledger') { loseAcknowledgement = false; throw new Error('Test lost acknowledgement'); } } } });
});

test('an aborted IndexedDB pending save sends no mutation and consumes no admission', { timeout: 60_000 }, async () => {
  await fixture(async f => {
    const actor = await f.actor(); let commands = 0;
    actor.page.on('request', request => { if (request.url().endsWith('/api/rooms/v1/command/')) commands++; });
    const original = await profile(actor.page);
    await actor.page.evaluate(() => {
      const put = IDBObjectStore.prototype.put;
      IDBObjectStore.prototype.put = function (...args) {
        const request = put.apply(this, args);
        if (this.name === 'profile') { IDBObjectStore.prototype.put = put; request.addEventListener('success', () => this.transaction.abort()); }
        return request;
      };
    });
    const { token } = await issueAdmission({ dbPath: f.dbPath });
    await assert.rejects(() => actor.page.evaluate(({ plan, token }) => window.communityClient.createCommunity(plan, token), { plan, token }));
    assert.equal(commands, 0); assert.deepEqual(await profile(actor.page), original);
    const room = await actor.page.evaluate(({ plan, token }) => window.communityClient.createCommunity(plan, token), { plan, token });
    assert.equal(room.members.length, 1); assert.equal(commands > 0, true);
  });
});

test('a mismatched local signing key keeps verified cached history exportable and blocks new signatures', { timeout: 60_000 }, async () => {
  await fixture(async f => {
    const actor = await f.actor(), room = await publish(f, actor), opened = await approve(actor, room);
    const original = await actor.page.evaluate(id => window.communityClient.exportCommunityRecord(id), room.roomId);
    await replaceKey(actor.page); await actor.page.reload();
    const session = await actor.page.evaluate(() => window.communityClient.connectCommunity());
    assert.equal(session.writable, false); assert.equal(session.publicKey, actor.session.publicKey);
    assert.equal(await actor.page.evaluate(id => window.communityClient.exportCommunityRecord(id), room.roomId), original);
    await assert.rejects(() => actor.page.evaluate(room => window.communityClient.runCommunityScenario(room.roomId, room.head), opened), /read-only/i);
    assert.equal(events(f, room.roomId).filter(event => event.request.command.type === 'commit_ledger').length, 1);
  });
});

test('service identity change and valid historical rollback fail closed without replacing pinned local history', { timeout: 60_000 }, async () => {
  await fixture(async f => {
    const actor = await f.actor(), room = await publish(f, actor);
    const historical = { version: 1, serviceId: f.server.serviceId, roomId: room.roomId, events: events(f, room.roomId) };
    await approve(actor, room);
    const original = await profile(actor.page);
    await actor.context.route('**/api/rooms/v1/command/', route => route.fulfill({ contentType: 'application/json', body: JSON.stringify(historical) }));
    await assert.rejects(() => actor.page.evaluate(id => window.communityClient.refreshCommunity(id), room.roomId));
    assert.deepEqual(await profile(actor.page), original);
    await actor.context.unroute('**/api/rooms/v1/command/');
    await actor.context.route('**/api/rooms/v1/info/', route => route.fulfill({ contentType: 'application/json', body: JSON.stringify({ version: 1, serviceId: '12'.repeat(32) }) }));
    await assert.rejects(() => actor.page.evaluate(() => window.communityClient.connectCommunity()), /verification/i);
    assert.deepEqual(await profile(actor.page), original);
  });
});

test('competing tabs cannot replace another tab\'s durable prepared mutation', { timeout: 60_000 }, async () => {
  await fixture(async f => {
    const actor = await f.actor(), room = await publish(f, actor);
    const second = await actor.context.newPage(); await open(second, f.server.origin);
    await second.evaluate(() => window.communityClient.connectCommunity());
    let intercepted;
    const blocked = new Promise(resolve => { intercepted = resolve; });
    await actor.context.route('**/api/rooms/v1/command/', async route => {
      if (route.request().postDataJSON()?.command.type === 'create_invite') { intercepted(); await route.abort('failed'); }
      else await route.continue();
    });
    await assert.rejects(() => actor.page.evaluate(id => window.communityClient.createCommunityInvite(id), room.roomId));
    await blocked;
    const pending = await profile(actor.page);
    await assert.rejects(() => second.evaluate(id => window.communityClient.createCommunityInvite(id), room.roomId), /pending|Recover/i);
    assert.deepEqual(await profile(second), pending);
    await actor.context.unroute('**/api/rooms/v1/command/');
    const restored = await second.evaluate(() => window.communityClient.recoverCommunityPending());
    assert.equal(restored.pending, false); assert.match(restored.inviteUrl, /#room=/);
    assert.equal(events(f, room.roomId).filter(event => event.request.command.type === 'create_invite').length, 1);
  });
});

test('simultaneous tabs starting from one profile revision admit exactly one prepared mutation', { timeout: 60_000 }, async () => {
  await fixture(async f => {
    const actor = await f.actor(), room = await publish(f, actor);
    const second = await actor.context.newPage(); await open(second, f.server.origin);
    await second.evaluate(() => window.communityClient.connectCommunity());
    // Both callers read the same profile before their service-identity checks.
    // Release those checks together so the actual strict IDB CAS decides.
    let arrived = 0, release;
    const barrier = new Promise(resolve => { release = resolve; });
    await actor.context.route('**/api/rooms/v1/info/', async route => {
      if (arrived < 2) { arrived++; if (arrived === 2) release(); await barrier; }
      await route.continue();
    });
    const results = await Promise.allSettled([actor.page, second].map(page => page.evaluate(id => window.communityClient.createCommunityInvite(id), room.roomId)));
    assert.equal(results.filter(result => result.status === 'fulfilled').length, 1);
    assert.equal(results.filter(result => result.status === 'rejected').length, 1);
    assert.match(results.find(result => result.status === 'rejected').reason.message, /Another tab|pending|Recover/i);
    assert.equal(events(f, room.roomId).filter(event => event.request.command.type === 'create_invite').length, 1);
    assert.equal((await profile(actor.page)).pending, null);
  });
});

test('an unsigned proxy 4xx cannot erase a committed pending request; only its exact receipt recovers it', { timeout: 60_000 }, async () => {
  await fixture(async f => {
    const actor = await f.actor(), room = await publish(f, actor);
    await actor.context.route('**/api/rooms/v1/command/', async route => {
      if (route.request().postDataJSON()?.command.type === 'create_invite') {
        const actual = await route.fetch(); assert.equal(actual.status(), 200);
        await route.fulfill({ status: 403, contentType: 'application/json', body: JSON.stringify({ error: { code: 'FORBIDDEN' } }) });
      } else await route.continue();
    });
    await assert.rejects(() => actor.page.evaluate(id => window.communityClient.createCommunityInvite(id), room.roomId));
    const pending = await profile(actor.page); assert.equal(pending.pending.packet.command.type, 'create_invite');
    const requestId = pending.pending.packet.requestId;
    assert.equal(events(f, room.roomId).filter(event => event.request.requestId === requestId).length, 1);
    await actor.context.unroute('**/api/rooms/v1/command/');
    await f.restart();
    const recovered = await actor.page.evaluate(() => window.communityClient.recoverCommunityPending());
    assert.equal(recovered.pending, false); assert.match(recovered.inviteUrl, /#room=/);
    assert.equal(events(f, room.roomId).filter(event => event.request.requestId === requestId).length, 1);
  });
});

test('a durable service-signed terminal rejection clears only its matching unaccepted pending request', { timeout: 60_000 }, async () => {
  await fixture(async f => {
    const leader = await f.actor(), outsider = await f.actor(), room = await publish(f, leader);
    await assert.rejects(() => outsider.page.evaluate(id => window.communityClient.createCommunityInvite(id), room.roomId));
    assert.equal((await profile(outsider.page)).pending, null);
    assert.equal((await outsider.page.evaluate(() => window.communityClient.connectCommunity())).pending, false);
    assert.equal(events(f, room.roomId).length, 1);
    const db = new DatabaseSync(f.dbPath);
    try { assert.equal(db.prepare('SELECT count(*) AS n FROM rejected_requests WHERE actor=?').get(outsider.session.publicKey).n, 1); }
    finally { db.close(); }
  });
});

test('a signed own-record revocation pins a stronger checkpoint than old shared history and still permits own settlement', { timeout: 60_000 }, async () => {
  await fixture(async f => {
    const leader = await f.actor(), member = await f.actor(), room = await publish(f, leader);
    const invitation = await leader.page.evaluate(id => window.communityClient.createCommunityInvite(id), room.roomId);
    const joined = await member.page.evaluate(link => window.communityClient.joinCommunity(link), invitation.inviteUrl);
    await approve(member, joined);
    const historical = { version: 1, serviceId: f.server.serviceId, roomId: room.roomId, events: events(f, room.roomId) };
    await leader.page.evaluate(id => window.communityClient.refreshCommunity(id), room.roomId);
    await leader.page.evaluate(({ id, member }) => window.communityClient.revokeCommunityMember(id, member), { id: room.roomId, member: member.session.publicKey });
    const revokedView = await member.page.evaluate(id => window.communityClient.refreshCommunity(id), room.roomId);
    assert.equal(revokedView.ownOnly, true);
    assert.equal(revokedView.members.find(item => item.publicKey === member.session.publicKey).status, 'revoked');
    const preserved = await profile(member.page);
    assert.ok(preserved.checkpoints[room.roomId].some(point => point.count > historical.events.length));
    await member.context.route('**/api/rooms/v1/command/', route => route.fulfill({ contentType: 'application/json', body: JSON.stringify(historical) }));
    await assert.rejects(() => member.page.evaluate(id => window.communityClient.refreshCommunity(id), room.roomId));
    assert.deepEqual(await profile(member.page), preserved);
    await member.context.unroute('**/api/rooms/v1/command/');
    const settled = await member.page.evaluate(room => window.communityClient.runCommunityScenario(room.roomId, room.head), revokedView);
    assert.equal(settled.members.find(item => item.publicKey === member.session.publicKey).phase, 'settled');
    const ownExport = JSON.parse(await member.page.evaluate(id => window.communityClient.exportCommunityRecord(id), room.roomId));
    assert.equal(ownExport.kind, 'own'); assert.equal(ownExport.member.ledger.records.length, 8);
    assert.equal(Object.hasOwn(ownExport, 'events'), false);
  });
});

test('a joined member revoked before its first history download recovers the accepted join into an own-only view', { timeout: 60_000 }, async () => {
  let loseJoinAcknowledgement = false;
  await fixture(async f => {
    const leader = await f.actor(), member = await f.actor(), room = await publish(f, leader);
    const invitation = await leader.page.evaluate(id => window.communityClient.createCommunityInvite(id), room.roomId);
    loseJoinAcknowledgement = true;
    await assert.rejects(() => member.page.evaluate(link => window.communityClient.joinCommunity(link), invitation.inviteUrl));
    const before = await profile(member.page);
    assert.equal(Object.keys(before.rooms).length, 0);
    assert.equal(before.pending.packet.command.type, 'join_room');
    await leader.page.evaluate(id => window.communityClient.refreshCommunity(id), room.roomId);
    await leader.page.evaluate(({ id, member }) => window.communityClient.revokeCommunityMember(id, member), { id: room.roomId, member: member.session.publicKey });
    await f.restart(); await member.page.reload();
    const recovered = await member.page.evaluate(() => window.communityClient.recoverCommunityPending());
    assert.equal(recovered.pending, false); assert.equal(recovered.rooms.length, 1);
    const own = recovered.rooms[0];
    assert.equal(own.ownOnly, true); assert.deepEqual(own.plan, plan);
    assert.equal(own.members.find(item => item.publicKey === member.session.publicKey).status, 'revoked');
    const exported = JSON.parse(await member.page.evaluate(id => window.communityClient.exportCommunityRecord(id), room.roomId));
    assert.equal(exported.kind, 'own'); assert.equal(exported.member.publicKey, member.session.publicKey);
    assert.equal(exported.member.ledger, null);
    assert.equal(events(f, room.roomId).filter(event => event.request.command.type === 'join_room').length, 1);
  }, { testHooks: { afterCommit({ request }) { if (loseJoinAcknowledgement && request.command.type === 'join_room') { loseJoinAcknowledgement = false; throw new Error('Test lost join acknowledgement'); } } } });
});
