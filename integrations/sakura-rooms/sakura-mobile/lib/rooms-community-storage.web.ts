// Separate from the local paper pilot. One strict transaction owns identity,
// pending delivery and independently retained server-history checkpoints.
export const COMMUNITY_DATABASE = 'sakura-rooms-community-v1';
const STORE = 'profile';
async function open(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    let failed = false;
    const request = indexedDB.open(COMMUNITY_DATABASE, 1);
    request.onupgradeneeded = () => request.result.createObjectStore(STORE, { keyPath: 'id' });
    request.onerror = request.onblocked = () => { failed = true; reject(new Error('Community storage is unavailable. Saved identity and history were not reset.')); };
    request.onsuccess = () => {
      if (failed) { request.result.close(); return; }
      request.result.onversionchange = () => request.result.close();
      resolve(request.result);
    };
  });
}
export async function readCommunityProfile(): Promise<unknown> {
  const db = await open();
  try {
    return await new Promise((resolve, reject) => {
      const tx = db.transaction(STORE, 'readonly');
      const request = tx.objectStore(STORE).get('identity');
      tx.oncomplete = () => resolve(request.result);
      tx.onabort = tx.onerror = () => reject(new Error('Community history could not be read. Saved data has been preserved.'));
    });
  } finally { db.close(); }
}
export async function writeCommunityProfile<T extends { id: 'identity'; revision: number }>(next: T, expectedRevision: number | null): Promise<void> {
  const db = await open();
  try {
    await new Promise<void>((resolve, reject) => {
      const tx = db.transaction(STORE, 'readwrite', { durability: 'strict' });
      if (tx.durability !== 'strict') { tx.abort(); reject(new Error('Strict durable storage is required before sending a paper action.')); return; }
      let message = 'The save was not confirmed. Recover the pending request before continuing.';
      tx.oncomplete = () => resolve();
      tx.onabort = () => reject(new Error(message));
      const store = tx.objectStore(STORE);
      const read = store.get('identity');
      read.onsuccess = () => {
        if ((read.result?.revision ?? null) !== expectedRevision) {
          message = 'Another tab changed your community state. Reload and review it before continuing.';
          tx.abort(); return;
        }
        store.put(next);
      };
    });
  } finally { db.close(); }
}
