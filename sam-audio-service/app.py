"""SAM-Audio source-isolation sidecar (HTTP).

Runs on the CUDA box (the Spark). The AutoMixer app POSTs an audio file + a text
description; the service isolates that sound in the background and serves back the
`target` (isolated) and `residual` (everything else) stems.

Async job model so the request returns instantly and the app polls for progress.
"""
import os
import threading
import traceback
import uuid

from fastapi import FastAPI, File, Form, HTTPException, UploadFile
from fastapi.responses import FileResponse

from sam_infer import SamAudioEngine

JOB_DIR = os.environ.get("SAM_JOB_DIR", "/tmp/sam-jobs")
os.makedirs(JOB_DIR, exist_ok=True)

ENGINE = SamAudioEngine(idle_unload_s=int(os.environ.get("SAM_IDLE_UNLOAD_S", "600")))
JOBS: dict[str, dict] = {}

app = FastAPI(title="sam-audio-service")


@app.get("/health")
def health():
    return {"status": "ok", **ENGINE.status()}


def _run_job(job_id: str, in_path: str, description: str, want_residual: bool, reranking: int):
    job = JOBS[job_id]
    try:
        job["status"] = "running"
        target = os.path.join(JOB_DIR, f"{job_id}-target.wav")
        residual = os.path.join(JOB_DIR, f"{job_id}-residual.wav") if want_residual else None
        meta = ENGINE.separate(in_path, description, target, residual, reranking)
        job.update(status="done", target=target, residual=residual, **meta)
    except Exception as e:  # noqa: BLE001 - surface any failure to the client
        job.update(status="error", error=f"{e}\n{traceback.format_exc()}")
    finally:
        try:
            os.remove(in_path)
        except OSError:
            pass


@app.post("/isolate")
async def isolate(
    file: UploadFile = File(...),
    description: str = Form(...),
    residual: bool = Form(False),
    reranking: int = Form(0),
):
    job_id = uuid.uuid4().hex
    in_path = os.path.join(JOB_DIR, f"{job_id}-in.wav")
    with open(in_path, "wb") as f:
        f.write(await file.read())
    JOBS[job_id] = {"status": "queued", "description": description}
    threading.Thread(
        target=_run_job, args=(job_id, in_path, description, residual, reranking), daemon=True
    ).start()
    return {"job_id": job_id}


@app.get("/jobs/{job_id}")
def job_status(job_id: str):
    j = JOBS.get(job_id)
    if not j:
        raise HTTPException(404, "unknown job")
    out = {k: v for k, v in j.items() if k not in ("target", "residual")}
    out["has_target"] = bool(j.get("target"))
    out["has_residual"] = bool(j.get("residual"))
    return out


@app.get("/jobs/{job_id}/target.wav")
def job_target(job_id: str):
    j = JOBS.get(job_id)
    if not j or j.get("status") != "done" or not j.get("target"):
        raise HTTPException(404, "target not ready")
    return FileResponse(j["target"], media_type="audio/wav", filename="target.wav")


@app.get("/jobs/{job_id}/residual.wav")
def job_residual(job_id: str):
    j = JOBS.get(job_id)
    if not j or not j.get("residual"):
        raise HTTPException(404, "residual not available")
    return FileResponse(j["residual"], media_type="audio/wav", filename="residual.wav")
