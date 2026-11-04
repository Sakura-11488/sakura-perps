// Browser-local paper state only. Never store a connected wallet or real funds.
export const ROOMS_DATABASE = 'sakura-rooms-paper-v1';
export const ROOMS_STORE = 'rooms';
export const MAX_LOCAL_ROOMS = 24;

export class RoomsStorageError extends Error {}

async function database(): Promise<IDBDatabase> {
  if (typeof indexedDB === 'undefined') throw new RoomsStorageError('This browser cannot save a paper room. Use a browser with IndexedDB.');
  return new Promise((resolve, reject) => {
    let failed = false;
    const request = indexedDB.open(ROOMS_DATABASE, 1);
    request.onupgradeneeded = () => request.result.createObjectStore(ROOMS_STORE, { keyPath: 'id' });
    request.onerror = () => { failed = true; reject(new RoomsStorageError('Local storage could not be opened. Your saved rooms have not been reset.')); };
    request.onblocked = () => { failed = true; reject(new RoomsStorageError('Another tab is holding an older room database open. Close that tab and recover again.')); };
    request.onsuccess = () => {
      const db = request.result;
      if (failed) { db.close(); return; }
      db.onversionchange = () => db.close();
      resolve(db);
    };
  });
}

export async function readRooms(): Promise<unknown[]> {
  const db = await database();
  try {
    return await new Promise((resolve, reject) => {
      const tx = db.transaction(ROOMS_STORE, 'readonly');
      const request = tx.objectStore(ROOMS_STORE).getAll(undefined, MAX_LOCAL_ROOMS + 1);
      let values: unknown[] = [];
      request.onsuccess = () => { values = request.result; };
      tx.oncomplete = () => values.length > MAX_LOCAL_ROOMS
        ? reject(new RoomsStorageError('The local room limit was exceeded. Saved data has been preserved.'))
        : resolve(values);
      tx.onabort = tx.onerror = () => reject(new RoomsStorageError('Saved rooms could not be read. Recover again; no room has been reset.'));
    });
  } finally { db.close(); }
}

export async function readRoom(id: string): Promise<unknown> {
  const db = await database();
  try {
    return await new Promise((resolve, reject) => {
      const tx = db.transaction(ROOMS_STORE, 'readonly');
      const request = tx.objectStore(ROOMS_STORE).get(id);
      tx.oncomplete = () => resolve(request.result);
      tx.onabort = tx.onerror = () => reject(new RoomsStorageError('This room could not be read. Recover again before continuing.'));
    });
  } finally { db.close(); }
}

/** All signing/replay happens before opening this transaction. No async gaps. */
export async function commitRoom<T extends { id: string }>(
  next: T,
  expected: string | null,
  fingerprint: (value: unknown) => string,
): Promise<void> {
  const db = await database();
  try {
    await new Promise<void>((resolve, reject) => {
      let tx: IDBTransaction;
      try {
        tx = db.transaction(ROOMS_STORE, 'readwrite', { durability: 'strict' });
        if (tx.durability !== 'strict') {
          tx.abort();
          throw new Error('Strict durability is unavailable');
        }
      } catch {
        reject(new RoomsStorageError('This browser cannot confirm durable saves. No paper action was accepted.'));
        return;
      }
      let reason = 'Save could not be confirmed. Recover saved rooms before trying again.';
      const store = tx.objectStore(ROOMS_STORE);
      const fail = (message: string) => { reason = message; tx.abort(); };
      tx.oncomplete = () => resolve();
      tx.onabort = () => reject(new RoomsStorageError(reason));
      // onabort is the final outcome; prevent an unhandled request error from
      // masquerading as a completed write. Never acknowledge request.onsuccess.
      tx.onerror = () => { reason = 'Storage rejected the save. Recover saved rooms before trying again.'; };
      if (expected === null) {
        const count = store.count();
        count.onsuccess = () => {
          if (count.result >= MAX_LOCAL_ROOMS) { fail('This device has reached its 24-room pilot limit. Export your records; existing rooms are preserved.'); return; }
          store.add(next);
        };
      } else {
        const previous = store.get(next.id);
        previous.onsuccess = () => {
          try {
            if (fingerprint(previous.result) !== expected) {
              fail('This room changed in another tab. Recover saved rooms and review the latest state.');
              return;
            }
            store.put(next);
          } catch {
            fail('Saved room verification failed. The existing record has been preserved.');
          }
        };
      }
    });
  } finally { db.close(); }
}
