#!/usr/bin/env bash

# Shared defaults for AutoMixer's external model server. Machine-specific
# overrides belong in model-service/config.env (ignored by Git).

AUTOMIXER_MODEL_SERVICE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
AUTOMIXER_MODEL_CONFIG_FILE="${AUTOMIXER_MODEL_CONFIG_FILE:-$AUTOMIXER_MODEL_SERVICE_DIR/config.env}"

if [[ -f "$AUTOMIXER_MODEL_CONFIG_FILE" ]]; then
  # shellcheck disable=SC1090
  source "$AUTOMIXER_MODEL_CONFIG_FILE"
fi

AUTOMIXER_MODEL_RUNTIME="${AUTOMIXER_MODEL_RUNTIME:-llama_cpp}"
AUTOMIXER_MODEL_ROOT="${AUTOMIXER_MODEL_ROOT:-$HOME/vLLM}"
AUTOMIXER_MODEL_HOST="${AUTOMIXER_MODEL_HOST:-127.0.0.1}"
AUTOMIXER_MODEL_PORT="${AUTOMIXER_MODEL_PORT:-2261}"
AUTOMIXER_MODEL_ALIASES="${AUTOMIXER_MODEL_ALIASES:-qwen3.6-35b-a3b,qwythos-9b}"
AUTOMIXER_MODEL_CONTEXT_SIZE="${AUTOMIXER_MODEL_CONTEXT_SIZE:-122880}"

AUTOMIXER_MODEL_LLAMA_SERVER_BIN="${AUTOMIXER_MODEL_LLAMA_SERVER_BIN:-/opt/homebrew/bin/llama-server}"
AUTOMIXER_MODEL_LLAMA_FILE="${AUTOMIXER_MODEL_LLAMA_FILE:-$AUTOMIXER_MODEL_ROOT/models/Qwen3.6-35B-A3B-UD-Q5_K_M.gguf}"
AUTOMIXER_MODEL_MMPROJ_FILE="${AUTOMIXER_MODEL_MMPROJ_FILE:-$AUTOMIXER_MODEL_ROOT/models/mmproj-F16.gguf}"
AUTOMIXER_MODEL_GPU_LAYERS="${AUTOMIXER_MODEL_GPU_LAYERS:-99}"
AUTOMIXER_MODEL_PARALLEL="${AUTOMIXER_MODEL_PARALLEL:-1}"
AUTOMIXER_MODEL_CACHE_TYPE_K="${AUTOMIXER_MODEL_CACHE_TYPE_K:-q8_0}"
AUTOMIXER_MODEL_CACHE_TYPE_V="${AUTOMIXER_MODEL_CACHE_TYPE_V:-q8_0}"
AUTOMIXER_MODEL_CACHE_REUSE="${AUTOMIXER_MODEL_CACHE_REUSE:-256}"
AUTOMIXER_MODEL_SLOT_SAVE_PATH="${AUTOMIXER_MODEL_SLOT_SAVE_PATH:-$AUTOMIXER_MODEL_ROOT/kv_cache}"
AUTOMIXER_MODEL_FLASH_ATTN="${AUTOMIXER_MODEL_FLASH_ATTN:-1}"

AUTOMIXER_MODEL_STATE_DIR="${AUTOMIXER_MODEL_STATE_DIR:-$HOME/.automixer/model-server}"
AUTOMIXER_MODEL_PID_FILE="${AUTOMIXER_MODEL_PID_FILE:-$AUTOMIXER_MODEL_STATE_DIR/model-server.pid}"
AUTOMIXER_MODEL_LOG_FILE="${AUTOMIXER_MODEL_LOG_FILE:-$AUTOMIXER_MODEL_STATE_DIR/model-server.log}"
AUTOMIXER_MODEL_ERROR_LOG_FILE="${AUTOMIXER_MODEL_ERROR_LOG_FILE:-$AUTOMIXER_MODEL_STATE_DIR/model-server.error.log}"

AUTOMIXER_MODEL_LAUNCHD_LABEL="${AUTOMIXER_MODEL_LAUNCHD_LABEL:-com.automixer.model-server}"
AUTOMIXER_MODEL_LAUNCH_AGENT_PATH="${AUTOMIXER_MODEL_LAUNCH_AGENT_PATH:-$HOME/Library/LaunchAgents/$AUTOMIXER_MODEL_LAUNCHD_LABEL.plist}"

if ! declare -p AUTOMIXER_MODEL_LLAMA_EXTRA_ARGS >/dev/null 2>&1; then
  declare -a AUTOMIXER_MODEL_LLAMA_EXTRA_ARGS=()
fi
automixer_model_base_url() {
  printf 'http://%s:%s' "$AUTOMIXER_MODEL_HOST" "$AUTOMIXER_MODEL_PORT"
}

automixer_model_ensure_state_dir() {
  mkdir -p "$AUTOMIXER_MODEL_STATE_DIR"
}

automixer_model_is_healthy() {
  /usr/bin/curl --max-time 2 --fail --silent "$(automixer_model_base_url)/health" >/dev/null 2>&1
}

automixer_model_launchd_domain() {
  printf 'gui/%s' "$(id -u)"
}

automixer_model_launchd_is_loaded() {
  launchctl print "$(automixer_model_launchd_domain)/$AUTOMIXER_MODEL_LAUNCHD_LABEL" >/dev/null 2>&1
}

automixer_model_pid_is_ours() {
  local model_pid="$1"
  local model_command

  [[ "$model_pid" =~ ^[0-9]+$ ]] || return 1
  kill -0 "$model_pid" >/dev/null 2>&1 || return 1
  model_command="$(ps -p "$model_pid" -o command= 2>/dev/null || true)"

  # The detached starter records the PID just before run.sh replaces itself
  # with llama-server. Accept that short, well-scoped startup window.
  if [[ "$model_command" == *"$AUTOMIXER_MODEL_SERVICE_DIR/run.sh"* ]]; then
    return 0
  fi

  [[ "$AUTOMIXER_MODEL_RUNTIME" == "llama_cpp" &&
    "$model_command" == *"$AUTOMIXER_MODEL_LLAMA_SERVER_BIN"* &&
    "$model_command" == *"$AUTOMIXER_MODEL_LLAMA_FILE"* &&
    "$model_command" == *"--port $AUTOMIXER_MODEL_PORT"* ]]
}

automixer_model_print_command() {
  printf '  '
  printf '%q ' "$@"
  printf '\n'
}
