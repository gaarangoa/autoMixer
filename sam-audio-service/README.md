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
- `sam_infer.py` — SAM-Audio wrapper: lazy-load on first use, serialize inference jobs, split long inputs into overlapping chunks, and **unload after idle** (`SAM_IDLE_UNLOAD_S`, default 600s) so the model doesn't sit pinned.
- `deploy_to_spark.sh` — sync the source, prepare the CUDA `uv` environment, and restart the Spark service.
- `run_service.sh` — start or restart the remote service independently of the SSH session.
- `pyproject.toml` / `test_isolate.py` — local dev/reference (the Mac can't actually run the model).

## Deploy
```bash
./deploy_to_spark.sh                 # ship + build + run on spark-6a22:7330
```
Then point AutoMixer's SAM endpoint at `http://spark-6a22.local:7330`.

Long inputs default to 20-second chunks with a 2-second crossfade. Override with
`SAM_CHUNK_SECONDS` and `SAM_CHUNK_OVERLAP_SECONDS`. Span prediction is disabled
by default to keep memory bounded; set `SAM_PREDICT_SPANS=true` to enable it.
Completed, failed, and cancelled jobs are removed after `SAM_JOB_TTL_S` (one hour
by default). Clients may cancel a job or delete its artifacts immediately.

The gated model weights download into the Spark user's Hugging Face cache on the
first `/isolate` request and persist across service restarts.
