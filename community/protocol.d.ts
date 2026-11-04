export type Plan = { title: string; thesis: string; invalidation: string };
export type Ledger = { genesis: string; records: string[]; head: string };
export type Command =
  | { type: 'create_room'; roomId: string; plan: Plan; credentialHash: string }
  | { type: 'create_invite' | 'revoke_invite'; roomId: string; inviteHash: string }
  | { type: 'join_room'; roomId: string; credentialHash: string }
  | { type: 'revoke_member'; roomId: string; member: string }
  | { type: 'get_room' | 'get_my_record'; roomId: string }
  | { type: 'commit_ledger'; roomId: string; expectedHead: string | null; ledger: Ledger };
export type RequestFields = { version: 1; serviceId: string; actor: string; requestId: string; issuedAt: number; expiresAt: number; command: Command };
export type SignedRequest = RequestFields & { signature: string };
export type TransportRequest = SignedRequest & { credential?: string };
export type RejectionFields = { version: 1; serviceId: string; actor: string; requestId: string; requestHash: string; expiresAt: number; code: string };
export type SignedRejection = RejectionFields & { signature: string };
export type JournalEvent = { index: number; previousHash: string; request: SignedRequest; hash: string; signature: string };
export type Checkpoint = { count: number; head: string };
export type RoomState = { version: 1; serviceId: string; roomId: string; plan: Plan; leader: string; members: { publicKey: string; status: 'active' | 'revoked'; ledger: Ledger | null }[]; invites: { hash: string; status: 'active' | 'claimed' | 'revoked'; claimedBy: string | null }[]; head: string; events: JournalEvent[] };
export type EngineRoom = { genesis_hex(): string; records_json(): string; summary_json(): string; checkpoint_json(): string; prepare(action: string): string; submit(intent: string, signature: string): string; settle_scenario(): string; free(): void };
export type Engine = { WebRoom: { create(key: string, nonce: string, planHash: string): EngineRoom; restore(genesis: string, records: string, head: string): EngineRoom }; preview_allocation(action: string): string };
export const VERSION: 1;
export const ZERO_HASH: string;
export const MAX_MEMBERS: number;
export const MAX_INVITES: number;
export const MAX_EVENTS: number;
export const MAX_REQUEST_BYTES: number;
export const MAX_HISTORY_BYTES: number;
export const MAX_REQUEST_AGE_MS: number;
export class ProtocolError extends Error { readonly code: string; constructor(code?: string); }
export function hex(value: ArrayBuffer | Uint8Array): string;
export function unhex(value: string): Uint8Array<ArrayBuffer>;
export function randomHex(): string;
export function sha256(value: string | Uint8Array): Promise<string>;
export function credentialHash(token: string): Promise<string>;
export function normalizePlan(value: unknown): Plan;
export function normalizeCommand(value: unknown): Command;
export function normalizeRequest(value: unknown, transport?: boolean): TransportRequest;
export function publicRequest(value: unknown): SignedRequest;
export function signingBytes(value: RequestFields | SignedRequest): Uint8Array<ArrayBuffer>;
export function signRequest(value: RequestFields, key: CryptoKey): Promise<SignedRequest>;
export function verifyRequest(value: unknown, options?: { serviceId?: string; now?: number; allowExpired?: boolean; requireCredential?: boolean }): Promise<SignedRequest>;
export function requestHash(value: unknown): Promise<string>;
export function createRejection(value: RejectionFields, serviceKey: CryptoKey): Promise<SignedRejection>;
export function verifyRejectionProof(value: unknown, serviceId: string): Promise<SignedRejection>;
export function verifyRejection(value: unknown, serviceId: string, expectedRequest: SignedRequest): Promise<SignedRejection>;
export function planHash(binding: { serviceId: string; roomId: string; leader: string; plan: Plan }): Promise<string>;
export function verifyLedger(engine: Engine, actor: string, roomId: string, bindingHash: string, ledger: unknown): Record<string, unknown>;
export function createEvent(request: SignedRequest, index: number, previousHash: string, serviceKey: CryptoKey): Promise<JournalEvent>;
export function verifyEvent(value: unknown, serviceId: string): Promise<JournalEvent>;
export function applyRequest(engine: Engine, state: RoomState | null, request: SignedRequest): Promise<RoomState>;
export function replayRoom(engine: Engine, serviceId: string, events: unknown, checkpoint?: Checkpoint | null): Promise<RoomState>;
export function normalizeLedger(value: unknown): Ledger;
