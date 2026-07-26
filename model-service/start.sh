#!/usr/bin/env bash
# Start the model server manually. When the LaunchAgent is installed, this
# delegates to launchd so there is only one owner of the process.
set -euo pipefail

MODEL_SERVICE_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=common.sh
source "$MODEL_SERVICE_DIR/common.sh"

if automixer_model_is_healthy; then
  echo "Model server is already healthy at $(automixer_model_base_url)."
  exit 0
fi

if [[ -f "$AUTOMIXER_MODEL_LAUNCH_AGENT_PATH" ]]; then
  exec "$MODEL_SERVICE_DIR/launchd.sh" start
fi

if [[ -f "$AUTOMIXER_MODEL_PID_FILE" ]]; then
  EXISTING_PID="$(tr -d '[:space:]' < "$AUTOMIXER_MODEL_PID_FILE")"
  if automixer_model_pid_is_ours "$EXISTING_PID"; then
    echo "Model server process $EXISTING_PID is still starting."
    echo "Log: $AUTOMIXER_MODEL_LOG_FILE"
    exit 0
  fi
  rm -f "$AUTOMIXER_MODEL_PID_FILE"
fi

automixer_model_ensure_state_dir
mkdir -p "$AUTOMIXER_MODEL_SLOT_SAVE_PATH"

echo "Starting $AUTOMIXER_MODEL_RUNTIME model server at $(automixer_model_base_url)..."
nohup "$MODEL_SERVICE_DIR/run.sh" >>"$AUTOMIXER_MODEL_LOG_FILE" 2>>"$AUTOMIXER_MODEL_ERROR_LOG_FILE" &
MODEL_PID=$!
printf '%s\n' "$MODEL_PID" > "$AUTOMIXER_MODEL_PID_FILE"

for _ in {1..180}; do
  if automixer_model_is_healthy; then
    echo "Model server is ready (PID $MODEL_PID)."
    echo "Log: $AUTOMIXER_MODEL_LOG_FILE"
    exit 0
  fi
  if ! automixer_model_pid_is_ours "$MODEL_PID"; then
    echo "Model server exited before becoming healthy." >&2
    tail -n 40 "$AUTOMIXER_MODEL_ERROR_LOG_FILE" "$AUTOMIXER_MODEL_LOG_FILE" 2>/dev/null || true
    exit 1
  fi
  sleep 1
done

echo "Timed out waiting for $(automixer_model_base_url)/health." >&2
echo "Inspect: $AUTOMIXER_MODEL_LOG_FILE" >&2
exit 1
