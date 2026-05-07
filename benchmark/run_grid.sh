#!/bin/zsh
# Run the benchmark sequentially across general-purpose Ollama models.
# Skips vision/medical models. Continues on failure.

set -u
cd "$(dirname "$0")"

MODELS=(
  "hf.co/unsloth/Qwen3-1.7B-GGUF:Q4_K_M"
  "qwen3.5:latest"
  "hf.co/unsloth/Qwen3-14B-GGUF:Q4_K_M"
  "gpt-oss:20b"
  "qwen3.6:latest"
  "gemma4-31b:latest"
  "gpt-oss:120b"
)

mkdir -p logs
for m in "${MODELS[@]}"; do
  safe=$(echo "$m" | sed 's/[^a-zA-Z0-9.-]/_/g')
  echo "=== Starting $m at $(date) ===" | tee -a logs/grid.log
  start=$(date +%s)
  MODEL="$m" node run.mjs > "logs/$safe.log" 2>&1
  end=$(date +%s)
  echo "=== Finished $m in $((end-start))s at $(date) ===" | tee -a logs/grid.log
done
echo "=== Grid complete at $(date) ===" | tee -a logs/grid.log
