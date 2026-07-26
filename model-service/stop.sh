#!/usr/bin/env bash
set -euo pipefail

MODEL_SERVICE_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=common.sh
source "$MODEL_SERVICE_DIR/common.sh"

if [[ -f "$AUTOMIXER_MODEL_LAUNCH_AGENT_PATH" ]] || automixer_model_launchd_is_loaded; then
  exec "$MODEL_SERVICE_DIR/launchd.sh" stop
fi

if [[ ! -f "$AUTOMIXER_MODEL_PID_FILE" ]]; then
  echo "No manually managed model-server PID file was found."
  exit 0
fi

MODEL_PID="$(tr -d '[:space:]' < "$AUTOMIXER_MODEL_PID_FILE")"
if ! automixer_model_pid_is_ours "$MODEL_PID"; then
  echo "Ignoring stale PID file; PID $MODEL_PID is not an AutoMixer model server."
  rm -f "$AUTOMIXER_MODEL_PID_FILE"
  exit 0
fi

echo "Stopping model server (PID $MODEL_PID)..."
kill "$MODEL_PID"
for _ in {1..15}; do
  if ! kill -0 "$MODEL_PID" >/dev/null 2>&1; then
    rm -f "$AUTOMIXER_MODEL_PID_FILE"
    echo "Stopped."
    exit 0
  fi
  sleep 1
done

echo "The process did not stop gracefully; forcing PID $MODEL_PID."
kill -9 "$MODEL_PID"
rm -f "$AUTOMIXER_MODEL_PID_FILE"
