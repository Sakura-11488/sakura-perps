import assert from 'node:assert/strict';
import { fork } from 'node:child_process';
import { once } from 'node:events';
import { request as httpRequest } from 'node:http';
import { mkdtempSync, readFileSync, statSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { DatabaseSync } from 'node:sqlite';
import test from 'node:test';
import { credentialHash, hex, planHash, randomHex, replayRoom, signRequest, unhex, verifyLedger, verifyRejection } from './protocol.mjs';
import { loadEngine } from './engine.mjs';
import { backupDatabase, initializeStore, issueAdmission, startServer } from './server.mjs';

const plan = { title: 'Two independent allocations', thesis: 'Compare bounded choices in the fixed SOL scenario.', invalidation: 'A reflection, not an automated order.' };
const directory = () => mkdtempSync(path.join(tmpdir(), 'sakura-community-test-'));
async function identity() {
  const keys = await crypto.subtle.generateKey('Ed25519', false, ['sign', 'verify']);
  return { key: keys.privateKey, actor: hex(await crypto.subtle.exportKey('raw', keys.publicKey)) };
}
async function packet(service, who, command, overrides = {}) {
  const now = Date.now();
  return signRequest({ version: 1, serviceId: service.serviceId, actor: who.actor, requestId: randomHex(), issuedAt: now, expiresAt: now + 60_000, command, ...overrides }, who.key);
}
async function post(service, value, overrides = {}) {
  const response = await fetch(`${service.origin}/api/rooms/v1/command`, { method: 'POST', headers: { origin: service.origin, 'content-type': 'application/json', ...overrides.headers }, body: overrides.body ?? JSON.stringify(value), redirect: 'error' });
  return { status: response.status, body: await response.json() };
}
async function fixture(run, options = {}) {
  const dir = directory(); const dbPath = path.join(dir, 'rooms.sqlite');
  initializeStore(dbPath);
  let server = await startServer({ dbPath, rateLimit: 1000, ...options });
  try { await run({ dir, dbPath, get server() { return server; }, async restart() { const port = server.port; await server.close(); server = await startServer({ dbPath, port, rateLimit: 1000, ...options }); return server; } }); }
  finally { await server.close(); }
}
async function create(f, who = null) {
  who ??= await identity();
  const admission = await issueAdmission({ dbPath: f.dbPath });
  const roomId = randomHex();
  const request = { ...await packet(f.server, who, { type: 'create_room', roomId, plan, credentialHash: admission.hash }), credential: admission.token };
  const result = await post(f.server, request); assert.equal(result.status, 200);
  return { who, admission, roomId, request, response: result.body };
}
async function invite(f, room) {
  const token = randomHex(); const hash = await credentialHash(token);
  const request = await packet(f.server, room.who, { type: 'create_invite', roomId: room.roomId, inviteHash: hash });
  const result = await post(f.server, request); assert.equal(result.status, 200);
  return { token, hash, request };
}
async function join(f, room, invitation, who = null) {
  who ??= await identity();
  const request = { ...await packet(f.server, who, { type: 'join_room', roomId: room.roomId, credentialHash: invitation.hash }), credential: invitation.token };
  const result = await post(f.server, request); assert.equal(result.status, 200);
  return { who, request, response: result.body };
}
async function ledger(service, room, who, settled = false) {
  const engine = await loadEngine();
  const session = engine.WebRoom.create(who.actor, room.roomId, await planHash({ serviceId: service.serviceId, roomId: room.roomId, leader: room.who.actor, plan }));
  try {
    const sign = async request => {
      const prepared = JSON.parse(session.prepare(JSON.stringify(request)));
      session.submit(prepared.intent_hex, hex(await crypto.subtle.sign('Ed25519', who.key, unhex(prepared.signing_bytes_hex))));
    };
    for (let i = 0; i < 4; i++) await sign({ action: 'initialize' });
    for (let i = 0; i < 2; i++) await sign({ action: 'allocate', budget_credits: '25', leverage: '2', side: 'long' });
    const open = { genesis: session.genesis_hex(), records: JSON.parse(session.records_json()), head: JSON.parse(session.checkpoint_json()).head_hash };
    if (!settled) return open;
    await sign({ action: 'run_scenario' }); session.settle_scenario();
    return { open, final: { genesis: session.genesis_hex(), records: JSON.parse(session.records_json()), head: JSON.parse(session.checkpoint_json()).head_hash } };
  } finally { session.free(); }
}

test('two identities own independent ledgers under one immutable plan and replay after restart', async () => {
  await fixture(async f => {
    const room = await create(f); const invitation = await invite(f, room); const member = await join(f, room, invitation);
    for (const who of [room.who, member.who]) {
      const book = await ledger(f.server, room, who, true);
      let result = await post(f.server, await packet(f.server, who, { type: 'commit_ledger', roomId: room.roomId, expectedHead: null, ledger: book.open }));
      assert.equal(result.status, 200);
      result = await post(f.server, await packet(f.server, who, { type: 'commit_ledger', roomId: room.roomId, expectedHead: book.open.head, ledger: book.final }));
      assert.equal(result.status, 200);
    }
    const before = await post(f.server, await packet(f.server, member.who, { type: 'get_room', roomId: room.roomId }));
    const serviceId = f.server.serviceId;
    await f.restart(); assert.equal(f.server.serviceId, serviceId);
    const after = await post(f.server, await packet(f.server, member.who, { type: 'get_room', roomId: room.roomId }));
    assert.deepEqual(after, before);
    const state = await replayRoom(await loadEngine(), serviceId, after.body.events);
    assert.equal(state.members.length, 2);
    for (const entry of state.members) {
      const summary = verifyLedger(await loadEngine(), entry.publicKey, room.roomId, await planHash({ serviceId, roomId: state.roomId, leader: state.leader, plan: state.plan }), entry.ledger);
      assert.equal(summary.phase, 'settled'); assert.equal(summary.settlement.net_pnl_quote_atoms, '1946100');
    }
    assert.equal(statSync(f.dbPath).mode & 0o077, 0);
    const db = new DatabaseSync(f.dbPath);
    const publicRows = JSON.stringify({ rooms: db.prepare('SELECT * FROM rooms').all(), requests: db.prepare('SELECT * FROM requests').all(), admissions: db.prepare('SELECT * FROM admissions').all() }); db.close();
    assert.equal(publicRows.includes(room.admission.token), false); assert.equal(publicRows.includes(invitation.token), false);
  });
});

test('unsigned, forged, expired and wrong-service commands fail before any history is accepted', async () => {
  await fixture(async f => {
    const who = await identity(); const admission = await issueAdmission({ dbPath: f.dbPath });
    const command = { type: 'create_room', roomId: randomHex(), plan, credentialHash: admission.hash };
    assert.equal((await post(f.server, {})).status, 400);
    const valid = await packet(f.server, who, command);
    const forged = { ...valid, command: { ...command, plan: { ...plan, title: 'Altered' } }, credential: admission.token };
    assert.equal((await post(f.server, forged)).status, 401);
    assert.equal((await post(f.server, { ...await packet(f.server, who, command, { serviceId: randomHex() }), credential: admission.token })).status, 401);
    assert.equal((await post(f.server, { ...await packet(f.server, who, command, { issuedAt: Date.now() - 120_000, expiresAt: Date.now() - 60_000 }), credential: admission.token })).status, 401);
    assert.equal((await post(f.server, { ...valid, credential: randomHex() })).status, 401);
    assert.equal((await post(f.server, valid)).status, 401);
    assert.equal((await post(f.server, { ...valid, credential: admission.token, extra: true })).status, 400);
    const db = new DatabaseSync(f.dbPath); assert.equal(db.prepare('SELECT count(*) AS n FROM rooms').get().n, 0); db.close();
    const permanentlyRejected = await post(f.server, { ...valid, credential: admission.token });
    assert.equal(permanentlyRejected.status, 401);
    await verifyRejection(permanentlyRejected.body.error.rejection, f.server.serviceId, valid);
    assert.equal((await post(f.server, { ...await packet(f.server, who, command), credential: admission.token })).status, 200);
  });
});

test('capability tokens are single-use and a stranger cannot read or administer a room', async () => {
  await fixture(async f => {
    const room = await create(f); const stranger = await identity(); const invitation = await invite(f, room);
    assert.equal((await post(f.server, await packet(f.server, stranger, { type: 'get_room', roomId: room.roomId }))).status, 403);
    assert.equal((await post(f.server, await packet(f.server, stranger, { type: 'create_invite', roomId: room.roomId, inviteHash: randomHex() }))).status, 403);
    const member = await join(f, room, invitation);
    assert.equal((await post(f.server, { ...await packet(f.server, stranger, { type: 'join_room', roomId: room.roomId, credentialHash: invitation.hash }), credential: invitation.token })).status, 409);
    assert.equal((await post(f.server, await packet(f.server, member.who, { type: 'revoke_member', roomId: room.roomId, member: room.who.actor }))).status, 403);
    const newCreation = { ...await packet(f.server, stranger, { type: 'create_room', roomId: randomHex(), plan, credentialHash: room.admission.hash }), credential: room.admission.token };
    assert.equal((await post(f.server, newCreation)).status, 403);
  });
});

test('identical requests recover durable receipts after expiry; reused IDs cannot change commands', async () => {
  let clock = Date.now();
  await fixture(async f => {
    const room = await create(f);
    clock += 600_000;
    const retry = await post(f.server, room.request); assert.equal(retry.status, 200); assert.deepEqual(retry.body, room.response);
    const changed = { ...await packet(f.server, room.who, { ...room.request.command, plan: { ...plan, title: 'Changed after acceptance' } }, { requestId: room.request.requestId, issuedAt: room.request.issuedAt, expiresAt: room.request.expiresAt }), credential: room.admission.token };
    assert.equal((await post(f.server, changed)).status, 409);
    const unaccepted = await packet(f.server, room.who, { type: 'create_invite', roomId: room.roomId, inviteHash: randomHex() });
    assert.equal((await post(f.server, unaccepted)).status, 401);
    await f.restart(); assert.deepEqual((await post(f.server, room.request)).body, room.response);
    const db = new DatabaseSync(f.dbPath); assert.equal(db.prepare('SELECT count(*) AS n FROM requests').get().n, 1); db.close();
  }, { now: () => clock });
});

test('two service instances claiming the same invitation commit exactly one member', async () => {
  await fixture(async f => {
    const room = await create(f); const invitation = await invite(f, room);
    const second = await startServer({ dbPath: f.dbPath, rateLimit: 1000 });
    try {
      const [one, two] = await Promise.all([identity(), identity()]);
      const commands = await Promise.all([one, two].map(who => packet(f.server, who, { type: 'join_room', roomId: room.roomId, credentialHash: invitation.hash })));
      const results = await Promise.all([f.server, second].map((service, index) => post(service, { ...commands[index], credential: invitation.token })));
      assert.deepEqual(results.map(result => result.status).sort(), [200, 409]);
      const state = await post(f.server, await packet(f.server, room.who, { type: 'get_room', roomId: room.roomId }));
      assert.equal(state.body.events.length, 3);
    } finally { await second.close(); }
  });
});

test('a leader cannot substitute a member ledger or allocate on their behalf', async () => {
  await fixture(async f => {
    const room = await create(f); const member = await join(f, room, await invite(f, room));
    const book = await ledger(f.server, room, member.who);
    const forged = await packet(f.server, room.who, { type: 'commit_ledger', roomId: room.roomId, expectedHead: null, ledger: book });
    assert.equal((await post(f.server, forged)).status, 400);
    const accepted = await post(f.server, await packet(f.server, member.who, { type: 'commit_ledger', roomId: room.roomId, expectedHead: null, ledger: book }));
    assert.equal(accepted.status, 200); assert.equal(accepted.body.event.index, 4);
    assert.equal((await post(f.server, await packet(f.server, member.who, { type: 'commit_ledger', roomId: room.roomId, expectedHead: null, ledger: book }))).status, 409);
  });
});

test('revocation blocks new allocations and room reads but preserves own settlement and export', async () => {
  await fixture(async f => {
    const room = await create(f); const member = await join(f, room, await invite(f, room));
    const book = await ledger(f.server, room, member.who, true);
    assert.equal((await post(f.server, await packet(f.server, member.who, { type: 'commit_ledger', roomId: room.roomId, expectedHead: null, ledger: book.open }))).status, 200);
    assert.equal((await post(f.server, await packet(f.server, room.who, { type: 'revoke_member', roomId: room.roomId, member: member.who.actor }))).status, 200);
    assert.equal((await post(f.server, await packet(f.server, member.who, { type: 'get_room', roomId: room.roomId }))).status, 403);
    const settled = await post(f.server, await packet(f.server, member.who, { type: 'commit_ledger', roomId: room.roomId, expectedHead: book.open.head, ledger: book.final }));
    assert.equal(settled.status, 200); assert.equal(settled.body.kind, 'receipt'); assert.equal(settled.body.event.request.command.ledger.records.length, 8); assert.equal(Object.hasOwn(settled.body, 'events'), false);
    const exported = await post(f.server, await packet(f.server, member.who, { type: 'get_my_record', roomId: room.roomId }));
    assert.equal(exported.status, 200); assert.equal(exported.body.kind, 'own'); assert.deepEqual(exported.body.member.ledger, book.final); assert.equal(exported.body.revocation.request.command.member, member.who.actor);
    const unfunded = await join(f, room, await invite(f, room));
    assert.equal((await post(f.server, await packet(f.server, room.who, { type: 'revoke_member', roomId: room.roomId, member: unfunded.who.actor }))).status, 200);
    assert.equal((await post(f.server, await packet(f.server, unfunded.who, { type: 'commit_ledger', roomId: room.roomId, expectedHead: null, ledger: await ledger(f.server, room, unfunded.who) }))).status, 403);
  });
});

for (const phase of ['beforeCommit', 'afterCommit']) test(`SIGKILL ${phase} preserves atomic history and retry idempotency`, { timeout: 30_000 }, async () => {
  const dbPath = path.join(directory(), 'rooms.sqlite'); initializeStore(dbPath); const admission = await issueAdmission({ dbPath });
  const child = fork(fileURLToPath(new URL('./test/child-server.mjs', import.meta.url)), [dbPath, phase], { stdio: ['ignore', 'ignore', 'ignore', 'ipc'] });
  const [service] = await once(child, 'message');
  const who = await identity();
  const request = { ...await packet(service, who, { type: 'create_room', roomId: randomHex(), plan, credentialHash: admission.hash }), credential: admission.token };
  const exit = once(child, 'exit');
  await assert.rejects(post(service, request));
  const [, signal] = await exit; assert.equal(signal, 'SIGKILL');
  const db = new DatabaseSync(dbPath);
  assert.equal(db.prepare('SELECT count(*) AS n FROM rooms').get().n, phase === 'beforeCommit' ? 0 : 1);
  assert.equal(db.prepare('SELECT count(*) AS n FROM requests').get().n, phase === 'beforeCommit' ? 0 : 1);
  assert.equal(db.prepare('SELECT used_by FROM admissions').get().used_by === null, phase === 'beforeCommit');
  db.close();
  const recovered = await startServer({ dbPath });
  try {
    assert.equal(recovered.serviceId, service.serviceId);
    const retry = await post(recovered, request); assert.equal(retry.status, 200); assert.equal(retry.body.event.index, 1);
    assert.deepEqual((await post(recovered, request)).body, retry.body);
  } finally { await recovered.close(); }
});

test('published mutation signatures recover only their receipt, never another member history', async () => {
  await fixture(async f => {
    const room = await create(f); const member = await join(f, room, await invite(f, room));
    const book = await ledger(f.server, room, member.who);
    assert.equal((await post(f.server, await packet(f.server, member.who, { type: 'commit_ledger', roomId: room.roomId, expectedHead: null, ledger: book }))).status, 200);
    // Anyone with a public creation export possesses this signed request, but
    // that evidence must not reveal the later member's plan allocation/history.
    const replayed = await post(f.server, room.request);
    assert.deepEqual(Object.keys(replayed.body).sort(), ['version', 'kind', 'serviceId', 'roomId', 'event'].sort());
    assert.deepEqual(replayed.body.event, room.response.event);
    assert.equal(JSON.stringify(replayed.body).includes(member.who.actor), false);
    const fabricatedRead = { ...room.request, command: { type: 'get_room', roomId: room.roomId } }; delete fabricatedRead.credential;
    assert.equal((await post(f.server, fabricatedRead)).status, 401);
  });
});

test('online SQLite backup restores identity and history; retained newer checkpoints reject old backups', async () => {
  await fixture(async f => {
    const room = await create(f); const backupPath = path.join(f.dir, 'backup.sqlite');
    const result = await backupDatabase({ dbPath: f.dbPath, outputPath: backupPath });
    assert.equal(result.serviceId, f.server.serviceId); assert.equal(statSync(backupPath).mode & 0o077, 0);
    await assert.rejects(backupDatabase({ dbPath: f.dbPath, outputPath: backupPath }), /EEXIST/);
    await invite(f, room);
    const current = await post(f.server, await packet(f.server, room.who, { type: 'get_room', roomId: room.roomId }));
    const checkpoint = { count: current.body.events.length, head: current.body.events.at(-1).hash };
    const restored = await startServer({ dbPath: backupPath });
    try {
      assert.equal(restored.serviceId, f.server.serviceId);
      const old = await post(restored, await packet(restored, room.who, { type: 'get_room', roomId: room.roomId }));
      assert.equal(old.status, 200); assert.equal(old.body.events.length, 1);
      await assert.rejects(replayRoom(await loadEngine(), restored.serviceId, old.body.events, checkpoint), /checkpoint_conflict/);
      assert.deepEqual((await post(restored, room.request)).body, room.response);
    } finally { await restored.close(); }
  });
});

test('corrupted journal or receipt indexes stop service reopening without resetting data', async () => {
  const dir = directory(); const dbPath = path.join(dir, 'rooms.sqlite'); initializeStore(dbPath);
  let server = await startServer({ dbPath });
  const room = await create({ dbPath, server }); await server.close();
  let db = new DatabaseSync(dbPath); const original = db.prepare('SELECT events FROM rooms').get().events; db.prepare('UPDATE rooms SET events=?').run('{bad'); db.close();
  await assert.rejects(startServer({ dbPath }), /STORAGE_VERIFICATION_FAILED/);
  db = new DatabaseSync(dbPath); assert.equal(db.prepare('SELECT events FROM rooms').get().events, '{bad'); db.prepare('UPDATE rooms SET events=?').run(original); db.prepare('UPDATE requests SET head=?').run(randomHex()); db.close();
  await assert.rejects(startServer({ dbPath }), /STORAGE_VERIFICATION_FAILED/);
  assert.ok(room.roomId);
});

test('origin, host, request encoding and streamed size guards reject untrusted transport', async () => {
  await fixture(async f => {
    assert.equal((await post(f.server, {}, { headers: { origin: 'https://attacker.example' } })).status, 403);
    const wrongHost = await new Promise((resolve, reject) => {
      const request = httpRequest(f.server.origin + '/api/rooms/v1/info', { headers: { Host: 'attacker.example' } }, response => { response.resume(); resolve(response.statusCode); });
      request.on('error', reject); request.end();
    });
    assert.equal(wrongHost, 403);
    assert.equal((await post(f.server, {}, { headers: { 'content-encoding': 'gzip' } })).status, 400);
    assert.equal((await post(f.server, {}, { headers: { cookie: 'anything=1' } })).status, 400);
    assert.equal((await post(f.server, {}, { body: Buffer.from([123, 34, 0xff, 34, 58, 49, 125]) })).status, 400);
    assert.equal((await post(f.server, {}, { body: 'x'.repeat(513 * 1024) })).status, 413);
    assert.equal((await fetch(`${f.server.origin}/api/rooms/v1/info?room=leak`)).status, 404);
  });
});

test('bounded rate limiting and explicit storage initialization fail closed', async () => {
  const dbPath = path.join(directory(), 'rooms.sqlite');
  await assert.rejects(startServer({ dbPath }), /INITIALIZATION_REQUIRED/);
  initializeStore(dbPath); assert.throws(() => initializeStore(dbPath), /EEXIST/);
  const outputPath = path.join(path.dirname(dbPath), 'admission.txt');
  const admission = await issueAdmission({ dbPath, outputPath });
  assert.equal(readFileSync(outputPath, 'utf8').trim(), admission.token); assert.equal(statSync(outputPath).mode & 0o077, 0);
  const service = await startServer({ dbPath, rateLimit: 2 });
  try {
    assert.equal((await fetch(service.origin + '/api/rooms/v1/info')).status, 200);
    assert.equal((await fetch(service.origin + '/api/rooms/v1/info')).status, 200);
    assert.equal((await fetch(service.origin + '/api/rooms/v1/info')).status, 429);
  } finally { await service.close(); }
});

test('signed rejection is durable and a rejected join cannot execute after its invitation appears', async () => {
  await fixture(async f => {
    const room = await create(f); const who = await identity(); const token = randomHex(); const hash = await credentialHash(token);
    const rejectedPacket = { ...await packet(f.server, who, { type: 'join_room', roomId: room.roomId, credentialHash: hash }), credential: token };
    const rejected = await post(f.server, rejectedPacket);
    assert.equal(rejected.status, 409);
    await verifyRejection(rejected.body.error.rejection, f.server.serviceId, rejectedPacket);
    assert.equal((await post(f.server, await packet(f.server, room.who, { type: 'create_invite', roomId: room.roomId, inviteHash: hash }))).status, 200);
    await f.restart();
    assert.deepEqual(await post(f.server, rejectedPacket), rejected);
    const fresh = { ...await packet(f.server, who, { type: 'join_room', roomId: room.roomId, credentialHash: hash }), credential: token };
    assert.equal((await post(f.server, fresh)).status, 200);
    const db = new DatabaseSync(f.dbPath); assert.equal(db.prepare('SELECT count(*) AS n FROM rejected_requests').get().n, 1); db.close();
  });
});

test('a post-commit failure yields no signed rejection and exact retry returns the accepted receipt', async () => {
  let fail = true;
  await fixture(async f => {
    const who = await identity(); const admission = await issueAdmission({ dbPath: f.dbPath });
    const request = { ...await packet(f.server, who, { type: 'create_room', roomId: randomHex(), plan, credentialHash: admission.hash }), credential: admission.token };
    const ambiguous = await post(f.server, request); assert.equal(ambiguous.status, 503); assert.equal(Object.hasOwn(ambiguous.body.error, 'rejection'), false);
    fail = false;
    const recovered = await post(f.server, request); assert.equal(recovered.status, 200); assert.equal(recovered.body.kind, 'receipt');
    const db = new DatabaseSync(f.dbPath); assert.equal(db.prepare('SELECT count(*) AS n FROM rejected_requests').get().n, 0); assert.equal(db.prepare('SELECT count(*) AS n FROM requests').get().n, 1); db.close();
  }, { testHooks: { afterCommit: () => { if (fail) throw new Error('Injected lost reply'); } } });
});

test('pruned rejections cannot execute after a backwards clock and future packets cannot pin tombstones', async () => {
  const initial = Date.now(); let clock = initial;
  await fixture(async f => {
    const room = await create(f); const who = await identity(); const token = randomHex(); const hash = await credentialHash(token);
    const rejectedPacket = { ...await packet(f.server, who, { type: 'join_room', roomId: room.roomId, credentialHash: hash }), credential: token };
    assert.equal((await post(f.server, rejectedPacket)).status, 409);
    clock = initial + 120_000;
    const clockFields = { issuedAt: clock, expiresAt: clock + 60_000 };
    assert.equal((await post(f.server, await packet(f.server, room.who, { type: 'create_invite', roomId: room.roomId, inviteHash: hash }, clockFields))).status, 200);
    assert.equal((await post(f.server, await packet(f.server, who, { type: 'create_invite', roomId: room.roomId, inviteHash: randomHex() }, clockFields))).status, 403);
    let db = new DatabaseSync(f.dbPath);
    assert.equal(db.prepare('SELECT count(*) AS n FROM rejected_requests WHERE request_id=?').get(rejectedPacket.requestId).n, 0); db.close();
    clock = initial;
    const expired = await post(f.server, rejectedPacket);
    assert.equal(expired.status, 401); await verifyRejection(expired.body.error.rejection, f.server.serviceId, rejectedPacket);
    const future = await packet(f.server, who, { type: 'create_invite', roomId: room.roomId, inviteHash: randomHex() }, { issuedAt: initial + 10 ** 10, expiresAt: initial + 10 ** 10 + 60_000 });
    const refused = await post(f.server, future); assert.equal(refused.status, 401); assert.equal(Object.hasOwn(refused.body.error, 'rejection'), false);
    db = new DatabaseSync(f.dbPath); assert.equal(db.prepare('SELECT count(*) AS n FROM rejected_requests WHERE request_id=?').get(future.requestId).n, 0); assert.equal(db.prepare('SELECT event_count FROM rooms').get().event_count, 2); db.close();
  }, { now: () => clock });
});

test('altering a tombstone expiry fails startup verification against its signed proof', async () => {
  const dbPath = path.join(directory(), 'rooms.sqlite'); initializeStore(dbPath);
  const server = await startServer({ dbPath }); const room = await create({ dbPath, server }); const who = await identity();
  assert.equal((await post(server, await packet(server, who, { type: 'create_invite', roomId: room.roomId, inviteHash: randomHex() }))).status, 403);
  await server.close();
  const db = new DatabaseSync(dbPath); db.exec('UPDATE rejected_requests SET expires_at=expires_at-1'); db.close();
  await assert.rejects(startServer({ dbPath }), /STORAGE_VERIFICATION_FAILED/);
});
