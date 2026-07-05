"""Viability test: load SAM-Audio + run one text-prompt separation, measuring
load time, device (MPS vs CPU), and separation time on this Mac.

Usage: uv run python test_isolate.py <audio.wav> "<description>"
"""
import sys
import time

import torch
import torchaudio
from sam_audio import SAMAudio, SAMAudioProcessor

audio = sys.argv[1] if len(sys.argv) > 1 else "test.wav"
desc = sys.argv[2] if len(sys.argv) > 2 else "vocals"

# Prefer Metal (MPS) on Apple Silicon; fall back to CPU if unavailable.
if torch.backends.mps.is_available():
    device = "mps"
elif torch.cuda.is_available():
    device = "cuda"
else:
    device = "cpu"
print(f"[test] device = {device}")

t0 = time.time()
try:
    model = SAMAudio.from_pretrained("facebook/sam-audio-large").to(device).eval()
except Exception as e:
    print(f"[test] load on {device} failed ({e}); retrying on cpu")
    device = "cpu"
    model = SAMAudio.from_pretrained("facebook/sam-audio-large").to(device).eval()
processor = SAMAudioProcessor.from_pretrained("facebook/sam-audio-large")
print(f"[test] model load: {time.time() - t0:.1f}s  (sr={processor.audio_sampling_rate})")

t1 = time.time()
inputs = processor(audios=[audio], descriptions=[desc]).to(device)
with torch.inference_mode():
    result = model.separate(inputs, predict_spans=True)
sep = time.time() - t1
import soundfile as sf
info = sf.info(audio)
dur = info.frames / info.samplerate
print(f"[test] separate '{desc}': {sep:.1f}s for {dur:.1f}s audio  (RTF={sep/dur:.2f}x)")

torchaudio.save("target.wav", result.target[0].unsqueeze(0).cpu(), processor.audio_sampling_rate)
torchaudio.save("residual.wav", result.residual[0].unsqueeze(0).cpu(), processor.audio_sampling_rate)
print("[test] wrote target.wav + residual.wav")
