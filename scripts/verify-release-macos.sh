#!/usr/bin/env bash
# Read-only sanity check for the distributable Apple-Silicon application.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP="${1:-$ROOT/src-tauri/target/release/bundle/macos/AutoMixer.app}"
RESOURCES="$APP/Contents/Resources"

if [[ ! -d "$APP" ]]; then
  echo "release app not found: $APP" >&2
  exit 1
fi

require_executable() {
  local path="$1"
  if [[ ! -x "$path" ]]; then
    echo "missing executable: $path" >&2
    exit 1
  fi
}

require_file() {
  local path="$1"
  if [[ ! -f "$path" ]]; then
    echo "missing file: $path" >&2
    exit 1
  fi
}

require_executable "$APP/Contents/MacOS/automixer"
require_executable "$RESOURCES/runtime/bin/uv"
require_executable "$RESOURCES/runtime/bin/ffmpeg"
require_executable "$RESOURCES/runtime/bin/ffprobe"
require_executable "$RESOURCES/runtime/llama.cpp/llama-server"
require_file "$RESOURCES/runtime/manifest.json"
require_file "$RESOURCES/runtime/THIRD_PARTY_NOTICES.txt"
require_file "$RESOURCES/hermes-service/pyproject.toml"
require_file "$RESOURCES/audio-service/pyproject.toml"
require_file "$RESOURCES/model-service/run.sh"

/usr/bin/file "$APP/Contents/MacOS/automixer" | /usr/bin/grep -q "arm64"
"$RESOURCES/runtime/bin/uv" --version
"$RESOURCES/runtime/bin/ffmpeg" -version 2>&1 | /usr/bin/head -n 1
"$RESOURCES/runtime/bin/ffprobe" -version 2>&1 | /usr/bin/head -n 1
"$RESOURCES/runtime/llama.cpp/llama-server" --version 2>&1 | /usr/bin/head -n 1
/usr/bin/codesign --verify --deep --strict "$APP"

echo "release sanity check passed: $APP"
