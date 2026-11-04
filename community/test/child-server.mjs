// Process-crash fixture. This file is never imported by the production service.
import { startServer } from '../server.mjs';
const [dbPath, crashPhase] = process.argv.slice(2);
const crash = () => process.kill(process.pid, 'SIGKILL');
const server = await startServer({ dbPath, testHooks: crashPhase ? { [crashPhase]: crash } : undefined });
process.send({ origin: server.origin, serviceId: server.serviceId });
process.on('message', async message => { if (message === 'stop') { await server.close(); process.exit(0); } });
