"""SAM-Audio inference wrapper.

Lazy-loads facebook/sam-audio-large on first use, keeps it warm, and unloads it
after an idle timeout to release GPU/host memory. Text-prompted separation
returns both the isolated sound (`target`) and everything else (`residual`).
"""
import threading
import time

import torch
import torchaudio
from sam_audio import SAMAudio, SAMAudioProcessor

MODEL_ID = "facebook/sam-audio-large"


def _pick_device() -> str:
    if torch.cuda.is_available():
        return "cuda"
    if getattr(torch.backends, "mps", None) and torch.backends.mps.is_available():
        return "mps"
    return "cpu"


class SamAudioEngine:
    """Thread-safe, lazy-loaded SAM-Audio holder with idle unloading."""

    def __init__(self, idle_unload_s: int = 600):
        self._lock = threading.Lock()
        self._model = None
        self._proc = None
        self._device = None
        self._last_used = 0.0
        self._idle_unload_s = idle_unload_s
        self._load_seconds = None
        t = threading.Thread(target=self._idle_watch, daemon=True)
        t.start()

    def _ensure_loaded(self):
        with self._lock:
            if self._model is None:
                dev = _pick_device()
                t0 = time.time()
                model = SAMAudio.from_pretrained(MODEL_ID).to(dev).eval()
                proc = SAMAudioProcessor.from_pretrained(MODEL_ID)
                self._model, self._proc, self._device = model, proc, dev
                self._load_seconds = round(time.time() - t0, 1)
            self._last_used = time.time()
            return self._model, self._proc, self._device

    def _idle_watch(self):
        while True:
            time.sleep(30)
            with self._lock:
                if self._model is not None and (time.time() - self._last_used) > self._idle_unload_s:
                    self._model = None
                    self._proc = None
                    self._device = None
                    if torch.cuda.is_available():
                        torch.cuda.empty_cache()

    def status(self) -> dict:
        return {
            "loaded": self._model is not None,
            "device": self._device,
            "load_seconds": self._load_seconds,
            "idle_unload_s": self._idle_unload_s,
        }

    def separate(self, in_wav: str, description: str, out_target: str,
                 out_residual: str | None = None, reranking: int = 0) -> dict:
        """Isolate `description` from `in_wav`. Writes target (and optional residual)."""
        model, proc, dev = self._ensure_loaded()
        t0 = time.time()
        inputs = proc(audios=[in_wav], descriptions=[description]).to(dev)
        kwargs = {"predict_spans": True}
        if reranking and reranking > 1:
            kwargs["reranking_candidates"] = reranking
        with torch.inference_mode():
            result = model.separate(inputs, **kwargs)
        torchaudio.save(out_target, result.target[0].unsqueeze(0).cpu(), proc.audio_sampling_rate)
        if out_residual is not None:
            torchaudio.save(out_residual, result.residual[0].unsqueeze(0).cpu(), proc.audio_sampling_rate)
        self._last_used = time.time()
        return {
            "sampling_rate": int(proc.audio_sampling_rate),
            "separate_seconds": round(time.time() - t0, 1),
            "device": dev,
        }
