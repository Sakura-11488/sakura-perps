import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { existsSync } from 'node:fs';
import { before, after, test } from 'node:test';
import { serveRoomsHarness } from './serve.mjs';

// These tests use the packaged browser client and the real Rust/WASM engine.
// Fault injection changes only storage/completion scheduling, never economics.
const require = createRequire(new URL('../integrations/sakura-rooms/package.json', import.meta.url));
const { chromium } = require('playwright');
const DATABASE = 'sakura-rooms-paper-v1';
const STORE = 'rooms';
const plan = { title: 'A measured paper plan', thesis: 'Observe the fixed SOL scenario with a bounded allocation.', invalidation: 'Review the result at the scenario terminal price.' };
const allocation = { budget: 20, leverage: 2, side: 'long' };
let harness;
let browser;

before(async () => {
  harness = await serveRoomsHarness();
  browser = await chromium.launch({
    headless: true,
    executablePath: process.env.SAKURA_BROWSER_EXECUTABLE || (existsSync('/Applications/Google Chrome.app/Contents/MacOS/Google Chrome') ? '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome' : undefined),
  });
});
after(async () => {
  await browser?.close();
  await harness?.close();
});

async function pageFor(context) {
  const page = await context.newPage();
  await page.goto(`${harness.origin}/app/rooms-harness.html`);
  await page.waitForFunction(() => typeof window.roomsClient?.publishPlan === 'function');
  return page;
}
async function isolated(run) {
  const context = await browser.newContext({ serviceWorkers: 'block' });
  try { await run(await pageFor(context), context); }
  finally { await context.close(); }
}
async function publish(page) {
  return page.evaluate(draft => window.roomsClient.publishPlan(draft), plan);
}
async function open(page, room, input = allocation) {
  return page.evaluate(({ room, input }) => window.roomsClient.approveAllocation(room.id, input, room.headHash), { room, input });
}
async function settle(page, room) {
  return page.evaluate(room => window.roomsClient.runScenario(room.id, room.headHash), room);
}
async function saved(page) {
  return page.evaluate(() => window.roomsClient.listRooms());
}
async function exported(page, id) {
  return page.evaluate(id => window.roomsClient.exportRoom(id), id);
}
async function reload(page) {
  await page.reload();
  await page.waitForFunction(() => typeof window.roomsClient?.listRooms === 'function');
}
async function rawRows(page) {
  return page.evaluate(({ database, store }) => new Promise((resolve, reject) => {
    const request = indexedDB.open(database, 1);
    request.onerror = () => reject(request.error);
    request.onsuccess = () => {
      const db = request.result;
      const tx = db.transaction(store, 'readonly');
      const read = tx.objectStore(store).getAll();
      tx.oncomplete = () => {
        db.close();
        resolve(read.result.map(row => {
          const { privateKey, ...publicData } = row;
          return { publicData, hasKey: Object.hasOwn(row, 'privateKey'), key: privateKey ? { type: privateKey.type, extractable: privateKey.extractable, algorithm: privateKey.algorithm?.name, usages: [...privateKey.usages] } : null };
        }));
      };
      tx.onabort = () => { db.close(); reject(tx.error); };
    };
  }), { database: DATABASE, store: STORE });
}
async function mutateStored(page, id, kind) {
  await page.evaluate(async ({ database, store, id, kind }) => {
    const replacement = kind === 'wrong-key' ? (await crypto.subtle.generateKey('Ed25519', false, ['sign', 'verify'])).privateKey : null;
    await new Promise((resolve, reject) => {
      const request = indexedDB.open(database, 1);
      request.onerror = () => reject(request.error);
      request.onsuccess = () => {
        const db = request.result;
        const tx = db.transaction(store, 'readwrite', { durability: 'strict' });
        const objectStore = tx.objectStore(store);
        const get = objectStore.get(id);
        get.onsuccess = () => {
          const row = get.result;
          if (kind === 'missing-key') delete row.privateKey;
          else if (kind === 'wrong-key') row.privateKey = replacement;
          else if (kind === 'corrupt-record') row.records[0] = `${row.records[0].slice(0, -2)}${row.records[0].endsWith('00') ? '01' : '00'}`;
          else if (kind === 'future-engine') row.engine = 'sakura-rooms-paper/999';
          else throw new Error(`Unknown mutation ${kind}`);
          objectStore.put(row);
        };
        tx.oncomplete = () => { db.close(); resolve(); };
        tx.onabort = () => { db.close(); reject(tx.error); };
      };
    });
  }, { database: DATABASE, store: STORE, id, kind });
}
async function abortNextWrite(page, method) {
  await page.evaluate(({ method, store }) => {
    const original = IDBObjectStore.prototype[method];
    IDBObjectStore.prototype[method] = function (...args) {
      const request = original.apply(this, args);
      if (this.name === store) {
        IDBObjectStore.prototype[method] = original;
        const tx = this.transaction;
        request.addEventListener('success', () => tx.abort(), { once: true });
      }
      return request;
    };
  }, { method, store: STORE });
}

test('real engine lifecycle survives reload and public archives contain no signing key', { timeout: 60_000 }, async () => isolated(async page => {
  const ready = await publish(page);
  assert.equal(ready.phase, 'ready');
  assert.equal(ready.receiptCount, 4);
  assert.equal(ready.verified, true);
  let rows = await rawRows(page);
  assert.equal(rows.length, 1);
  assert.deepEqual(rows[0].key, { type: 'private', extractable: false, algorithm: 'Ed25519', usages: ['sign'] });
  assert.equal(await page.evaluate(({ database, store, id }) => new Promise((resolve, reject) => {
    const request = indexedDB.open(database, 1);
    request.onsuccess = () => {
      const db = request.result;
      const get = db.transaction(store).objectStore(store).get(id);
      get.onsuccess = async () => {
        db.close();
        try { await crypto.subtle.exportKey('pkcs8', get.result.privateKey); resolve(false); }
        catch { resolve(true); }
      };
      get.onerror = () => reject(get.error);
    };
  }), { database: DATABASE, store: STORE, id: ready.id }), true);
  await reload(page);
  assert.deepEqual(await saved(page), [ready]);
  const opened = await open(page, ready);
  assert.equal(opened.phase, 'open');
  assert.equal(opened.receiptCount, 6);
  await reload(page);
  assert.deepEqual(await saved(page), [opened]);
  const finished = await settle(page, opened);
  assert.equal(finished.phase, 'settled');
  assert.equal(finished.receiptCount, 8);
  await reload(page);
  assert.deepEqual(await saved(page), [finished]);
  const text = await exported(page, ready.id);
  const archive = JSON.parse(text);
  assert.equal(Object.hasOwn(archive, 'privateKey'), false);
  assert.equal(/privateKey|secretKey|seed|pkcs8/.test(text), false);
  const verified = await page.evaluate(text => window.roomsClient.verifyExport(text), text);
  assert.equal(verified.headHash, finished.headHash);
  assert.equal(verified.writable, false);
  assert.deepEqual(verified.outcome, finished.outcome);
  rows = await rawRows(page);
  assert.equal(rows.length, 1, 'verification is a readonly operation');
  assert.deepEqual(rows[0].publicData, archive);
}));

test('aborting setup after add succeeds saves neither receipts nor identity', { timeout: 60_000 }, async () => isolated(async page => {
  await abortNextWrite(page, 'add');
  await assert.rejects(publish(page), /save|storage|confirm/i);
  assert.deepEqual(await rawRows(page), []);
  await reload(page);
  assert.deepEqual(await saved(page), []);
  const ready = await publish(page);
  assert.equal(ready.receiptCount, 4, 'a new explicitly requested plan starts at the complete setup boundary');
}));

for (const phase of ['allocation', 'settlement']) {
  test(`aborting ${phase} after put succeeds rolls back its entire two-record batch`, { timeout: 60_000 }, async () => isolated(async page => {
    const ready = await publish(page);
    const current = phase === 'allocation' ? ready : await open(page, ready);
    const before = await exported(page, current.id);
    const beforeRows = await rawRows(page);
    await abortNextWrite(page, 'put');
    await assert.rejects(phase === 'allocation' ? open(page, current) : settle(page, current), /save|storage|confirm/i);
    assert.deepEqual(await rawRows(page), beforeRows);
    assert.equal(await exported(page, current.id), before);
    await reload(page);
    assert.deepEqual(await saved(page), [current]);
    const accepted = phase === 'allocation' ? await open(page, current) : await settle(page, current);
    assert.equal(accepted.receiptCount, current.receiptCount + 2);
  }));
}

test('two tabs prepared from the same head commit exactly one allocation', { timeout: 60_000 }, async () => isolated(async (first, context) => {
  const ready = await publish(first);
  const second = await pageFor(context);
  assert.deepEqual(await saved(second), [ready]);
  for (const page of [first, second]) {
    await page.evaluate(() => {
      const original = crypto.subtle.sign.bind(crypto.subtle);
      let intentCount = 0;
      crypto.subtle.sign = async function (algorithm, key, data) {
        const text = new TextDecoder().decode(data);
        if (text.startsWith('SakuraRiskRooms/Intent') && ++intentCount === 2) {
          window.__roomsBarrierWaiting = true;
          await new Promise(resolve => { window.__releaseRoomsBarrier = resolve; });
        }
        return original(algorithm, key, data);
      };
    });
  }
  const attempts = [first, second].map((page, index) => page.evaluate(async ({ ready, index }) => {
    try {
      const room = await window.roomsClient.approveAllocation(ready.id, { budget: index ? 30 : 20, leverage: 2, side: 'long' }, ready.headHash);
      return { ok: true, room };
    } catch (error) { return { ok: false, message: String(error) }; }
  }, { ready, index }));
  await Promise.all([first, second].map(page => page.waitForFunction(() => window.__roomsBarrierWaiting === true)));
  await Promise.all([first, second].map(page => page.evaluate(() => window.__releaseRoomsBarrier())));
  const results = await Promise.all(attempts);
  assert.equal(results.filter(result => result.ok).length, 1);
  assert.match(results.find(result => !result.ok).message, /another tab|changed|latest state/i);
  const winner = results.find(result => result.ok).room;
  assert.equal(winner.receiptCount, 6);
  await reload(first);
  await reload(second);
  assert.deepEqual(await saved(first), [winner]);
  assert.deepEqual(await saved(second), [winner]);
}));

test('commit before lost acknowledgement recovers once and rejects an old retry', { timeout: 60_000 }, async () => isolated(async (page, context) => {
  const ready = await publish(page);
  await page.evaluate(store => {
    const original = IDBDatabase.prototype.transaction;
    IDBDatabase.prototype.transaction = function (...args) {
      const tx = original.apply(this, args);
      if (args[1] === 'readwrite' && (args[0] === store || [...args[0]].includes(store))) {
        IDBDatabase.prototype.transaction = original;
        Object.defineProperty(tx, 'oncomplete', {
          configurable: true,
          set() { tx.addEventListener('complete', () => { window.__roomsDurablyCommitted = true; }, { once: true }); },
        });
      }
      return tx;
    };
  }, STORE);
  await page.evaluate(({ ready, allocation }) => {
    window.__roomsAcknowledged = false;
    void window.roomsClient.approveAllocation(ready.id, allocation, ready.headHash).then(() => { window.__roomsAcknowledged = true; });
  }, { ready, allocation });
  await page.waitForFunction(() => window.__roomsDurablyCommitted === true);
  assert.equal(await page.evaluate(() => window.__roomsAcknowledged), false);
  await page.close();
  const recoveredPage = await pageFor(context);
  const [recovered] = await saved(recoveredPage);
  assert.equal(recovered.phase, 'open');
  assert.equal(recovered.receiptCount, 6);
  const before = await exported(recoveredPage, ready.id);
  await assert.rejects(open(recoveredPage, ready), /changed|another tab|latest state/i);
  assert.equal(await exported(recoveredPage, ready.id), before);
  const finished = await settle(recoveredPage, recovered);
  assert.equal(finished.receiptCount, 8);
}));

test('archives reject altered history and metadata without modifying saved state', { timeout: 60_000 }, async () => isolated(async page => {
  const ready = await publish(page);
  const finished = await settle(page, await open(page, ready));
  const text = await exported(page, finished.id);
  const original = JSON.parse(text);
  const variants = [];
  const variant = (name, change) => { const value = structuredClone(original); change(value); variants.push({ name, value }); };
  variant('truncated history retaining original head', value => { value.records = value.records.slice(0, 6); });
  variant('reordered history', value => { [value.records[0], value.records[1]] = [value.records[1], value.records[0]]; });
  variant('forged receipt bytes', value => { value.records[2] = `${value.records[2].slice(0, -2)}${value.records[2].endsWith('00') ? '01' : '00'}`; });
  variant('changed checkpoint', value => { value.head = '0'.repeat(64); });
  variant('changed public identity', value => { value.publicKey = '0'.repeat(64); });
  variant('changed immutable plan', value => { value.plan.thesis += ' Changed after signing.'; });
  variant('changed creation metadata', value => { value.createdAt = '2025-01-01T00:00:00.000Z'; });
  variant('changed nonce and id', value => { value.nonce = value.id = 'f'.repeat(64); });
  variant('wrong scenario', value => { value.scenario = 'arbitrary-live-market'; });
  variant('unknown engine semantics', value => { value.engine = 'sakura-rooms-paper/999'; });
  variant('private key field', value => { value.privateKey = {}; });
  variant('unknown metadata', value => { value.adminApproved = true; });
  variant('partial batch', value => { value.records = value.records.slice(0, 7); });
  for (const { name, value } of variants) {
    await assert.rejects(page.evaluate(text => window.roomsClient.verifyExport(text), JSON.stringify(value)), undefined, name);
  }
  await assert.rejects(page.evaluate(() => window.roomsClient.verifyExport(' '.repeat(280_001))), /size limit|exceeds/i);
  assert.equal(await exported(page, finished.id), text);
  assert.deepEqual(await saved(page), [finished]);
}));

test('a valid old public checkpoint stays readonly and never rolls back current state', { timeout: 60_000 }, async () => isolated(async page => {
  const ready = await publish(page);
  const oldArchive = await exported(page, ready.id);
  const finished = await settle(page, await open(page, ready));
  const current = await exported(page, ready.id);
  const historical = await page.evaluate(text => window.roomsClient.verifyExport(text), oldArchive);
  assert.equal(historical.phase, 'ready');
  assert.equal(historical.headHash, ready.headHash);
  assert.equal(historical.writable, false);
  assert.equal(await exported(page, ready.id), current);
  assert.deepEqual(await saved(page), [finished]);
}));

test('a public archive on a fresh browser verifies without creating a writable room or identity', { timeout: 60_000 }, async () => isolated(async page => {
  const ready = await publish(page);
  const opened = await open(page, ready);
  const archive = await exported(page, ready.id);
  const otherDevice = await browser.newContext({ serviceWorkers: 'block' });
  try {
    const otherPage = await pageFor(otherDevice);
    const verified = await otherPage.evaluate(text => window.roomsClient.verifyExport(text), archive);
    assert.equal(verified.headHash, opened.headHash);
    assert.equal(verified.writable, false);
    assert.deepEqual(await saved(otherPage), []);
    await assert.rejects(open(otherPage, verified));
    assert.deepEqual(await rawRows(otherPage), []);
  } finally { await otherDevice.close(); }
}));

for (const kind of ['missing-key', 'wrong-key']) {
  test(`${kind} preserves verified public recovery and export but refuses new signatures`, { timeout: 60_000 }, async () => isolated(async page => {
    const ready = await publish(page);
    const original = await exported(page, ready.id);
    await mutateStored(page, ready.id, kind);
    const before = await rawRows(page);
    await reload(page);
    const [recovered] = await saved(page);
    assert.equal(recovered.id, ready.id);
    assert.equal(recovered.headHash, ready.headHash);
    assert.equal(recovered.receiptCount, 4);
    assert.equal(recovered.writable, false);
    assert.equal(typeof recovered.recoveryNotice, 'string');
    assert.equal(await exported(page, ready.id), original);
    await assert.rejects(open(page, recovered), /key|identity|read.only|recovery|verification/i);
    assert.deepEqual(await rawRows(page), before, 'recovery never replaces the key or resets counters');
  }));
}

for (const kind of ['corrupt-record', 'future-engine']) {
  test(`${kind} fails closed without silently resetting the local journal`, { timeout: 60_000 }, async () => isolated(async page => {
    const ready = await publish(page);
    await mutateStored(page, ready.id, kind);
    const before = await rawRows(page);
    await reload(page);
    await assert.rejects(saved(page), /verification|verify|engine|version|saved|preserved/i);
    await assert.rejects(open(page, ready));
    assert.deepEqual(await rawRows(page), before);
  }));
}

test('a browser that cannot confirm strict durability accepts no action', { timeout: 60_000 }, async () => isolated(async page => {
  await page.evaluate(() => {
    const original = IDBDatabase.prototype.transaction;
    IDBDatabase.prototype.transaction = function (...args) {
      if (args[1] === 'readwrite') return original.call(this, args[0], args[1]);
      return original.apply(this, args);
    };
  });
  await assert.rejects(publish(page), /durable|durability|accepted/i);
  assert.deepEqual(await rawRows(page), []);
}));

test('an existing global cannot replace the manifest-pinned engine instance', { timeout: 60_000 }, async () => isolated(async page => {
  await page.evaluate(() => {
    window.__untrustedEngineUsed = false;
    const reused = async () => { window.__untrustedEngineUsed = true; };
    reused.WebRoom = {
      create() { window.__untrustedEngineUsed = true; throw new Error('Old or unrelated engine instance'); },
      restore() { window.__untrustedEngineUsed = true; throw new Error('Old or unrelated engine instance'); },
    };
    reused.preview_allocation = () => { window.__untrustedEngineUsed = true; throw new Error('Old or unrelated engine instance'); };
    window.sakura_rooms_engine = reused;
  });
  const ready = await publish(page);
  assert.equal(ready.receiptCount, 4);
  assert.equal(await page.evaluate(() => window.__untrustedEngineUsed), false);
  assert.deepEqual(await saved(page), [ready]);
}));

test('modified JavaScript is rejected by SRI and a clean retry works', { timeout: 60_000 }, async () => isolated(async page => {
  let intercepted = false;
  const target = /\/app\/rooms-engine\/[^/]+\.js$/;
  await page.route(target, async route => {
    intercepted = true;
    const response = await route.fetch();
    await route.fulfill({ response, body: `${await response.text()}\nwindow.__incorrectEngineScriptExecuted = true;\n` });
  });
  await assert.rejects(publish(page), /engine.*load|engine.*unavailable/i);
  assert.equal(intercepted, true);
  assert.equal(await page.evaluate(() => window.__incorrectEngineScriptExecuted), undefined);
  assert.deepEqual(await saved(page), []);
  await page.unroute(target);
  const ready = await publish(page);
  assert.equal(ready.receiptCount, 4);
  assert.deepEqual(await saved(page), [ready]);
}));

test('valid WASM with the wrong checksum is rejected and the engine retries cleanly', { timeout: 60_000 }, async () => isolated(async page => {
  let intercepted = false;
  const target = /\/app\/rooms-engine\/[^/]+\.wasm$/;
  await page.route(target, async route => {
    intercepted = true;
    const response = await route.fetch();
    // Append a well-formed custom section: this remains valid WebAssembly but
    // does not match the manifest. The checksum, not parser failure, must gate it.
    const changed = Buffer.concat([await response.body(), Buffer.from([0, 3, 1, 120, 0])]);
    assert.equal(WebAssembly.validate(changed), true);
    await route.fulfill({ response, body: changed });
  });
  await assert.rejects(publish(page), /engine.*load|engine.*unavailable/i);
  assert.equal(intercepted, true);
  assert.deepEqual(await saved(page), []);
  await page.unroute(target);
  // The previous load already evaluated the valid JS wrapper. Retrying in the
  // same page must replace it safely and initialize only the verified bytes.
  const ready = await publish(page);
  assert.equal(ready.receiptCount, 4);
  await reload(page);
  assert.deepEqual(await saved(page), [ready]);
}));
