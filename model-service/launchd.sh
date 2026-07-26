#!/usr/bin/env bash
# Install and manage a per-user LaunchAgent. It starts at login and launchd
# restarts it after unexpected exits. No sudo is required.
set -euo pipefail

MODEL_SERVICE_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=common.sh
source "$MODEL_SERVICE_DIR/common.sh"

COMMAND="${1:-status}"
LAUNCHD_DOMAIN="$(automixer_model_launchd_domain)"
LAUNCHD_SERVICE="$LAUNCHD_DOMAIN/$AUTOMIXER_MODEL_LAUNCHD_LABEL"
PLIST_TEMPLATE="$MODEL_SERVICE_DIR/com.automixer.model-server.plist.in"

xml_escape() {
  printf '%s' "$1" | sed \
    -e 's/&/\&amp;/g' \
    -e 's/</\&lt;/g' \
    -e 's/>/\&gt;/g' \
    -e 's/"/\&quot;/g' \
    -e "s/'/\&apos;/g"
}

render_plist() {
  local temp_plist="$1"
  local run_script state_dir stdout_log stderr_log working_dir

  run_script="$(xml_escape "$MODEL_SERVICE_DIR/run.sh")"
  state_dir="$(xml_escape "$AUTOMIXER_MODEL_STATE_DIR")"
  stdout_log="$(xml_escape "$AUTOMIXER_MODEL_LOG_FILE")"
  stderr_log="$(xml_escape "$AUTOMIXER_MODEL_ERROR_LOG_FILE")"
  working_dir="$(xml_escape "$(cd "$MODEL_SERVICE_DIR/.." && pwd)")"

  sed \
    -e "s|__LABEL__|$AUTOMIXER_MODEL_LAUNCHD_LABEL|g" \
    -e "s|__RUN_SCRIPT__|$run_script|g" \
    -e "s|__WORKING_DIRECTORY__|$working_dir|g" \
    -e "s|__STATE_DIRECTORY__|$state_dir|g" \
    -e "s|__STDOUT_LOG__|$stdout_log|g" \
    -e "s|__STDERR_LOG__|$stderr_log|g" \
    "$PLIST_TEMPLATE" > "$temp_plist"
}

case "$COMMAND" in
  install)
    automixer_model_ensure_state_dir
    mkdir -p "$(dirname "$AUTOMIXER_MODEL_LAUNCH_AGENT_PATH")"
    TEMP_PLIST="$(mktemp -t automixer-model-server.XXXXXX.plist)"
    trap 'rm -f "$TEMP_PLIST"' EXIT
    render_plist "$TEMP_PLIST"
    plutil -lint "$TEMP_PLIST"
    install -m 0644 "$TEMP_PLIST" "$AUTOMIXER_MODEL_LAUNCH_AGENT_PATH"
    launchctl enable "$LAUNCHD_SERVICE"

    if ! automixer_model_launchd_is_loaded && automixer_model_is_healthy; then
      echo "Installed $AUTOMIXER_MODEL_LAUNCHD_LABEL."
      echo "An unmanaged server already owns $(automixer_model_base_url), so the LaunchAgent was not loaded now."
      echo "It will start automatically at the next login after that process exits during shutdown."
      exit 0
    fi

    launchctl bootout "$LAUNCHD_SERVICE" >/dev/null 2>&1 || true
    launchctl bootstrap "$LAUNCHD_DOMAIN" "$AUTOMIXER_MODEL_LAUNCH_AGENT_PATH"
    launchctl kickstart -k "$LAUNCHD_SERVICE"
    echo "Installed and started $AUTOMIXER_MODEL_LAUNCHD_LABEL."
    echo "It will start automatically when this user logs in."
    ;;

  uninstall)
    launchctl bootout "$LAUNCHD_SERVICE" >/dev/null 2>&1 || true
    rm -f "$AUTOMIXER_MODEL_LAUNCH_AGENT_PATH"
    echo "Uninstalled $AUTOMIXER_MODEL_LAUNCHD_LABEL."
    ;;

  start)
    if [[ ! -f "$AUTOMIXER_MODEL_LAUNCH_AGENT_PATH" ]]; then
      echo "LaunchAgent is not installed. Run: $0 install" >&2
      exit 1
    fi
    if automixer_model_is_healthy; then
      if automixer_model_launchd_is_loaded; then
        echo "$AUTOMIXER_MODEL_LAUNCHD_LABEL is already healthy at $(automixer_model_base_url)."
      else
        echo "A server is already healthy at $(automixer_model_base_url)."
        echo "The installed LaunchAgent remains unloaded until the next login."
      fi
      exit 0
    fi
    if ! automixer_model_launchd_is_loaded; then
      launchctl bootstrap "$LAUNCHD_DOMAIN" "$AUTOMIXER_MODEL_LAUNCH_AGENT_PATH"
    fi
    launchctl enable "$LAUNCHD_SERVICE"
    launchctl kickstart -k "$LAUNCHD_SERVICE"
    echo "Started $AUTOMIXER_MODEL_LAUNCHD_LABEL."
    ;;

  stop)
    launchctl bootout "$LAUNCHD_SERVICE" >/dev/null 2>&1 || true
    echo "Stopped $AUTOMIXER_MODEL_LAUNCHD_LABEL."
    echo "The plist remains installed and will load again at the next login."
    ;;

  restart)
    "$0" stop
    "$0" start
    ;;

  status)
    if automixer_model_launchd_is_loaded; then
      launchctl print "$LAUNCHD_SERVICE" | sed -n '1,80p'
    elif [[ -f "$AUTOMIXER_MODEL_LAUNCH_AGENT_PATH" ]]; then
      echo "Installed but not loaded: $AUTOMIXER_MODEL_LAUNCH_AGENT_PATH"
      exit 1
    else
      echo "Not installed. Run: $0 install"
      exit 1
    fi
    ;;

  *)
    echo "Usage: $0 {install|uninstall|start|stop|restart|status}" >&2
    exit 2
    ;;
esac
