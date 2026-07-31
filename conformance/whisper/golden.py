# /// script
# requires-python = ">=3.11"
# dependencies = ["faster-whisper==1.2.1"]
# ///
"""Reference transcription of a fixture WAV via pinned faster-whisper.

faster-whisper (CTranslate2) is the pinned oracle for whisper large-v3-turbo
— the exact segment text and boundaries our Rust `Transcriber` (whisper.cpp
via whisper-rs) must approximate. faster-whisper has no torch dependency, so
it isn't exposed to torch's MPS auto-selection bug that corrupted the
text-encoder oracle on CI's virtualized Metal in this phase's PR 2 — but
`device="cpu"` is set anyway (CTranslate2 also has a CoreML/GPU path) to pin
the oracle's execution device explicitly rather than rely on its default.

Usage: uv run conformance/whisper/golden.py --audio fixture.wav --out golden.json
"""

import argparse
import json

import faster_whisper
from faster_whisper import WhisperModel


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--audio", required=True)
    parser.add_argument("--out", required=True)
    args = parser.parse_args()
    model = WhisperModel("large-v3-turbo", device="cpu", compute_type="int8")  # pin device, see module docstring
    segments, info = model.transcribe(args.audio)
    rows = [
        {"start_ms": int(s.start * 1000), "end_ms": int(s.end * 1000), "text": s.text}
        for s in segments
    ]
    print(f"detected language: {info.language} (p={info.language_probability:.3f})")
    print(f"segments: {len(rows)}")
    out = {
        "meta": {
            "model": "large-v3-turbo",
            "faster_whisper": faster_whisper.__version__,
            "device": "cpu",
            "compute_type": "int8",
        },
        "segments": rows,
    }
    with open(args.out, "w", encoding="utf-8") as handle:
        json.dump(out, handle)
    print(f"golden transcript -> {args.out}")


if __name__ == "__main__":
    main()
