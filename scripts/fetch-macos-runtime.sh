#!/usr/bin/env bash
# Fetch the pinned macOS/Apple-Silicon runtime used by the packaged app. Every
# archive is verified before extraction so release builds never package an
# unchecked executable.
set -euo pipefail

export LC_ALL=C

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEST="${1:-$ROOT/src-tauri/resources/runtime}"
CACHE="$ROOT/.runtime-cache/macos-arm64"

if [[ "$(uname -s)" != "Darwin" || "$(uname -m)" != "arm64" ]]; then
  echo "managed runtime staging currently supports macOS arm64 only" >&2
  exit 1
fi

UV_VERSION="0.12.1"
LLAMA_VERSION="b9740"
FFMPEG_VERSION="b6.1.1"

UV_ARCHIVE="$CACHE/uv-aarch64-apple-darwin.tar.gz"
LLAMA_ARCHIVE="$CACHE/llama-$LLAMA_VERSION-bin-macos-arm64.tar.gz"
FFMPEG_ARCHIVE="$CACHE/ffmpeg-darwin-arm64.gz"
FFPROBE_ARCHIVE="$CACHE/ffprobe-darwin-arm64.gz"

mkdir -p "$CACHE"

sha256_file() {
  /usr/bin/shasum -a 256 "$1" | /usr/bin/awk '{print $1}'
}

fetch_checked() {
  local url="$1" expected="$2" output="$3" actual=""
  if [[ -f "$output" ]]; then
    actual="$(sha256_file "$output")"
  fi
  if [[ "$actual" != "$expected" ]]; then
    rm -f "$output.part"
    echo "downloading $(basename "$output")"
    /usr/bin/curl --fail --location --retry 3 --progress-bar "$url" -o "$output.part"
    actual="$(sha256_file "$output.part")"
    if [[ "$actual" != "$expected" ]]; then
      rm -f "$output.part"
      echo "checksum mismatch for $(basename "$output"): expected $expected, got $actual" >&2
      exit 1
    fi
    mv "$output.part" "$output"
  fi
}

fetch_checked \
  "https://github.com/astral-sh/uv/releases/download/$UV_VERSION/uv-aarch64-apple-darwin.tar.gz" \
  "77d2906988e8074fd43f2f329ec452ebbf9b0c257ba1c66451c71de70a6baf42" \
  "$UV_ARCHIVE"
fetch_checked \
  "https://github.com/eugeneware/ffmpeg-static/releases/download/$FFMPEG_VERSION/ffmpeg-darwin-arm64.gz" \
  "8923876afa8db5585022d7860ec7e589af192f441c56793971276d450ed3bbfa" \
  "$FFMPEG_ARCHIVE"
fetch_checked \
  "https://github.com/eugeneware/ffmpeg-static/releases/download/$FFMPEG_VERSION/ffprobe-darwin-arm64.gz" \
  "d986a8ec7b030899fe66a8a288ed809a3543338705a3ce178cfb85869c5d80be" \
  "$FFPROBE_ARCHIVE"
fetch_checked \
  "https://github.com/ggml-org/llama.cpp/releases/download/$LLAMA_VERSION/llama-$LLAMA_VERSION-bin-macos-arm64.tar.gz" \
  "cc976ae81bd5716d1b55efc5de14327c41957be2d6b7bf767f15ea965e61e8d1" \
  "$LLAMA_ARCHIVE"

TEMP_DIR="$(mktemp -d /tmp/automixer-runtime.XXXXXX)"
trap 'rm -rf "$TEMP_DIR"' EXIT
rm -rf "$DEST"
mkdir -p "$DEST/bin" "$DEST/llama.cpp"

/usr/bin/tar -xzf "$UV_ARCHIVE" -C "$TEMP_DIR"
UV_BIN="$(find "$TEMP_DIR" -type f -name uv -perm -111 -print -quit)"
if [[ -z "$UV_BIN" ]]; then
  echo "uv archive did not contain an executable" >&2
  exit 1
fi
/bin/cp "$UV_BIN" "$DEST/bin/uv"
/usr/bin/gunzip -c "$FFMPEG_ARCHIVE" > "$DEST/bin/ffmpeg"
/usr/bin/gunzip -c "$FFPROBE_ARCHIVE" > "$DEST/bin/ffprobe"
/bin/chmod 0755 "$DEST/bin/uv" "$DEST/bin/ffmpeg" "$DEST/bin/ffprobe"

rm -rf "$TEMP_DIR/llama"
mkdir -p "$TEMP_DIR/llama"
/usr/bin/tar -xzf "$LLAMA_ARCHIVE" -C "$TEMP_DIR/llama"
LLAMA_SERVER="$(find "$TEMP_DIR/llama" -type f -name llama-server -perm -111 -print -quit)"
if [[ -z "$LLAMA_SERVER" ]]; then
  echo "llama.cpp archive did not contain llama-server" >&2
  exit 1
fi
LLAMA_BIN_DIR="$(dirname "$LLAMA_SERVER")"
/bin/cp -R "$LLAMA_BIN_DIR/." "$DEST/llama.cpp/"
/bin/chmod 0755 "$DEST/llama.cpp/llama-server"

cat > "$DEST/manifest.json" <<EOF
{
  "platform": "macos-arm64",
  "uv": "$UV_VERSION",
  "ffmpeg": "6.0 (ffmpeg-static b6.1.1)",
  "llamaCpp": "$LLAMA_VERSION",
  "hermesAgent": "0.17.0"
}
EOF

cat > "$DEST/THIRD_PARTY_NOTICES.txt" <<'EOF'
AutoMixer managed runtime notices

uv is Copyright Astral Software Inc. and contributors, licensed under Apache-2.0 OR MIT.
https://github.com/astral-sh/uv

FFmpeg and FFprobe are distributed from eugeneware/ffmpeg-static under GPL-3.0-or-later.
Corresponding source/build provenance:
https://github.com/eugeneware/ffmpeg-static/tree/b6.1.1
https://ffmpeg.org/

llama.cpp is Copyright the llama.cpp contributors, licensed under MIT.
https://github.com/ggml-org/llama.cpp

Hermes Agent is Copyright Nous Research and contributors, licensed under MIT.
It is installed on demand from hermes-agent[acp]==0.17.0.
https://github.com/NousResearch/hermes-agent
EOF

echo "staged managed runtime -> $DEST"
du -sh "$DEST"/* 2>/dev/null || true
