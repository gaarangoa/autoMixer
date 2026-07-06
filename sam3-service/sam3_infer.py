"""SAM 3 video segmentation wrapper.

Lazy-loads facebook/sam3 (build_sam3_video_predictor), keeps it warm, unloads
after idle. One job = text-prompted segmentation of a whole video:
  start_session(video) -> add_prompt(text, frame 0) -> propagate_in_video (stream)
Collects a per-frame UNION mask of all matched objects and renders:
  mask.mp4    - grayscale matte (white = object)
  cutout.mp4  - the object(s) over black
  overlay.mp4 - original with the object(s) tinted green (QC)
  meta.json   - frames, fps, object count, timing
"""
import json
import os
import subprocess
import threading
import time

import numpy as np

MODEL_LOCK = threading.Lock()
# One segmentation at a time: the sam3 predictor is not proven thread-safe, and
# two concurrent sessions interleaving handle_request calls would corrupt state.
JOB_LOCK = threading.Lock()


def _probe(path: str) -> dict:
    out = subprocess.run(
        ["ffprobe", "-v", "error", "-select_streams", "v:0",
         "-show_entries", "stream=width,height,r_frame_rate,nb_frames",
         "-show_entries", "format=duration", "-of", "json", path],
        capture_output=True, text=True, check=True,
    )
    data = json.loads(out.stdout)
    stream = data["streams"][0]
    num, den = (stream.get("r_frame_rate") or "30/1").split("/")
    fps = float(num) / max(1.0, float(den))
    duration = float(data.get("format", {}).get("duration") or 0.0)
    nb = int(stream.get("nb_frames") or 0) or max(1, int(round(duration * fps)))
    return {"width": int(stream["width"]), "height": int(stream["height"]), "fps": fps, "frames": nb}


def _to_bool_mask(value) -> np.ndarray | None:
    """Best-effort conversion of one object's mask payload to a 2D bool array."""
    try:
        import torch
        if isinstance(value, torch.Tensor):
            arr = value.detach().to("cpu").numpy()
        else:
            arr = value
    except Exception:
        arr = value
    if isinstance(arr, np.ndarray):
        arr = np.squeeze(arr)
        if arr.ndim == 3:
            if arr.shape[0] == 0:
                return None  # frame sin detecciones
            # (N objetos, H, W) → unión de todos los objetos
            arr = arr.max(axis=0)
        if arr.ndim == 2:
            return arr > 0.5 if arr.dtype != np.bool_ else arr
        return None
    if isinstance(arr, dict) and "counts" in arr and "size" in arr:
        # COCO RLE
        try:
            from pycocotools import mask as mask_utils
            return mask_utils.decode(arr).astype(bool)
        except Exception:
            return None
    return None


def _collect_frame_masks(payload, sink: dict[int, np.ndarray], default_index: int | None = None):
    """Walk an outputs payload defensively, unioning every mask found into sink."""
    if payload is None:
        return
    if isinstance(payload, (list, tuple)):
        for item in payload:
            _collect_frame_masks(item, sink, default_index)
        return
    if not isinstance(payload, dict):
        return
    index = payload.get("frame_index", payload.get("frame_idx", default_index))
    # Direct mask-bearing keys first.
    for key in ("out_binary_masks", "masks", "mask", "masklets", "rle_masks", "pred_masks", "results", "outputs", "objects"):
        if key in payload:
            value = payload[key]
            if isinstance(value, dict) and not ("counts" in value and "size" in value):
                # e.g. {object_id: mask}
                for sub in value.values():
                    _union_into(sink, index, sub)
            elif isinstance(value, (list, tuple)):
                for sub in value:
                    if isinstance(sub, dict):
                        _collect_frame_masks(sub, sink, index)
                    else:
                        _union_into(sink, index, sub)
            else:
                _union_into(sink, index, value)


def _union_into(sink: dict[int, np.ndarray], index, value):
    mask = _to_bool_mask(value)
    if mask is None or index is None:
        return
    index = int(index)
    if index in sink:
        existing = sink[index]
        if existing.shape == mask.shape:
            sink[index] = existing | mask
    else:
        sink[index] = mask


class Sam3Engine:
    """Thread-safe lazy holder for the SAM 3 video predictor."""

    def __init__(self, idle_unload_s: int = 900):
        self._predictor = None
        self._last_used = 0.0
        self._idle_unload_s = idle_unload_s
        self._load_seconds = None
        threading.Thread(target=self._idle_watch, daemon=True).start()

    def _ensure(self):
        with MODEL_LOCK:
            if self._predictor is None:
                from sam3.model_builder import build_sam3_video_predictor
                t0 = time.time()
                self._predictor = build_sam3_video_predictor()
                self._load_seconds = round(time.time() - t0, 1)
            self._last_used = time.time()
            return self._predictor

    def _idle_watch(self):
        while True:
            time.sleep(60)
            with MODEL_LOCK:
                if self._predictor is not None and (time.time() - self._last_used) > self._idle_unload_s:
                    self._predictor = None
                    try:
                        import torch
                        torch.cuda.empty_cache()
                    except Exception:
                        pass

    def status(self) -> dict:
        return {"loaded": self._predictor is not None, "loadSeconds": self._load_seconds}

    def segment(self, video_path: str, prompt: str, out_dir: str,
                want_cutout: bool = True, want_overlay: bool = True,
                background_path: str | None = None) -> dict:
        with JOB_LOCK:
            return self._segment_locked(video_path, prompt, out_dir, want_cutout, want_overlay, background_path)

    def _segment_locked(self, video_path: str, prompt: str, out_dir: str,
                        want_cutout: bool, want_overlay: bool,
                        background_path: str | None) -> dict:
        predictor = self._ensure()
        info = _probe(video_path)
        t0 = time.time()

        import torch
        with torch.autocast("cuda", dtype=torch.bfloat16):
            return self._run_session(predictor, video_path, prompt, out_dir,
                                     want_cutout, want_overlay, background_path, info, t0)

    def _run_session(self, predictor, video_path, prompt, out_dir,
                     want_cutout, want_overlay, background_path, info, t0):
        response = predictor.handle_request(request=dict(type="start_session", resource_path=video_path))
        session_id = response["session_id"]
        masks: dict[int, np.ndarray] = {}
        try:
            response = predictor.handle_request(request=dict(
                type="add_prompt", session_id=session_id, frame_index=0, text=prompt,
            ))
            _collect_frame_masks(response.get("outputs"), masks, default_index=0)
            # Log the first response's structure once — output schemas vary between
            # sam3 revisions, and this makes fixing the walker trivial.
            try:
                summary = {k: type(v).__name__ for k, v in (response.get("outputs") or {}).items()} \
                    if isinstance(response.get("outputs"), dict) else type(response.get("outputs")).__name__
                print(f"[sam3] add_prompt outputs schema: {summary}", flush=True)
            except Exception:
                pass
            logged_stream = False
            for item in predictor.handle_stream_request(request=dict(
                type="propagate_in_video", session_id=session_id,
            )):
                if isinstance(item, dict):
                    if not logged_stream:
                        logged_stream = True
                        try:
                            print(f"[sam3] stream item keys: { {k: type(v).__name__ for k, v in item.items()} }", flush=True)
                        except Exception:
                            pass
                    payload = item.get("outputs", item)
                    index = item.get("frame_index", item.get("frame_idx"))
                    if index is None and isinstance(payload, dict):
                        index = payload.get("frame_index", payload.get("frame_idx"))
                    _collect_frame_masks(payload, masks, index)
        finally:
            try:
                predictor.handle_request(request=dict(type="close_session", session_id=session_id))
            except Exception:
                pass

        self._last_used = time.time()
        segment_seconds = round(time.time() - t0, 1)
        if not masks:
            raise RuntimeError(f"SAM 3 found no '{prompt}' in the video (no masks returned).")

        # ---- Render mask.mp4 (grayscale matte) --------------------------------
        width, height, fps, total = info["width"], info["height"], info["fps"], info["frames"]
        mask_path = os.path.join(out_dir, "mask.mp4")
        writer = subprocess.Popen(
            ["ffmpeg", "-y", "-hide_banner", "-loglevel", "error",
             "-f", "rawvideo", "-pixel_format", "gray", "-video_size", f"{width}x{height}",
             "-framerate", f"{fps:.6f}", "-i", "pipe:0",
             "-c:v", "libx264", "-preset", "veryfast", "-crf", "12", "-pix_fmt", "yuv420p", mask_path],
            stdin=subprocess.PIPE,
        )
        last = np.zeros((height, width), dtype=np.uint8)
        matched = 0
        for i in range(total):
            m = masks.get(i)
            if m is not None:
                if m.shape != (height, width):
                    m = _resize_bool(m, width, height)
                last = (m.astype(np.uint8)) * 255
                matched += 1
            writer.stdin.write(last.tobytes())
        writer.stdin.close()
        if writer.wait() != 0:
            raise RuntimeError("mask encode failed")

        outputs = {"mask": "mask.mp4"}
        # ---- cutout.mp4: object over black ------------------------------------
        if want_cutout:
            cut = os.path.join(out_dir, "cutout.mp4")
            duration = total / max(0.01, fps)
            subprocess.run(
                ["ffmpeg", "-y", "-hide_banner", "-loglevel", "error",
                 "-i", video_path, "-i", mask_path,
                 "-filter_complex",
                 # color= es una fuente INFINITA: sin d= el encode nunca termina
                 # (con -shortest solo no basta cuando el input no trae audio).
                 f"color=black:s={width}x{height}:r={fps:.6f}:d={duration:.3f}[bg];"
                 f"[1:v]format=gray[m];[bg][0:v][m]maskedmerge=shortest=1,format=yuv420p[v]",
                 "-map", "[v]", "-map", "0:a?", "-c:a", "copy", "-shortest",
                 "-t", f"{duration:.3f}",
                 "-c:v", "libx264", "-preset", "veryfast", "-crf", "18", cut],
                check=True,
            )
            outputs["cutout"] = "cutout.mp4"
        # ---- overlay.mp4: green-tinted object on the original (QC) ------------
        if want_overlay:
            over = os.path.join(out_dir, "overlay.mp4")
            subprocess.run(
                ["ffmpeg", "-y", "-hide_banner", "-loglevel", "error",
                 "-i", video_path, "-i", mask_path,
                 "-filter_complex",
                 "[0:v]split[a][b];[b]colorchannelmixer=rr=0.35:gg=1.0:bb=0.35[green];"
                 "[1:v]format=gray[m];[a][green][m]maskedmerge,format=yuv420p[v]",
                 "-map", "[v]", "-an",
                 "-c:v", "libx264", "-preset", "veryfast", "-crf", "20", over],
                check=True,
            )
            outputs["overlay"] = "overlay.mp4"

        # ---- composite.mp4: subject over a REPLACEMENT background ------------
        if background_path:
            comp = os.path.join(out_dir, "composite.mp4")
            subprocess.run(
                ["ffmpeg", "-y", "-hide_banner", "-loglevel", "error",
                 "-i", video_path, "-i", mask_path,
                 "-loop", "1", "-i", background_path,
                 "-filter_complex",
                 (
                     # Background image scaled to cover the frame; mask lightly
                     # feathered so the subject's edges blend instead of cutting hard.
                     f"[2:v]scale={width}:{height}:force_original_aspect_ratio=increase,"
                     f"crop={width}:{height},setsar=1[bg];"
                     "[1:v]format=gray,gblur=sigma=1.5[m];"
                     "[bg][0:v][m]maskedmerge=shortest=1,format=yuv420p[v]"
                 ),
                 "-map", "[v]", "-map", "0:a?", "-c:a", "copy", "-shortest",
                 "-t", f"{total / max(0.01, fps):.3f}",
                 "-c:v", "libx264", "-preset", "veryfast", "-crf", "18", comp],
                check=True,
            )
            outputs["composite"] = "composite.mp4"

        meta = {
            "prompt": prompt,
            "frames": total,
            "matchedFrames": matched,
            "fps": fps,
            "width": width,
            "height": height,
            "segmentSeconds": segment_seconds,
            "outputs": outputs,
        }
        with open(os.path.join(out_dir, "meta.json"), "w") as f:
            json.dump(meta, f)
        return meta


def _resize_bool(mask: np.ndarray, width: int, height: int) -> np.ndarray:
    """Nearest-neighbor resize without extra deps."""
    ys = (np.linspace(0, mask.shape[0] - 1, height)).astype(int)
    xs = (np.linspace(0, mask.shape[1] - 1, width)).astype(int)
    return mask[np.ix_(ys, xs)]
