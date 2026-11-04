import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
export async function serveRoomsHarness() {
  const server = createServer(async (req, res) => {
    const pathname = new URL(req.url, 'http://localhost').pathname;
    let file;
    if (['/app/rooms-harness.html', '/app/rooms-client.js', '/app/rooms-screen.html', '/app/rooms-screen.js', '/app/rooms-screen.css'].includes(pathname)) file = path.join(root, 'target/rooms-harness', pathname.split('/').at(-1));
    else if (/^\/app\/rooms-engine\/rooms-[a-f0-9]{20}\.(js|wasm)$/.test(pathname)) file = path.join(root, 'integrations/sakura-rooms/web/public', pathname.slice('/app/'.length));
    else { res.writeHead(404).end(); return; }
    try {
      const content = await readFile(file);
      const type = file.endsWith('.wasm') ? 'application/wasm' : file.endsWith('.js') ? 'application/javascript' : file.endsWith('.css') ? 'text/css' : 'text/html';
      res.writeHead(200, { 'Content-Type': type, 'Cache-Control': 'no-store', 'X-Content-Type-Options': 'nosniff' }); res.end(content);
    } catch { res.writeHead(404).end(); }
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  return { origin: `http://127.0.0.1:${server.address().port}`, close: () => new Promise(resolve => server.close(resolve)) };
}
