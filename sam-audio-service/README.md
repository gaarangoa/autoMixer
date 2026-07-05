# sam-audio-service

SAM-Audio source-isolation sidecar for AutoMixer. Select a track, ask the agent
to "isolate the vocals / the cough / the piano", and it returns a new stem.

Backed by [`facebook/sam-audio-large`](https://huggingface.co/facebook/sam-audio-large)
(promptable sound separation). Text prompting returns **`target`** (the isolated
sound) and **`residual`** (everything else) — so "isolate X" and "remove X" are both free.

## Why it runs on the Spark (not the Mac)
SAM-Audio depends on `xformers` (CUDA-only; no macOS wheel, won't compile on Apple
clang) and `decord`. So it runs on the **CUDA box (the Spark, GB10/aarch64)** inside
the **NVIDIA NGC PyTorch container**, which ships torch + CUDA + xformers prebuilt.
The Mac AutoMixer app calls this service over the network.

This directory is the **canonical source** — deploy copies it to the Spark.

## Files
- `app.py` — FastAPI service: `/health`, `POST /isolate` (multipart: file, description, residual, reranking) → `{job_id}`, `GET /jobs/{id}`, `GET /jobs/{id}/target.wav`, `/residual.wav`. Async jobs.
- `sam_infer.py` — SAM-Audio wrapper: lazy-load on first use, keep warm, **unload after idle** (`SAM_IDLE_UNLOAD_S`, default 600s) so the ~15 GB model doesn't sit pinned.
- `Dockerfile` — `FROM nvcr.io/nvidia/pytorch` + sam-audio + service deps.
- `deploy_to_spark.sh` — rsync → docker build → run on the Spark.
- `pyproject.toml` / `test_isolate.py` — local dev/reference (the Mac can't actually run the model).

## Deploy
```bash
./deploy_to_spark.sh                 # ship + build + run on spark-6a22:7330
```
Then point AutoMixer's SAM endpoint at `http://spark-6a22.local:7330`.

The model weights (gated, ~15 GB) download on first `/isolate` into the mounted
`/weights/hf` volume on the Spark (persists across container restarts).
