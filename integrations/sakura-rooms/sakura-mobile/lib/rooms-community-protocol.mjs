// Shared browser/server protocol. No private keys, HTTP, storage, or alternate
// accounting engine live here. The caller supplies the pinned Rust WASM facade.
export const VERSION = 1;
export const ZERO_HASH = '0'.repeat(64);
export const MAX_MEMBERS = 8;
export const MAX_INVITES = 64;
export const MAX_EVENTS = 256;
export const MAX_REQUEST_BYTES = 320 * 1024;
export const MAX_HISTORY_BYTES = 4 * 1024 * 1024;
export const MAX_REQUEST_AGE_MS = 5 * 60 * 1000;
const encoder = new TextEncoder();
const HASH = /^[0-9a-f]{64}$/;
const SIGNATURE = /^[0-9a-f]{128}$/;
const REQUEST_DOMAIN = 'SakuraRoomsCommunity/Request/v1\0';
const EVENT_DOMAIN = 'SakuraRoomsCommunity/Event/v1\0';
const EVENT_SIGNATURE_DOMAIN = 'SakuraRoomsCommunity/EventSignature/v1\0';
const PLAN_DOMAIN = 'SakuraRoomsCommunity/Plan/v1\0';
const REJECTION_DOMAIN = 'SakuraRoomsCommunity/Rejection/v1\0';
const REQUEST_FIELDS = ['version', 'serviceId', 'actor', 'requestId', 'issuedAt', 'expiresAt', 'command'];

export class ProtocolError extends Error {
  constructor(code = 'invalid_request') { super(code); this.name = 'ProtocolError'; this.code = code; }
}
function fail(code = 'invalid_request') { throw new ProtocolError(code); }
function object(value) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) fail();
  const prototype = Object.getPrototypeOf(value);
  if (prototype !== Object.prototype && prototype !== null) fail();
  return value;
}
function keys(value, allowed) {
  object(value);
  if (Object.keys(value).length !== allowed.length || allowed.some(key => !Object.hasOwn(value, key))) fail();
}
function hash(value) { if (typeof value !== 'string' || !HASH.test(value)) fail(); return value; }
function signature(value) { if (typeof value !== 'string' || !SIGNATURE.test(value)) fail(); return value; }
function boundedJSON(value, max) {
  let text;
  try { text = JSON.stringify(value); } catch { fail(); }
  if (typeof text !== 'string' || encoder.encode(text).length > max) fail('limit_exceeded');
  return text;
}
export function hex(value) { return [...(value instanceof Uint8Array ? value : new Uint8Array(value))].map(byte => byte.toString(16).padStart(2, '0')).join(''); }
export function unhex(value) {
  if (typeof value !== 'string' || value.length > 32_768 || !/^(?:[0-9a-f]{2})+$/.test(value)) fail();
  return Uint8Array.from(value.match(/../g).map(pair => parseInt(pair, 16)));
}
export function randomHex() { return hex(crypto.getRandomValues(new Uint8Array(32))); }
export async function sha256(value) { return hex(await crypto.subtle.digest('SHA-256', typeof value === 'string' ? encoder.encode(value) : value)); }
export async function credentialHash(token) { return sha256(unhex(hash(token))); }

export function normalizePlan(value) {
  keys(value, ['title', 'thesis', 'invalidation']);
  const result = {};
  for (const [name, limit] of [['title', 80], ['thesis', 1000], ['invalidation', 500]]) {
    const text = value[name];
    if (typeof text !== 'string' || text.length > limit || /[\u0000-\u0008\u000b\u000c\u000e-\u001f]/.test(text)) fail('invalid_plan');
    result[name] = text.normalize('NFC').trim();
    if (!result[name] || result[name].length > limit) fail('invalid_plan');
  }
  return result;
}

export function normalizeLedger(value) {
  keys(value, ['genesis', 'records', 'head']);
  unhex(value.genesis);
  if (!Array.isArray(value.records) || ![6, 8].includes(value.records.length)) fail('invalid_ledger');
  for (const record of value.records) unhex(record);
  return { genesis: value.genesis, records: [...value.records], head: hash(value.head) };
}

export function normalizeCommand(value) {
  object(value);
  const roomId = hash(value.roomId);
  switch (value.type) {
    case 'create_room': {
      keys(value, ['type', 'roomId', 'plan', 'credentialHash']);
      const plan = normalizePlan(value.plan);
      // Plans in signed commands must already be canonical. Changing Unicode
      // normalization or whitespace after signing would create two meanings.
      if (Object.entries(plan).some(([key, text]) => value.plan[key] !== text)) fail('invalid_plan');
      return { type: value.type, roomId, plan, credentialHash: hash(value.credentialHash) };
    }
    case 'create_invite': case 'revoke_invite':
      keys(value, ['type', 'roomId', 'inviteHash']);
      return { type: value.type, roomId, inviteHash: hash(value.inviteHash) };
    case 'join_room':
      keys(value, ['type', 'roomId', 'credentialHash']);
      return { type: value.type, roomId, credentialHash: hash(value.credentialHash) };
    case 'revoke_member':
      keys(value, ['type', 'roomId', 'member']);
      return { type: value.type, roomId, member: hash(value.member) };
    case 'get_room': case 'get_my_record':
      keys(value, ['type', 'roomId']);
      return { type: value.type, roomId };
    case 'commit_ledger':
      keys(value, ['type', 'roomId', 'expectedHead', 'ledger']);
      return { type: value.type, roomId, expectedHead: value.expectedHead === null ? null : hash(value.expectedHead), ledger: normalizeLedger(value.ledger) };
    default: fail('unknown_command');
  }
}

function fields(value, allowed = REQUEST_FIELDS) {
  keys(value, allowed);
  if (value.version !== VERSION || !Number.isSafeInteger(value.issuedAt) || value.issuedAt < 0
      || !Number.isSafeInteger(value.expiresAt) || value.expiresAt <= value.issuedAt
      || value.expiresAt - value.issuedAt > MAX_REQUEST_AGE_MS) fail();
  return { version: VERSION, serviceId: hash(value.serviceId), actor: hash(value.actor), requestId: hash(value.requestId),
    issuedAt: value.issuedAt, expiresAt: value.expiresAt, command: normalizeCommand(value.command) };
}

export function normalizeRequest(value, transport = false) {
  object(value);
  const hasCredential = Object.hasOwn(value, 'credential');
  const allowed = [...REQUEST_FIELDS, 'signature', ...(transport && hasCredential ? ['credential'] : [])];
  const result = { ...fields(value, allowed), signature: signature(value.signature) };
  if (hasCredential) {
    if (!['create_room', 'join_room'].includes(result.command.type)) fail('unexpected_credential');
    result.credential = hash(value.credential);
  }
  boundedJSON(result, MAX_REQUEST_BYTES);
  return result;
}
export function publicRequest(value) {
  const normalized = normalizeRequest(value, true);
  const { credential: omitted, ...result } = normalized;
  return result;
}
export function signingBytes(value) {
  const signed = Object.hasOwn(object(value), 'signature');
  const normalized = fields(value, [...REQUEST_FIELDS, ...(signed ? ['signature'] : [])]);
  return encoder.encode(REQUEST_DOMAIN + boundedJSON(normalized, MAX_REQUEST_BYTES));
}
async function verifySignature(publicKey, proof, bytes) {
  try {
    const key = await crypto.subtle.importKey('raw', unhex(hash(publicKey)), 'Ed25519', false, ['verify']);
    if (!await crypto.subtle.verify('Ed25519', key, unhex(signature(proof)), bytes)) fail('invalid_signature');
  } catch (error) { if (error instanceof ProtocolError) throw error; fail('invalid_signature'); }
}
export async function signRequest(value, key) {
  const normalized = fields(value);
  const proof = hex(await crypto.subtle.sign('Ed25519', key, signingBytes(normalized)));
  const result = { ...normalized, signature: proof };
  // A mismatched local identity must fail before an HTTP request is emitted.
  await verifyRequest(result);
  return result;
}
export async function verifyRequest(value, options = {}) {
  const transport = normalizeRequest(value, true);
  const result = publicRequest(transport);
  if (options.serviceId !== undefined && result.serviceId !== hash(options.serviceId)) fail('wrong_service');
  if (options.now !== undefined && !options.allowExpired) {
    if (!Number.isSafeInteger(options.now) || options.now < 0) fail();
    if (result.issuedAt > options.now + 30_000 || result.expiresAt < options.now) fail('request_expired');
  }
  if (transport.credential !== undefined && await credentialHash(transport.credential) !== result.command.credentialHash) fail('invalid_credential');
  if (options.requireCredential && ['create_room', 'join_room'].includes(result.command.type) && transport.credential === undefined) fail('invalid_credential');
  await verifySignature(result.actor, result.signature, signingBytes(result));
  return result;
}
export async function requestHash(value) { return sha256(REQUEST_DOMAIN + JSON.stringify(publicRequest(value))); }

function rejectionFields(value, signed = false) {
  keys(value, ['version', 'serviceId', 'actor', 'requestId', 'requestHash', 'expiresAt', 'code', ...(signed ? ['signature'] : [])]);
  if (value.version !== VERSION || !Number.isSafeInteger(value.expiresAt) || value.expiresAt <= 0 || typeof value.code !== 'string' || !/^[A-Z][A-Z_]{1,59}$/.test(value.code)) fail('invalid_rejection');
  return { version: VERSION, serviceId: hash(value.serviceId), actor: hash(value.actor), requestId: hash(value.requestId), requestHash: hash(value.requestHash), expiresAt: value.expiresAt, code: value.code };
}
export async function createRejection(value, serviceKey) {
  const fields = rejectionFields(value);
  const bytes = encoder.encode(REJECTION_DOMAIN + JSON.stringify(fields));
  const proof = hex(await crypto.subtle.sign('Ed25519', serviceKey, bytes));
  await verifySignature(fields.serviceId, proof, bytes);
  return { ...fields, signature: proof };
}
export async function verifyRejectionProof(value, serviceId) {
  const fields = rejectionFields(value, true);
  if (fields.serviceId !== hash(serviceId)) fail('wrong_service');
  await verifySignature(serviceId, value.signature, encoder.encode(REJECTION_DOMAIN + JSON.stringify(fields)));
  return { ...fields, signature: value.signature };
}
export async function verifyRejection(value, serviceId, expectedRequest) {
  const proof = await verifyRejectionProof(value, serviceId);
  const request = await verifyRequest(expectedRequest, { serviceId });
  if (proof.actor !== request.actor || proof.requestId !== request.requestId || proof.requestHash !== await requestHash(request) || proof.expiresAt !== request.expiresAt) fail('invalid_rejection');
  return proof;
}

export async function planHash(binding) {
  keys(binding, ['serviceId', 'roomId', 'leader', 'plan']);
  const plan = normalizePlan(binding.plan);
  return sha256(PLAN_DOMAIN + JSON.stringify({ version: VERSION, serviceId: hash(binding.serviceId), roomId: hash(binding.roomId), leader: hash(binding.leader), plan }));
}

function assertIdentity(engine, publicKey, roomId, bindingHash) {
  // The existing Rust genesis constructor checks canonical, non-weak,
  // prime-order Ed25519 keys. WebCrypto verification alone is not that policy.
  let expected;
  try { expected = engine.WebRoom.create(publicKey, roomId, bindingHash); return expected.genesis_hex(); }
  catch { fail('invalid_identity'); }
  finally { expected?.free(); }
}

export function verifyLedger(engine, actor, roomId, bindingHash, value) {
  const ledger = normalizeLedger(value);
  const expected = assertIdentity(engine, hash(actor), hash(roomId), hash(bindingHash));
  if (ledger.genesis !== expected) fail('wrong_ledger_domain');
  let session;
  try {
    session = engine.WebRoom.restore(ledger.genesis, JSON.stringify(ledger.records), ledger.head);
    const summary = JSON.parse(session.summary_json());
    const phase = ledger.records.length === 6 ? 'open' : 'settled';
    if (summary.phase !== phase || Number(summary.record_count) !== ledger.records.length) fail('invalid_ledger');
    return summary;
  } catch (error) { if (error instanceof ProtocolError) throw error; fail('invalid_ledger'); }
  finally { session?.free(); }
}

function eventFields(value) {
  keys(value, ['index', 'previousHash', 'request', 'hash', 'signature']);
  if (!Number.isSafeInteger(value.index) || value.index < 1 || value.index > MAX_EVENTS) fail('invalid_event');
  // Public journal entries never contain capability secrets.
  const request = normalizeRequest(value.request);
  return { index: value.index, previousHash: hash(value.previousHash), request, hash: hash(value.hash), signature: signature(value.signature) };
}
async function eventHash(index, previousHash, request) {
  return sha256(EVENT_DOMAIN + JSON.stringify({ index, previousHash, request }));
}
export async function createEvent(value, index, previousHash, serviceKey) {
  if (!Number.isSafeInteger(index) || index < 1 || index > MAX_EVENTS) fail('limit_exceeded');
  hash(previousHash);
  const request = await verifyRequest(publicRequest(value));
  const eventDigest = await eventHash(index, previousHash, request);
  const proof = hex(await crypto.subtle.sign('Ed25519', serviceKey, encoder.encode(EVENT_SIGNATURE_DOMAIN + eventDigest)));
  const event = { index, previousHash, request, hash: eventDigest, signature: proof };
  await verifySignature(request.serviceId, proof, encoder.encode(EVENT_SIGNATURE_DOMAIN + eventDigest));
  return event;
}
export async function verifyEvent(value, serviceId) {
  const event = eventFields(value);
  await verifyRequest(event.request, { serviceId });
  if (await eventHash(event.index, event.previousHash, event.request) !== event.hash) fail('invalid_event');
  await verifySignature(serviceId, event.signature, encoder.encode(EVENT_SIGNATURE_DOMAIN + event.hash));
  return event;
}

export async function applyRequest(engine, original, value) {
  const request = await verifyRequest(value, original ? { serviceId: original.serviceId } : {});
  const command = request.command;
  if (!original) {
    if (command.type !== 'create_room') fail('room_not_found');
    const bindingHash = await planHash({ serviceId: request.serviceId, roomId: command.roomId, leader: request.actor, plan: command.plan });
    assertIdentity(engine, request.actor, command.roomId, bindingHash);
    return { version: VERSION, serviceId: request.serviceId, roomId: command.roomId, plan: command.plan, leader: request.actor,
      members: [{ publicKey: request.actor, status: 'active', ledger: null }], invites: [], head: ZERO_HASH, events: [] };
  }
  if (command.roomId !== original.roomId) fail('wrong_room');
  if (original.events.length >= MAX_EVENTS) fail('limit_exceeded');
  // No operation mutates its input: candidates are disposable until DB COMMIT.
  const state = structuredClone(original);
  const member = state.members.find(item => item.publicKey === request.actor);
  const isLeader = request.actor === state.leader && member?.status === 'active';
  switch (command.type) {
    case 'create_invite': {
      if (!isLeader) fail('forbidden');
      if (state.invites.length >= MAX_INVITES || state.members.length >= MAX_MEMBERS) fail('limit_exceeded');
      if (state.invites.some(item => item.hash === command.inviteHash)) fail('invite_exists');
      state.invites.push({ hash: command.inviteHash, status: 'active', claimedBy: null });
      break;
    }
    case 'revoke_invite': {
      if (!isLeader) fail('forbidden');
      const invite = state.invites.find(item => item.hash === command.inviteHash);
      if (!invite || invite.status !== 'active') fail('invite_unavailable');
      invite.status = 'revoked';
      break;
    }
    case 'join_room': {
      if (member) fail('already_member');
      if (state.members.length >= MAX_MEMBERS) fail('limit_exceeded');
      const invite = state.invites.find(item => item.hash === command.credentialHash);
      if (!invite || invite.status !== 'active') fail('invite_unavailable');
      assertIdentity(engine, request.actor, state.roomId, await planHash({ serviceId: state.serviceId, roomId: state.roomId, leader: state.leader, plan: state.plan }));
      invite.status = 'claimed'; invite.claimedBy = request.actor;
      state.members.push({ publicKey: request.actor, status: 'active', ledger: null });
      break;
    }
    case 'revoke_member': {
      if (!isLeader || command.member === state.leader) fail('forbidden');
      const target = state.members.find(item => item.publicKey === command.member);
      if (!target || target.status !== 'active') fail('member_unavailable');
      target.status = 'revoked';
      break;
    }
    case 'commit_ledger': {
      if (!member) fail('forbidden');
      const next = command.ledger;
      if (member.ledger === null) {
        if (member.status !== 'active') fail('forbidden');
        if (command.expectedHead !== null || next.records.length !== 6) fail('head_conflict');
      } else {
        if (command.expectedHead !== member.ledger.head || member.ledger.records.length !== 6 || next.records.length !== 8) fail('head_conflict');
        if (next.genesis !== member.ledger.genesis || member.ledger.records.some((record, index) => record !== next.records[index])) fail('ledger_prefix_changed');
      }
      verifyLedger(engine, request.actor, state.roomId,
        await planHash({ serviceId: state.serviceId, roomId: state.roomId, leader: state.leader, plan: state.plan }), next);
      member.ledger = next;
      break;
    }
    default: fail('invalid_transition');
  }
  return state;
}

export async function replayRoom(engine, serviceId, values, checkpoint = null) {
  hash(serviceId);
  if (!Array.isArray(values) || values.length < 1 || values.length > MAX_EVENTS) fail('invalid_history');
  boundedJSON(values, MAX_HISTORY_BYTES);
  if (checkpoint !== null) {
    keys(checkpoint, ['count', 'head']);
    if (!Number.isSafeInteger(checkpoint.count) || checkpoint.count < 1 || checkpoint.count > MAX_EVENTS || values.length < checkpoint.count) fail('checkpoint_conflict');
    hash(checkpoint.head);
  }
  let state = null;
  let previousHash = ZERO_HASH;
  const seen = new Set();
  for (let offset = 0; offset < values.length; offset++) {
    const event = await verifyEvent(values[offset], serviceId);
    if (event.index !== offset + 1 || event.previousHash !== previousHash) fail('invalid_history');
    if (checkpoint && checkpoint.count === event.index && checkpoint.head !== event.hash) fail('checkpoint_conflict');
    const identity = `${event.request.actor}/${event.request.requestId}`;
    if (seen.has(identity)) fail('duplicate_request');
    seen.add(identity);
    state = await applyRequest(engine, state, event.request);
    state.events.push(event); state.head = event.hash;
    previousHash = event.hash;
  }
  return state;
}
