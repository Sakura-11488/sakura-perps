#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
if [[ "$(wasm-bindgen --version)" != 'wasm-bindgen 0.2.126' ]]; then
  echo 'Install wasm-bindgen-cli 0.2.126; see integrations/sakura-rooms/README.md.' >&2
  exit 1
fi
cargo build -p sakura-perps-rooms-web --target wasm32-unknown-unknown --release --locked
mkdir -p target/rooms-web
wasm-bindgen target/wasm32-unknown-unknown/release/sakura_perps_rooms_web.wasm \
  --out-dir target/rooms-web --target no-modules --out-name sakura_rooms \
  --no-modules-global sakura_rooms_engine
node scripts/package-rooms.mjs engine
