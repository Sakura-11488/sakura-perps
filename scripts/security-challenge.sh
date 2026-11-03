#!/usr/bin/env bash
# Local-only test entry point. Never reads a wallet, .env or RPC configuration.
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"
mode="${1:-paper}"
if [[ "$#" -gt 1 || ( "$mode" != paper && "$mode" != full ) ]]; then
  echo "Usage: scripts/security-challenge.sh [paper|full]" >&2
  exit 2
fi
for executable in cargo rustc; do
  command -v "$executable" >/dev/null || { echo "Missing $executable; install Rust 1.89.0 through rustup." >&2; exit 1; }
done
if [[ "$(rustc --version)" != 'rustc 1.89.0 '* ]]; then
  echo "Use the pinned Rust 1.89.0 compiler; put rustup's cargo AND rustc shims first in PATH." >&2
  exit 1
fi

echo 'Sakura security challenge: local simulation; no live transactions or real funds.'
cargo test -p sakura-perps-rooms --locked
cargo run -p sakura-perps-rooms --example community_season --locked
cargo test -p sakura-perps-rooms-auth --locked
cargo run -p sakura-perps-rooms-auth --example signed_room --locked
if [[ "$mode" == paper ]]; then
  echo 'Paper checks passed. Use full mode for the compiled Solana program and service tests.'
  exit 0
fi

for executable in node npm anchor cargo-build-sbf; do
  command -v "$executable" >/dev/null || { echo "Missing $executable; see docs/security-challenge.md prerequisites." >&2; exit 1; }
done
node -e 'if (Number(process.versions.node.split(".")[0]) < 22) { console.error("Node 22 or newer is required"); process.exit(1); }'
if [[ "$(anchor --version)" != 'anchor-cli 1.1.2' ]]; then
  echo 'Anchor CLI 1.1.2 is required to generate the matching client ABI.' >&2
  exit 1
fi

mkdir -p target/idl target/types target/security-challenge
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test -p sakura-perps-risk --locked
cargo test -p sakura-perps --lib --locked
cargo-build-sbf --manifest-path programs/sakura-perps/Cargo.toml --tools-version v1.54 -- --locked
anchor idl build --out target/idl/sakura_perps.json --out-ts target/types/sakura_perps.ts -- --locked
diff -u idl/sakura_perps.json target/idl/sakura_perps.json
diff -u idl/sakura_perps.ts target/types/sakura_perps.ts
SAKURA_PERPS_SO="$repo_root/target/deploy/sakura_perps.so" cargo test -p sakura-perps-svm-tests --locked

# Dependency installation may access registries; tests use mocks and local SVM.
# Do not replace these commands with anchor test or tests/devnet-*.ts.
npm ci --ignore-scripts
npm exec -- tsc --noEmit
npm run test:keeper
npm --prefix dashboard ci --ignore-scripts
npm --prefix dashboard test

node scripts/challenge-manifest.mjs > target/security-challenge/manifest.json
echo 'Full local checks passed. Build/source manifest: target/security-challenge/manifest.json'
