// Node adapter for the same content-pinned engine served to the browser.
import { readFile } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { createContext, runInContext } from 'node:vm';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
let loading;
export async function loadEngine() {
  if (loading) return loading;
  loading = (async () => {
    const directory = path.join(root, 'integrations/sakura-rooms');
    const manifest = JSON.parse(await readFile(path.join(directory, 'engine-manifest.json'), 'utf8'));
    if (manifest.schema !== 1 || manifest.semanticVersion !== 'sakura-rooms-paper/1'
        || !/^rooms-[a-f0-9]{20}\.js$/.test(manifest.javascript) || !/^rooms-[a-f0-9]{20}\.wasm$/.test(manifest.wasm)) throw new Error('Invalid engine manifest');
    const [javascript, wasm] = await Promise.all([manifest.javascript, manifest.wasm].map(file => readFile(path.join(directory, 'web/public/rooms-engine', file))));
    const jsHash = createHash('sha256').update(javascript).digest();
    const wasmHash = createHash('sha256').update(wasm).digest('hex');
    if (javascript.length > 512 * 1024 || wasm.length > 3 * 1024 * 1024 || `sha256-${jsHash.toString('base64')}` !== manifest.integrity
        || wasmHash !== manifest.sha256 || manifest.javascript !== `rooms-${jsHash.toString('hex').slice(0, 20)}.js`
        || manifest.wasm !== `rooms-${wasmHash.slice(0, 20)}.wasm`) throw new Error('Engine integrity check failed');
    // Only repository-generated, manifest-pinned JS is evaluated. No user input
    // is code, and no network/file/process APIs are exposed in its realm.
    const context = createContext({ TextDecoder, TextEncoder, WebAssembly, Uint8Array, ArrayBuffer, console });
    runInContext(javascript.toString('utf8'), context, { timeout: 2000, filename: manifest.javascript });
    context.__roomsWasm = Uint8Array.from(wasm);
    await runInContext('sakura_rooms_engine({ module_or_path: __roomsWasm })', context, { timeout: 2000 });
    delete context.__roomsWasm;
    return context.sakura_rooms_engine;
  })().catch(error => { loading = undefined; throw error; });
  return loading;
}
