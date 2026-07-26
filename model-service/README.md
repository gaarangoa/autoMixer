# AutoMixer model service

This directory tracks how AutoMixer's external language/vision model is served.
The model weights and runtime stay outside this repository; only reproducible
configuration and lifecycle scripts live here.

The current Mac deployment uses:

- Runtime: `llama.cpp` (`llama-server`)
- External files: `~/vLLM/models/`
- Model: `Qwen3.6-35B-A3B-UD-Q5_K_M.gguf`
- Vision projector: `mmproj-F16.gguf`
- Endpoint: `http://127.0.0.1:2261`
- API aliases: `qwen3.6-35b-a3b`, `qwythos-9b`
- Context: 122,880 tokens
- Apple Metal: all layers offloaded, q8 K/V cache

Despite the external folder being named `vLLM`, this Mac deployment uses
llama.cpp. Native vLLM is also supported by the runner for CUDA machines.

## Commands

From the repository root:

```bash
npm run models:status
npm run models:start
npm run models:stop
```

Preview the exact command without starting anything:

```bash
bash model-service/run.sh --dry-run
```

Logs and PID state live under `~/.automixer/model-server/`, not in Git.

## Start automatically after a Mac restart

Install the per-user LaunchAgent once:

```bash
npm run models:install
```

It starts the model server at login and restarts it after unexpected exits. No
`sudo` is required. If an independently started server is already using port
2261, installation records the LaunchAgent without loading it into the current
login session; it takes ownership automatically at the next login. Manage it
with:

```bash
bash model-service/launchd.sh status
bash model-service/launchd.sh restart
bash model-service/launchd.sh stop
bash model-service/launchd.sh uninstall
```

`stop` unloads it for the current login session; because the plist remains
installed, it starts again at the next login. `uninstall` removes that behavior.

## Machine-specific overrides

The checked-in defaults match this Mac. To use different paths, ports, models,
or the native vLLM backend:

```bash
cp model-service/config.env.example model-service/config.env
```

Edit `config.env`; it is intentionally ignored by Git.

For native vLLM on an NVIDIA/CUDA machine:

```bash
AUTOMIXER_MODEL_RUNTIME="vllm"
AUTOMIXER_MODEL_VLLM_BIN="/absolute/path/to/vllm"
AUTOMIXER_MODEL_VLLM_ID="Qwen/Qwen3.6-35B-A3B-FP8"
```

The same `start`, `status`, and LaunchAgent commands then invoke `vllm serve`
instead of `llama-server`.
