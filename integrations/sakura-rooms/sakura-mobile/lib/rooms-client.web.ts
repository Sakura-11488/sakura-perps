import { commitRoom, readRoom, readRooms, RoomsStorageError } from './rooms-storage.web';
import { ROOMS_ENGINE } from './rooms-engine-version';

export type PlanDraft = { title: string; thesis: string; invalidation: string };
export type AllocationInput = { budget: number; leverage: 1 | 2 | 3; side: 'long' | 'short' };
export type AllocationQuote = {
  budget: string; collateral: string; entryFee: string; notional: string;
  profitCap: string; maxPayout: string; freeBalance: string;
};
export type RoomView = {
  id: string; plan: PlanDraft; phase: 'ready' | 'open' | 'settled'; createdAt: string;
  publicKey: string; headHash: string; receiptCount: number; allocation: AllocationInput | null;
  quote: AllocationQuote | null; outcome: { payout: string; netPnl: string; endingBalance: string } | null;
  verified: true; writable: boolean; recoveryNotice: string | null;
};
type PublicRoom = {
  schema: 1; engine: 'sakura-rooms-paper/1'; id: string; publicKey: string; nonce: string; plan: PlanDraft;
  createdAt: string; scenario: 'sol-storm-v1'; genesis: string; records: string[]; head: string;
};
type StoredRoom = PublicRoom & { privateKey: CryptoKey };
type RawQuote = {
  budget_credits: string; leverage: string; side: 'long' | 'short'; budget_quote_atoms: string;
  collateral_quote_atoms: string; fee_quote_atoms: string; notional_quote_atoms: string;
  profit_cap_quote_atoms: string; maximum_payout_quote_atoms: string; unused_quote_atoms: string;
};
type Summary = {
  phase: string; record_count: string; balance_quote_atoms: string; allocation: RawQuote | null;
  settlement: { payout_quote_atoms: string; net_pnl_quote_atoms: string } | null;
};
type EngineRoom = {
  genesis_hex(): string; records_json(): string; summary_json(): string; checkpoint_json(): string;
  prepare(action: string): string; submit(intent: string, signature: string): string;
  settle_scenario(): string; free(): void;
};
type Engine = {
  (input: { module_or_path: Uint8Array }): Promise<unknown>;
  WebRoom: { create(key: string, nonce: string, planHash: string): EngineRoom; restore(genesis: string, records: string, head: string): EngineRoom };
  preview_allocation(action: string): string;
};
declare global { interface Window { sakura_rooms_engine?: Engine } }

const HEX32 = /^[0-9a-f]{64}$/;
const PUBLIC_FIELDS = ['schema', 'engine', 'id', 'publicKey', 'nonce', 'plan', 'createdAt', 'scenario', 'genesis', 'records', 'head'];
const encoder = new TextEncoder();
let loading: Promise<Engine> | undefined;
class RoomsError extends Error {}
const failure = () => new RoomsError('Room verification failed. Saved data has been preserved; do not approve another action until recovery succeeds.');

function hex(bytes: ArrayBuffer | Uint8Array): string {
  return [...(bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes))].map(value => value.toString(16).padStart(2, '0')).join('');
}
function unhex(value: string): Uint8Array<ArrayBuffer> {
  if (!/^(?:[0-9a-f]{2})+$/.test(value) || value.length > 32_768) throw failure();
  return Uint8Array.from(value.match(/../g)!.map(pair => parseInt(pair, 16)));
}
function object(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw failure();
  return value as Record<string, unknown>;
}
function exactKeys(value: Record<string, unknown>, keys: string[]) {
  if (Object.keys(value).length !== keys.length || keys.some(key => !Object.hasOwn(value, key))) throw failure();
}
function normalizePlan(value: PlanDraft): PlanDraft {
  const raw = object(value);
  exactKeys(raw, ['title', 'thesis', 'invalidation']);
  const normalized: Record<string, string> = {};
  for (const [key, max] of [['title', 80], ['thesis', 1000], ['invalidation', 500]] as const) {
    const text = raw[key];
    if (typeof text !== 'string' || text.length > max || /[\u0000-\u0008\u000b\u000c\u000e-\u001f]/.test(text)) throw new RoomsError('Complete the plan within the displayed text limits.');
    normalized[key] = text.normalize('NFC').trim();
    if (!normalized[key] || normalized[key].length > max) throw new RoomsError('Add a title, a strategy thesis, and an invalidation condition before publishing.');
  }
  return normalized as PlanDraft;
}
function publicRoom(value: unknown, stored = false): PublicRoom {
  const raw = object(value);
  exactKeys(raw, stored && Object.hasOwn(raw, 'privateKey') ? [...PUBLIC_FIELDS, 'privateKey'] : PUBLIC_FIELDS);
  if (raw.schema !== 1 || raw.engine !== 'sakura-rooms-paper/1' || raw.scenario !== 'sol-storm-v1') throw failure();
  for (const key of ['id', 'publicKey', 'nonce', 'head']) if (typeof raw[key] !== 'string' || !HEX32.test(raw[key] as string)) throw failure();
  if (raw.id !== raw.nonce || typeof raw.createdAt !== 'string' || !/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$/.test(raw.createdAt) || !Number.isFinite(Date.parse(raw.createdAt))) throw failure();
  if (typeof raw.genesis !== 'string' || raw.genesis.length > 32_768) throw failure();
  unhex(raw.genesis);
  if (!Array.isArray(raw.records) || ![4, 6, 8].includes(raw.records.length)) throw failure();
  for (const record of raw.records) { if (typeof record !== 'string') throw failure(); unhex(record); }
  const plan = normalizePlan(raw.plan as PlanDraft);
  if (JSON.stringify(plan) !== JSON.stringify(raw.plan)) throw failure();
  return {
    schema: 1, engine: 'sakura-rooms-paper/1', id: raw.id as string, publicKey: raw.publicKey as string, nonce: raw.nonce as string,
    plan, createdAt: raw.createdAt, scenario: 'sol-storm-v1', genesis: raw.genesis,
    records: [...raw.records] as string[], head: raw.head as string,
  };
}
function fingerprint(value: unknown): string { return JSON.stringify(publicRoom(value, true)); }
async function digest(value: Uint8Array<ArrayBuffer>): Promise<string> { return hex(await crypto.subtle.digest('SHA-256', value)); }
async function planHash(room: Pick<PublicRoom, 'plan' | 'createdAt' | 'scenario'>): Promise<string> {
  return digest(encoder.encode(JSON.stringify({ schema: 1, engine: 'sakura-rooms-paper/1', scenario: room.scenario, createdAt: room.createdAt, ...room.plan })));
}
function action(input: AllocationInput): string {
  const raw = object(input); exactKeys(raw, ['budget', 'leverage', 'side']);
  if (!Number.isInteger(input.budget) || input.budget < 1 || input.budget > 100 || ![1, 2, 3].includes(input.leverage) || !['long', 'short'].includes(input.side)) {
    throw new RoomsError('Choose a whole paper budget from 1 to 100, leverage from 1 to 3, and a direction.');
  }
  return JSON.stringify({ action: 'allocate', budget_credits: String(input.budget), leverage: String(input.leverage), side: input.side });
}

async function engine(): Promise<Engine> {
  if (!globalThis.isSecureContext || !crypto?.subtle) throw new RoomsError('Paper Rooms needs a secure browser connection and Web Crypto.');
  if (loading) return loading;
  loading = (async () => {
    // A fresh attempt always loads the pinned module. A preexisting global may
    // be an initialized engine from another bundle; wasm-bindgen would reuse it
    // and ignore our freshly verified WASM bytes.
    await new Promise<void>((resolve, reject) => {
        const script = document.createElement('script');
        script.src = `/app/rooms-engine/${ROOMS_ENGINE.javascript}`;
        script.integrity = ROOMS_ENGINE.integrity;
        script.crossOrigin = 'anonymous';
        const timer = setTimeout(() => { script.remove(); reject(new Error('Engine timeout')); }, 20_000);
        script.onload = () => { clearTimeout(timer); resolve(); };
        script.onerror = () => { clearTimeout(timer); script.remove(); reject(new Error('Engine unavailable')); };
        document.head.appendChild(script);
    });
    const module = window.sakura_rooms_engine;
    if (!module) throw failure();
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), 15_000);
    try {
      const response = await fetch(`/app/rooms-engine/${ROOMS_ENGINE.wasm}`, { signal: controller.signal, redirect: 'error', credentials: 'omit' });
      if (!response.ok || !response.body) throw failure();
      const reader = response.body.getReader();
      const chunks: Uint8Array[] = []; let length = 0;
      for (;;) {
        const chunk = await reader.read(); if (chunk.done) break;
        length += chunk.value.byteLength;
        if (length > 3 * 1024 * 1024) { await reader.cancel(); throw failure(); }
        chunks.push(chunk.value);
      }
      const bytes = new Uint8Array(length); let offset = 0;
      for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.length; }
      if (await digest(bytes) !== ROOMS_ENGINE.sha256) throw failure();
      await module({ module_or_path: bytes });
      return module;
    } finally { clearTimeout(timer); }
  })().catch(() => { loading = undefined; throw new RoomsError('The verified paper engine could not load. Check your connection and recover again.'); });
  return loading;
}

// Shared beta reuses the exact pinned Rust engine, not another accounting model.
export { engine as loadPaperEngine };

async function restore(value: unknown, writable: boolean): Promise<{ data: PublicRoom; session: EngineRoom; key?: CryptoKey }> {
  const data = publicRoom(value, writable);
  const module = await engine();
  const expected = module.WebRoom.create(data.publicKey, data.nonce, await planHash(data));
  try { if (expected.genesis_hex() !== data.genesis) throw failure(); } finally { expected.free(); }
  const session = module.WebRoom.restore(data.genesis, JSON.stringify(data.records), data.head);
  try {
    const summary = JSON.parse(session.summary_json()) as Summary;
    const expectedCount = ({ ready: 4, open: 6, settled: 8 } as Record<string, number>)[summary.phase];
    if (expectedCount !== data.records.length || Number(summary.record_count) !== expectedCount) throw failure();
    if (!writable) return { data, session };
    try {
      const key = object(value).privateKey as CryptoKey;
      if (!key || key.type !== 'private' || key.extractable || key.algorithm?.name !== 'Ed25519' || key.usages.length !== 1 || key.usages[0] !== 'sign') return { data, session };
      const challenge = encoder.encode(`SakuraRooms/local-key-check/v1:${data.genesis}`);
      const proof = await crypto.subtle.sign('Ed25519', key, challenge);
      const publicKey = await crypto.subtle.importKey('raw', unhex(data.publicKey), 'Ed25519', false, ['verify']);
      if (!await crypto.subtle.verify('Ed25519', publicKey, proof, challenge)) return { data, session };
      return { data, session, key };
    } catch { return { data, session }; }
  } catch (error) { session.free(); throw error; }
}
function quote(raw: RawQuote): AllocationQuote {
  return { budget: raw.budget_quote_atoms, collateral: raw.collateral_quote_atoms, entryFee: raw.fee_quote_atoms,
    notional: raw.notional_quote_atoms, profitCap: raw.profit_cap_quote_atoms, maxPayout: raw.maximum_payout_quote_atoms, freeBalance: raw.unused_quote_atoms };
}
function view(data: PublicRoom, session: EngineRoom, writable = true): RoomView {
  const summary = JSON.parse(session.summary_json()) as Summary;
  if (!['ready', 'open', 'settled'].includes(summary.phase)) throw failure();
  return {
    id: data.id, plan: data.plan, phase: summary.phase as RoomView['phase'], createdAt: data.createdAt,
    publicKey: data.publicKey, headHash: data.head, receiptCount: data.records.length, verified: true, writable,
    recoveryNotice: writable ? null : 'Read-only verified history. The matching local paper identity is unavailable here. Export this record to preserve it; no replacement identity will be created for this room.',
    allocation: summary.allocation ? { budget: Number(summary.allocation.budget_credits), leverage: Number(summary.allocation.leverage) as 1 | 2 | 3, side: summary.allocation.side } : null,
    quote: summary.allocation ? quote(summary.allocation) : null,
    outcome: summary.settlement ? { payout: summary.settlement.payout_quote_atoms, netPnl: summary.settlement.net_pnl_quote_atoms, endingBalance: summary.balance_quote_atoms } : null,
  };
}
async function signedStep(session: EngineRoom, key: CryptoKey, request: string): Promise<void> {
  const prepared = JSON.parse(session.prepare(request)) as { intent_hex: string; signing_bytes_hex: string };
  const signature = await crypto.subtle.sign('Ed25519', key, unhex(prepared.signing_bytes_hex));
  session.submit(prepared.intent_hex, hex(signature));
}
function nextData(base: PublicRoom, session: EngineRoom): PublicRoom {
  const checkpoint = JSON.parse(session.checkpoint_json()) as { head_hash: string; record_count: string };
  const next = { ...base, genesis: session.genesis_hex(), records: JSON.parse(session.records_json()), head: checkpoint.head_hash };
  return publicRoom(next);
}
async function safely<T>(operation: () => Promise<T>): Promise<T> {
  try { return await operation(); } catch (error) {
    if (error instanceof RoomsError || error instanceof RoomsStorageError) throw error;
    throw failure();
  }
}

export function formatCredits(value: string): string {
  if (!/^-?\d{1,40}$/.test(value)) return '—';
  const negative = value.startsWith('-'); const atoms = BigInt(negative ? value.slice(1) : value);
  const fraction = String(atoms % 1_000_000n).padStart(6, '0').replace(/0+$/, '');
  return `${negative && atoms !== 0n ? '−' : ''}${atoms / 1_000_000n}${fraction ? `.${fraction}` : ''}`;
}
export async function quoteAllocation(input: AllocationInput): Promise<AllocationQuote> {
  return safely(async () => quote(JSON.parse((await engine()).preview_allocation(action(input)))));
}
export async function listRooms(): Promise<RoomView[]> {
  return safely(async () => {
    const output: RoomView[] = [];
    for (const row of await readRooms()) {
      const recovered = await restore(row, true);
      try { output.push(view(recovered.data, recovered.session, !!recovered.key)); } finally { recovered.session.free(); }
    }
    return output.sort((a, b) => b.createdAt.localeCompare(a.createdAt) || a.id.localeCompare(b.id));
  });
}
export async function publishPlan(draft: PlanDraft): Promise<RoomView> {
  return safely(async () => {
    const plan = normalizePlan(draft); const module = await engine();
    let pair: CryptoKeyPair;
    try { pair = await crypto.subtle.generateKey('Ed25519', false, ['sign', 'verify']) as CryptoKeyPair; }
    catch { throw new RoomsError('This browser cannot create a local Ed25519 paper identity. Try a current supported browser.'); }
    const publicKey = hex(await crypto.subtle.exportKey('raw', pair.publicKey));
    const nonce = hex(crypto.getRandomValues(new Uint8Array(32)));
    const base: PublicRoom = { schema: 1, engine: 'sakura-rooms-paper/1', id: nonce, publicKey, nonce, plan, createdAt: new Date().toISOString(), scenario: 'sol-storm-v1', genesis: '', records: [], head: '' };
    const session = module.WebRoom.create(publicKey, nonce, await planHash(base));
    try {
      for (let step = 0; step < 4; step++) await signedStep(session, pair.privateKey, '{"action":"initialize"}');
      const next = nextData(base, session);
      const result = view(next, session);
      await commitRoom({ ...next, privateKey: pair.privateKey }, null, fingerprint);
      return result;
    } finally { session.free(); }
  });
}
async function changeRoom(id: string, expectedHead: string, change: (session: EngineRoom, key: CryptoKey) => Promise<void>): Promise<RoomView> {
  return safely(async () => {
    if (!HEX32.test(id) || !HEX32.test(expectedHead)) throw failure();
    const raw = await readRoom(id);
    const recovered = await restore(raw, true);
    try {
      if (recovered.data.head !== expectedHead) throw new RoomsError('This room changed in another tab. Recover saved rooms and review the latest state.');
      if (!recovered.key) throw new RoomsError('This room is read-only because its matching local paper identity is unavailable. Export its verified history.');
      const previous = fingerprint(raw);
      await change(recovered.session, recovered.key!);
      const next = nextData(recovered.data, recovered.session);
      const result = view(next, recovered.session);
      await commitRoom({ ...next, privateKey: recovered.key! }, previous, fingerprint);
      return result;
    } finally { recovered.session.free(); }
  });
}
export async function approveAllocation(id: string, input: AllocationInput, expectedHead: string): Promise<RoomView> {
  const request = action(input);
  return changeRoom(id, expectedHead, async (session, key) => {
    if (JSON.parse(session.summary_json()).phase !== 'ready') throw new RoomsError('This room already has an allocation. Recover the saved result.');
    await signedStep(session, key, request);
    await signedStep(session, key, request);
  });
}
export async function runScenario(id: string, expectedHead: string): Promise<RoomView> {
  return changeRoom(id, expectedHead, async (session, key) => {
    if (JSON.parse(session.summary_json()).phase !== 'open') throw new RoomsError('Approve an allocation before running this scenario, or recover its saved result.');
    await signedStep(session, key, '{"action":"run_scenario"}');
    session.settle_scenario();
  });
}
export async function exportRoom(id: string): Promise<string> {
  return safely(async () => {
    if (!HEX32.test(id)) throw failure();
    const recovered = await restore(publicRoom(await readRoom(id), true), false);
    try { return JSON.stringify(recovered.data, null, 2); } finally { recovered.session.free(); }
  });
}
export async function verifyExport(text: string): Promise<RoomView> {
  return safely(async () => {
    if (typeof text !== 'string' || text.length > 280_000) throw new RoomsError('This public record exceeds the paper pilot size limit.');
    const recovered = await restore(JSON.parse(text), false);
    try { return view(recovered.data, recovered.session, false); } finally { recovered.session.free(); }
  });
}
