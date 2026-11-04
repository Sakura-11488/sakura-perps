import { loadPaperEngine, quoteAllocation, type AllocationInput, type AllocationQuote, type PlanDraft } from './rooms-client.web';
import { readCommunityProfile, writeCommunityProfile } from './rooms-community-storage.web';
import {
  hex, unhex, randomHex, credentialHash, normalizePlan, signRequest, verifyRequest,
  planHash, replayRoom, verifyEvent, verifyLedger, normalizeLedger, verifyRejection,
  type Command, type TransportRequest, type JournalEvent, type RoomState, type Ledger, type EngineRoom, type Checkpoint,
} from './rooms-community-protocol.mjs';

export type CommunityMember = { publicKey: string; status: 'active' | 'revoked'; phase: 'ready' | 'open' | 'settled'; allocation: AllocationInput | null; quote: AllocationQuote | null; outcome: { payout: string; netPnl: string; endingBalance: string } | null };
export type CommunityView = { roomId: string; plan: PlanDraft; leader: string; head: string; eventCount: number; members: CommunityMember[]; invites: { inviteHash: string; status: 'active' | 'claimed' | 'revoked' }[]; ownOnly?: boolean };
export type CommunitySession = { serviceId: string; publicKey: string; writable: boolean; pending: boolean; rooms: CommunityView[]; recoveryNotice: string | null; inviteUrl?: string };
type OwnRecord = { version: 1; kind: 'own'; serviceId: string; roomId: string; creation: JournalEvent; member: { publicKey: string; status: 'active' | 'revoked'; ledger: Ledger | null }; commit: JournalEvent | null; revocation: JournalEvent | null };
type Profile = { id: 'identity'; schema: 1; revision: number; serviceId: string; publicKey: string; privateKey?: CryptoKey; rooms: Record<string, JournalEvent[]>; own: Record<string, OwnRecord>; checkpoints: Record<string, Checkpoint[]>; pending: { packet: TransportRequest; inviteToken?: string; accepted?: JournalEvent } | null };
const HEX = /^[0-9a-f]{64}$/;
const encoder = new TextEncoder();
const binding = (room: Pick<RoomState, 'serviceId' | 'roomId' | 'leader' | 'plan'>) => planHash({ serviceId: room.serviceId, roomId: room.roomId, leader: room.leader, plan: room.plan });
class CommunityError extends Error { constructor(message: string, readonly code = '') { super(message); } }
const invalid = () => new CommunityError('Shared history verification failed. Your saved checkpoints have been preserved.');
const messages: Record<string, string> = {
  FORBIDDEN: 'This identity no longer has permission for that action. Your own paper record remains available.',
  UNAUTHORIZED: 'The service could not verify this paper identity.',
  CONFLICT: 'The room changed, or this request conflicts with an accepted action. Reload and review the shared history.',
  INVALID_CREDENTIAL: 'This admission or invitation code is invalid, expired, or already used.',
  RATE_LIMITED: 'Too many requests. Wait briefly, then recover the same pending request.',
};
function asObject(value: unknown): Record<string, unknown> { if (!value || typeof value !== 'object' || Array.isArray(value)) throw invalid(); return value as Record<string, unknown>; }
function profile(value: unknown): Profile {
  const data = asObject(value);
  if (data.id !== 'identity' || data.schema !== 1 || !Number.isSafeInteger(data.revision) || (data.revision as number) < 0 || typeof data.serviceId !== 'string' || !HEX.test(data.serviceId) || typeof data.publicKey !== 'string' || !HEX.test(data.publicKey)) throw invalid();
  const rooms = asObject(data.rooms), own = asObject(data.own), checkpoints = asObject(data.checkpoints);
  if (Object.keys(rooms).length > 32 || Object.keys(own).length > 32 || Object.keys(checkpoints).length > 32 || [...Object.keys(rooms), ...Object.keys(own), ...Object.keys(checkpoints)].some(id => !HEX.test(id))) throw invalid();
  for (const values of Object.values(checkpoints)) {
    if (!Array.isArray(values) || values.length > 256) throw invalid();
    const indexes = new Set();
    for (const point of values) { if (!Number.isSafeInteger(point?.count) || point.count < 1 || point.count > 256 || !HEX.test(point.head) || indexes.has(point.count)) throw invalid(); indexes.add(point.count); }
  }
  if (data.pending !== null) {
    const pending = asObject(data.pending), packet = asObject(pending.packet);
    if (packet.actor !== data.publicKey || packet.serviceId !== data.serviceId || (pending.inviteToken !== undefined && (typeof pending.inviteToken !== 'string' || !HEX.test(pending.inviteToken)))) throw invalid();
  }
  return data as unknown as Profile;
}
async function read(): Promise<Profile> { return profile(await readCommunityProfile()); }
async function save(previous: Profile, changes: Partial<Profile>): Promise<Profile> {
  const next = profile({ ...previous, ...changes, revision: previous.revision + 1 });
  await writeCommunityProfile(next, previous.revision); return next;
}
function pins(data: Profile, roomId: string, events: JournalEvent[]): Profile['checkpoints'] {
  const current = [...(data.checkpoints[roomId] || [])];
  for (const event of events) {
    const existing = current.find(point => point.count === event.index);
    if (existing && existing.head !== event.hash) throw invalid();
    if (!existing) current.push({ count: event.index, head: event.hash });
  }
  return { ...data.checkpoints, [roomId]: current };
}
async function usableKey(data: Profile): Promise<CryptoKey | null> {
  try {
    const key = data.privateKey;
    if (!key || key.type !== 'private' || key.extractable || key.algorithm.name !== 'Ed25519' || key.usages.length !== 1 || key.usages[0] !== 'sign') return null;
    const challenge = encoder.encode(`SakuraRoomsCommunity/LocalIdentity/v1:${data.serviceId}`);
    const proof = await crypto.subtle.sign('Ed25519', key, challenge);
    const publicKey = await crypto.subtle.importKey('raw', unhex(data.publicKey), 'Ed25519', false, ['verify']);
    return await crypto.subtle.verify('Ed25519', publicKey, proof, challenge) ? key : null;
  } catch { return null; }
}
async function json(path: string, packet?: TransportRequest): Promise<unknown> {
  const controller = new AbortController(), timer = setTimeout(() => controller.abort(), 15_000);
  try {
    const response = await fetch(path, { method: packet ? 'POST' : 'GET', credentials: 'omit', redirect: 'error', cache: 'no-store', signal: controller.signal,
      ...(packet ? { headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(packet) } : {}) });
    if (!response.body) throw invalid();
    const reader = response.body.getReader(), chunks: Uint8Array[] = []; let length = 0;
    for (;;) { const next = await reader.read(); if (next.done) break; length += next.value.length; if (length > 4 * 1024 * 1024) { await reader.cancel(); throw invalid(); } chunks.push(next.value); }
    const bytes = new Uint8Array(length); let offset = 0; for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.length; }
    const body = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(bytes));
    if (!response.ok) {
      const code = typeof body?.error?.code === 'string' ? body.error.code : 'SERVICE_ERROR';
      const error = new CommunityError(messages[code] || 'The community service rejected this request. Reload history before continuing.', code);
      Object.assign(error, { rejection: body.error?.rejection, status: response.status });
      throw error;
    }
    return body;
  } catch (error) {
    if (error instanceof CommunityError) throw error;
    throw new CommunityError('The service response could not be confirmed. Recover the saved request before trying another action.');
  } finally { clearTimeout(timer); }
}
async function serviceId(): Promise<string> {
  const info = asObject(await json('/api/rooms/v1/info'));
  if (info.version !== 1 || typeof info.serviceId !== 'string' || !HEX.test(info.serviceId)) throw invalid();
  return info.serviceId;
}
async function signed(data: Profile, command: Command): Promise<TransportRequest> {
  const key = await usableKey(data); if (!key) throw new CommunityError('This identity is read-only. Its matching local signing key is unavailable; no replacement was created.');
  const now = Date.now();
  return signRequest({ version: 1, serviceId: data.serviceId, actor: data.publicKey, requestId: randomHex(), issuedAt: now, expiresAt: now + 300_000, command }, key);
}
async function state(data: Profile, roomId: string): Promise<RoomState> {
  if (!HEX.test(roomId) || !Object.hasOwn(data.rooms, roomId)) throw invalid();
  return replayRoom(await loadPaperEngine(), data.serviceId, data.rooms[roomId]);
}
async function verifyOwn(data: Profile, value: unknown): Promise<OwnRecord> {
  const raw = asObject(value) as unknown as OwnRecord;
  if (raw.version !== 1 || raw.kind !== 'own' || raw.serviceId !== data.serviceId || !HEX.test(raw.roomId) || raw.member?.publicKey !== data.publicKey || raw.member.status !== 'revoked') throw invalid();
  const creation = await verifyEvent(raw.creation, data.serviceId);
  if (creation.index !== 1 || creation.request.command.type !== 'create_room' || creation.request.command.roomId !== raw.roomId) throw invalid();
  const initial = await replayRoom(await loadPaperEngine(), data.serviceId, [creation]);
  const known = data.rooms[raw.roomId];
  if (known && known[0].hash !== creation.hash) throw invalid();
  const previous = data.own[raw.roomId];
  if (previous?.commit && (!raw.commit || raw.commit.index < previous.commit.index)) throw invalid();
  if (previous?.revocation && (!raw.revocation || raw.revocation.index < previous.revocation.index || raw.revocation.hash !== previous.revocation.hash)) throw invalid();
  if (raw.member.ledger) {
    const commit = await verifyEvent(raw.commit, data.serviceId);
    if (commit.request.actor !== data.publicKey || commit.request.command.type !== 'commit_ledger' || commit.request.command.roomId !== raw.roomId || JSON.stringify(commit.request.command.ledger) !== JSON.stringify(normalizeLedger(raw.member.ledger))) throw invalid();
    verifyLedger(await loadPaperEngine(), data.publicKey, raw.roomId, await binding(initial), raw.member.ledger);
    const prior = data.own[raw.roomId]?.member.ledger || (known ? (await state(data, raw.roomId)).members.find(member => member.publicKey === data.publicKey)?.ledger : null);
    if (prior && (raw.member.ledger.records.length < prior.records.length || prior.records.some((record, index) => record !== raw.member.ledger!.records[index]))) throw invalid();
  } else if (raw.commit !== null || data.own[raw.roomId]?.member.ledger || (known && (await state(data, raw.roomId)).members.find(member => member.publicKey === data.publicKey)?.ledger)) throw invalid();
  if (raw.member.status === 'revoked') {
    const revoked = await verifyEvent(raw.revocation, data.serviceId);
    if (revoked.request.actor !== initial.leader || revoked.request.command.type !== 'revoke_member' || revoked.request.command.member !== data.publicKey || revoked.request.command.roomId !== raw.roomId) throw invalid();
  } else if (raw.revocation !== null) throw invalid();
  return raw;
}
async function view(data: Profile, room: RoomState): Promise<CommunityView> {
  const module = await loadPaperEngine(), bindingHash = await binding(room);
  const own = data.own[room.roomId] ? await verifyOwn(data, data.own[room.roomId]) : null;
  const roster = [...room.members];
  if (own && !roster.some(member => member.publicKey === data.publicKey)) roster.push(own.member);
  const members: CommunityMember[] = roster.map(item => {
    const member = item.publicKey === data.publicKey && own ? own.member : item;
    if (!member.ledger) return { publicKey: member.publicKey, status: member.status, phase: 'ready', allocation: null, quote: null, outcome: null };
    const summary = verifyLedger(module, member.publicKey, room.roomId, bindingHash, member.ledger) as any;
    const quote = summary.allocation;
    return { publicKey: member.publicKey, status: member.status, phase: summary.phase, allocation: { budget: Number(quote.budget_credits), leverage: Number(quote.leverage) as 1 | 2 | 3, side: quote.side },
      quote: { budget: quote.budget_quote_atoms, collateral: quote.collateral_quote_atoms, entryFee: quote.fee_quote_atoms, notional: quote.notional_quote_atoms, profitCap: quote.profit_cap_quote_atoms, maxPayout: quote.maximum_payout_quote_atoms, freeBalance: quote.unused_quote_atoms },
      outcome: summary.settlement ? { payout: summary.settlement.payout_quote_atoms, netPnl: summary.settlement.net_pnl_quote_atoms, endingBalance: summary.balance_quote_atoms } : null };
  });
  return { roomId: room.roomId, plan: room.plan, leader: room.leader, head: room.head, eventCount: room.events.length, members, invites: room.invites.map(invite => ({ inviteHash: invite.hash, status: invite.status })), ...(own ? { ownOnly: true } : {}) };
}
async function session(data: Profile, notice: string | null = null): Promise<CommunitySession> {
  const writable = !!await usableKey(data), rooms = [];
  for (const id of Object.keys(data.rooms)) rooms.push(await view(data, await state(data, id)));
  return { serviceId: data.serviceId, publicKey: data.publicKey, writable, pending: data.pending !== null, rooms,
    recoveryNotice: !writable ? 'The local signing key is unavailable. Verified cached history is read-only and can still be exported.' : notice };
}
export async function connectCommunity(): Promise<CommunitySession> {
  if (!globalThis.isSecureContext || !crypto.subtle) throw new CommunityError('Community Rooms requires a secure browser and Web Crypto.');
  const stored = await readCommunityProfile();
  if (stored) {
    const data = profile(stored);
    try { if (await serviceId() !== data.serviceId) throw invalid(); }
    catch (error) { if (error instanceof CommunityError && error.message.startsWith('Shared history')) throw error; return session(data, 'The service is unavailable. Showing verified cached history; requests require the same pinned service.'); }
    return session(data);
  }
  const service = await serviceId();
  const pair = await crypto.subtle.generateKey('Ed25519', false, ['sign', 'verify']) as CryptoKeyPair;
  const data: Profile = { id: 'identity', schema: 1, revision: 0, serviceId: service, publicKey: hex(await crypto.subtle.exportKey('raw', pair.publicKey)), privateKey: pair.privateKey, rooms: {}, own: {}, checkpoints: {}, pending: null };
  await writeCommunityProfile(data, null);
  return session(data);
}
async function accept(data: Profile, response: unknown, expectedRoomId: string, clearPending = false): Promise<Profile> {
  const raw = asObject(response);
  if (raw.version !== 1 || raw.serviceId !== data.serviceId || raw.roomId !== expectedRoomId) throw invalid();
  if (raw.kind === 'own') {
    const record = await verifyOwn(data, raw);
    const checkpoints = pins(data, expectedRoomId, [record.creation, record.commit, record.revocation].filter((event): event is JournalEvent => !!event));
    // A join may commit, lose its acknowledgement and then be revoked before
    // this browser ever receives a full history. Retain the signed creation
    // solely as historical plan provenance and show the verified own record.
    const rooms = data.rooms[expectedRoomId] ? data.rooms : { ...data.rooms, [expectedRoomId]: [record.creation] };
    return save(data, { rooms, own: { ...data.own, [expectedRoomId]: record }, checkpoints, ...(clearPending ? { pending: null } : {}) });
  }
  const prior = data.rooms[expectedRoomId];
  const checkpoint = prior ? { count: prior.length, head: prior.at(-1)!.hash } : null;
  const room = await replayRoom(await loadPaperEngine(), data.serviceId, raw.events, checkpoint);
  for (const point of data.checkpoints[expectedRoomId] || []) if (room.events[point.count - 1]?.hash !== point.head) throw invalid();
  if (room.roomId !== expectedRoomId || !room.members.some(member => member.publicKey === data.publicKey)) throw invalid();
  if (Object.keys(data.rooms).length >= 32 && !prior) throw invalid();
  const own = { ...data.own }; delete own[expectedRoomId];
  return save(data, { rooms: { ...data.rooms, [room.roomId]: room.events }, own, checkpoints: pins(data, expectedRoomId, [room.events.at(-1)!]), ...(clearPending ? { pending: null } : {}) });
}
async function deliver(data: Profile): Promise<Profile> {
  if (!data.pending) return data;
  const { packet } = data.pending;
  await verifyRequest(packet, { serviceId: data.serviceId, allowExpired: true, requireCredential: true });
  try {
    const response = await json('/api/rooms/v1/command', packet);
    const raw = asObject(response);
    if (raw.version !== 1 || raw.kind !== 'receipt' || raw.serviceId !== data.serviceId || raw.roomId !== packet.command.roomId) throw invalid();
    const event = await verifyEvent(raw.event, data.serviceId);
    if (event.request.requestId !== packet.requestId || event.request.signature !== packet.signature || event.request.actor !== data.publicKey) throw invalid();
    // Keep acceptance evidence even if a subsequent authenticated read fails.
    data = await save(data, { checkpoints: pins(data, packet.command.roomId, [event]), pending: { ...data.pending!, accepted: event } });
    let history;
    try { history = await json('/api/rooms/v1/command', await signed(data, { type: 'get_room', roomId: packet.command.roomId })); }
    catch (error) { if ((error as any).status !== 403) throw error; history = await json('/api/rooms/v1/command', await signed(data, { type: 'get_my_record', roomId: packet.command.roomId })); }
    return await accept(data, history, packet.command.roomId, true);
  } catch (error) {
    // HTTP status alone never proves that an action was rejected.
    if (error instanceof CommunityError && (error as any).rejection) {
      await verifyRejection((error as any).rejection, data.serviceId, packet);
      if (data.pending?.accepted) throw invalid();
      await save(data, { pending: null });
    }
    throw error;
  }
}
async function mutate(command: Command, credential?: string, inviteToken?: string): Promise<Profile> {
  const data = await read();
  if (data.pending) throw new CommunityError('A prepared request is still pending. Recover it before approving another action.');
  if (await serviceId() !== data.serviceId) throw invalid();
  const packet = { ...await signed(data, command), ...(credential ? { credential } : {}) };
  const prepared = await save(data, { pending: { packet, ...(inviteToken ? { inviteToken } : {}) } });
  return deliver(prepared);
}
export async function refreshCommunity(roomId: string): Promise<CommunityView> {
  let data = await read();
  if (data.pending) throw new CommunityError('Recover the saved request before refreshing shared history.');
  if (!await usableKey(data)) return view(data, await state(data, roomId));
  let response;
  try { response = await json('/api/rooms/v1/command', await signed(data, { type: 'get_room', roomId })); }
  catch (error) { if ((error as any).status !== 403) throw error; response = await json('/api/rooms/v1/command', await signed(data, { type: 'get_my_record', roomId })); }
  data = await accept(data, response, roomId);
  return view(data, await state(data, roomId));
}
export async function createCommunity(plan: PlanDraft, admission: string): Promise<CommunityView> {
  const token = admission.trim(); if (!HEX.test(token)) throw new CommunityError('Enter the operator-issued admission code.');
  const roomId = randomHex();
  const data = await mutate({ type: 'create_room', roomId, plan: normalizePlan(plan), credentialHash: await credentialHash(token) }, token);
  return view(data, await state(data, roomId));
}
function inviteLink(roomId: string, token: string) { return `${location.origin}/app/perps/community#room=${roomId}&invite=${token}`; }
export async function joinCommunity(inviteText: string): Promise<CommunityView> {
  let url: URL; try { url = new URL(inviteText); } catch { throw new CommunityError('Paste the complete invitation link.'); }
  const fragment = new URLSearchParams(url.hash.slice(1)), roomId = fragment.get('room'), token = fragment.get('invite');
  if (url.origin !== location.origin || url.pathname.replace(/\/$/, '') !== '/app/perps/community' || url.search || !roomId || !token || !HEX.test(roomId) || !HEX.test(token) || [...fragment.keys()].length !== 2) throw new CommunityError('Use an invitation for this exact community service.');
  const data = await mutate({ type: 'join_room', roomId, credentialHash: await credentialHash(token) }, token);
  return view(data, await state(data, roomId));
}
export async function createCommunityInvite(roomId: string): Promise<{ inviteUrl: string; inviteHash: string; room: CommunityView }> {
  const token = randomHex(), hash = await credentialHash(token);
  const data = await mutate({ type: 'create_invite', roomId, inviteHash: hash }, undefined, token);
  return { inviteUrl: inviteLink(roomId, token), inviteHash: hash, room: await view(data, await state(data, roomId)) };
}
export async function revokeCommunityInvite(roomId: string, inviteHash: string): Promise<CommunityView> {
  const data = await mutate({ type: 'revoke_invite', roomId, inviteHash }); return view(data, await state(data, roomId));
}
export async function revokeCommunityMember(roomId: string, publicKey: string): Promise<CommunityView> {
  const data = await mutate({ type: 'revoke_member', roomId, member: publicKey }); return view(data, await state(data, roomId));
}
export const quoteCommunityAllocation = quoteAllocation;
async function signStep(ledger: EngineRoom, key: CryptoKey, action: string) {
  const request = JSON.parse(ledger.prepare(action));
  ledger.submit(request.intent_hex, hex(await crypto.subtle.sign('Ed25519', key, unhex(request.signing_bytes_hex))));
}
async function changeLedger(roomId: string, expectedHead: string, input?: AllocationInput): Promise<CommunityView> {
  const data = await read(); if (data.pending) throw new CommunityError('Recover the pending request first.');
  const room = await state(data, roomId); if (room.head !== expectedHead) throw new CommunityError('Shared history changed. Review the current room before approving.');
  const member = room.members.find(item => item.publicKey === data.publicKey); if (!member) throw invalid();
  const key = await usableKey(data); if (!key) throw new CommunityError('This identity is read-only.');
  const module = await loadPaperEngine();
  const prior = data.own[roomId]?.member.ledger || member.ledger;
  if ((input && prior) || (!input && !prior)) throw new CommunityError('This allocation has already advanced. Reload its verified result.');
  if (input) await quoteAllocation(input);
  const ledger = prior ? module.WebRoom.restore(prior.genesis, JSON.stringify(prior.records), prior.head) : module.WebRoom.create(data.publicKey, roomId, await binding(room));
  try {
    if (input) {
      for (let step = 0; step < 4; step++) await signStep(ledger, key, '{"action":"initialize"}');
      const request = JSON.stringify({ action: 'allocate', budget_credits: String(input.budget), leverage: String(input.leverage), side: input.side });
      await signStep(ledger, key, request); await signStep(ledger, key, request);
    } else { await signStep(ledger, key, '{"action":"run_scenario"}'); ledger.settle_scenario(); }
    const next = normalizeLedger({ genesis: ledger.genesis_hex(), records: JSON.parse(ledger.records_json()), head: JSON.parse(ledger.checkpoint_json()).head_hash });
    const result = await mutate({ type: 'commit_ledger', roomId, expectedHead: prior?.head || null, ledger: next });
    return view(result, await state(result, roomId));
  } finally { ledger.free(); }
}
export function approveCommunityAllocation(roomId: string, input: AllocationInput, expectedHead: string) { return changeLedger(roomId, expectedHead, input); }
export function runCommunityScenario(roomId: string, expectedHead: string) { return changeLedger(roomId, expectedHead); }
export async function recoverCommunityPending(): Promise<CommunitySession> {
  let data = await read(); const pending = data.pending;
  if (!await usableKey(data)) return session(data);
  if (await serviceId() !== data.serviceId) throw invalid();
  if (pending) data = await deliver(data);
  const result = await session(data);
  if (pending?.inviteToken && pending.packet.command.type === 'create_invite') result.inviteUrl = inviteLink(pending.packet.command.roomId, pending.inviteToken);
  return result;
}
export async function exportCommunityRecord(roomId: string): Promise<string> {
  const data = await read(); await state(data, roomId);
  if (data.own[roomId]) { const own = await verifyOwn(data, data.own[roomId]); return JSON.stringify(own, null, 2); }
  return JSON.stringify({ version: 1, serviceId: data.serviceId, roomId, events: data.rooms[roomId] }, null, 2);
}
