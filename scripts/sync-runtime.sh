#!/bin/sh
# Builds the renderer runtime and copies the result where the application embeds it.
set -eu
cd "$(dirname "$0")/.."
(cd packages/runtime && pnpm install --frozen-lockfile && pnpm build)
cp packages/runtime/dist/loader.js packages/runtime/dist/loader.css crates/ghostify-app/assets/
echo "runtime synced"
