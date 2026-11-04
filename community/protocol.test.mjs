import assert from 'node:assert/strict';
import { before, test } from 'node:test';
import { loadEngine } from './engine.mjs';
import { MAX_EVENTS, MAX_REQUEST_AGE_MS, ZERO_HASH, ProtocolError, applyRequest, createEvent, createRejection, credentialHash, hex,
  normalizeCommand, normalizePlan, planHash, publicRequest, randomHex, replayRoom, requestHash, signRequest,
  unhex, verifyEvent, verifyLedger, verifyRejection, verifyRequest } from './protocol.mjs';

let engine;
before(async () => { engine = await loadEngine(); });
const plan = { title: 'A community paper plan', thesis: 'Each member chooses an independent bounded allocation.', invalidation: 'Review the fixed scenario together.' };
async function identity() {
  const pair = await crypto.subtle.generateKey('Ed25519', false, ['sign', 'verify']);
  return { key: pair.privateKey, publicKey: hex(await crypto.subtle.exportKey('raw', pair.publicKey)) };
}
async function fixture() {
  const [service, leader, member, outsider] = await Promise.all(Array.from({ length: 4 }, identity));
  return { service, leader, member, outsider, roomId: randomHex(), events: [], state: null };
}
async function request(f, actor, command, overrides = {}) {
  return signRequest({ version: 1, serviceId: f.service.publicKey, actor: actor.publicKey, requestId: randomHex(),
    issuedAt: 1_000_000, expiresAt: 1_100_000, command: { roomId: f.roomId, ...command }, ...overrides }, actor.key);
}
async function append(f, actor, command) {
  const signed = await request(f, actor, command);
  const candidate = await applyRequest(engine, f.state, signed);
  const event = await createEvent(signed, f.events.length + 1, f.state?.head ?? ZERO_HASH, f.service.key);
  candidate.events.push(event); candidate.head = event.hash;
  f.events.push(event); f.state = candidate;
  return event;
}
async function create(f) {
  return append(f, f.leader, { type: 'create_room', plan, credentialHash: await credentialHash(randomHex()) });
}
async function join(f) {
  const token = randomHex(), inviteHash = await credentialHash(token);
  await append(f, f.leader, { type: 'create_invite', inviteHash });
  await append(f, f.member, { type: 'join_room', credentialHash: inviteHash });
  return { token, inviteHash };
}
async function ledger(f, who, settle = false, budget = '25') {
  const binding = await planHash({ serviceId: f.service.publicKey, roomId: f.roomId, leader: f.leader.publicKey, plan });
  const session = engine.WebRoom.create(who.publicKey, f.roomId, binding);
  async function step(action) {
    const prepared = JSON.parse(session.prepare(JSON.stringify(action)));
    const signature = hex(await crypto.subtle.sign('Ed25519', who.key, unhex(prepared.signing_bytes_hex)));
    session.submit(prepared.intent_hex, signature);
  }
  try {
    for (let i = 0; i < 4; i++) await step({ action: 'initialize' });
    for (let i = 0; i < 2; i++) await step({ action: 'allocate', budget_credits: budget, leverage: '2', side: 'long' });
    if (settle) { await step({ action: 'run_scenario' }); session.settle_scenario(); }
    return { genesis: session.genesis_hex(), records: JSON.parse(session.records_json()), head: JSON.parse(session.checkpoint_json()).head_hash };
  } finally { session.free(); }
}
const rejected = async (run, code) => assert.rejects(run, error => error instanceof ProtocolError && (!code || error.code === code));

test('canonical request binds service, identity, time, command and token hash without publishing the credential', async () => {
  const f = await fixture(); const token = randomHex();
  const signed = await request(f, f.leader, { type: 'create_room', plan, credentialHash: await credentialHash(token) });
  const transmitted = { ...signed, credential: token };
  assert.deepEqual(await verifyRequest(transmitted, { serviceId: f.service.publicKey, now: 1_050_000, requireCredential: true }), signed);
  assert.equal(Object.hasOwn(publicRequest(transmitted), 'credential'), false);
  assert.equal(await requestHash(transmitted), await requestHash(signed));
  await rejected(() => verifyRequest({ ...transmitted, credential: randomHex() }), 'invalid_credential');
  await rejected(() => verifyRequest(signed, { requireCredential: true }), 'invalid_credential');
  await rejected(() => verifyRequest(signed, { serviceId: randomHex() }), 'wrong_service');
  for (const changed of [{ actor: f.outsider.publicKey }, { command: { ...signed.command, roomId: randomHex() } }, { expiresAt: 1_150_000 }, { requestId: randomHex() }]) {
    await rejected(() => verifyRequest({ ...signed, ...changed }), 'invalid_signature');
  }
  await rejected(() => verifyRequest({ ...signed, unexpected: true }));
  await rejected(() => signRequest({ ...signed, signature: undefined }, f.leader.key));
});

test('live request expiry is bounded while old committed records retain verifiable signatures', async () => {
  const f = await fixture();
  const signed = await request(f, f.leader, { type: 'get_room' });
  await rejected(() => verifyRequest(signed, { now: 1_100_001 }), 'request_expired');
  await rejected(() => verifyRequest(signed, { now: 900_000 }), 'request_expired');
  assert.deepEqual(await verifyRequest(signed, { now: 9_000_000, allowExpired: true }), signed);
  assert.deepEqual(await verifyRequest(signed), signed);
  await rejected(() => request(f, f.leader, { type: 'get_room' }, { expiresAt: 1_000_000 + MAX_REQUEST_AGE_MS + 1 }));
});

test('terminal rejection proof binds the exact service, actor, request and reason', async () => {
  const f = await fixture();
  const signed = await request(f, f.leader, { type: 'create_invite', inviteHash: randomHex() });
  const rejection = await createRejection({ version: 1, serviceId: f.service.publicKey, actor: signed.actor,
    requestId: signed.requestId, requestHash: await requestHash(signed), expiresAt: signed.expiresAt, code: 'FORBIDDEN' }, f.service.key);
  assert.deepEqual(await verifyRejection(rejection, f.service.publicKey, signed), rejection);
  for (const changed of [{ code: 'CONFLICT' }, { expiresAt: signed.expiresAt + 1 }, { actor: f.member.publicKey }, { requestId: randomHex() }, { requestHash: randomHex() }, { signature: '00'.repeat(64) }]) {
    await rejected(() => verifyRejection({ ...rejection, ...changed }, f.service.publicKey, signed));
  }
  await rejected(() => verifyRejection(rejection, f.member.publicKey, signed), 'wrong_service');
  const other = await request(f, f.leader, { type: 'create_invite', inviteHash: randomHex() }, { requestId: signed.requestId });
  await rejected(() => verifyRejection(rejection, f.service.publicKey, other), 'invalid_rejection');
  await rejected(() => verifyRejection({ ...rejection, extra: true }, f.service.publicKey, signed));
});

test('separate member keys approve only their own actual Rust allocation and settlement', async () => {
  const f = await fixture(); await create(f); await join(f);
  const memberOpen = await ledger(f, f.member), leaderOpen = await ledger(f, f.leader, false, '10');
  await rejected(() => append(f, f.leader, { type: 'commit_ledger', expectedHead: null, ledger: memberOpen }), 'wrong_ledger_domain');
  await append(f, f.member, { type: 'commit_ledger', expectedHead: null, ledger: memberOpen });
  await append(f, f.leader, { type: 'commit_ledger', expectedHead: null, ledger: leaderOpen });
  const settled = await ledger(f, f.member, true);
  await append(f, f.member, { type: 'commit_ledger', expectedHead: memberOpen.head, ledger: settled });
  const restored = await replayRoom(engine, f.service.publicKey, f.events);
  assert.deepEqual(restored, f.state);
  const binding = await planHash({ serviceId: f.service.publicKey, roomId: f.roomId, leader: f.leader.publicKey, plan });
  const actual = verifyLedger(engine, f.member.publicKey, f.roomId, binding, settled);
  assert.equal(actual.settlement.net_pnl_quote_atoms, '1946100');
  assert.equal(actual.balance_quote_atoms, '26946100');
  assert.equal(restored.members[0].ledger.records.length, 6);
  assert.equal(restored.members[1].ledger.records.length, 8);
});

test('invite use, membership and plan rules are independently replayed, not trusted from server summaries', async () => {
  const f = await fixture(); await create(f); const { inviteHash } = await join(f);
  await rejected(() => append(f, f.outsider, { type: 'join_room', credentialHash: inviteHash }), 'invite_unavailable');
  await rejected(() => append(f, f.member, { type: 'create_invite', inviteHash: randomHex() }), 'forbidden');
  await rejected(() => append(f, f.member, { type: 'revoke_member', member: f.leader.publicKey }), 'forbidden');
  await rejected(() => append(f, f.leader, { type: 'revoke_member', member: f.leader.publicKey }), 'forbidden');
  await rejected(() => append(f, f.leader, { type: 'create_room', plan: { ...plan, title: 'Changed plan' }, credentialHash: randomHex() }), 'invalid_transition');
  const tokenHash = randomHex();
  await append(f, f.leader, { type: 'create_invite', inviteHash: tokenHash });
  await append(f, f.leader, { type: 'revoke_invite', inviteHash: tokenHash });
  await rejected(() => append(f, f.outsider, { type: 'join_room', credentialHash: tokenHash }), 'invite_unavailable');
  await rejected(() => append(f, f.leader, { type: 'create_invite', inviteHash: tokenHash }), 'invite_exists');
});

test('revocation blocks a fresh allocation and rejoining but preserves settlement of an existing own allocation', async () => {
  const f = await fixture(); await create(f); await join(f);
  const open = await ledger(f, f.member); await append(f, f.member, { type: 'commit_ledger', expectedHead: null, ledger: open });
  await append(f, f.leader, { type: 'revoke_member', member: f.member.publicKey });
  await append(f, f.member, { type: 'commit_ledger', expectedHead: open.head, ledger: await ledger(f, f.member, true) });
  await rejected(() => append(f, f.member, { type: 'join_room', credentialHash: randomHex() }), 'already_member');
  const g = await fixture(); await create(g); await join(g);
  await append(g, g.leader, { type: 'revoke_member', member: g.member.publicKey });
  await rejected(async () => append(g, g.member, { type: 'commit_ledger', expectedHead: null, ledger: await ledger(g, g.member) }), 'forbidden');
});

test('same-member stale approvals, allocation substitutions and repeated settlements fail atomically', async () => {
  const f = await fixture(); await create(f); await join(f);
  const open = await ledger(f, f.member); await append(f, f.member, { type: 'commit_ledger', expectedHead: null, ledger: open });
  const before = structuredClone(f.state);
  await rejected(async () => append(f, f.member, { type: 'commit_ledger', expectedHead: null, ledger: await ledger(f, f.member, false, '26') }), 'head_conflict');
  await rejected(async () => append(f, f.member, { type: 'commit_ledger', expectedHead: open.head, ledger: await ledger(f, f.member, true, '26') }), 'ledger_prefix_changed');
  assert.deepEqual(f.state, before);
  const settled = await ledger(f, f.member, true);
  await rejected(() => append(f, f.member, { type: 'commit_ledger', expectedHead: randomHex(), ledger: settled }), 'head_conflict');
  await append(f, f.member, { type: 'commit_ledger', expectedHead: open.head, ledger: settled });
  await rejected(() => append(f, f.member, { type: 'commit_ledger', expectedHead: settled.head, ledger: settled }), 'head_conflict');
});

test('ledger replay rejects plan/service/room substitutions and malformed or changed signed records', async () => {
  const f = await fixture(); await create(f); await join(f);
  const open = await ledger(f, f.member);
  const binding = { serviceId: f.service.publicKey, roomId: f.roomId, leader: f.leader.publicKey, plan };
  for (const changed of [{ serviceId: randomHex() }, { leader: f.outsider.publicKey }, { plan: { ...plan, thesis: 'Other thesis' } }]) {
    assert.throws(() => verifyLedger(engine, f.member.publicKey, f.roomId, '0'.repeat(64), open), ProtocolError);
    const altered = await planHash({ ...binding, ...changed });
    assert.throws(() => verifyLedger(engine, f.member.publicKey, f.roomId, altered, open), ProtocolError);
  }
  const validHash = await planHash(binding);
  assert.throws(() => verifyLedger(engine, f.member.publicKey, randomHex(), validHash, open), ProtocolError);
  const mutations = [
    { ...open, head: randomHex() },
    { ...open, records: open.records.slice(0, 4) },
    { ...open, records: [...open.records.slice(0, 5), `${open.records[5]}00`] },
    { ...open, records: [...open.records].reverse() },
    { ...open, extra: true },
  ];
  for (const changed of mutations) assert.throws(() => verifyLedger(engine, f.member.publicKey, f.roomId, validHash, changed), ProtocolError);
});

test('retained checkpoints reject rollback and a differently signed service fork while valid extensions replay', async () => {
  const f = await fixture(); await create(f); await join(f);
  const checkpoint = { count: f.events.length, head: f.state.head };
  const prefix = structuredClone(f.events);
  await append(f, f.member, { type: 'commit_ledger', expectedHead: null, ledger: await ledger(f, f.member) });
  assert.deepEqual(await replayRoom(engine, f.service.publicKey, f.events, checkpoint), f.state);
  await rejected(() => replayRoom(engine, f.service.publicKey, prefix, { count: f.events.length, head: f.state.head }), 'checkpoint_conflict');
  await rejected(() => replayRoom(engine, f.service.publicKey, f.events, { ...checkpoint, head: randomHex() }), 'checkpoint_conflict');
  // A valid older prefix remains valid with no independently retained head.
  assert.equal((await replayRoom(engine, f.service.publicKey, prefix)).head, checkpoint.head);
});

test('journal rejects mutated actor/service signatures, reordered records, duplicates and credential leakage', async () => {
  const f = await fixture(); await create(f); await join(f);
  const last = f.events.at(-1);
  const changedRequest = structuredClone(last); changedRequest.request.command.credentialHash = randomHex();
  const leaked = structuredClone(last); leaked.request.credential = randomHex();
  for (const altered of [changedRequest, leaked, { ...last, hash: randomHex() }, { ...last, signature: '0'.repeat(128) }]) {
    await rejected(() => replayRoom(engine, f.service.publicKey, [...f.events.slice(0, -1), altered]));
  }
  await rejected(() => verifyEvent(last, f.outsider.publicKey), 'wrong_service');
  await rejected(() => replayRoom(engine, f.service.publicKey, [...f.events].reverse()), 'invalid_history');
  // Even a service-signed duplicate request cannot become a new accepted event.
  const duplicate = await createEvent(last.request, f.events.length + 1, f.state.head, f.service.key);
  await rejected(() => replayRoom(engine, f.service.publicKey, [...f.events, duplicate]), 'duplicate_request');
});

test('bounded strict input rejects unknown fields, ambiguous plans, oversized frames and excessive history', async () => {
  assert.deepEqual(normalizePlan({ ...plan, title: '  Title  ' }), { ...plan, title: 'Title' });
  assert.throws(() => normalizeCommand({ type: 'create_room', roomId: randomHex(), plan: { ...plan, title: ' Title ' }, credentialHash: randomHex() }), ProtocolError);
  for (const input of [{ ...plan, title: 'x'.repeat(81) }, { ...plan, thesis: '\u0000' }, { ...plan, extra: 'x' }]) assert.throws(() => normalizePlan(input), ProtocolError);
  const f = await fixture(); await create(f);
  await rejected(() => replayRoom(engine, f.service.publicKey, Array.from({ length: MAX_EVENTS + 1 }, () => f.events[0])), 'invalid_history');
  await rejected(() => request(f, f.leader, { type: 'commit_ledger', expectedHead: null, ledger: { genesis: 'aa'.repeat(20_000), records: [], head: randomHex() } }));
  await rejected(() => request(f, f.leader, { type: 'get_room', actor: f.outsider.publicKey }));
});
