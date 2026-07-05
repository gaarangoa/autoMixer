#!/usr/bin/env bash
# Ship + run the SAM-Audio service on the Spark (CUDA box) under uv — no Docker.
# Encodes every platform fix we had to discover for aarch64 + Blackwell + CUDA 13.
#
#   ./deploy_to_spark.sh
#
# Prereq (one-time, needs your password on the Spark):  sudo apt-get install -y ffmpeg
# Env overrides: SPARK_HOST (default spark-6a22), SAM_PORT (default 7330).
set -euo pipefail

HOST="${SPARK_HOST:-spark-6a22}"
REMOTE="sam-audio-service"
PORT="${SAM_PORT:-7330}"
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

TOKEN="$(python3 -c "import os;[print(l.split('=',1)[1].strip().strip(chr(34)).strip(chr(39))) for l in open(os.path.expanduser('~/.env')) if l.split('=',1)[0].strip().upper()=='HF_TOKEN']" 2>/dev/null | head -1)"
[ -z "$TOKEN" ] && echo "WARN: no HF_TOKEN in ~/.env — gated weight download will fail" >&2

echo "==> ensure uv on $HOST"
ssh "$HOST" "command -v uv >/dev/null || curl -LsSf https://astral.sh/uv/install.sh | sh"

echo "==> sync code to $HOST:~/$REMOTE"
ssh "$HOST" "mkdir -p ~/$REMOTE"
rsync -az "$DIR/app.py" "$DIR/sam_infer.py" "$DIR/pyproject.toml" "$DIR/.python-version" "$HOST:~/$REMOTE/"

echo "==> uv sync (torch cu130 + build xformers for sm_121)"
ssh "$HOST" "cd ~/$REMOTE && export PATH=\$HOME/.local/bin:/usr/local/cuda/bin:\$PATH CUDA_HOME=/usr/local/cuda TORCH_CUDA_ARCH_LIST=12.1 MAX_JOBS=8 && uv sync"

echo "==> fix: force-install the REAL nvidia cuda libs (consolidated nvidia-cu13 ships metadata only)"
ssh "$HOST" "cd ~/$REMOTE && export PATH=\$HOME/.local/bin:\$PATH && \
  DEPS=\$(.venv/bin/python -c \"import importlib.metadata as m; print(' '.join(r.split(';')[0].split('>')[0].split('=')[0].split('<')[0].strip() for r in (m.requires('torch') or []) if r.lower().startswith('nvidia')))\") && \
  uv pip install --reinstall --no-deps \$DEPS"

echo "==> fix: patch sam_audio for new huggingface_hub (optional proxies/resume_download)"
ssh "$HOST" "F=~/$REMOTE/.venv/lib/python3.11/site-packages/sam_audio/model/base.py; \
  sed -i 's/        proxies: Optional\[Dict\],/        proxies: Optional[Dict] = None,/; s/        resume_download: bool,/        resume_download: bool = False,/' \$F"

echo "==> (re)start service on port $PORT (LD_LIBRARY_PATH → bundled nvidia + cuda + system ffmpeg)"
ssh "$HOST" "cd ~/$REMOTE && export PATH=\$HOME/.local/bin:\$PATH HF_TOKEN='${TOKEN}' && \
  NVLIBS=\$(.venv/bin/python -c \"import glob,site; p=site.getsitepackages()[0]; print(':'.join(glob.glob(p+'/nvidia/*/lib')))\") && \
  export LD_LIBRARY_PATH=\$NVLIBS:/usr/local/cuda/lib64:/usr/lib/aarch64-linux-gnu && \
  pkill -f 'uvicorn app:app' 2>/dev/null || true; sleep 1; \
  nohup uv run uvicorn app:app --host 0.0.0.0 --port $PORT > ~/$REMOTE/server.log 2>&1 & \
  sleep 4; tail -4 ~/$REMOTE/server.log"

echo "==> health:"; sleep 2
ssh "$HOST" "curl -fsS http://127.0.0.1:${PORT}/health" || true
echo ""
echo "Reachable from the Mac at: http://${HOST}.local:${PORT}"
