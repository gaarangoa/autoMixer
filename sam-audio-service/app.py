"""SAM-Audio source-isolation sidecar (HTTP).

Runs on the CUDA box (the Spark). The AutoMixer app POSTs an audio file + a text
description; the service isolates that sound in the background and serves back the
`target` (isolated) and `residual` (everything else) stems.

Async job model so the request returns instantly and the app polls for progress.
"""
import os
import threading
import time
import traceback
import uuid

from fastapi import FastAPI, File, Form, HTTPException, UploadFile
from fastapi.responses import FileResponse

from sam_infer import SamAudioEngine, SeparationCancelled

JOB_DIR = os.environ.get("SAM_JOB_DIR", "/tmp/sam-jobs")
os.makedirs(JOB_DIR, exist_ok=True)

ENGINE = SamAudioEngine(
    idle_unload_s=int(os.environ.get("SAM_IDLE_UNLOAD_S", "600")),
    chunk_seconds=float(os.environ.get("SAM_CHUNK_SECONDS", "20")),
    chunk_overlap_seconds=float(os.environ.get("SAM_CHUNK_OVERLAP_SECONDS", "2")),
    predict_spans=os.environ.get("SAM_PREDICT_SPANS", "false").lower()
    in {"1", "true", "yes", "on"},
)
JOBS: dict[str, dict] = {}
JOB_TTL_S = int(os.environ.get("SAM_JOB_TTL_S", "3600"))

app = FastAPI(title="sam-audio-service")


def _delete_job_files(job: dict):
    for key in ("target", "residual", "_input"):
        path = job.get(key)
        if path:
            try:
                os.remove(path)
            except OSError:
                pass


def _cleanup_jobs():
    while True:
        time.sleep(60)
        cutoff = time.time() - JOB_TTL_S
        for job_id, job in list(JOBS.items()):
            finished_at = job.get("_finished_at")
            if job.get("status") in {"done", "error", "cancelled"} and finished_at and finished_at < cutoff:
                removed = JOBS.pop(job_id, None)
                if removed:
                    _delete_job_files(removed)


threading.Thread(target=_cleanup_jobs, daemon=True).start()


@app.get("/health")
def health():
    return {"status": "ok", **ENGINE.status()}


def _run_job(job_id: str, in_path: str, description: str, want_residual: bool, reranking: int):
    job = JOBS[job_id]
    try:
        job.update(status="running", phase="waiting", progress=0.0)
        target = os.path.join(JOB_DIR, f"{job_id}-target.wav")
        residual = os.path.join(JOB_DIR, f"{job_id}-residual.wav") if want_residual else None
        meta = ENGINE.separate(
            in_path,
            description,
            target,
            residual,
            reranking,
            progress=job.update,
            cancelled=lambda: bool(job.get("cancel_requested")),
        )
        job.update(
            status="done",
            phase="done",
            progress=1.0,
            target=target,
            residual=residual,
            **meta,
        )
    except SeparationCancelled:
        job.update(status="cancelled", phase="cancelled", progress=job.get("progress", 0.0))
    except Exception as e:  # noqa: BLE001 - surface any failure to the client
        job.update(
            status="error",
            phase="error",
            error=f"{e}\n{traceback.format_exc()}",
        )
    finally:
        job["_finished_at"] = time.time()
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
    JOBS[job_id] = {
        "status": "queued",
        "phase": "queued",
        "progress": 0.0,
        "description": description,
        "_input": in_path,
        "cancel_requested": False,
    }
    threading.Thread(
        target=_run_job, args=(job_id, in_path, description, residual, reranking), daemon=True
    ).start()
    return {"job_id": job_id}


@app.get("/jobs/{job_id}")
def job_status(job_id: str):
    j = JOBS.get(job_id)
    if not j:
        raise HTTPException(404, "unknown job")
    out = {
        k: v
        for k, v in j.items()
        if k not in ("target", "residual") and not k.startswith("_")
    }
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


@app.post("/jobs/{job_id}/cancel")
def cancel_job(job_id: str):
    job = JOBS.get(job_id)
    if not job:
        raise HTTPException(404, "unknown job")
    if job.get("status") in {"done", "error", "cancelled"}:
        return {"status": job.get("status")}
    job["cancel_requested"] = True
    job["phase"] = "cancelling"
    return {"status": "cancelling"}


@app.delete("/jobs/{job_id}")
def delete_job(job_id: str):
    job = JOBS.get(job_id)
    if not job:
        return {"deleted": False}
    if job.get("status") not in {"done", "error", "cancelled"}:
        raise HTTPException(409, "job is still running")
    JOBS.pop(job_id, None)
    _delete_job_files(job)
    return {"deleted": True}
