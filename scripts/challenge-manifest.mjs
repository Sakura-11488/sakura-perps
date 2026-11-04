// Public build metadata only. No environment dump, wallet access or RPC calls.
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const arguments_ = process.argv.slice(2);
if (arguments_.length > 1 || (arguments_.length === 1 && !['--rooms', '--full'].includes(arguments_[0]))) {
  throw new Error('Usage: node scripts/challenge-manifest.mjs [--rooms|--full]');
}
const profile = arguments_[0]?.slice(2) ?? 'program';
const git = (...args) => execFileSync('git', args, { cwd: root, encoding: 'utf8' }).trim();
const hash = (file) => createHash('sha256').update(readFileSync(path.join(root, file))).digest('hex');
const artifacts = profile === 'rooms' ? [] : ['target/deploy/sakura_perps.so', 'target/idl/sakura_perps.json', 'target/types/sakura_perps.ts'];
if (profile !== 'program') {
  // Verify the explicit public asset allowlist and hashes before recording it.
  // The browser profile needs no Solana installation or application checkout.
  execFileSync(process.execPath, [path.join(root, 'scripts/package-rooms.mjs'), 'check'], { cwd: root });
  const engineFile = 'integrations/sakura-rooms/engine-manifest.json';
  const engine = JSON.parse(readFileSync(path.join(root, engineFile), 'utf8'));
  if (!/^rooms-[a-f0-9]{20}\.js$/.test(engine.javascript) || !/^rooms-[a-f0-9]{20}\.wasm$/.test(engine.wasm)) {
    throw new Error('Invalid browser engine artifact names');
  }
  artifacts.push(engineFile, ...[engine.javascript, engine.wasm].map(file => `integrations/sakura-rooms/web/public/rooms-engine/${file}`));
}
const sourcePaths = git('ls-files', '--cached', '--others', '--exclude-standard', '-z')
  .split('\0').filter(Boolean)
  .filter(file => /^(programs|crates|community|idl|scripts|tests|rooms-tests|integrations\/sakura-rooms|keeper\/src|keeper\/test|dashboard\/app|dashboard\/lib|dashboard\/test|\.github\/workflows)\//.test(file)
    || /^keeper\/[^/]+\.ts$/.test(file)
    || ['Cargo.toml', 'Cargo.lock', 'Anchor.toml', 'rust-toolchain.toml', 'package.json', 'package-lock.json', 'tsconfig.json', 'dashboard/package.json', 'dashboard/package-lock.json', 'dashboard/next.config.mjs'].includes(file))
  .sort();
const deletedSources = sourcePaths.filter(file => !existsSync(path.join(root, file)));
const manifest = {
  schema: 2,
  profile,
  repository: 'https://github.com/Sakura-11488/sakura-perps',
  commit: git('rev-parse', 'HEAD'),
  dirty_worktree: git('status', '--porcelain').length > 0,
  target: 'local-only; not an on-chain deployment attestation',
  rust: execFileSync('rustc', ['--version'], { encoding: 'utf8' }).trim(),
  ...(profile !== 'rooms' ? {
    anchor: execFileSync('anchor', ['--version'], { encoding: 'utf8' }).trim(),
    sbf_platform_tools: 'v1.54',
    sbf_builder: execFileSync('cargo-build-sbf', ['--version'], { encoding: 'utf8' }).trim(),
  } : {}),
  ...(profile !== 'program' ? {
    wasm_bindgen: execFileSync('wasm-bindgen', ['--version'], { encoding: 'utf8' }).trim(),
    wasm_target: 'wasm32-unknown-unknown',
  } : {}),
  node: process.version,
  artifacts: Object.fromEntries(artifacts.map(file => [file, hash(file)])),
  source_sha256: Object.fromEntries(sourcePaths.filter(file => !deletedSources.includes(file)).map(file => [file, hash(file)])),
  deleted_sources: deletedSources,
};
process.stdout.write(`${JSON.stringify(manifest, null, 2)}\n`);
