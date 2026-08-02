#!/usr/bin/env bash
# Run the configured model server in the foreground. launchd uses this entry
# point directly; use start.sh for a detached manual process.
set -euo pipefail

MODEL_SERVICE_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=common.sh
source "$MODEL_SERVICE_DIR/common.sh"

DRY_RUN=0
if [[ "${1:-}" == "--dry-run" ]]; then
  DRY_RUN=1
elif [[ $# -gt 0 ]]; then
  echo "Usage: $0 [--dry-run]" >&2
  exit 2
fi

if [[ "$AUTOMIXER_MODEL_RUNTIME" != "llama_cpp" ]]; then
  echo "Unsupported AUTOMIXER_MODEL_RUNTIME: $AUTOMIXER_MODEL_RUNTIME" >&2
  echo "AutoMixer's managed local model runtime is llama.cpp only." >&2
  exit 2
fi

if [[ ! -x "$AUTOMIXER_MODEL_LLAMA_SERVER_BIN" ]]; then
  echo "llama-server is not executable: $AUTOMIXER_MODEL_LLAMA_SERVER_BIN" >&2
  echo "Run AutoMixer's Setup Assistant or set AUTOMIXER_MODEL_LLAMA_SERVER_BIN in config.env." >&2
  exit 1
fi
if [[ ! -f "$AUTOMIXER_MODEL_LLAMA_FILE" ]]; then
  echo "GGUF model not found: $AUTOMIXER_MODEL_LLAMA_FILE" >&2
  echo "Run AutoMixer's Setup Assistant or set AUTOMIXER_MODEL_LLAMA_FILE in config.env." >&2
  exit 1
fi

LLAMA_ARGS=(
  --model "$AUTOMIXER_MODEL_LLAMA_FILE"
  --alias "$AUTOMIXER_MODEL_ALIASES"
  --host "$AUTOMIXER_MODEL_HOST"
  --port "$AUTOMIXER_MODEL_PORT"
  --ctx-size "$AUTOMIXER_MODEL_CONTEXT_SIZE"
  --parallel "$AUTOMIXER_MODEL_PARALLEL"
  --n-gpu-layers "$AUTOMIXER_MODEL_GPU_LAYERS"
  --cache-type-k "$AUTOMIXER_MODEL_CACHE_TYPE_K"
  --cache-type-v "$AUTOMIXER_MODEL_CACHE_TYPE_V"
  --cache-reuse "$AUTOMIXER_MODEL_CACHE_REUSE"
  --slot-save-path "$AUTOMIXER_MODEL_SLOT_SAVE_PATH"
  --jinja
)
if [[ "$AUTOMIXER_MODEL_FLASH_ATTN" == "1" ]]; then
  LLAMA_ARGS+=(--flash-attn on)
else
  LLAMA_ARGS+=(--flash-attn off)
fi
if [[ -n "$AUTOMIXER_MODEL_MMPROJ_FILE" ]]; then
  if [[ ! -f "$AUTOMIXER_MODEL_MMPROJ_FILE" ]]; then
    echo "Vision projector not found: $AUTOMIXER_MODEL_MMPROJ_FILE" >&2
    echo "Set AUTOMIXER_MODEL_MMPROJ_FILE=\"\" for text-only mode." >&2
    exit 1
  fi
  LLAMA_ARGS+=(--mmproj "$AUTOMIXER_MODEL_MMPROJ_FILE")
fi
LLAMA_ARGS+=(
  "${AUTOMIXER_MODEL_LLAMA_EXTRA_ARGS[@]+"${AUTOMIXER_MODEL_LLAMA_EXTRA_ARGS[@]}"}"
)

if [[ "$DRY_RUN" == "1" ]]; then
  automixer_model_print_command "$AUTOMIXER_MODEL_LLAMA_SERVER_BIN" "${LLAMA_ARGS[@]}"
  exit 0
fi
automixer_model_ensure_state_dir
mkdir -p "$AUTOMIXER_MODEL_SLOT_SAVE_PATH"
exec "$AUTOMIXER_MODEL_LLAMA_SERVER_BIN" "${LLAMA_ARGS[@]}"
