# AutoMixer audio service

Local sidecar that runs music-structure analysis (`all-in-one`) for the
AutoMixer Tauri app.

## Run for development

```sh
cd audio-service
uv sync
uv run uvicorn main:app --host 127.0.0.1 --port 7321
```

The Tauri app spawns this process automatically when it starts. You only need
to run it manually if you want to debug it standalone.

## Endpoints

- `GET /health` — `{ ok, service, device }` (device is `cuda` / `mps` / `cpu`)
- `POST /analyze/structure` — body `{ "wav_path": "/abs/path.wav" }` → bpm,
  beats, downbeats, sections.

Results are cached under `~/.automixer/audio-cache/` keyed by file content +
mtime, so the same mix is only ever analyzed once.

## First run

The first analysis pulls down the `allin1` checkpoints + `htdemucs` weights
(~150–200 MB). On Apple Silicon it auto-selects the MPS GPU; on Linux+NVIDIA
it picks CUDA; otherwise CPU.
