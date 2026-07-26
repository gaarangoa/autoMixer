#!/usr/bin/env bash
set -euo pipefail

MODEL_SERVICE_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=common.sh
source "$MODEL_SERVICE_DIR/common.sh"

echo "Runtime:  $AUTOMIXER_MODEL_RUNTIME"
echo "Endpoint: $(automixer_model_base_url)"
if automixer_model_launchd_is_loaded; then
  echo "launchd:  loaded ($AUTOMIXER_MODEL_LAUNCHD_LABEL)"
elif [[ -f "$AUTOMIXER_MODEL_LAUNCH_AGENT_PATH" ]]; then
  echo "launchd:  installed but not loaded"
else
  echo "launchd:  not installed"
fi

if ! automixer_model_is_healthy; then
  echo "Health:   DOWN"
  exit 1
fi

echo "Health:   OK"
echo "Models:"
/usr/bin/curl --max-time 5 --fail --silent "$(automixer_model_base_url)/v1/models" \
  | /usr/bin/python3 -m json.tool
