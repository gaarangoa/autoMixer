# Clean Mac installation

The turnkey AutoMixer package currently targets Apple Silicon Macs. It does not
use Apple Developer ID signing or notarization; the app has only an ad-hoc local
signature.

## Install the release

1. Copy the generated `AutoMixer_*.dmg` to the new Mac and open it.
2. Drag **AutoMixer** into **Applications**.
3. On the first launch, Control-click AutoMixer in Applications and choose
   **Open**, then confirm **Open**. If macOS still blocks it, go to **System
   Settings → Privacy & Security** and choose **Open Anyway** for AutoMixer.
4. Complete the Setup Assistant.

No Homebrew, system Python, uv, FFmpeg, Ollama, vLLM, or LM Studio installation
is needed.

## Choose a model source

### Remote endpoint

Enter the base URL and model name for an OpenAI-compatible model server on the
network. An optional API key can be stored in AutoMixer's private Hermes agent
configuration. Setup tests the real agent path before it succeeds.

SAM-Audio is not installed by this workflow. If used, configure its remote URL
later in AutoMixer Settings.

### Local llama.cpp

Setup installs the bundled llama.cpp runtime under `~/.automixer`, downloads the
Qwen3.6 GGUF plus vision projector (about 27.4 GB decimal / 25.5 GiB), verifies
their SHA-256 hashes, and installs the per-user
`com.automixer.model-server` LaunchAgent. No administrator password is needed.
The server listens only on `127.0.0.1:2261`.

Interrupted downloads stay as `.part` files and resume the next time setup is
run. A matching legacy download in `~/vLLM/models` is verified and adopted
instead of downloaded again. The included model is best suited to a Mac with at
least 48 GB of unified memory.

## Managed files

AutoMixer keeps writable runtimes and logs outside the application bundle:

```text
~/.automixer/
  setup.json
  python/
  cache/uv/
  hermes-agent/venv/
  hermes-home/
  sidecars/
  model-runtime/llama.cpp/
  model-service/
  models/
  model-server/
```

The local-mode LaunchAgent is stored at:

```text
~/Library/LaunchAgents/com.automixer.model-server.plist
```

## Reopen setup and diagnose

Open **Settings → Installation & runtime → Open Setup Assistant** to switch
model sources or repair a managed runtime. Logs are available at:

```text
~/.automixer/audio-service.log
~/.automixer/hermes-service.log
~/.automixer/model-server/model-server.log
~/.automixer/model-server/model-server.error.log
```

Cancelling local setup is safe: partial model downloads are deliberately kept
for resume.
