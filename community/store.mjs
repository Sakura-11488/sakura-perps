import { DatabaseSync } from 'node:sqlite';
import { generateKeyPairSync, createPrivateKey, createPublicKey, randomBytes } from 'node:crypto';
import { chmodSync, closeSync, existsSync, lstatSync, mkdirSync, openSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { credentialHash } from './protocol.mjs';

export const MAX_ROOMS = 32;
export const MAX_ADMISSIONS = 128;
export class StoreError extends Error {
  constructor(code = 'STORAGE_UNAVAILABLE') { super(code); this.code = code; }
}
function privateFile(file) {
  const info = lstatSync(file);
  if (!info.isFile() || info.isSymbolicLink() || (info.mode & 0o077)) throw new StoreError('PRIVATE_STORAGE_REQUIRED');
}
function openDatabase(file) {
  privateFile(file);
  const db = new DatabaseSync(file, { timeout: 5000, enableForeignKeyConstraints: true, allowExtension: false });
  try {
    db.exec('PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON; PRAGMA trusted_schema=OFF; PRAGMA busy_timeout=5000;');
    if (db.prepare('PRAGMA synchronous').get().synchronous !== 2 || db.prepare('PRAGMA journal_mode').get().journal_mode !== 'wal') throw new StoreError('DURABLE_STORAGE_REQUIRED');
    return db;
  } catch (error) { db.close(); throw error; }
}

/** Explicit, exclusive initialization. An existing or partial database is never reset. */
export function initializeStore(dbPath) {
  const file = path.resolve(dbPath);
  mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
  const descriptor = openSync(file, 'wx', 0o600);
  closeSync(descriptor);
  const db = openDatabase(file);
  try {
    const keys = generateKeyPairSync('ed25519');
    const privateKey = keys.privateKey.export({ format: 'der', type: 'pkcs8' }).toString('hex');
    const serviceId = keys.publicKey.export({ format: 'der', type: 'spki' }).subarray(-32).toString('hex');
    db.exec(`BEGIN IMMEDIATE;
      CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL) STRICT;
      CREATE TABLE rooms (id TEXT PRIMARY KEY, head TEXT NOT NULL, event_count INTEGER NOT NULL, events TEXT NOT NULL) STRICT;
      CREATE TABLE requests (actor TEXT NOT NULL, request_id TEXT NOT NULL, digest TEXT NOT NULL, room_id TEXT NOT NULL REFERENCES rooms(id), event_count INTEGER NOT NULL, head TEXT NOT NULL, PRIMARY KEY(actor, request_id)) STRICT;
      CREATE TABLE rejected_requests (actor TEXT NOT NULL, request_id TEXT NOT NULL, digest TEXT NOT NULL, expires_at INTEGER NOT NULL, status INTEGER NOT NULL, proof TEXT NOT NULL, PRIMARY KEY(actor, request_id)) STRICT;
      CREATE TABLE admissions (hash TEXT PRIMARY KEY, created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL, used_by TEXT) STRICT;`);
    const put = db.prepare('INSERT INTO metadata(key,value) VALUES (?,?)');
    put.run('schema', '1'); put.run('service_id', serviceId); put.run('private_key_pkcs8', privateKey); put.run('clock_floor', String(Date.now()));
    db.exec('COMMIT');
    return { serviceId };
  } catch (error) {
    if (db.isTransaction) db.exec('ROLLBACK');
    throw error;
  } finally { db.close(); }
}

export async function openStore(dbPath) {
  const file = path.resolve(dbPath);
  if (!existsSync(file)) throw new StoreError('INITIALIZATION_REQUIRED');
  const db = openDatabase(file);
  try {
    const metadata = Object.fromEntries(db.prepare('SELECT key,value FROM metadata').all().map(row => [row.key, row.value]));
    if (Object.keys(metadata).length !== 4 || metadata.schema !== '1' || !/^[a-f0-9]{64}$/.test(metadata.service_id) || !/^[a-f0-9]{96}$/.test(metadata.private_key_pkcs8) || !/^\d+$/.test(metadata.clock_floor) || !Number.isSafeInteger(Number(metadata.clock_floor))) throw new StoreError('STORAGE_VERIFICATION_FAILED');
    const key = createPrivateKey({ key: Buffer.from(metadata.private_key_pkcs8, 'hex'), format: 'der', type: 'pkcs8' });
    if (key.asymmetricKeyType !== 'ed25519' || createPublicKey(key).export({ format: 'der', type: 'spki' }).subarray(-32).toString('hex') !== metadata.service_id) throw new StoreError('STORAGE_VERIFICATION_FAILED');
    if (db.prepare('PRAGMA quick_check').all().some(row => row.quick_check !== 'ok') || db.prepare('PRAGMA foreign_key_check').all().length) throw new StoreError('STORAGE_VERIFICATION_FAILED');
    if (db.prepare('SELECT count(*) AS total FROM rooms').get().total > MAX_ROOMS || db.prepare('SELECT count(*) AS total FROM admissions').get().total > MAX_ADMISSIONS || db.prepare('SELECT count(*) AS total FROM requests').get().total > MAX_ROOMS * 256 || db.prepare('SELECT count(*) AS total FROM rejected_requests').get().total > 1024) throw new StoreError('STORAGE_VERIFICATION_FAILED');
    const privateKey = await crypto.subtle.importKey('pkcs8', Buffer.from(metadata.private_key_pkcs8, 'hex'), 'Ed25519', false, ['sign']);
    return { db, file, serviceId: metadata.service_id, privateKey, close: () => { if (db.isOpen) db.close(); } };
  } catch (error) { db.close(); throw error instanceof StoreError ? error : new StoreError('STORAGE_VERIFICATION_FAILED'); }
}

/** Operator-only provisioning. Tokens are never sent to a log or journal. */
export async function issueAdmission({ dbPath, outputPath, ttlMs = 24 * 60 * 60 * 1000, now = Date.now() }) {
  if (!Number.isSafeInteger(ttlMs) || ttlMs < 1000 || ttlMs > 7 * 24 * 60 * 60 * 1000 || !Number.isSafeInteger(now) || now < 0) throw new StoreError('INVALID_ADMISSION_EXPIRY');
  const store = await openStore(dbPath);
  const token = randomBytes(32).toString('hex');
  const hash = await credentialHash(token);
  try {
    // Reserve an output file before consuming a capacity slot. A failure later
    // leaves only an unusable token; it cannot authorize an uncommitted grant.
    if (outputPath) writeFileSync(path.resolve(outputPath), `${token}\n`, { flag: 'wx', mode: 0o600 });
    store.db.exec('BEGIN IMMEDIATE');
    if (store.db.prepare('SELECT count(*) AS total FROM admissions').get().total >= MAX_ADMISSIONS) throw new StoreError('ADMISSION_LIMIT');
    store.db.prepare('INSERT INTO admissions(hash,created_at,expires_at,used_by) VALUES (?,?,?,NULL)').run(hash, now, now + ttlMs);
    store.db.exec('COMMIT');
    if (outputPath) chmodSync(path.resolve(outputPath), 0o600);
    return { token, hash, expiresAt: now + ttlMs };
  } catch (error) {
    if (store.db.isTransaction) store.db.exec('ROLLBACK');
    throw error;
  } finally { store.close(); }
}
