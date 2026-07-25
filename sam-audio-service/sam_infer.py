"""SAM-Audio inference wrapper.

Lazy-loads facebook/sam-audio-large on first use, keeps it warm, and unloads it
after an idle timeout to release GPU/host memory. Text-prompted separation
returns both the isolated sound (`target`) and everything else (`residual`).
"""
import gc
import threading
import time
from collections.abc import Callable

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


ProgressCallback = Callable[[dict], None]
CancelCallback = Callable[[], bool]


class SeparationCancelled(Exception):
    pass


class SamAudioEngine:
    """Thread-safe, lazy-loaded SAM-Audio holder with idle unloading."""

    def __init__(
        self,
        idle_unload_s: int = 600,
        chunk_seconds: float = 20.0,
        chunk_overlap_seconds: float = 2.0,
        predict_spans: bool = False,
    ):
        if chunk_seconds <= 0:
            raise ValueError("chunk_seconds must be positive")
        if chunk_overlap_seconds < 0 or chunk_overlap_seconds >= chunk_seconds:
            raise ValueError(
                "chunk overlap must be non-negative and smaller than chunk size"
            )
        self._lock = threading.Lock()
        self._inference_lock = threading.Lock()
        self._model = None
        self._proc = None
        self._device = None
        self._loading = False
        self._busy = False
        self._last_used = 0.0
        self._idle_unload_s = idle_unload_s
        self._load_seconds = None
        self._chunk_seconds = chunk_seconds
        self._chunk_overlap_seconds = chunk_overlap_seconds
        self._predict_spans = predict_spans
        t = threading.Thread(target=self._idle_watch, daemon=True)
        t.start()

    def _ensure_loaded(self):
        with self._lock:
            if self._model is None:
                self._loading = True
                try:
                    dev = _pick_device()
                    t0 = time.time()
                    model = SAMAudio.from_pretrained(MODEL_ID).to(dev).eval()
                    proc = SAMAudioProcessor.from_pretrained(MODEL_ID)
                    self._model, self._proc, self._device = model, proc, dev
                    self._load_seconds = round(time.time() - t0, 1)
                finally:
                    self._loading = False
            self._last_used = time.time()
            return self._model, self._proc, self._device

    def _idle_watch(self):
        while True:
            time.sleep(30)
            with self._lock:
                if (
                    self._model is not None
                    and not self._busy
                    and (time.time() - self._last_used) > self._idle_unload_s
                ):
                    self._model = None
                    self._proc = None
                    self._device = None
                    if torch.cuda.is_available():
                        torch.cuda.empty_cache()
                    gc.collect()

    def status(self) -> dict:
        return {
            "loaded": self._model is not None,
            "loading": self._loading,
            "busy": self._busy,
            "device": self._device,
            "load_seconds": self._load_seconds,
            "idle_unload_s": self._idle_unload_s,
            "chunk_seconds": self._chunk_seconds,
            "chunk_overlap_seconds": self._chunk_overlap_seconds,
            "predict_spans": self._predict_spans,
        }

    @staticmethod
    def _report(callback: ProgressCallback | None, **fields):
        if callback is not None:
            try:
                callback(fields)
            except Exception:
                pass

    @staticmethod
    def _fit_length(wav: torch.Tensor, samples: int) -> torch.Tensor:
        wav = wav.flatten()[:samples]
        if wav.numel() < samples:
            wav = torch.nn.functional.pad(wav, (0, samples - wav.numel()))
        return wav

    def separate(
        self,
        in_wav: str,
        description: str,
        out_target: str,
        out_residual: str | None = None,
        reranking: int = 0,
        progress: ProgressCallback | None = None,
        cancelled: CancelCallback | None = None,
    ) -> dict:
        """Isolate a prompt in bounded chunks and crossfade the resulting stems."""
        with self._inference_lock:
            self._busy = True
            try:
                if cancelled is not None and cancelled():
                    raise SeparationCancelled("separation cancelled")
                self._report(progress, phase="loading", progress=0.0)
                model, proc, dev = self._ensure_loaded()
                t0 = time.time()

                wav, sample_rate = torchaudio.load(in_wav)
                if sample_rate != proc.audio_sampling_rate:
                    wav = torchaudio.functional.resample(
                        wav, sample_rate, proc.audio_sampling_rate
                    )
                wav = wav.mean(dim=0, keepdim=True).contiguous()
                total_samples = wav.size(-1)
                if total_samples == 0:
                    raise ValueError("input audio is empty")
                chunk_samples = max(
                    1, round(self._chunk_seconds * proc.audio_sampling_rate)
                )
                overlap_samples = round(
                    self._chunk_overlap_seconds * proc.audio_sampling_rate
                )
                step_samples = chunk_samples - overlap_samples

                starts = []
                start = 0
                while True:
                    starts.append(start)
                    if start + chunk_samples >= total_samples:
                        break
                    start += step_samples

                target_sum = torch.zeros(total_samples, dtype=torch.float32)
                residual_sum = (
                    torch.zeros(total_samples, dtype=torch.float32)
                    if out_residual is not None
                    else None
                )
                weight_sum = torch.zeros(total_samples, dtype=torch.float32)
                kwargs = {"predict_spans": self._predict_spans}
                if reranking and reranking > 1:
                    kwargs["reranking_candidates"] = reranking

                for index, start in enumerate(starts):
                    if cancelled is not None and cancelled():
                        raise SeparationCancelled("separation cancelled")
                    end = min(start + chunk_samples, total_samples)
                    chunk = wav[:, start:end]
                    self._report(
                        progress,
                        phase="separating",
                        progress=index / len(starts),
                        chunk=index + 1,
                        chunks=len(starts),
                    )
                    inputs = proc(audios=[chunk], descriptions=[description]).to(dev)
                    with torch.inference_mode():
                        result = model.separate(inputs, **kwargs)

                    samples = end - start
                    target = self._fit_length(
                        result.target[0].detach().float().cpu(), samples
                    )
                    residual = None
                    if residual_sum is not None:
                        residual = self._fit_length(
                            result.residual[0].detach().float().cpu(), samples
                        )

                    weights = torch.ones(samples, dtype=torch.float32)
                    fade_samples = min(overlap_samples, samples)
                    if start > 0 and fade_samples:
                        weights[:fade_samples] = torch.linspace(0.0, 1.0, fade_samples)
                    if end < total_samples and fade_samples:
                        weights[-fade_samples:] *= torch.linspace(
                            1.0, 0.0, fade_samples
                        )
                    target_sum[start:end] += target * weights
                    if residual_sum is not None and residual is not None:
                        residual_sum[start:end] += residual * weights
                    weight_sum[start:end] += weights

                    del inputs, result, target, residual, chunk
                    if dev == "cuda":
                        torch.cuda.empty_cache()

                if cancelled is not None and cancelled():
                    raise SeparationCancelled("separation cancelled")
                self._report(progress, phase="writing", progress=1.0)
                weight_sum.clamp_min_(1e-8)
                torchaudio.save(
                    out_target,
                    (target_sum / weight_sum).unsqueeze(0),
                    proc.audio_sampling_rate,
                )
                if residual_sum is not None and out_residual is not None:
                    torchaudio.save(
                        out_residual,
                        (residual_sum / weight_sum).unsqueeze(0),
                        proc.audio_sampling_rate,
                    )
                self._last_used = time.time()
                return {
                    "sampling_rate": int(proc.audio_sampling_rate),
                    "separate_seconds": round(time.time() - t0, 1),
                    "device": dev,
                    "chunks": len(starts),
                    "chunk_seconds": self._chunk_seconds,
                    "predict_spans": self._predict_spans,
                }
            finally:
                self._busy = False
