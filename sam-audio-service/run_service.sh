#!/usr/bin/env bash
# Start (or restart) this service ON the Spark, fully detached from the ssh
# session. Usage: run_service.sh <service-dir> <port>
set -euo pipefail
DIR="$1"
PORT="$2"
cd "$HOME/$DIR"

export PATH="$HOME/.local/bin:$PATH"
export HF_HOME="$HOME/.cache/huggingface"
# HF_TOKEN puede venir del entorno (el deploy lo pasa) o de ~/.hf_token
if [ -z "${HF_TOKEN:-}" ] && [ -f "$HOME/.hf_token" ]; then
  export HF_TOKEN="$(cat "$HOME/.hf_token")"
fi
export HUGGING_FACE_HUB_TOKEN="${HF_TOKEN:-}"

NVLIBS="$(.venv/bin/python -c "import glob,site; p=site.getsitepackages()[0]; print(':'.join(glob.glob(p+'/nvidia/*/lib')))")"
export LD_LIBRARY_PATH="$NVLIBS:/usr/local/cuda/lib64:/usr/lib/aarch64-linux-gnu"

pkill -9 -f "uvicorn app:app.*--port $PORT" 2>/dev/null || true
sleep 1
# setsid + </dev/null: survives the ssh session ending.
setsid nohup .venv/bin/uvicorn app:app --host 0.0.0.0 --port "$PORT" \
  > server.log 2>&1 < /dev/null &
for _ in {1..15}; do
  if curl -fsS --max-time 3 "http://127.0.0.1:$PORT/health"; then
    echo " OK"
    exit 0
  fi
  sleep 2
done

echo "-- service did not become healthy; log tail:"
tail -10 server.log
exit 1
