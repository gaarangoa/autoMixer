"""Offline smoke test for the audio-service analysis path.

Usage:
    uv run python smoke_test.py <path/to/audio.wav>

Bypasses FastAPI and calls the analyzer directly so any failure surfaces
its real traceback in the terminal.
"""

from __future__ import annotations

import json
import sys
import time
from pathlib import Path

from main import _device, analyze_structure, StructureRequest


def main(wav: str) -> None:
    print(f"device: {_device()}")
    print(f"input:  {wav}")
    p = Path(wav)
    if not p.exists():
        print(f"!! file not found: {wav}")
        sys.exit(2)
    t0 = time.perf_counter()
    result = analyze_structure(StructureRequest(wav_path=str(p.resolve())))
    elapsed = time.perf_counter() - t0
    if hasattr(result, "model_dump"):
        body = result.model_dump()
    else:
        body = result
    print(f"elapsed: {elapsed:.1f}s")
    print(f"bpm: {body['bpm']:.1f}")
    print(f"beats: {len(body['beats'])}")
    print(f"downbeats: {len(body['downbeats'])}")
    print("sections:")
    for s in body["sections"]:
        print(f"  {s['start']:7.2f} -> {s['end']:7.2f}  {s['label']}")
    print("\n--- raw json (first 400 chars) ---")
    print(json.dumps(body)[:400])


if __name__ == "__main__":
    if len(sys.argv) != 2:
        print("usage: uv run python smoke_test.py <wav>", file=sys.stderr)
        sys.exit(1)
    main(sys.argv[1])
