import { createServer } from 'node:http';
import { readFile, realpath, stat } from 'node:fs/promises';
import { existsSync, closeSync, openSync } from 'node:fs';
import { backup } from 'node:sqlite';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { applyRequest, createEvent, createRejection, verifyRejection, verifyRejectionProof, MAX_EVENTS, MAX_HISTORY_BYTES, MAX_REQUEST_AGE_MS, publicRequest, replayRoom, requestHash, verifyRequest, ZERO_HASH } from './protocol.mjs';
import { loadEngine } from './engine.mjs';
import { initializeStore, issueAdmission, MAX_ROOMS, openStore, StoreError } from './store.mjs';

export { initializeStore, issueAdmission } from './store.mjs';
const BODY_LIMIT = 512 * 1024;
const HEX = /^[a-f0-9]{64}$/;
const MUTATIONS = new Set(['create_room', 'create_invite', 'revoke_invite', 'join_room', 'revoke_member', 'commit_ledger']);
class ServiceError extends Error {
  constructor(code, status = 400) { super(code); this.code = code; this.status = status; }
}
class RejectedRequest extends ServiceError {
  constructor(status, rejection) { super(rejection.code, status); this.rejection = rejection; }
}
function safeError(error) {
  if (error instanceof ServiceError) return error;
  if (error instanceof StoreError) return new ServiceError(error.code, 503);
  const code = typeof error?.code === 'string' ? error.code.toUpperCase() : undefined;
  if (['NOT_AUTHORIZED', 'NOT_A_MEMBER', 'MEMBER_REVOKED', 'LEADER_REQUIRED', 'FORBIDDEN', 'UNAUTHORIZED'].includes(code)) return new ServiceError('FORBIDDEN', 403);
  if (['CONFLICT', 'STALE_HEAD', 'HEAD_CONFLICT', 'INVITE_UNAVAILABLE', 'INVITE_EXISTS', 'MEMBER_UNAVAILABLE', 'MEMBER_EXISTS', 'ALREADY_MEMBER', 'ALREADY_EXISTS', 'REQUEST_REUSED', 'WRONG_PHASE', 'LIMIT_EXCEEDED'].includes(code)) return new ServiceError(code, 409);
  if (['INVALID_SIGNATURE', 'WRONG_SERVICE', 'EXPIRED_REQUEST', 'REQUEST_EXPIRED', 'INVALID_CREDENTIAL'].includes(code)) return new ServiceError('AUTHENTICATION_FAILED', 401);
  if (typeof code === 'string' && /^[A-Z_]{1,60}$/.test(code) && !code.startsWith('SQLITE')) return new ServiceError('INVALID_REQUEST', 400);
  return new ServiceError('SERVICE_UNAVAILABLE', 503);
}
function roomResponse(serviceId, events) { return { version: 1, serviceId, roomId: events[0].request.command.roomId, events }; }
function receiptResponse(serviceId, event) { return { version: 1, kind: 'receipt', serviceId, roomId: event.request.command.roomId, event }; }
function ownResponse(state, actor) {
  const member = state.members.find(value => value.publicKey === actor);
  if (!member) throw new ServiceError('ROOM_UNAVAILABLE', 403);
  return { version: 1, kind: 'own', serviceId: state.serviceId, roomId: state.roomId, creation: state.events[0], member,
    commit: state.events.findLast(event => event.request.actor === actor && event.request.command.type === 'commit_ledger') ?? null,
    revocation: state.events.findLast(event => event.request.command.type === 'revoke_member' && event.request.command.member === actor) ?? null };
}

async function verifiedState(store, engine, row) {
  if (!row || typeof row.events !== 'string' || Buffer.byteLength(row.events) > MAX_HISTORY_BYTES || !Number.isSafeInteger(row.event_count) || row.event_count < 1 || row.event_count > MAX_EVENTS) throw new StoreError('STORAGE_VERIFICATION_FAILED');
  try {
    const state = await replayRoom(engine, store.serviceId, JSON.parse(row.events), { count: row.event_count, head: row.head });
    if (state.roomId !== row.id || state.events.length !== row.event_count) throw new Error('Metadata differs');
    return state;
  } catch { throw new StoreError('STORAGE_VERIFICATION_FAILED'); }
}
async function verifyStoreHistories(store, engine) {
  for (const row of store.db.prepare('SELECT id,head,event_count,events FROM rooms').all()) {
    const state = await verifiedState(store, engine, row);
    const admission = store.db.prepare('SELECT used_by FROM admissions WHERE hash=?').get(state.events[0].request.command.credentialHash);
    if (admission?.used_by !== row.id) throw new StoreError('STORAGE_VERIFICATION_FAILED');
    const cached = store.db.prepare('SELECT actor,request_id,digest,event_count,head FROM requests WHERE room_id=?').all(row.id);
    if (cached.length !== state.events.length) throw new StoreError('STORAGE_VERIFICATION_FAILED');
    for (const entry of cached) {
      const event = state.events[entry.event_count - 1];
      if (!event || event.hash !== entry.head || event.request.actor !== entry.actor || event.request.requestId !== entry.request_id || await requestHash(event.request) !== entry.digest) throw new StoreError('STORAGE_VERIFICATION_FAILED');
    }
  }
  for (const admission of store.db.prepare('SELECT hash,created_at,expires_at,used_by FROM admissions').all()) {
    if (!HEX.test(admission.hash) || !Number.isSafeInteger(admission.created_at) || !Number.isSafeInteger(admission.expires_at) || admission.created_at < 0 || admission.expires_at <= admission.created_at || (admission.used_by !== null && !store.db.prepare('SELECT id FROM rooms WHERE id=?').get(admission.used_by))) throw new StoreError('STORAGE_VERIFICATION_FAILED');
  }
  for (const row of store.db.prepare('SELECT actor,request_id,digest,expires_at,status,proof FROM rejected_requests').all()) {
    try {
      if (row.proof.length > 2048 || !Number.isSafeInteger(row.expires_at) || row.expires_at < 0 || ![400, 401, 403, 404, 409].includes(row.status)) throw new Error('Invalid rejection');
      const proof = await verifyRejectionProof(JSON.parse(row.proof), store.serviceId);
      if (proof.actor !== row.actor || proof.requestId !== row.request_id || proof.requestHash !== row.digest || proof.expiresAt !== row.expires_at || store.db.prepare('SELECT 1 FROM requests WHERE actor=? AND request_id=?').get(row.actor, row.request_id)) throw new Error('Invalid rejection binding');
    } catch { throw new StoreError('STORAGE_VERIFICATION_FAILED'); }
  }
}

/** SQLite online backup, never a copy of a live database/WAL file pair. */
export async function backupDatabase({ dbPath, outputPath }) {
  if (!outputPath || path.resolve(outputPath) === path.resolve(dbPath)) throw new StoreError('BACKUP_DESTINATION_REQUIRED');
  const source = await openStore(dbPath);
  try {
    const engine = await loadEngine();
    await verifyStoreHistories(source, engine);
    // Exclusive creation prevents clobbering a prior backup or following a link.
    closeSync(openSync(path.resolve(outputPath), 'wx', 0o600));
    await backup(source.db, path.resolve(outputPath));
    const restored = await openStore(outputPath);
    try {
      if (restored.serviceId !== source.serviceId) throw new StoreError('STORAGE_VERIFICATION_FAILED');
      await verifyStoreHistories(restored, engine);
      return { serviceId: restored.serviceId };
    } finally { restored.close(); }
  } finally { source.close(); }
}

class RateLimiter {
  constructor(limit, capacity = 1024, interval = 60_000) { this.limit = limit; this.capacity = capacity; this.interval = interval; this.buckets = new Map(); }
  accept(key, now) {
    let bucket = this.buckets.get(key);
    if (bucket && now >= bucket.until) { this.buckets.delete(key); bucket = undefined; }
    if (!bucket) {
      if (this.buckets.size >= this.capacity) for (const [entry, value] of this.buckets) if (now >= value.until) this.buckets.delete(entry);
      if (this.buckets.size >= this.capacity) return false;
      bucket = { until: now + this.interval, count: 0 }; this.buckets.set(key, bucket);
    }
    return ++bucket.count <= this.limit;
  }
}

async function readBody(request) {
  const length = request.headers['content-length'];
  if (length !== undefined && (!/^\d+$/.test(length) || Number(length) > BODY_LIMIT)) throw new ServiceError('BODY_TOO_LARGE', 413);
  return new Promise((resolve, reject) => {
    let size = 0; const chunks = []; let finished = false;
    const finish = (error, value) => { if (finished) return; finished = true; clearTimeout(timer); error ? reject(error) : resolve(value); };
    const timer = setTimeout(() => { request.pause(); finish(new ServiceError('REQUEST_TIMEOUT', 408)); }, 5000);
    request.on('data', chunk => {
      size += chunk.length;
      if (size > BODY_LIMIT) { request.pause(); finish(new ServiceError('BODY_TOO_LARGE', 413)); return; }
      if (!finished) chunks.push(chunk);
    });
    request.on('end', () => {
      try { finish(null, JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(Buffer.concat(chunks)))); }
      catch { finish(new ServiceError('INVALID_JSON')); }
    });
    request.on('aborted', () => finish(new ServiceError('REQUEST_ABORTED')));
    request.on('error', () => finish(new ServiceError('REQUEST_ABORTED')));
  });
}

/** Long-running, persistent-volume service. No listener or database is created on import. */
export async function startServer({ dbPath, origin: configuredOrigin, port = 0, host = '127.0.0.1', appDist, initialize = false, now = Date.now, testHooks, rateLimit = 120 } = {}) {
  if (!dbPath) throw new StoreError('DATABASE_PATH_REQUIRED');
  if (initialize) initializeStore(dbPath);
  const store = await openStore(dbPath);
  let server;
  try {
    const engine = await loadEngine();
    const originUrl = configuredOrigin ? new URL(configuredOrigin) : null;
    if (originUrl && (originUrl.origin !== configuredOrigin || originUrl.username || originUrl.password || !['http:', 'https:'].includes(originUrl.protocol))) throw new Error('Use a canonical HTTP(S) origin without a path.');
    if (originUrl?.protocol === 'http:' && !['127.0.0.1', 'localhost', '[::1]'].includes(originUrl.hostname)) throw new Error('A non-loopback deployment requires an HTTPS origin.');
    if (!originUrl && host !== '127.0.0.1' && host !== '::1') throw new Error('Non-loopback binding requires an explicit HTTPS origin.');
    const staticRoot = appDist ? await realpath(path.resolve(appDist)) : null;
    if (staticRoot && !existsSync(path.join(staticRoot, 'index.html'))) throw new Error('The complete Expo export is required.');
    const rowFor = id => store.db.prepare('SELECT id,head,event_count,events FROM rooms WHERE id=?').get(id);
    const stateFor = row => verifiedState(store, engine, row);
    // Reopen only after all existing histories and durable receipt indexes pass.
    await verifyStoreHistories(store, engine);
    let queue = Promise.resolve();
    let active = 0;
    const ipLimit = new RateLimiter(rateLimit);
    const actorLimit = new RateLimiter(rateLimit);
    const cachedFor = request => store.db.prepare('SELECT digest,room_id,event_count,head FROM requests WHERE actor=? AND request_id=?').get(request.actor, request.requestId);
    const currentTime = () => Math.max(now(), Number(store.db.prepare("SELECT value FROM metadata WHERE key='clock_floor'").get().value));
    const retainClock = time => store.db.prepare("UPDATE metadata SET value=CAST(MAX(CAST(value AS INTEGER), CAST(? AS INTEGER)) AS TEXT) WHERE key='clock_floor'").run(time);
    const rejectedFor = request => store.db.prepare('SELECT digest,expires_at,status,proof FROM rejected_requests WHERE actor=? AND request_id=?').get(request.actor, request.requestId);
    async function cachedResult(entry, request, digest) {
      if (entry.digest !== digest || entry.room_id !== request.command.roomId) throw new ServiceError('REQUEST_REUSED', 409);
      const state = await stateFor(rowFor(entry.room_id));
      const event = state.events[entry.event_count - 1];
      if (!event || event.hash !== entry.head || event.request.actor !== request.actor || event.request.requestId !== request.requestId || await requestHash(event.request) !== digest) throw new StoreError('STORAGE_VERIFICATION_FAILED');
      // Signed mutation requests are public evidence in exports. Replaying one
      // must never act as a credential to read other members' room history.
      // Return only its already-public event; reading requires a fresh request.
      return receiptResponse(store.serviceId, event);
    }
    async function rejectedResult(entry, request, digest) {
      if (entry.digest !== digest) throw new ServiceError('REQUEST_REUSED', 409);
      try {
        const proof = await verifyRejection(JSON.parse(entry.proof), store.serviceId, request);
        if (![400, 401, 403, 404, 409].includes(entry.status) || entry.expires_at !== request.expiresAt) throw new Error('Invalid rejection');
        return new RejectedRequest(entry.status, proof);
      } catch { throw new StoreError('STORAGE_VERIFICATION_FAILED'); }
    }
    async function rejectDurably(request, digest, error) {
      // Only a valid, request-bound signed mutation reaches this function. The
      // durable tombstone prevents this rejected packet executing later if an
      // invite appears or permissions change after the client discards it.
      const proof = await createRejection({ version: 1, serviceId: store.serviceId, actor: request.actor, requestId: request.requestId, requestHash: digest, expiresAt: request.expiresAt, code: error.code }, store.privateKey);
      let accepted, rejected;
      try {
        store.db.exec('BEGIN IMMEDIATE');
        accepted = cachedFor(request);
        rejected = rejectedFor(request);
        if (!accepted && !rejected) {
          // The service clock is trusted. Keep the tombstone beyond the packet's
          // validity window; an expired packet cannot become valid on retry.
          const time = currentTime();
          store.db.prepare('DELETE FROM rejected_requests WHERE expires_at < ?').run(time - 30_000);
          if (store.db.prepare('SELECT count(*) AS total FROM rejected_requests').get().total >= 1024) throw new StoreError('REJECTION_CAPACITY');
          store.db.prepare('INSERT INTO rejected_requests(actor,request_id,digest,expires_at,status,proof) VALUES (?,?,?,?,?,?)').run(request.actor, request.requestId, digest, request.expiresAt, error.status, JSON.stringify(proof));
          retainClock(time);
        }
        store.db.exec('COMMIT');
      } catch (cause) {
        if (store.db.isTransaction) store.db.exec('ROLLBACK');
        throw cause;
      }
      // A competing instance might have accepted the packet before this
      // transaction obtained its lock. That receipt wins over any rejection.
      if (accepted) return cachedResult(accepted, request, digest);
      if (rejected) throw await rejectedResult(rejected, request, digest);
      throw new RejectedRequest(error.status, proof);
    }
    async function execute(packet) {
      // Clock is checked only after looking up an authenticated prior receipt.
      // A lost acknowledgement remains recoverable after request expiration.
      const request = await verifyRequest(publicRequest(packet), { serviceId: store.serviceId });
      if (!actorLimit.accept(request.actor, currentTime())) throw new ServiceError('RATE_LIMITED', 429);
      const digest = await requestHash(request);
      const oldReceipt = cachedFor(request);
      if (oldReceipt) return cachedResult(oldReceipt, request, digest);
      const oldRejection = rejectedFor(request);
      if (oldRejection) throw await rejectedResult(oldRejection, request, digest);
      // Do not allocate long-lived tombstones for arbitrarily future-dated
      // signed packets. Their browser must retain uncertainty and fix its clock.
      if (request.issuedAt > currentTime() + 30_000 || request.expiresAt > currentTime() + MAX_REQUEST_AGE_MS + 30_000) throw new ServiceError('CLOCK_OUT_OF_RANGE', 401);
      try { return await executeFresh(packet, request, digest); }
      catch (error) {
        const safe = safeError(error);
        if (MUTATIONS.has(request.command.type) && [400, 401, 403, 404, 409].includes(safe.status)) return rejectDurably(request, digest, safe);
        throw error;
      }
    }
    async function executeFresh(packet, request, digest) {
      await verifyRequest(packet, { serviceId: store.serviceId, now: currentTime(), requireCredential: ['create_room', 'join_room'].includes(request.command.type) });
      const command = request.command;
      const row = rowFor(command.roomId);
      const state = row ? await stateFor(row) : null;
      if (!MUTATIONS.has(command.type)) {
        const member = state?.members.find(value => value.publicKey === request.actor);
        if (!member || (command.type === 'get_room' && member.status !== 'active')) throw new ServiceError('ROOM_UNAVAILABLE', 403);
        return command.type === 'get_my_record' ? ownResponse(state, request.actor) : roomResponse(store.serviceId, state.events);
      }
      if (!state && command.type !== 'create_room') throw new ServiceError('ROOM_UNAVAILABLE', 403);
      await applyRequest(engine, state, request);
      const event = await createEvent(publicRequest(request), (state?.events.length ?? 0) + 1, state?.head ?? ZERO_HASH, store.privateKey);
      const events = [...(state?.events ?? []), event];
      const json = JSON.stringify(events);
      if (Buffer.byteLength(json) > MAX_HISTORY_BYTES) throw new ServiceError('ROOM_LIMIT', 409);
      // Everything asynchronous is above this boundary. The SQLite write lock
      // covers membership/admission consumption, CAS, journal and receipt index.
      let racedReceipt;
      try {
        store.db.exec('BEGIN IMMEDIATE');
        racedReceipt = cachedFor(request);
        if (rejectedFor(request)) throw new ServiceError('REQUEST_REJECTED', 409);
        if (!racedReceipt) {
          const current = rowFor(command.roomId);
          if ((current?.head ?? null) !== (row?.head ?? null) || (current?.events ?? null) !== (row?.events ?? null)) throw new ServiceError('STALE_ROOM', 409);
          if (command.type === 'create_room') {
            if (current) throw new ServiceError('ROOM_EXISTS', 409);
            if (store.db.prepare('SELECT count(*) AS total FROM rooms').get().total >= MAX_ROOMS) throw new ServiceError('ROOM_LIMIT', 409);
            const admission = store.db.prepare('SELECT expires_at,used_by FROM admissions WHERE hash=?').get(command.credentialHash);
            if (!admission || admission.used_by !== null || admission.expires_at < currentTime()) throw new ServiceError('ADMISSION_UNAVAILABLE', 403);
            store.db.prepare('UPDATE admissions SET used_by=? WHERE hash=? AND used_by IS NULL').run(command.roomId, command.credentialHash);
            store.db.prepare('INSERT INTO rooms(id,head,event_count,events) VALUES (?,?,?,?)').run(command.roomId, event.hash, events.length, json);
          } else {
            const changed = store.db.prepare('UPDATE rooms SET head=?,event_count=?,events=? WHERE id=? AND head=?').run(event.hash, events.length, json, command.roomId, state.head);
            if (changed.changes !== 1) throw new ServiceError('STALE_ROOM', 409);
          }
          store.db.prepare('INSERT INTO requests(actor,request_id,digest,room_id,event_count,head) VALUES (?,?,?,?,?,?)').run(request.actor, request.requestId, digest, command.roomId, events.length, event.hash);
          retainClock(currentTime());
          // Test hook deliberately runs synchronously inside the transaction.
          // The production CLI never accepts hooks or arbitrary executable input.
          testHooks?.beforeCommit?.({ request });
        }
        store.db.exec('COMMIT');
      } catch (error) {
        if (store.db.isTransaction) store.db.exec('ROLLBACK');
        throw error;
      }
      if (racedReceipt) return cachedResult(racedReceipt, request, digest);
      testHooks?.afterCommit?.({ request });
      return receiptResponse(store.serviceId, event);
    }
    let expectedOrigin;
    const headers = { 'Content-Type': 'application/json; charset=utf-8', 'Cache-Control': 'no-store', 'X-Content-Type-Options': 'nosniff', 'Referrer-Policy': 'no-referrer', 'Content-Security-Policy': "default-src 'none'; frame-ancestors 'none'", 'X-Frame-Options': 'DENY' };
    function reply(response, status, body, close = false) {
      response.writeHead(status, { ...headers, ...(close ? { Connection: 'close' } : {}) });
      response.end(JSON.stringify(body));
    }
    server = createServer(async (request, response) => {
      if (active >= 32) { reply(response, 503, { error: { code: 'SERVICE_BUSY' } }, true); return; }
      active++;
      let released = false;
      const release = () => { if (!released) { released = true; active--; } };
      response.once('finish', release); response.once('close', release);
      try {
        if (!expectedOrigin || request.headers.host !== new URL(expectedOrigin).host || (request.headers.origin && request.headers.origin !== expectedOrigin)) throw new ServiceError('ORIGIN_REJECTED', 403);
        if (!ipLimit.accept(request.socket.remoteAddress ?? 'unknown', currentTime())) throw new ServiceError('RATE_LIMITED', 429);
        // Canonical browser URLs include the slash so a hosting-layer 308 cannot
        // redirect a signed mutation. Retain only the exact old aliases locally.
        if (request.method === 'GET' && ['/api/rooms/v1/info/', '/api/rooms/v1/info'].includes(request.url)) { reply(response, 200, { version: 1, serviceId: store.serviceId }); return; }
        if (request.method === 'POST' && ['/api/rooms/v1/command/', '/api/rooms/v1/command'].includes(request.url)) {
          if (request.headers.origin !== expectedOrigin || request.headers.cookie || request.headers.authorization || request.headers['content-encoding'] || !/^application\/json(?:;\s*charset=utf-8)?$/i.test(request.headers['content-type'] ?? '')) throw new ServiceError('INVALID_TRANSPORT', 400);
          const packet = await readBody(request);
          const operation = queue.then(() => execute(packet));
          queue = operation.catch(() => {});
          const result = await operation;
          reply(response, 200, result); return;
        }
        if (request.method === 'GET' && staticRoot && /^\/app(?:\/|$)/.test(request.url)) {
          const parsed = new URL(request.url, expectedOrigin);
          const pathname = decodeURIComponent(parsed.pathname);
          let file = path.resolve(staticRoot, pathname.replace(/^\/app\/?/, '') || 'index.html');
          if (file !== staticRoot && !file.startsWith(staticRoot + path.sep)) throw new ServiceError('NOT_FOUND', 404);
          const info = await stat(file).catch(() => null);
          if (!info?.isFile()) {
            if (path.extname(pathname)) throw new ServiceError('NOT_FOUND', 404);
            file = path.join(staticRoot, 'index.html');
          }
          file = await realpath(file);
          if (!file.startsWith(staticRoot + path.sep)) throw new ServiceError('NOT_FOUND', 404);
          if ((await stat(file)).size > 24 * 1024 * 1024) throw new ServiceError('NOT_FOUND', 404);
          const types = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.json': 'application/json', '.wasm': 'application/wasm', '.png': 'image/png', '.webp': 'image/webp', '.jpg': 'image/jpeg', '.svg': 'image/svg+xml', '.ttf': 'font/ttf', '.woff2': 'font/woff2' };
          response.writeHead(200, { 'Content-Type': types[path.extname(file)] ?? 'application/octet-stream', 'Cache-Control': 'no-store', 'X-Content-Type-Options': 'nosniff', 'Referrer-Policy': 'no-referrer' });
          response.end(await readFile(file)); return;
        }
        throw new ServiceError('NOT_FOUND', 404);
      } catch (error) {
        const safe = safeError(error);
        if (!response.headersSent && !response.destroyed) reply(response, safe.status, { error: { code: safe.code, ...(safe.rejection ? { rejection: safe.rejection } : {}) } }, [408, 413].includes(safe.status));
      }
    });
    server.headersTimeout = 10_000; server.requestTimeout = 15_000; server.keepAliveTimeout = 5000;
    server.maxHeadersCount = 32;
    server.maxConnections = 64;
    server.setTimeout(15_000, socket => socket.destroy());
    await new Promise((resolve, reject) => { server.once('error', reject); server.listen(port, host, resolve); });
    expectedOrigin = configuredOrigin ?? `http://${host.includes(':') ? `[${host}]` : host}:${server.address().port}`;
    return { origin: expectedOrigin, serviceId: store.serviceId, port: server.address().port,
      close: async () => { await new Promise(resolve => server.close(resolve)); await queue; store.close(); } };
  } catch (error) { if (server?.listening) await new Promise(resolve => server.close(resolve)); store.close(); throw error; }
}

async function cli() {
  const args = process.argv.slice(2); const action = args.shift();
  const options = {};
  while (args.length) { const key = args.shift(); const value = args.shift(); if (!['--db', '--out', '--origin', '--port', '--host', '--app-dist'].includes(key) || !value || Object.hasOwn(options, key)) throw new Error('Invalid arguments.'); options[key] = value; }
  if (!options['--db']) throw new Error('Supply --db /absolute/path/to/private/rooms.sqlite');
  if (action === 'init') { initializeStore(options['--db']); console.log('Initialized a private community paper database.'); return; }
  if (action === 'issue-admission') {
    if (!options['--out']) throw new Error('Supply --out /private/path/token.txt; tokens are never printed.');
    await issueAdmission({ dbPath: options['--db'], outputPath: options['--out'] }); console.log('Wrote one admission token to the requested private file.'); return;
  }
  if (action === 'backup') {
    if (!options['--out']) throw new Error('Supply --out /private/path/new-backup.sqlite');
    await backupDatabase({ dbPath: options['--db'], outputPath: options['--out'] }); console.log('Verified a new private SQLite backup.'); return;
  }
  if (action !== 'serve') throw new Error('Usage: node community/server.mjs init|issue-admission|backup|serve --db <path> [--out <path>|--port <port> --origin <origin> --app-dist <export>]');
  const port = options['--port'] === undefined ? 8090 : Number(options['--port']);
  if (!Number.isSafeInteger(port) || port < 1 || port > 65535) throw new Error('Invalid port.');
  const instance = await startServer({ dbPath: options['--db'], port, host: options['--host'] ?? '127.0.0.1', origin: options['--origin'], appDist: options['--app-dist'] });
  console.log(`Community paper service listening at ${instance.origin}`);
  let closing = false;
  const stop = async () => { if (closing) return; closing = true; await instance.close(); process.exit(0); };
  process.on('SIGTERM', stop); process.on('SIGINT', stop);
}
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) cli().catch(error => { console.error(error instanceof StoreError ? error.code : 'Community service could not start; check its private storage and arguments.'); process.exitCode = 1; });
