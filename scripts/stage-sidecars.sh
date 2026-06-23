#!/usr/bin/env bash
# Stage clean sidecar source (no venvs/caches/scratch) into src-tauri/resources/
# so Tauri can bundle it into the packaged app. The bundled source is read-only;
# the app copies it to a writable dir on first run and lets `uv` build the env
# there (see hermes_service::runnable_dir).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
STAGE="$ROOT/src-tauri/resources"

rm -rf "$STAGE"
mkdir -p "$STAGE"

copy_clean() {
  local src="$1" dst="$2"
  mkdir -p "$dst"
  rsync -a \
    --exclude '.venv' \
    --exclude '__pycache__' \
    --exclude '*.pyc' \
    --exclude '*.stderr' \
    --exclude '*.log' \
    --exclude '.pytest_cache' \
    --exclude 'demix' \
    --exclude 'spec' \
    "$src/" "$dst/"
}

copy_clean "$ROOT/audio-service"  "$STAGE/audio-service"
copy_clean "$ROOT/hermes-service" "$STAGE/hermes-service"

echo "staged sidecars -> $STAGE"
du -sh "$STAGE"/* 2>/dev/null || true
