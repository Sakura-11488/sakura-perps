import test from 'node:test';
import { runCommunityBrowserFlow } from './community-flow.mjs';

test('Community screens: separate identities, own approvals, shared results and durable restart/lost-ack recovery', { timeout: 180_000 }, async () => {
  await runCommunityBrowserFlow();
});
