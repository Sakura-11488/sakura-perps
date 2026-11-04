import test from 'node:test';
import path from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath, pathToFileURL } from 'node:url';

// Run the actual full Expo export through the real durable community service.
// No mocks of signatures, membership, paper accounting or persistence.
const root = fileURLToPath(new URL('../../', import.meta.url));
const perps = process.env.SAKURA_PERPS_ROOT || path.resolve(root, '../sakura-perps');
const { runCommunityBrowserFlow } = await import(pathToFileURL(path.join(perps, 'rooms-tests/community-flow.mjs')).href);

test('Full Expo community beta: two browsers, own approvals, service restart, lost acknowledgement and revocation', { timeout: 180_000 }, async () => {
  await runCommunityBrowserFlow({
    appDist: path.resolve(process.env.SAKURA_EXPORT_DIST || path.join(root, 'web/dist')),
    artifacts: path.resolve(process.env.SAKURA_COMMUNITY_ARTIFACTS || path.join(tmpdir(), 'sakura-community-ui-artifacts')),
    fullApp: true,
  });
});
