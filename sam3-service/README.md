# sam3-service

SAM 3 (Segment Anything 3) **video segmentation** sidecar. Text-prompted: send a
video + "the guitarist" / "the red car" and it detects, segments and **tracks
every matching object through the whole video**.

Runs on the Spark (CUDA GB10) — same platform recipe as `sam-audio-service/`
(cu130 torch, compiled xformers, real nvidia libs, LD_LIBRARY_PATH, ffmpeg).
This directory is the **canonical source**; `deploy_to_spark.sh` ships it.

> Standalone service — NOT wired into autoMixer (yet).

## API (port 7331)
- `GET  /health` — `{status, loaded, loadSeconds}`
- `POST /segment` — multipart: `file` (video), `prompt` (text), `cutout`/`overlay` (bools, default true) → `{job_id}`
- `GET  /jobs/{id}` — status/meta (`queued|running|done|error`, frames, matchedFrames, segmentSeconds)
- `GET  /jobs/{id}/mask.mp4` — grayscale matte (white = matched objects)
- `GET  /jobs/{id}/cutout.mp4` — objects over black (keeps source audio)
- `GET  /jobs/{id}/overlay.mp4` — original with objects tinted green (QC)
- `DELETE /jobs/{id}` — cleanup

## Files
- `app.py` — FastAPI job server (async jobs, poll + download).
- `sam3_infer.py` — lazy engine: `build_sam3_video_predictor()` →
  `start_session` → `add_prompt(text)` → `propagate_in_video` (stream), unions all
  object masks per frame, renders outputs with ffmpeg. Idle-unloads after 15 min.
  The mask-payload walker is defensive (tensor / ndarray / COCO-RLE) and logs the
  first response's schema to server.log — if a sam3 revision changes the output
  shape, adjust `_collect_frame_masks` from that log line.
- `pyproject.toml` — sam3 from git + cu130 torch (Spark) + pycocotools.
- `deploy_to_spark.sh` — rsync → uv sync → platform fixes → run on :7331.

## Deploy + smoke test
```bash
./deploy_to_spark.sh
# then, from the Mac:
curl -F "file=@clip.mp4" -F "prompt=the person" http://spark-6a22.local:7331/segment
curl http://spark-6a22.local:7331/jobs/<id>
curl -O http://spark-6a22.local:7331/jobs/<id>/overlay.mp4
```

Weights: `facebook/sam3` (gated, ~6.9 GB) — auto-downloaded on first load using
`HF_TOKEN` (exported by the deploy script from `~/.env`).
