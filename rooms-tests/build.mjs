import { createRequire } from 'node:module';
import { mkdirSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const integration = path.join(root, 'integrations/sakura-rooms');
const require = createRequire(path.join(integration, 'package.json'));
const { build } = require('esbuild');
mkdirSync(path.join(root, 'target/rooms-harness'), { recursive: true });
await build({ entryPoints: [path.join(integration, 'sakura-mobile/lib/rooms-client.web.ts')], bundle: true,
  format: 'iife', globalName: 'roomsClient', target: 'es2022', outfile: path.join(root, 'target/rooms-harness/rooms-client.js') });
writeFileSync(path.join(root, 'target/rooms-harness/rooms-harness.html'), '<!doctype html><html><head><meta charset="utf-8"><title>Rooms recovery test harness</title></head><body><script src="/app/rooms-client.js"></script></body></html>');
await build({ entryPoints: [path.join(integration, 'test-adapters/entry.tsx')], bundle: true,
  format: 'iife', target: 'es2022', jsx: 'automatic', tsconfig: path.join(integration, 'tsconfig.json'),
  outfile: path.join(root, 'target/rooms-harness/rooms-screen.js') });
writeFileSync(path.join(root, 'target/rooms-harness/rooms-screen.html'), '<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>Rooms component integration</title><link rel="stylesheet" href="/app/rooms-screen.css"></head><body style="margin:0"><div id="root"></div><script src="/app/rooms-screen.js"></script></body></html>');
